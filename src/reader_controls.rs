use super::{
    App, Drag, Gesture, MainWindow, Point, Prompt, SidebarMode, TabId, absolute, color_value,
    title, with_ui,
};
use crate::{
    storage::{Mode, Zoom},
    worker::Command,
};
use slint::{ComponentHandle, TimerMode};
use std::time::Duration;

impl App {
    pub(super) fn search(&mut self, ui: &MainWindow, query: &str) {
        let Some(index) = self.active else { return };
        let tab = &mut self.tabs[index];
        if query == tab.search.query {
            return;
        }
        tab.search.token += 1;
        tab.search.query = query.chars().take(512).collect();
        tab.search.matches.clear();
        tab.search.selected = None;
        tab.search.done = query.is_empty();
        ui.set_search_text(tab.search.query.as_str().into());
        self.worker.send(Command::Search(
            tab.id,
            tab.search.token,
            tab.search.query.clone(),
        ));
        self.search_status(ui);
        self.refresh(ui, false);
    }

    pub(super) fn search_status(&self, ui: &MainWindow) {
        let Some(index) = self.active else { return };
        let search = &self.tabs[index].search;
        let count = search.matches.len();
        let status = if search.query.is_empty() {
            String::new()
        } else if count == 0 && search.done {
            "No matches".into()
        } else if count == 0 {
            "Searching…".into()
        } else {
            let extra = if !search.done || count >= 20_000 {
                "+"
            } else {
                ""
            };
            format!("{} / {count}{extra}", search.selected.map_or(0, |n| n + 1))
        };
        ui.set_search_status(status.into());
    }

    fn find_match(&mut self, ui: &MainWindow, backwards: bool) {
        let Some(index) = self.active else { return };
        let tab = &mut self.tabs[index];
        let count = tab.search.matches.len();
        if count == 0 {
            return;
        }
        let selected = match tab.search.selected {
            Some(selected) if backwards => (selected + count - 1) % count,
            Some(selected) => (selected + 1) % count,
            None if backwards => count - 1,
            None => tab
                .search
                .matches
                .iter()
                .position(|result| result.page >= tab.reading.page)
                .unwrap_or(0),
        };
        self.jump_to_match(ui, selected);
    }

    pub(super) fn jump_to_match(&mut self, ui: &MainWindow, selected: usize) {
        let Some(index) = self.active else { return };
        let tab = &mut self.tabs[index];
        tab.search.selected = Some(selected);
        let result = &tab.search.matches[selected];
        tab.reading.page = result.page;
        tab.reading.fraction = 0.;
        let point = match tab.reading.rotation {
            1 => Point {
                x: 1. - result.position.y,
                y: result.position.x,
            },
            2 => Point {
                x: 1. - result.position.x,
                y: 1. - result.position.y,
            },
            3 => Point {
                x: result.position.y,
                y: 1. - result.position.x,
            },
            _ => result.position,
        };
        let page = result.page;
        ui.set_page_number((page + 1) as i32);
        self.relayout(ui);
        self.center_on(ui, page, point);
        self.search_status(ui);
        self.save_later();
    }

    pub(super) fn toggle_fullscreen(&mut self, ui: &MainWindow) {
        self.fullscreen = !self.fullscreen;
        ui.set_fullscreen(self.fullscreen);
        ui.window().set_fullscreen(self.fullscreen);
        if self.fullscreen {
            self.reveal(ui);
        } else {
            self.fullscreen_timer.stop();
            ui.set_controls_visible(false);
        }
    }

    fn reveal(&self, ui: &MainWindow) {
        if !self.fullscreen {
            return;
        }
        if !ui.get_controls_visible() {
            ui.set_controls_visible(true);
        }
        self.fullscreen_timer
            .start(TimerMode::SingleShot, Duration::from_secs(3), || {
                with_ui(|_, ui| ui.set_controls_visible(false));
            });
    }

