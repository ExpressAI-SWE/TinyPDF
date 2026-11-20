use super::{
    App, MainWindow, PageSlot, PageTile, Raster, SidebarItem, SidebarMode, feedback::overlay,
};
use crate::{
    storage::{Mode, Zoom},
    worker::TILE,
};
use slint::{ComponentHandle, Image, Model, VecModel};
use std::rc::Rc;

const MAX_TILES: usize = 96;
const MAX_TILE_AXIS: u32 = 512 * 1024;

impl App {
    pub(super) fn relayout(&mut self, ui: &MainWindow) {
        if let Some(index) = self.active {
            let tab = &self.tabs[index];
            self.layout.rebuild(&tab.sizes, &tab.reading, self.viewport);
            self.scroll.0 = tab
                .horizontal
                .unwrap_or((self.layout.width - self.viewport.0) / 2.)
                .clamp(0., (self.layout.width - self.viewport.0).max(0.));
            self.scroll.1 =
                self.layout
                    .position(tab.reading.page, tab.reading.fraction, self.viewport.1);
            let percent = 100. * self.layout.scale * (72. / 96.);
            ui.set_zoom_percent(percent);
            ui.set_zoom_label(format!("{percent:.0}%").into());
        }
        self.refresh(ui, true);
    }

    pub(super) fn go_to(&mut self, ui: &MainWindow, page: usize) {
        let Some(index) = self.active else { return };
        let tab = &mut self.tabs[index];
        if tab.sizes.is_empty() {
            return;
        }
        let page = page.min(tab.sizes.len() - 1);
        if page != tab.reading.page {
            self.scroll_forward = page > tab.reading.page;
        }
        let rebuild = tab.reading.mode != Mode::Continuous && page != tab.reading.page;
        tab.reading.page = page;
        tab.reading.fraction = 0.;
        ui.set_page_number((page + 1) as i32);
        if rebuild {
            self.relayout(ui);
        } else {
            self.scroll.1 = self.layout.position(page, 0., self.viewport.1);
            self.refresh(ui, true);
        }
        self.save_later();
    }

    pub(super) fn zoom(&mut self, ui: &MainWindow, zoom: Zoom) {
        let Some(index) = self.active else { return };
        let tab = &mut self.tabs[index];
        if tab.reading.zoom == zoom {
            return;
        }
        tab.reading.zoom = zoom;
        tab.horizontal = None;
        self.relayout(ui);
        self.save_later();
    }

