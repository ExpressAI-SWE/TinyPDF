use crate::{
    MainWindow,
    app::{App, DocumentLoad, SidebarMode, title, with_ui},
    layout::Placement,
    pdf::{PdfError, Point, Rect},
    worker::Event,
};
use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer, TimerMode};
use std::time::Duration;

impl App {
    pub(super) fn event(&mut self, ui: &MainWindow, event: Event) {
        match event {
            Event::Opened(id, count, size) => {
                let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
                    return;
                };
                let tab = &mut self.tabs[index];
                tab.reading.page = tab.reading.page.min(count - 1);
                tab.reading.bookmarks.retain(|&page| page < count);
                tab.sizes = vec![valid_size(size); count];
                tab.load = DocumentLoad::Ready;
                if tab.downloaded.is_none() {
                    self.settings.record(tab.path.clone(), tab.reading.clone());
                }
                if self.active == Some(index) {
                    ui.set_status_text("".into());
                    self.activate(ui, index);
                } else {
                    self.sync(ui);
                }
                self.recent(ui);
            }
            Event::OpenFailed(id, error) => {
                let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
                    return;
                };
                let tab = &mut self.tabs[index];
                tab.load = match error {
                    PdfError::PasswordRequired => DocumentLoad::Password {
                        rejected: matches!(tab.load, DocumentLoad::Opening { unlocking: true }),
                    },
                    PdfError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        DocumentLoad::Failed(
                            "This file could not be found. Check its location and try again."
                                .into(),
                        )
                    }
                    PdfError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                        DocumentLoad::Failed(
                            "TinyPDF does not have permission to read this file.".into(),
                        )
                    }
                    error => DocumentLoad::Failed(error.to_string()),
                };
                if self.active == Some(index) {
                    self.sync_load(ui);
                }
            }
            Event::Status(message) => ui.set_status_text(message.into()),
            Event::Dimensions(id, start, sizes) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                    let tab = &mut self.tabs[index];
                    let mut changed = false;
                    for (old, new) in tab
                        .sizes
                        .iter_mut()
                        .skip(start)
                        .zip(sizes.into_iter().map(valid_size))
                    {
                        changed |= *old != new;
                        *old = new;
                    }
                    if changed && self.active == Some(index) {
                        self.queue_relayout();
                    }
                }
            }
            Event::Tile(key, pixels) => {
                if self
                    .active
                    .is_none_or(|index| self.tabs[index].id != key.tab)
                {
                    return;
                }
                let tab = &self.tabs[self.active.unwrap()];
                if *tab.revision.get(&key.page).unwrap_or(&0) != key.revision {
                    return;
                }
                self.failed.remove(&key);
                if key.thumbnail {
                    if self.requested.contains(&key)
                        && self.rail_range.contains(&key.page)
                        && self.sidebar_mode == SidebarMode::Thumbnails
                        && key.rotation == tab.reading.rotation
                        && let Some(mut row) = self.rail.row_data(key.page)
                    {
                        self.thumbnails.insert(key, pixels, &[]);
                        if let Some(image) = self.thumbnails.get(key) {
                            row.thumbnail = image;
                            self.rail.set_row_data(key.page, row);
                        }
                    }
                } else if let Some(index) = self.plan.iter().position(|item| *item == key)
                    && let Some(mut tile) = self.tiles.row_data(index)
                {
                    self.cache.insert(key, pixels, &self.plan);
                    if let Some(bitmap) = self.cache.get(key) {
                        tile.bitmap = bitmap;
                        self.tiles.set_row_data(index, tile);
                        if let Some(started) = self.show_timing.take() {
                            eprintln!(
                                "first_tile_ms={:.1}",
                                started.elapsed().as_secs_f64() * 1000.
                            );
                        }
                    }
                } else if self.requested.contains(&key) {
                    self.cache.insert(key, pixels, &self.plan);
                }
            }
            Event::TileFailed(key, error) => {
                let visible = self.plan.contains(&key);
                if !self.requested.contains(&key) || !(visible || key.thumbnail) {
                    return;
                }
                let attempts = self.failed.entry(key).or_default();
                *attempts = attempts.saturating_add(1);
                if *attempts == 1 && !self.render_retry_timer.running() {
                    self.render_retry_timer.start(
                        TimerMode::SingleShot,
                        Duration::from_millis(120),
                        || {
                            with_ui(|app, ui| {
                                app.render_tab = 0;
                                app.refresh(ui, true);
                            });
                        },
                    );
                } else if *attempts == 2 && visible {
                    ui.set_status_text(
                        format!("Could not render page {}: {error}", key.page + 1).into(),
                    );
                }
            }
            Event::Outline(id, outline) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                    self.tabs[index].outline = Some(outline);
                    if self.active == Some(index) && self.sidebar_mode == SidebarMode::Outline {
                        self.rebuild_rail(ui);
                    }
                }
            }
            Event::Metadata(id, metadata) => {
                if let Some(tab) = self
                    .active
                    .and_then(|index| self.tabs.get(index))
                    .filter(|tab| tab.id == id)
                {
                    let mut details = format!("{}\n{} pages", title(&tab.path), tab.sizes.len());
                    for (label, value) in metadata {
                        if !value.trim().is_empty() {
                            details.push_str(&format!("\n{label}  {value}"));
                        }
                    }
                    self.report_message(ui, "Document properties", &details);
                }
            }
            Event::Matches(id, token, results, done) => {
                if let Some(index) = self
                    .tabs
                    .iter()
                    .position(|tab| tab.id == id && tab.search.token == token)
                {
                    let tab = &mut self.tabs[index];
                    tab.search.matches.extend(results);
                    tab.search.done = done;
                    if self.active == Some(index) {
                        if tab.search.open
                            && tab.search.selected.is_none()
                            && !tab.search.matches.is_empty()
                        {
                            let current = tab
                                .search
                                .matches
                                .iter()
                                .position(|result| result.page >= tab.reading.page);
                            if let Some(current) = current.or_else(|| done.then_some(0)) {
                                self.jump_to_match(ui, current);
                            }
                        }
                        self.search_status(ui);
                        self.refresh(ui, false);
                    }
                }
            }
            Event::SearchFailed(id, token, error) => {
                if let Some(index) = self
                    .tabs
                    .iter()
                    .position(|tab| tab.id == id && tab.search.token == token)
                {
                    self.tabs[index].search.failed = true;
                    if self.active == Some(index) {
                        self.search_status(ui);
                        ui.set_status_text(
                            format!("Could not finish searching this PDF. {error}").into(),
                        );
                    }
                }
            }
            Event::PageText(id, result) => {
                if self.active.is_some_and(|index| self.tabs[index].id == id) {
                    let (heading, text) = match result {
                        Ok(text) if !text.trim().is_empty() => ("Page text", text),
                        Ok(_) => (
                            "Page text",
                            "No selectable text was found. Use OCR for scanned pages.".into(),
                        ),
                        Err(error) => ("Could not extract page text", error),
                    };
                    self.report_message(ui, heading, &text);
                }
            }
            Event::Recognized(id, result) => {
                if self.active.is_some_and(|index| self.tabs[index].id == id) {
                    let (heading, text) = match result {
                        Ok(text) if !text.trim().is_empty() => ("Extracted text", text),
                        Ok(_) => ("Extracted text", "No text was recognized.".into()),
                        Err(error) => ("Could not extract text", error),
                    };
                    ui.set_status_text("".into());
                    self.report_message(ui, heading, &text);
                }
            }
            Event::Selected(id, page, selection) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                    let tab = &mut self.tabs[index];
                    let old = tab.selection.as_ref().map(|(page, _)| *page);
                    tab.selection = (!selection.is_empty()).then_some((page, selection));
                    if self.active == Some(index) {
                        if let Some(old) = old.filter(|old| *old != page) {
                            self.refresh_overlay(old);
                        }
                        self.refresh_overlay(page);
                    }
                }
            }
            Event::Coordinates(id, page, rotation, coordinates) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                    let tab = &mut self.tabs[index];
                    tab.coordinates
                        .retain(|(existing, angle, _)| *existing != page || *angle != rotation);
                    if tab.coordinates.len() == 32 {
                        tab.coordinates.pop_front();
                    }
                    tab.coordinates.push_back((page, rotation, coordinates));
                    if self.active == Some(index) {
                        self.refresh_overlay(page);
                    }
                }
            }
            Event::Edited(id, page, dirty) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                    let tab = &mut self.tabs[index];
                    *tab.revision.entry(page).or_default() += 1;
                    tab.dirty = dirty;
                    if self.active == Some(index) {
                        if let Some(mut row) = self.rail.row_data(page)
                            && self.sidebar_mode == SidebarMode::Thumbnails
                        {
                            row.thumbnail = Image::default();
                            self.rail.set_row_data(page, row);
                        }
                        self.refresh(ui, true);
                    }
                    self.sync(ui);
                }
            }
            Event::ActionDone(id) => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    tab.pending = tab.pending.saturating_sub(1);
                }
            }
            Event::Saved(id, path) => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    tab.path = path;
                    tab.downloaded = None;
                    tab.dirty = false;
                    tab.saving = false;
                    tab.save_error = None;
                    self.sync(ui);
                    self.save_later();
                    if self.active.is_some_and(|index| self.tabs[index].id == id) {
                        ui.set_status_text("Document saved.".into());
                    }
                }
            }
            Event::SaveFailed(id, error) => {
                let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
                    return;
                };
                let tab = &mut self.tabs[index];
                tab.saving = false;
                let message = format!("Could not save {}. {error}", title(&tab.path));
                tab.save_error = Some(message.clone());
                if self.active == Some(index) {
                    ui.set_status_text(message.into());
                    self.report_message(ui, "Could not save PDF", &error);
                }
            }
            Event::AnnotationText(id, text) => {
                if self.active.is_some_and(|index| self.tabs[index].id == id) {
                    self.report_message(ui, "Annotation", &text);
                }
            }
            Event::Clipboard(text) => {
                let result = if let Some(clipboard) = self.clipboard.as_mut() {
                    clipboard.set_text(text)
                } else {
                    arboard::Clipboard::new().and_then(|mut clipboard| {
                        clipboard.set_text(text)?;
                        self.clipboard = Some(clipboard);
                        Ok(())
                    })
                };
                ui.set_status_text(match result {
                    Ok(()) => "Text copied.".into(),
                    Err(error) => format!("Could not copy text: {error}").into(),
                });
            }
        }
    }
}