    pub(super) fn ask(&mut self, ui: &MainWindow, prompt: Prompt, title: &str, value: &str) {
        ui.set_input_title(title.into());
        ui.set_input_value(value.into());
        ui.set_input_request(
            match prompt {
                Prompt::Message => "message",
                Prompt::Note(..) => "annotation",
                _ => "confirm",
            }
            .into(),
        );
        self.prompt = Some(prompt);
    }

    pub(super) fn input_submit(&mut self, ui: &MainWindow, value: String) {
        ui.set_input_request("".into());
        match self.prompt.take() {
            Some(Prompt::Close(id)) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                    self.close(ui, index, true);
                }
            }
            Some(Prompt::Exit) => {
                if self.tabs.iter().all(|tab| tab.pending == 0 && !tab.saving) {
                    self.persist(ui);
                    self.leaving = true;
                    let _ = ui.hide();
                }
            }
            Some(Prompt::Note(id, page, rotation, mut gesture)) if !value.trim().is_empty() => {
                gesture.text = value.chars().take(8192).collect();
                self.send_edit(id, Command::Gesture(id, page, rotation, gesture));
            }
            _ => {}
        }
    }

    fn send_edit(&mut self, id: TabId, command: Command) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.pending += 1;
            self.worker.send(command);
        }
    }

    pub(super) fn pointer(&mut self, ui: &MainWindow, page: usize, phase: &str, point: Point) {
        let Some(index) = self.active else { return };
        let tab = &self.tabs[index];
        if !tab.opened || page >= tab.sizes.len() {
            return;
        }
        let id = tab.id;
        let rotation = tab.reading.rotation;
        match phase {
            "down" => {
                self.drag = Some(Drag {
                    id,
                    page,
                    gesture: Gesture {
                        tool: self.tool.clone(),
                        points: vec![point],
                        color: color_value(&self.settings.pen_color).unwrap_or([230, 87, 97, 255]),
                        thickness: self.settings.pen_width,
                        text: String::new(),
                    },
                })
            }
            "move" => {
                let Some(drag) = &mut self.drag else { return };
                if drag.id != id || drag.page != page {
                    return;
                }
                let points = &mut drag.gesture.points;
                if drag.gesture.tool == "ink" {
                    if points.len() < 2048
                        && points.last().is_none_or(|last| {
                            (last.x - point.x).abs() + (last.y - point.y).abs() >= 0.001
                        })
                    {
                        points.push(point);
                    }
                } else {
                    points.truncate(1);
                    points.push(point);
                }
            }
            "up" => {
                let Some(mut drag) = self.drag.take() else {
                    return;
                };
                if drag.id != id || drag.page != page {
                    return;
                }
                drag.gesture.points.push(point);
                match drag.gesture.tool.as_str() {
                    "ocr" => self.worker.send(Command::Recognize(
                        id,
                        page,
                        rotation,
                        Some((drag.gesture.points[0], point)),
                    )),
                    "note" | "text" => self.ask(
                        ui,
                        Prompt::Note(id, page, rotation, drag.gesture),
                        "Annotation text",
                        "",
                    ),
                    _ => self.send_edit(id, Command::Gesture(id, page, rotation, drag.gesture)),
                }
            }
            "cancel" => self.drag = None,
            _ => return,
        }
        self.refresh_overlay(page);
    }

    pub(super) fn command(&mut self, ui: &MainWindow, command: &str) {
        match command {
            "new-tab" => {
                self.new_tab(ui);
                return;
            }
            "toggle-theme" => {
                self.settings.dark = !self.settings.dark;
                ui.set_dark_mode(self.settings.dark);
                #[cfg(windows)]
                crate::platform::set_caption_theme(ui.window(), self.settings.dark);
                self.save_later();
                return;
            }
            "settings-session"
            | "settings-fit"
            | "settings-memory"
            | "settings-continuous"
            | "settings-single"
            | "settings-spread" => {
                match command {
                    "settings-session" => {
                        self.settings.restore_session = !self.settings.restore_session
                    }
                    "settings-fit" => {
                        self.settings.default_zoom = if self.settings.default_zoom == Zoom::FitWidth
                        {
                            Zoom::FitPage
                        } else {
                            Zoom::FitWidth
                        }
                    }
                    "settings-memory" => {
                        self.settings.memory_saver = !self.settings.memory_saver;
                        self.refresh(ui, true);
                    }
                    "settings-single" => self.settings.default_mode = Mode::Single,
                    "settings-spread" => self.settings.default_mode = Mode::Spread,
                    _ => self.settings.default_mode = Mode::Continuous,
                }
                self.sync_settings(ui);
                self.save_later();
                return;
            }
            #[cfg(windows)]
            "stop-reading" => {
                self.worker.send(Command::StopSpeech);
                ui.set_status_text("".into());
                return;
            }
            "open" => {
                if let Some(paths) = rfd::FileDialog::new()
                    .set_title("Open PDF")
                    .add_filter("PDF documents", &["pdf"])
                    .pick_files()
                {
                    self.open(ui, paths);
                }
                return;
            }
            "reveal-controls" => {
                self.reveal(ui);
                return;
            }
            "input-cancel" => {
                self.prompt = None;
                ui.set_input_request("".into());
                return;
            }
            "cancel-password" => {
                ui.set_password_requested(false);
                if let Some(id) = self.password.take()
                    && let Some(index) = self.tabs.iter().position(|tab| tab.id == id)
                {
                    self.close(ui, index, true);
                }
                return;
            }
            _ => {}
        }
        let Some(index) = self.active else { return };
        let id = self.tabs[index].id;
        let page = self.tabs[index].reading.page;
        let count = self.tabs[index].sizes.len();
        match command {
            "toggle-fullscreen" => self.toggle_fullscreen(ui),
            "close-current-tab" => self.close(ui, index, false),
            "next-tab" => self.activate(ui, (index + 1) % self.tabs.len()),
            "previous-tab" => self.activate(ui, (index + self.tabs.len() - 1) % self.tabs.len()),
            _ if !self.tabs[index].opened => {}
            "previous-page" => self.step(ui, -1),
            "next-page" => self.step(ui, 1),
            "first-page" => self.go_to(ui, 0),
            "last-page" => self.go_to(ui, count.saturating_sub(1)),
            "zoom-in" | "zoom-out" => {
                let factor = if command == "zoom-in" { 1.2 } else { 1. / 1.2 };
                self.zoom(
                    ui,
                    Zoom::Fixed((self.layout.scale * (72. / 96.) * factor).clamp(0.1, 8.)),
                );
            }
            "fit-width" => self.zoom(ui, Zoom::FitWidth),
            "fit-page" => self.zoom(ui, Zoom::FitPage),
            "rotate" => {
                self.tabs[index].reading.rotation = (self.tabs[index].reading.rotation + 1) % 4;
                self.rebuild_rail(ui);
                self.relayout(ui);
                self.save_later();
            }
            "toggle-sidebar" | "thumbnails" | "outline" | "bookmarks" => {
                if command == "toggle-sidebar" {
                    self.settings.sidebar = !self.settings.sidebar;
                } else {
                    self.settings.sidebar = true;
                    self.sidebar_mode = match command {
                        "outline" => SidebarMode::Outline,
                        "bookmarks" => SidebarMode::Bookmarks,
                        _ => SidebarMode::Thumbnails,
                    };
                    ui.set_sidebar_mode(command.into());
                }
                if self.settings.sidebar
                    && self.sidebar_mode == SidebarMode::Outline
                    && self.tabs[index].outline.is_none()
                {
                    self.worker.send(Command::Outline(id));
                }
                if !self.settings.sidebar {
                    self.thumbnails.clear();
                }
                ui.set_sidebar_open(self.settings.sidebar);
                self.rebuild_rail(ui);
                self.refresh(ui, true);
                self.save_later();
            }
            "bookmark" => {
                let bookmarks = &mut self.tabs[index].reading.bookmarks;
                match bookmarks.binary_search(&page) {
                    Ok(index) => {
                        bookmarks.remove(index);
                        ui.set_status_text("Bookmark removed.".into());
                    }
                    Err(index) if bookmarks.len() < 1024 => {
                        bookmarks.insert(index, page);
                        ui.set_status_text("Page bookmarked.".into());
                    }
                    Err(_) => ui.set_status_text("Bookmark limit reached.".into()),
                }
                if self.sidebar_mode == SidebarMode::Bookmarks {
                    self.rebuild_rail(ui);
                }
                self.save_later();
            }
            "toggle-search" => {
                let search = &mut self.tabs[index].search;
                search.open = !search.open;
                ui.set_search_open(search.open);
                self.refresh(ui, false);
            }
            "find-next" => self.find_match(ui, false),
            "find-previous" => self.find_match(ui, true),
            "page-text" => self.worker.send(Command::PageText(id, page)),
            #[cfg(windows)]
            "read-aloud" => self.worker.send(Command::Speak(id, page)),
            "recognize-page" => {
                ui.set_status_text("Recognizing page text…".into());
                self.worker.send(Command::Recognize(
                    id,
                    page,
                    self.tabs[index].reading.rotation,
                    None,
                ));
            }
            "copy" => self.worker.send(Command::Copy(id)),
            "metadata" => self.worker.send(Command::Metadata(id)),
            "undo" | "redo" => self.send_edit(id, Command::Undo(id, command == "redo")),
            "save" | "save-as" => self.save_document(ui, index, command == "save-as"),
            #[cfg(windows)]
            "print" => {
                let tab = &self.tabs[index];
                match crate::platform::PrintJob::choose(None, count as u32, page as u32) {
                    Ok(Some(job)) => {
                        ui.set_status_text("Printing…".into());
                        self.worker.send(Command::Print(
                            id,
                            job,
                            title(&tab.path),
                            tab.reading.rotation,
                        ));
                    }
                    Ok(None) => {}
                    Err(error) => self.ask(ui, Prompt::Message, "Print failed", &error),
                }
            }
            "reading-continuous" | "reading-single" | "reading-spread" => {
                let mode = match command {
                    "reading-single" => Mode::Single,
                    "reading-spread" => Mode::Spread,
                    _ => Mode::Continuous,
                };
                self.tabs[index].reading.mode = mode;
                self.tabs[index].reading.fraction = 0.;
                self.tabs[index].horizontal = None;
                ui.set_reading_mode(mode.label().into());
                self.relayout(ui);
                self.save_later();
            }
            _ if command.starts_with("tool-") => {
                self.tool = command[5..].into();
                ui.set_annotation_tool(self.tool.as_str().into());
            }
            _ => {}
        }
    }

    fn save_document(&mut self, ui: &MainWindow, index: usize, save_as: bool) {
        let tab = &self.tabs[index];
        if tab.saving {
            return;
        }
        if !tab.dirty && !save_as && tab.pending == 0 {
            ui.set_status_text("No unsaved changes.".into());
            return;
        }
        let path = if save_as {
            let Some(mut path) = rfd::FileDialog::new()
                .set_title("Save PDF")
                .add_filter("PDF documents", &["pdf"])
                .set_file_name(title(&tab.path))
                .save_file()
            else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension("pdf");
            }
            absolute(path)
        } else {
            tab.path.clone()
        };
        let id = tab.id;
        if self
            .tabs
            .iter()
            .any(|other| other.id != id && other.path == path)
        {
            self.ask(
                ui,
                Prompt::Message,
                "Could not save",
                "That file is already open in another tab.",
            );
            return;
        }
        self.tabs[index].saving = true;
        self.worker.send(Command::Save(id, path));
        ui.set_status_text("Saving document…".into());
    }
}
