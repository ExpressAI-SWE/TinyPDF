use crate::storage::{Mode, Reading, Zoom};

const GAP: f32 = 16.;

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub page: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Default)]
pub struct Layout {
    pub pages: Vec<Placement>,
    pub scale: f32,
    pub width: f32,
    pub height: f32,
}

impl Layout {
    pub fn rebuild(&mut self, sizes: &[(f32, f32)], reading: &Reading, viewport: (f32, f32)) {
        self.pages.clear();
        let (available_width, available_height) = viewport;
        let rotate = |(width, height)| {
            if reading.rotation % 2 == 0 {
                (width, height)
            } else {
                (height, width)
            }
        };
        let reference = sizes.get(reading.page).copied().unwrap_or((612., 792.));
        let first = if reading.mode == Mode::Spread {
            reading.page / 2 * 2
        } else {
            reading.page
        };
        let last = match reading.mode {
            Mode::Continuous => sizes.len(),
            Mode::Single => (first + 1).min(sizes.len()),
            Mode::Spread => (first + 2).min(sizes.len()),
        };
        let start = if reading.mode == Mode::Continuous {
            0
        } else {
            first
        };
        let mut reference_size = rotate(reference);
        let mut spread_gap = 0.;
        if reading.mode == Mode::Continuous {
            // A uniform scale keeps page geometry stable as a mixed-size document scrolls.
            reference_size = rotate(sizes.first().copied().unwrap_or(reference));
        } else if reading.mode == Mode::Spread && last > first {
            let (width, height) = rotate(sizes[first]);
            reference_size = (width, height);
            if last - first == 2 {
                let (width, height) = rotate(sizes[first + 1]);
                reference_size = (reference_size.0 + width, reference_size.1.max(height));
                spread_gap = GAP;
            }
        }
        let width_fit = (available_width - 2. * GAP - spread_gap) / reference_size.0;
        let height_fit = (available_height - 2. * GAP) / reference_size.1;
        self.scale = match reading.zoom {
            Zoom::Fixed(scale) => scale * (96. / 72.),
            Zoom::FitWidth => width_fit,
            Zoom::FitPage => width_fit.min(height_fit),
        }
        .clamp(0.05, 12.);

        let mut row_width: f32 = 0.;
        let mut row_height: f32 = 0.;
        let mut top = GAP;
        let mut widest: f32 = 0.;
        for (page, size) in sizes.iter().copied().enumerate().take(last).skip(start) {
            let (width, height) = rotate(size);
            let width = width * self.scale;
            let height = height * self.scale;
            let (x, y) = match reading.mode {
                Mode::Continuous => {
                    let y = top;
                    top += height + GAP;
                    widest = widest.max(width);
                    (0., y)
                }
                _ => {
                    let x = row_width;
                    row_width += width + GAP;
                    row_height = row_height.max(height);
                    widest = row_width - GAP;
                    (x, GAP)
                }
            };
            self.pages.push(Placement {
                page,
                x,
                y,
                width,
                height,
            });
        }
        self.width = available_width.max(widest + 2. * GAP);
        let document_height = if reading.mode == Mode::Continuous {
            top
        } else {
            row_height + 2. * GAP
        };
        self.height = available_height.max(document_height);
        for page in &mut self.pages {
            if reading.mode == Mode::Spread {
                page.x += (self.width - widest) / 2.;
            } else {
                page.x = (self.width - page.width) / 2.;
            }
            if reading.mode != Mode::Continuous {
                page.y = (self.height - page.height) / 2.;
            }
        }
    }

    pub fn position(&self, page: usize, fraction: f32, height: f32) -> f32 {
        let Some(placement) = self.pages.first().and_then(|first| {
            self.pages
                .get(page.saturating_sub(first.page))
                .filter(|item| item.page == page)
        }) else {
            return 0.;
        };
        (placement.y - GAP + placement.height * fraction).clamp(0., (self.height - height).max(0.))
    }

    pub fn anchor(&self, top: f32) -> Option<(usize, f32)> {
        let index = self
            .pages
            .partition_point(|item| item.y + item.height <= top + GAP);
        let item = self.pages.get(index).or_else(|| self.pages.last())?;
        Some((
            item.page,
            ((top + GAP - item.y) / item.height).clamp(0., 1.),
        ))
    }

    pub fn visible(&self, top: f32, height: f32) -> &[Placement] {
        if self.pages.len() <= 2 {
            return &self.pages;
        }
        let start = self
            .pages
            .partition_point(|item| item.y + item.height <= top);
        let end = self.pages.partition_point(|item| item.y < top + height);
        &self.pages[start.min(end)..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_layout_retains_fraction_through_zoom_and_rotation() {
        let mut layout = Layout::default();
        let mut reading = Reading::default();
        let sizes = [(100., 200.); 10];
        for angle in [0, 1] {
            reading.rotation = angle;
            for zoom in [Zoom::FitPage, Zoom::Fixed(2.)] {
                reading.zoom = zoom;
                layout.rebuild(&sizes, &reading, (300., 400.));
                let top = layout.position(4, 0.25, 400.);
                let (page, fraction) = layout.anchor(top).unwrap();
                assert_eq!(page, 4);
                assert!((fraction - 0.25).abs() < 0.0001);
                assert!(layout.visible(top, 400.).iter().any(|page| page.page == 4));
            }
        }
    }
}