fn valid_size((width, height): (f32, f32)) -> (f32, f32) {
    if width.is_finite() && height.is_finite() && width > 0.1 && height > 0.1 {
        (width.min(100_000.), height.min(100_000.))
    } else {
        (612., 792.)
    }
}

pub(super) fn overlay(tab: &super::Tab, placement: Placement, drag: Option<&super::Drag>) -> Image {
    let page = placement.page;
    let selection = tab.selection.as_ref().filter(|(other, _)| *other == page);
    let matches = if tab.search.open {
        let start = tab.search.matches.partition_point(|item| item.page < page);
        let end = tab.search.matches.partition_point(|item| item.page <= page);
        &tab.search.matches[start..end]
    } else {
        &[]
    };
    let drag = drag.filter(|drag| drag.id == tab.id && drag.page == page);
    if selection.is_none() && matches.is_empty() && drag.is_none() {
        return Image::default();
    }
    let coords = tab
        .coordinates
        .iter()
        .find(|(other, rotation, _)| *other == page && *rotation == tab.reading.rotation)
        .map(|(_, _, position)| *position);
    let ratio = (420. / placement.width.max(placement.height)).min(1.);
    let width = (placement.width * ratio).ceil().max(1.) as u32;
    let height = (placement.height * ratio).ceil().max(1.) as u32;
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let mut canvas = Overlay {
        width: width as i32,
        height: height as i32,
        pixels: pixels.make_mut_slice(),
    };
    if let Some(coords) = coords {
        let mut mark = |rect: Rect, color| {
            let corners = [
                Point {
                    x: rect.left,
                    y: rect.top,
                },
                Point {
                    x: rect.right,
                    y: rect.bottom,
                },
            ]
            .map(|point| coords.from_pdf(point));
            canvas.rectangle(corners[0], corners[1], color);
        };
        for result in matches {
            let active = tab
                .search
                .selected
                .and_then(|index| tab.search.matches.get(index))
                .is_some_and(|selected| std::ptr::eq(selected, result));
            for &rect in &result.parts {
                mark(
                    rect,
                    if active {
                        [255, 141, 38, 155]
                    } else {
                        [255, 209, 53, 113]
                    },
                );
            }
        }
        if let Some((_, selection)) = selection {
            for &rect in selection {
                mark(rect, [53, 129, 245, 96]);
            }
        }
    }
    if let Some(drag) = drag {
        let points = &drag.gesture.points;
        let first = points[0];
        let last = *points.last().unwrap_or(&first);
        let color = drag.gesture.color;
        let radius = (drag.gesture.thickness * ratio * placement.width / tab.sizes[page].0 / 2.)
            .ceil()
            .clamp(1., 12.) as i32;
        match drag.gesture.tool.as_str() {
            "ink" => {
                for pair in points.windows(2) {
                    canvas.line(pair[0], pair[1], radius, color);
                }
            }
            "line" | "arrow" | "move" => canvas.line(first, last, radius, color),
            "rectangle" => {
                let top_right = Point {
                    x: last.x,
                    y: first.y,
                };
                let bottom_left = Point {
                    x: first.x,
                    y: last.y,
                };
                for (a, b) in [
                    (first, top_right),
                    (top_right, last),
                    (last, bottom_left),
                    (bottom_left, first),
                ] {
                    canvas.line(a, b, radius, color);
                }
            }
            "ellipse" => {
                let center = Point {
                    x: (first.x + last.x) / 2.,
                    y: (first.y + last.y) / 2.,
                };
                let point = |segment: i32| {
                    let angle = segment as f32 * std::f32::consts::TAU / 32.;
                    Point {
                        x: center.x + (last.x - first.x) * angle.cos() / 2.,
                        y: center.y + (last.y - first.y) * angle.sin() / 2.,
                    }
                };
                for segment in 0..32 {
                    canvas.line(point(segment), point(segment + 1), radius, color);
                }
            }
            "select" | "highlight" | "underline" | "strikeout" | "ocr" if first != last => {
                canvas.rectangle(first, last, [53, 129, 245, 54]);
            }
            _ => {}
        }
    }
    Image::from_rgba8(pixels)
}

