use crate::{
    MainWindow, PageSlot, PageTile, SidebarItem, TabItem,
    layout::Layout,
    links::DownloadedPdf,
    pdf::{OutlineEntry, PageCoordinates, Point, Rect},
    storage::{Reading, Settings, Zoom},
    worker::{Command, Event, Gesture, Match, Raster, TabId, Worker},
};
use slint::{
    CloseRequestResponse, ComponentHandle, Image, Model, Rgba8Pixel, SharedPixelBuffer, Timer,
    TimerMode, VecModel,
};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

#[path = "feedback.rs"]
mod feedback;
#[path = "reader_controls.rs"]
mod reader_controls;
#[path = "reader_view.rs"]
mod reader_view;

const MAX_TABS: usize = 32;
const RASTER_CACHE_BYTES: usize = 6 * 1024 * 1024;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_ui<T>(callback: impl FnOnce(&mut App, &MainWindow) -> T) -> Option<T> {
    APP.with(|state| {
        let mut state = state.borrow_mut();
        let app = state.as_mut()?;
        let ui = app.ui.upgrade()?;
        Some(callback(app, &ui))
    })
}

pub fn deliver(event: Event) {
    with_ui(|app, ui| app.event(ui, event));
}

pub fn run(
    paths: Vec<PathBuf>,
    show_timing: Option<Instant>,
) -> Result<(), Box<dyn std::error::Error>> {
    let ui = MainWindow::new()?;
    ui.set_ocr_available(cfg!(windows));
    ui.set_windows_settings_available(cfg!(windows));
    let worker = Worker::start()?;
    let path = crate::storage::path();
    let settings = Settings::load(&path);
    let pages = Rc::new(VecModel::default());
    ui.set_pages(pages.clone().into());
    let tiles = Rc::new(VecModel::default());
    ui.set_tiles(tiles.clone().into());
    let tab_items = Rc::new(VecModel::default());
    ui.set_tabs(tab_items.clone().into());
    let app = App {
        ui: ui.as_weak(),
        worker,
        settings,
        settings_path: path,
        tabs: Vec::new(),
        tab_items,
        active: None,
        next_id: 0,
        pages,
        tiles,
        rail: Rc::default(),
        rail_range: 0..8,
        layout: Layout::default(),
        viewport: (900., 620.),
        scroll: (0., 0.),
        cache: Cache::new(RASTER_CACHE_BYTES),
        thumbnails: Cache::new(2 * 1024 * 1024),
        plan: Vec::new(),
        failed: HashMap::new(),
        scroll_forward: true,
        requested: Vec::new(),
        render_tab: 0,
        render_visible: 0,
        drag: None,
        prompt: None,
        downloading: false,
        fullscreen: false,
        leaving: false,
        sidebar_mode: SidebarMode::Thumbnails,
        tool: "select".into(),
        save_timer: Timer::default(),
        fullscreen_timer: Timer::default(),
        layout_timer: Timer::default(),
        render_retry_timer: Timer::default(),
        clipboard: None,
        show_timing,
    };
    APP.with(|state| state.replace(Some(app)));
    ui.on_command(|command| {
        with_ui(|app, ui| app.command(ui, command.as_str()));
    });
    ui.on_go_to_page(|page| {
        with_ui(|app, ui| app.go_to(ui, page.saturating_sub(1).max(0) as usize));
    });
    ui.on_select_tab(|index| {
        with_ui(|app, ui| app.activate(ui, index as usize));
    });
    ui.on_close_tab(|index| {
        with_ui(|app, ui| app.close(ui, index as usize, false));
    });
    ui.on_open_recent(|index| {
        with_ui(|app, ui| {
            if let Some(recent) = app.settings.recent.get(index as usize) {
                app.open(ui, vec![recent.path.clone()]);
            }
        });
    });
    ui.on_find(|query| {
        with_ui(|app, ui| app.search(ui, query.trim()));
    });
    ui.on_unlock(|password| {
        with_ui(|app, ui| app.unlock(ui, password.to_string()));
    });
    ui.on_input_submit(|value| {
        with_ui(|app, ui| app.input_submit(ui, value.to_string()));
    });
    ui.on_zoom_to(|percent| {
        if percent.is_finite() {
            with_ui(|app, ui| app.zoom(ui, Zoom::Fixed((percent / 100.).clamp(0.1, 8.))));
        }
    });
    ui.on_pen_color(|color| {
        if color_value(color.as_str()).is_some() {
            with_ui(|app, ui| {
                app.settings.pen_color = color.into();
                ui.set_pen_color_value(app.settings.pen_color.as_str().into());
                app.save_later();
            });
        }
    });
    ui.on_pen_width(|width| {
        if width.is_finite() {
            with_ui(|app, ui| {
                app.settings.pen_width = width.clamp(1., 16.);
                ui.set_pen_width_value(app.settings.pen_width);
                app.save_later();
            });
        }
    });
    ui.on_scroll_to(|fraction| {
        with_ui(|app, ui| app.scroll_to(ui, fraction));
    });
    ui.on_wheel(|x, y, zoom| {
        with_ui(|app, ui| app.wheel(ui, x, y, zoom));
    });
    ui.on_page_pointer(|page, phase, x, y| {
        if x.is_finite() && y.is_finite() {
            with_ui(|app, ui| {
                app.pointer(
                    ui,
                    page as usize,
                    phase.as_str(),
                    Point {
                        x: x.clamp(0., 1.),
                        y: y.clamp(0., 1.),
                    },
                )
            });
        }
    });
    ui.on_viewport_changed(|width, height| {
        let _ = slint::invoke_from_event_loop(move || {
            with_ui(|app, _| {
                app.viewport = (width.max(1.), height.max(1.));
                app.queue_relayout();
            });
        });
    });
    ui.on_sidebar_visible(|start, end| {
        let _ = slint::invoke_from_event_loop(move || {
            with_ui(|app, ui| app.sidebar_visible(ui, start.max(0) as usize, end.max(0) as usize));
        });
    });
    ui.on_can_drop_files(|data| data.file_paths().is_ok_and(|mut paths| paths.any(is_pdf)));
    ui.on_drop_files(|data| {
        let paths = data
            .file_paths()
            .into_iter()
            .flatten()
            .map(PathBuf::from)
            .filter(|path| is_pdf(path))
            .collect();
        let _ = slint::invoke_from_event_loop(move || {
            with_ui(|app, ui| app.open(ui, paths));
        });
    });
    ui.window().on_close_requested(|| {
        with_ui(|app, ui| {
            if app.prompt.is_some() || ui.get_password_requested() {
                return CloseRequestResponse::KeepWindowShown;
            }
            if app.tabs.iter().any(|tab| tab.saving || tab.pending > 0) {
                ui.set_status_text("Finishing document changes…".into());
                return CloseRequestResponse::KeepWindowShown;
            }
            if app.tabs.iter().any(|tab| tab.dirty) {
                app.ask(
                    ui,
                    Prompt::Exit,
                    "Close TinyPDF?",
                    "Discard unsaved annotations and close all tabs?",
                );
                return CloseRequestResponse::KeepWindowShown;
            }
            app.persist(ui);
            app.leaving = true;
            CloseRequestResponse::HideWindow
        })
        .unwrap_or(CloseRequestResponse::HideWindow)
    });
    with_ui(|app, ui| {
        ui.set_dark_mode(app.settings.dark);
        ui.set_pen_color_value(app.settings.pen_color.as_str().into());
        ui.set_pen_width_value(app.settings.pen_width);
        app.recent(ui);
        app.sync_settings(ui);
        if paths.is_empty() && app.settings.restore_session {
            let saved = app.settings.open.clone();
            let selected = app.settings.active.clone();
            for path in saved.into_iter().filter(|path| path.is_file()) {
                app.add_tab(path);
            }
            if !app.tabs.is_empty() {
                let index = selected
                    .and_then(|path| app.tabs.iter().position(|tab| tab.path == path))
                    .unwrap_or(0);
                app.activate(ui, index);
            }
        } else if !paths.is_empty() {
            app.open(ui, paths);
        }
    });
    #[cfg(windows)]
    Timer::single_shot(Duration::from_millis(50), || {
        with_ui(|app, ui| crate::platform::set_caption_theme(ui.window(), app.settings.dark));
    });
    let result = ui.run();
    if let Some(mut app) = APP.with(|state| state.take()) {
        if !app.leaving {
            app.persist(&ui);
        }
        app.worker.send(Command::Stop);
    }
    result?;
    Ok(())
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

fn title(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn absolute(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path)
        .or_else(|_| std::path::absolute(&path))
        .unwrap_or(path)
}

fn color_value(color: &str) -> Option<[u8; 4]> {
    let value = color.strip_prefix('#')?;
    if value.len() != 6 {
        return None;
    }
    let bits = u32::from_str_radix(value, 16).ok()?;
    Some([(bits >> 16) as u8, (bits >> 8) as u8, bits as u8, 255])
}

struct Tab {
    id: TabId,
    path: PathBuf,
    reading: Reading,
    sizes: Vec<(f32, f32)>,
    load: DocumentLoad,
    dirty: bool,
    saving: bool,
    save_error: Option<String>,
    pending_message: Option<(String, String)>,
    downloaded: Option<Arc<DownloadedPdf>>,
    pending: usize,
    horizontal: Option<f32>,
    revision: HashMap<usize, u64>,
    coordinates: VecDeque<(usize, u8, PageCoordinates)>,
    outline: Option<Vec<OutlineEntry>>,
    search: Search,
    selection: Option<(usize, Vec<Rect>)>,
}

enum DocumentLoad {
    Idle,
    Opening { unlocking: bool },
    Password { rejected: bool },
    Failed(String),
    Ready,
}

impl Tab {
    fn opened(&self) -> bool {
        matches!(self.load, DocumentLoad::Ready)
    }
}

#[derive(Default)]
struct Search {
    open: bool,
    query: String,
    token: u64,
    matches: Vec<Match>,
    selected: Option<usize>,
    done: bool,
    failed: bool,
}

enum Prompt {
    Message,
    OpenLink,
    Close(TabId),
    Exit,
    Note(TabId, usize, u8, Gesture),
}

struct Drag {
    id: TabId,
    page: usize,
    gesture: Gesture,
}

#[derive(Clone, Copy, PartialEq)]
enum SidebarMode {
    Thumbnails,
    Outline,
    Bookmarks,
}

struct App {
    ui: slint::Weak<MainWindow>,
    worker: Worker,
    settings: Settings,
    settings_path: PathBuf,
    tabs: Vec<Tab>,
    tab_items: Rc<VecModel<TabItem>>,
    active: Option<usize>,
    next_id: TabId,
    pages: Rc<VecModel<PageSlot>>,
    tiles: Rc<VecModel<PageTile>>,
    rail: Rc<VecModel<SidebarItem>>,
    rail_range: std::ops::Range<usize>,
    layout: Layout,
    viewport: (f32, f32),
    scroll: (f32, f32),
    cache: Cache,
    thumbnails: Cache,
    plan: Vec<Raster>,
    failed: HashMap<Raster, u8>,
    scroll_forward: bool,
    requested: Vec<Raster>,
    render_tab: TabId,
    render_visible: usize,
    drag: Option<Drag>,
    prompt: Option<Prompt>,
    downloading: bool,
    fullscreen: bool,
    leaving: bool,
    sidebar_mode: SidebarMode,
    tool: String,
    save_timer: Timer,
    fullscreen_timer: Timer,
    layout_timer: Timer,
    render_retry_timer: Timer,
    clipboard: Option<arboard::Clipboard>,
    show_timing: Option<Instant>,
}

impl App {
    fn queue_relayout(&self) {
        // Merge incoming dimension batches and resize events into one layout per frame.
        if !self.layout_timer.running() {
            self.layout_timer
                .start(TimerMode::SingleShot, Duration::from_millis(16), || {
                    with_ui(|app, ui| app.relayout(ui));
                });
        }
    }

    fn add_tab(&mut self, path: PathBuf) {
        let reading = self
            .settings
            .recent
            .iter()
            .find(|item| item.path == path)
            .map(|item| item.reading.clone())
            .unwrap_or_else(|| Reading {
                zoom: self.settings.default_zoom,
                mode: self.settings.default_mode,
                ..Reading::default()
            });
        self.next_id += 1;
        self.tabs.push(Tab {
            id: self.next_id,
            path,
            reading,
            sizes: Vec::new(),
            load: DocumentLoad::Idle,
            dirty: false,
            saving: false,
            save_error: None,
            pending_message: None,
            downloaded: None,
            pending: 0,
            horizontal: None,
            revision: HashMap::new(),
            coordinates: VecDeque::new(),
            outline: None,
            search: Search::default(),
            selection: None,
        });
    }

    // An empty path represents a home tab and is never passed to PDFium.
    fn new_tab(&mut self, ui: &MainWindow) {
        if self.tabs.len() < MAX_TABS {
            if self.fullscreen {
                self.toggle_fullscreen(ui);
            }
            self.add_tab(PathBuf::new());
            self.activate(ui, self.tabs.len() - 1);
        } else {
            ui.set_status_text("TinyPDF supports up to 32 open tabs.".into());
        }
    }

    fn open(&mut self, ui: &MainWindow, paths: Vec<PathBuf>) {
        let mut selected = None;
        let mut home = self
            .active
            .filter(|&index| self.tabs[index].path.as_os_str().is_empty());
        for path in paths {
            let path = absolute(path);
            if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
                selected.get_or_insert(index);
            } else if let Some(index) = home.take() {
                self.add_tab(path);
                self.tabs[index] = self.tabs.pop().unwrap();
                selected.get_or_insert(index);
            } else if self.tabs.len() < MAX_TABS {
                selected.get_or_insert(self.tabs.len());
                self.add_tab(path);
            } else {
                ui.set_status_text("TinyPDF supports up to 32 open tabs.".into());
                break;
            }
        }
        if let Some(index) = selected {
            self.activate(ui, index);
        }
    }

    fn activate(&mut self, ui: &MainWindow, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.drag = None;
        if self.active != Some(index) {
            #[cfg(windows)]
            self.worker.send(Command::StopSpeech);
            // Drawing tools apply only to the tab on which they were activated.
            self.tool = "select".into();
            ui.set_annotation_tool("select".into());
            self.cache.clear();
            self.thumbnails.clear();
            self.requested.clear();
            self.failed.clear();
            self.scroll_forward = true;
            self.rail_range = 0..8;
            self.tiles.set_vec(Vec::new());
        }
        self.active = Some(index);
        let tab = &mut self.tabs[index];
        ui.set_status_text(tab.save_error.as_deref().unwrap_or("").into());
        if !tab.path.as_os_str().is_empty() && matches!(tab.load, DocumentLoad::Idle) {
            tab.load = DocumentLoad::Opening { unlocking: false };
            self.worker.send(Command::Open(
                tab.id,
                tab.path.clone(),
                None,
                tab.downloaded.clone(),
            ));
        }
        if tab.opened()
            && self.settings.sidebar
            && self.sidebar_mode == SidebarMode::Outline
            && tab.outline.is_none()
        {
            self.worker.send(Command::Outline(tab.id));
        }
        self.sync(ui);
        self.rebuild_rail(ui);
        self.relayout(ui);
        self.show_pending_message(ui);
        self.save_later();
    }

    fn unlock(&mut self, ui: &MainWindow, password: String) {
        let Some(index) = self.active.filter(|_| ui.get_password_requested()) else {
            return;
        };
        let tab = &mut self.tabs[index];
        if matches!(tab.load, DocumentLoad::Password { .. }) {
            tab.load = DocumentLoad::Opening { unlocking: true };
            self.worker.send(Command::Open(
                tab.id,
                tab.path.clone(),
                Some(password),
                tab.downloaded.clone(),
            ));
            self.sync_load(ui);
        }
    }

    fn sync_load(&self, ui: &MainWindow) {
        let load = self.active.map(|index| &self.tabs[index].load);
        ui.set_document_loading(matches!(load, Some(DocumentLoad::Opening { .. })));
        ui.set_open_error(match load {
            Some(DocumentLoad::Failed(message)) => message.as_str().into(),
            _ => "".into(),
        });
        ui.set_password_error(matches!(
            load,
            Some(DocumentLoad::Password { rejected: true })
        ));
        ui.set_password_requested(
            matches!(load, Some(DocumentLoad::Password { .. })) && self.prompt.is_none(),
        );
    }

    fn start_download(&mut self, ui: &MainWindow, value: String) {
        let url = match crate::links::parse(&value) {
            Ok(url) => url,
            Err(error) => {
                self.ask(ui, Prompt::OpenLink, "Open PDF link", &value);
                ui.set_input_error(error.into());
                return;
            }
        };
        if self.downloading {
            return;
        }
        if self.tabs.len() == MAX_TABS
            && self
                .active
                .is_none_or(|index| !self.tabs[index].path.as_os_str().is_empty())
        {
            self.report_message(
                ui,
                "Could not open PDF link",
                "Close a tab before opening another PDF.",
            );
            return;
        }
        let origin = self.active.map(|index| self.tabs[index].id);
        match std::thread::Builder::new()
            .name("TinyPDF download".into())
            .spawn(move || {
                let result = crate::links::download(url);
                let _ = slint::invoke_from_event_loop(move || {
                    with_ui(|app, ui| app.finish_download(ui, origin, result));
                });
            }) {
            Ok(_) => {
                self.downloading = true;
                ui.set_link_downloading(true);
            }
            Err(error) => {
                self.report_message(ui, "Could not start the download", &error.to_string())
            }
        }
    }

    fn finish_download(
        &mut self,
        ui: &MainWindow,
        origin: Option<TabId>,
        result: Result<DownloadedPdf, String>,
    ) {
        self.downloading = false;
        ui.set_link_downloading(false);
        let pdf = match result {
            Ok(pdf) => Arc::new(pdf),
            Err(error) => {
                ui.set_status_text(format!("Could not open PDF link. {error}").into());
                self.report_message(ui, "Could not open PDF link", &error);
                return;
            }
        };
        let foreground = !ui.get_password_requested()
            && self.prompt.is_none()
            && self.active.map(|index| self.tabs[index].id) == origin;
        let home = self
            .active
            .filter(|&index| foreground && self.tabs[index].path.as_os_str().is_empty());
        if self.tabs.len() == MAX_TABS && home.is_none() {
            self.report_message(
                ui,
                "Could not open downloaded PDF",
                "Close a tab and open the link again.",
            );
            return;
        }
        self.add_tab(pdf.path.clone());
        self.tabs.last_mut().unwrap().downloaded = Some(pdf);
        let index = if let Some(home) = home {
            self.tabs[home] = self.tabs.pop().unwrap();
            home
        } else {
            self.tabs.len() - 1
        };
        if foreground || self.active.is_none() {
            self.activate(ui, index);
        } else {
            self.sync(ui);
            ui.set_status_text("The downloaded PDF is ready in a new tab.".into());
        }
    }

    fn sync_settings(&self, ui: &MainWindow) {
        ui.set_dark_mode(self.settings.dark);
        ui.set_restore_session(self.settings.restore_session);
        ui.set_default_fit_width(self.settings.default_zoom == Zoom::FitWidth);
        ui.set_default_reading_mode(self.settings.default_mode.label().into());
        ui.set_memory_saver(self.settings.memory_saver);
    }

    fn sync(&self, ui: &MainWindow) {
        let items = self.tabs.iter().map(|tab| TabItem {
            title: if tab.path.as_os_str().is_empty() {
                "New tab".into()
            } else {
                title(&tab.path).into()
            },
            dirty: tab.dirty,
        });
        if self.tab_items.row_count() == self.tabs.len() {
            for (index, item) in items.enumerate() {
                if self.tab_items.row_data(index).as_ref() != Some(&item) {
                    self.tab_items.set_row_data(index, item);
                }
            }
        } else {
            self.tab_items.set_vec(items.collect::<Vec<_>>());
        }
        if let Some(index) = self.active {
            let tab = &self.tabs[index];
            ui.set_document_open(!tab.path.as_os_str().is_empty());
            ui.set_active_tab(index as i32);
            ui.set_page_count(tab.sizes.len() as i32);
            ui.set_page_number((tab.reading.page + 1) as i32);
            ui.set_reading_mode(tab.reading.mode.label().into());
            ui.set_search_open(tab.search.open);
            ui.set_search_text(tab.search.query.as_str().into());
            self.search_status(ui);
            ui.set_print_available(cfg!(windows) && tab.opened());
        } else {
            ui.set_document_open(false);
            ui.set_active_tab(-1);
            ui.set_page_count(0);
            ui.set_search_open(false);
            ui.set_print_available(false);
        }
        ui.set_sidebar_open(self.settings.sidebar);
        self.sync_load(ui);
    }

    fn recent(&self, ui: &MainWindow) {
        ui.set_recent_items(
            Rc::new(VecModel::from(
                self.settings
                    .recent
                    .iter()
                    .take(5)
                    .map(|item| SidebarItem {
                        label: title(&item.path).into(),
                        page: 0,
                        thumbnail: Image::default(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
    }

    fn close(&mut self, ui: &MainWindow, index: usize, discard: bool) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.pending > 0 || tab.saving {
            ui.set_status_text("Finishing document changes…".into());
            return;
        }
        if tab.dirty && !discard {
            self.ask(
                ui,
                Prompt::Close(tab.id),
                "Close document?",
                "Discard unsaved annotations in this tab?",
            );
            return;
        }
        let tab = self.tabs.remove(index);
        if tab.opened() && tab.downloaded.is_none() {
            self.settings.record(tab.path, tab.reading);
        }
        self.worker.send(Command::Close(tab.id));
        #[cfg(windows)]
        if self.active == Some(index) {
            self.worker.send(Command::StopSpeech);
        }
        if self.active == Some(index) {
            self.active = None;
            if self.tabs.is_empty() {
                if self.fullscreen {
                    self.toggle_fullscreen(ui);
                }
                self.cache.clear();
                self.thumbnails.clear();
                self.requested.clear();
                self.failed.clear();
                self.plan.clear();
                self.pages.set_vec(Vec::new());
                self.tiles.set_vec(Vec::new());
                self.render_tab = 0;
                self.worker.render(0, Vec::new(), 0);
                self.sync(ui);
                self.recent(ui);
                ui.set_status_text("".into());
            } else {
                self.activate(ui, index.min(self.tabs.len() - 1));
            }
        } else {
            if let Some(active) = self.active.as_mut() {
                *active -= usize::from(index < *active);
            }
            self.sync(ui);
        }
        self.save_later();
    }

    fn save_later(&self) {
        self.save_timer
            .start(TimerMode::SingleShot, Duration::from_millis(1200), || {
                with_ui(|app, ui| app.persist(ui));
            });
    }

    fn persist(&mut self, ui: &MainWindow) {
        self.settings.open = self
            .tabs
            .iter()
            .filter(|tab| !tab.path.as_os_str().is_empty() && tab.downloaded.is_none())
            .map(|tab| tab.path.clone())
            .collect();
        self.settings.active = self
            .active
            .filter(|&index| self.tabs[index].downloaded.is_none())
            .map(|index| self.tabs[index].path.clone())
            .filter(|path| !path.as_os_str().is_empty());
        for tab in &self.tabs {
            if !tab.opened() || tab.downloaded.is_some() {
                continue;
            }
            if let Some(recent) = self
                .settings
                .recent
                .iter_mut()
                .find(|item| item.path == tab.path)
            {
                recent.reading = tab.reading.clone();
            } else {
                self.settings.record(tab.path.clone(), tab.reading.clone());
            }
        }
        if let Err(error) = self.settings.save(&self.settings_path) {
            ui.set_status_text(format!("Could not save reading session: {error}").into());
        }
    }
}

struct Cache {
    entries: VecDeque<(Raster, Image, usize)>,
    bytes: usize,
    limit: usize,
}

impl Cache {
    fn new(limit: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn set_limit(&mut self, bytes: usize, visible: &[Raster]) {
        self.limit = bytes;
        self.make_room(0, visible);
    }

    fn make_room(&mut self, bytes: usize, visible: &[Raster]) {
        while self.bytes + bytes > self.limit {
            let Some(index) = self
                .entries
                .iter()
                .position(|(key, _, _)| !visible.contains(key))
            else {
                break;
            };
            self.bytes -= self.entries.remove(index).unwrap().2;
        }
    }

    fn contains(&self, key: Raster) -> bool {
        self.entries.iter().any(|(item, _, _)| *item == key)
    }

    fn get(&mut self, key: Raster) -> Option<Image> {
        let index = self.entries.iter().position(|(item, _, _)| *item == key)?;
        let entry = self.entries.remove(index)?;
        let image = entry.1.clone();
        self.entries.push_back(entry);
        Some(image)
    }

    fn previous_revision(&self, key: Raster) -> Option<Image> {
        self.entries.iter().rev().find_map(|(old, image, _)| {
            (old.revision < key.revision
                && Raster {
                    revision: key.revision,
                    ..*old
                } == key)
                .then(|| image.clone())
        })
    }

    fn insert(&mut self, key: Raster, pixels: SharedPixelBuffer<Rgba8Pixel>, visible: &[Raster]) {
        let bytes = (pixels.width() * pixels.height() * 4) as usize;
        if bytes > self.limit || self.contains(key) {
            return;
        }
        self.make_room(bytes, visible);
        if self.bytes + bytes > self.limit {
            return;
        }
        self.entries
            .push_back((key, Image::from_rgba8(pixels), bytes));
        self.bytes += bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_fallback_uses_only_the_matching_older_tile() {
        let tile = Raster {
            tab: 1,
            page: 0,
            rotation: 0,
            revision: 1,
            full_width: 1,
            full_height: 1,
            x: 0,
            y: 0,
            thumbnail: false,
        };
        let mut cache = Cache::new(4);
        cache.insert(tile, SharedPixelBuffer::new(1, 1), &[]);
        assert!(
            cache
                .previous_revision(Raster {
                    revision: 2,
                    ..tile
                })
                .is_some()
        );
        assert!(cache.previous_revision(tile).is_none());
        assert!(
            cache
                .previous_revision(Raster {
                    rotation: 1,
                    revision: 2,
                    ..tile
                })
                .is_none()
        );
    }

    #[test]
    fn shrinking_and_inserting_protect_the_incoming_viewport() {
        let retained = Raster {
            tab: 1,
            page: 1,
            rotation: 0,
            revision: 0,
            full_width: 1,
            full_height: 1,
            x: 0,
            y: 0,
            thumbnail: false,
        };
        let obsolete = Raster {
            page: 0,
            ..retained
        };
        let incoming = Raster {
            page: 2,
            ..retained
        };
        let mut cache = Cache::new(12);
        for key in [retained, obsolete, incoming] {
            cache.insert(key, SharedPixelBuffer::new(1, 1), &[]);
        }
        let visible = [retained, incoming];
        cache.set_limit(8, &visible);
        assert!(cache.contains(retained));
        assert!(cache.contains(incoming));
        assert!(!cache.contains(obsolete));
        cache.insert(obsolete, SharedPixelBuffer::new(1, 1), &visible);
        assert_eq!(cache.bytes, 8);
        assert!(!cache.contains(obsolete));
        cache.insert(
            obsolete,
            SharedPixelBuffer::new(1, 1),
            &[obsolete, incoming],
        );
        assert!(cache.contains(obsolete));
        assert!(cache.contains(incoming));
        assert!(!cache.contains(retained));
    }
}