    pub(super) fn wheel(&mut self, ui: &MainWindow, x: f32, y: f32, zoom: bool) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let Some(index) = self.active else { return };
        if zoom {
            let scale = (self.layout.scale * (72. / 96.) * (y / 540.).exp()).clamp(0.1, 8.);
            self.zoom(ui, Zoom::Fixed(scale));
            return;
        }
        let old_scroll = self.scroll;
        self.scroll.0 =
            (self.scroll.0 - x).clamp(0., (self.layout.width - self.viewport.0).max(0.));
        self.scroll.1 =
            (self.scroll.1 - y).clamp(0., (self.layout.height - self.viewport.1).max(0.));
        if old_scroll == self.scroll {
            if y.abs() > 20.
                && self.tabs[index].reading.mode != Mode::Continuous
                && (if y > 0. {
                    self.scroll.1 <= 0.
                } else {
                    self.scroll.1 >= (self.layout.height - self.viewport.1).max(0.)
                })
            {
                self.step(ui, if y > 0. { -1 } else { 1 });
            }
        } else {
            if self.scroll.1 != old_scroll.1 {
                self.scroll_forward = self.scroll.1 > old_scroll.1;
            }
            self.scroll_changed(ui, index);
        }
    }

    pub(super) fn scroll_to(&mut self, ui: &MainWindow, fraction: f32) {
        if let Some(index) = self.active
            && fraction.is_finite()
        {
            let top = fraction.clamp(0., 1.) * (self.layout.height - self.viewport.1).max(0.);
            if top != self.scroll.1 {
                self.scroll_forward = top > self.scroll.1;
                self.scroll.1 = top;
                self.scroll_changed(ui, index);
            }
        }
    }

    pub(super) fn center_on(&mut self, ui: &MainWindow, page: usize, position: crate::pdf::Point) {
        let Some(index) = self.active else { return };
        let Some(item) = self
            .layout
            .visible(self.scroll.1, self.viewport.1)
            .iter()
            .find(|item| item.page == page)
        else {
            return;
        };
        self.scroll.0 = (item.x + item.width * position.x - self.viewport.0 * 0.5)
            .clamp(0., (self.layout.width - self.viewport.0).max(0.));
        self.scroll.1 = (item.y + item.height * position.y - self.viewport.1 * 0.36)
            .clamp(0., (self.layout.height - self.viewport.1).max(0.));
        let reading = &mut self.tabs[index].reading;
        reading.fraction = ((self.scroll.1 + 16. - item.y) / item.height).clamp(0., 1.);
        self.tabs[index].horizontal = Some(self.scroll.0);
        self.refresh(ui, true);
    }

    fn scroll_changed(&mut self, ui: &MainWindow, index: usize) {
        let tab = &mut self.tabs[index];
        tab.horizontal = Some(self.scroll.0);
        if tab.reading.mode == Mode::Continuous {
            if let Some((page, fraction)) = self.layout.anchor(self.scroll.1) {
                tab.reading.page = page;
                tab.reading.fraction = fraction;
                ui.set_page_number((page + 1) as i32);
            }
        } else if let Some(item) = self
            .layout
            .pages
            .iter()
            .find(|item| item.page == tab.reading.page)
        {
            tab.reading.fraction = ((self.scroll.1 + 16. - item.y) / item.height).clamp(0., 1.);
        }
        self.refresh(ui, true);
        self.save_later();
    }

    pub(super) fn step(&mut self, ui: &MainWindow, delta: isize) {
        if let Some(index) = self.active {
            let reading = &self.tabs[index].reading;
            let page = if reading.mode == Mode::Spread {
                reading.page / 2 * 2
            } else {
                reading.page
            };
            let delta = if reading.mode == Mode::Spread {
                delta * 2
            } else {
                delta
            };
            let target = page.saturating_add_signed(delta);
            if target < self.tabs[index].sizes.len() {
                self.go_to(ui, target);
            }
        }
    }

    pub(super) fn rebuild_rail(&mut self, ui: &MainWindow) {
        let Some(index) = self.active else { return };
        let tab = &self.tabs[index];
        if !self.settings.sidebar {
            self.rail = Rc::default();
            ui.set_sidebar_items(self.rail.clone().into());
            return;
        }
        let items = match self.sidebar_mode {
            SidebarMode::Thumbnails => (0..tab.sizes.len())
                .map(|page| SidebarItem {
                    label: format!("Page {}", page + 1).into(),
                    page: page as i32,
                    thumbnail: Image::default(),
                })
                .collect::<Vec<_>>(),
            SidebarMode::Outline => tab
                .outline
                .iter()
                .flatten()
                .filter_map(|entry| {
                    entry.page.map(|page| SidebarItem {
                        label: format!(
                            "{}{}",
                            "  ".repeat(entry.depth.min(8) as usize),
                            entry.title
                        )
                        .into(),
                        page: page as i32,
                        thumbnail: Image::default(),
                    })
                })
                .collect(),
            SidebarMode::Bookmarks => tab
                .reading
                .bookmarks
                .iter()
                .map(|&page| SidebarItem {
                    label: format!("Page {}", page + 1).into(),
                    page: page as i32,
                    thumbnail: Image::default(),
                })
                .collect(),
        };
        self.rail = Rc::new(VecModel::from(items));
        ui.set_sidebar_items(self.rail.clone().into());
    }

    pub(super) fn sidebar_visible(&mut self, ui: &MainWindow, start: usize, end: usize) {
        let range = start..end.max(start).min(start + 24);
        if self.rail_range == range {
            return;
        }
        for old in self.rail_range.clone().filter(|old| !range.contains(old)) {
            if let Some(mut row) = self.rail.row_data(old)
                && row.thumbnail != Image::default()
            {
                row.thumbnail = Image::default();
                self.rail.set_row_data(old, row);
            }
        }
        self.rail_range = range;
        self.refresh(ui, true);
    }

    pub(super) fn refresh(&mut self, ui: &MainWindow, schedule: bool) {
        let Some(index) = self.active else { return };
        let tab = &self.tabs[index];
        let scale = ui.window().scale_factor();
        ui.set_scroll_visible_fraction((self.viewport.1 / self.layout.height.max(1.)).min(1.));
        ui.set_scroll_progress(self.scroll.1 / (self.layout.height - self.viewport.1).max(1.));
        let mut slots = Vec::new();
        let mut plan = Vec::new();
        let mut tiles = Vec::new();
        let visible = if tab.opened() {
            self.layout.visible(self.scroll.1, self.viewport.1)
        } else {
            &[]
        };
        let mut view_bytes = 0;
        for item in visible {
            let x = item.x - self.scroll.0;
            let y = item.y - self.scroll.1;
            if x >= self.viewport.0
                || x + item.width <= 0.
                || y >= self.viewport.1
                || y + item.height <= 0.
            {
                continue;
            }
            slots.push(PageSlot {
                page: item.page as i32,
                x,
                y,
                width: item.width,
                height: item.height,
                overlay: overlay(tab, *item, self.drag.as_ref()),
            });
            let (width, height) =
                raster_size(item.width * scale, item.height * scale, MAX_TILE_AXIS);
            let per_x = width as f32 / item.width;
            let per_y = height as f32 / item.height;
            let left = ((-x * per_x).max(0.) as u32 / TILE) * TILE;
            let top = ((-y * per_y).max(0.) as u32 / TILE) * TILE;
            let right = ((self.viewport.0 - x) * per_x).ceil().min(width as f32) as u32;
            let bottom = ((self.viewport.1 - y) * per_y).ceil().min(height as f32) as u32;
            for row in (top..bottom).step_by(TILE as usize) {
                for col in (left..right).step_by(TILE as usize) {
                    if plan.len() >= MAX_TILES {
                        break;
                    }
                    let key = Raster {
                        tab: tab.id,
                        page: item.page,
                        rotation: tab.reading.rotation,
                        revision: *tab.revision.get(&item.page).unwrap_or(&0),
                        full_width: width,
                        full_height: height,
                        x: col,
                        y: row,
                        thumbnail: false,
                    };
                    let region = key.region();
                    view_bytes += region.width as usize * region.height as usize * 4;
                    plan.push(key);
                    tiles.push(PageTile {
                        x: x + col as f32 / per_x,
                        y: y + row as f32 / per_y,
                        width: region.width as f32 / per_x,
                        height: region.height as f32 / per_y,
                        bitmap: Image::default(),
                    });
                }
                if plan.len() >= MAX_TILES {
                    break;
                }
            }
        }
        // Always fit the visible tiles, with a small reuse budget on smaller windows.
        let reserve = if self.settings.memory_saver {
            2 * 1024 * 1024
        } else {
            super::RASTER_CACHE_BYTES
        };
        self.cache.set_limit(view_bytes.max(reserve), &plan);
        self.failed.retain(|key, _| {
            plan.contains(key) || (key.thumbnail && self.rail_range.contains(&key.page))
        });
        let mut requested = Vec::new();
        for (&key, tile) in plan.iter().zip(&mut tiles) {
            if let Some(image) = self.cache.get(key) {
                tile.bitmap = image;
            } else {
                // A failed replacement should still leave the last committed tile visible.
                tile.bitmap = self.cache.previous_revision(key).unwrap_or_default();
                if self.failed.get(&key).copied().unwrap_or(0) < 2 {
                    requested.push(key);
                }
            }
        }
        if self.plan == plan {
            for (index, tile) in tiles.into_iter().enumerate() {
                if self.tiles.row_data(index).as_ref() != Some(&tile) {
                    self.tiles.set_row_data(index, tile);
                }
            }
        } else {
            self.tiles.set_vec(tiles);
        }
        let same_pages = self.pages.row_count() == slots.len()
            && slots.iter().enumerate().all(|(i, slot)| {
                self.pages
                    .row_data(i)
                    .is_some_and(|old| old.page == slot.page)
            });
        if same_pages {
            for (index, slot) in slots.into_iter().enumerate() {
                self.pages.set_row_data(index, slot);
            }
        } else {
            self.pages.set_vec(slots);
        }
        let visible_jobs = requested.len();
        let mut prefetch_bytes = self.cache.limit - view_bytes;
        // Warm the entering edge in the current direction first.
        if !self.settings.memory_saver && tab.reading.mode == Mode::Continuous && prefetch_bytes > 0
        {
            let next = visible.last().map(|item| item.page + 1);
            let previous = visible.first().and_then(|item| item.page.checked_sub(1));
            let mut neighbors = [(next, false), (previous, true)];
            if !self.scroll_forward {
                neighbors.reverse();
            }
            for (index, entering_from_bottom) in neighbors {
                let Some(index) = index else { continue };
                if let Some(item) = self.layout.pages.get(index) {
                    let (width, height) =
                        raster_size(item.width * scale, item.height * scale, MAX_TILE_AXIS);
                    let x = (((self.scroll.0 - item.x).max(0.) * width as f32 / item.width) as u32
                        / TILE)
                        * TILE;
                    if x >= width {
                        continue;
                    }
                    let key = Raster {
                        tab: tab.id,
                        page: item.page,
                        rotation: tab.reading.rotation,
                        revision: *tab.revision.get(&item.page).unwrap_or(&0),
                        full_width: width,
                        full_height: height,
                        x,
                        y: if entering_from_bottom {
                            (height - 1) / TILE * TILE
                        } else {
                            0
                        },
                        thumbnail: false,
                    };
                    let region = key.region();
                    let bytes = (region.width * region.height * 4) as usize;
                    if bytes <= prefetch_bytes && !self.cache.contains(key) {
                        requested.push(key);
                        prefetch_bytes -= bytes;
                    }
                }
            }
        }
        if self.settings.sidebar && !self.fullscreen && self.sidebar_mode == SidebarMode::Thumbnails
        {
            for page in self.rail_range.clone() {
                let Some(&(mut width, mut height)) = tab.sizes.get(page) else {
                    break;
                };
                if tab.reading.rotation % 2 != 0 {
                    std::mem::swap(&mut width, &mut height);
                }
                let ratio = (110. / width).min(130. / height) * scale.min(2.);
                let (width, height) = raster_size(width * ratio, height * ratio, TILE);
                let key = Raster {
                    tab: tab.id,
                    page,
                    rotation: tab.reading.rotation,
                    revision: *tab.revision.get(&page).unwrap_or(&0),
                    full_width: width,
                    full_height: height,
                    x: 0,
                    y: 0,
                    thumbnail: true,
                };
                if let Some(image) = self.thumbnails.get(key) {
                    if let Some(mut item) = self.rail.row_data(page)
                        && item.thumbnail != image
                    {
                        item.thumbnail = image;
                        self.rail.set_row_data(page, item);
                    }
                } else if self.failed.get(&key).copied().unwrap_or(0) < 2 {
                    requested.push(key);
                }
            }
        }
        self.plan = plan;
        let id = tab.id;
        if schedule
            && (self.render_tab != id
                || requested != self.requested
                || self.render_visible != visible_jobs)
        {
            self.render_tab = id;
            self.render_visible = visible_jobs;
            self.requested = requested.clone();
            self.worker.render(id, requested, visible_jobs);
        }
    }

    pub(super) fn refresh_overlay(&self, page: usize) {
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let Some(item) = self
            .layout
            .visible(self.scroll.1, self.viewport.1)
            .iter()
            .find(|item| item.page == page)
        else {
            return;
        };
        if let Some((index, mut slot)) = self
            .pages
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.page as usize == page)
        {
            slot.overlay = overlay(tab, *item, self.drag.as_ref());
            self.pages.set_row_data(index, slot);
        }
    }
}

fn raster_size(width: f32, height: f32, max: u32) -> (u32, u32) {
    let factor = (max as f32 / width.max(height)).min(1.);
    (
        (width * factor).round().max(1.) as u32,
        (height * factor).round().max(1.) as u32,
    )
}