struct Overlay<'a> {
    width: i32,
    height: i32,
    pixels: &'a mut [Rgba8Pixel],
}

impl Overlay<'_> {
    fn xy(&self, point: Point) -> (i32, i32) {
        (
            (point.x * self.width as f32) as i32,
            (point.y * self.height as f32) as i32,
        )
    }

    fn fill(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, [r, g, b, a]: [u8; 4]) {
        let left = x1.min(x2).clamp(0, self.width - 1) as usize;
        let right = x1.max(x2).clamp(0, self.width - 1) as usize;
        let top = y1.min(y2).clamp(0, self.height - 1) as usize;
        let bottom = y1.max(y2).clamp(0, self.height - 1) as usize;
        let width = self.width as usize;
        for row in top..=bottom {
            self.pixels[row * width + left..=row * width + right].fill(Rgba8Pixel { r, g, b, a });
        }
    }

    fn rectangle(&mut self, first: Point, last: Point, color: [u8; 4]) {
        let (x1, y1) = self.xy(first);
        let (x2, y2) = self.xy(last);
        self.fill(x1, y1, x2, y2, color);
    }

    fn line(&mut self, first: Point, last: Point, radius: i32, color: [u8; 4]) {
        let (x, y) = self.xy(first);
        let (x2, y2) = self.xy(last);
        let (dx, dy) = (x2 - x, y2 - y);
        let steps = dx.abs().max(dy.abs()).max(1);
        for step in 0..=steps {
            let x = x + dx * step / steps;
            let y = y + dy * step / steps;
            self.fill(x - radius, y - radius, x + radius, y + radius, color);
        }
    }
}
