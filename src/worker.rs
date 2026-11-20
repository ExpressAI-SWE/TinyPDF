use crate::{
    app,
    pdf::{
        Annotation, AnnotationKind, Document, OutlineEntry, PageCoordinates, PdfError, Pdfium,
        Point, Rect, RenderRegion, Selection,
    },
};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
};

pub type TabId = u64;
pub const TILE: u32 = 1024;
const MAX_UNDO: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Raster {
    pub tab: TabId,
    pub page: usize,
    pub rotation: u8,
    pub revision: u64,
    pub full_width: u32,
    pub full_height: u32,
    pub x: u32,
    pub y: u32,
    pub thumbnail: bool,
}

impl Raster {
    pub fn region(self) -> RenderRegion {
        RenderRegion {
            x: self.x,
            y: self.y,
            width: (self.full_width - self.x).min(TILE),
            height: (self.full_height - self.y).min(TILE),
            full_width: self.full_width,
            full_height: self.full_height,
        }
    }
}

pub enum Command {
    Open(TabId, PathBuf, Option<String>),
    Close(TabId),
    Outline(TabId),
    Metadata(TabId),
    Search(TabId, u64, String),
    Gesture(TabId, usize, u8, Gesture),
    Copy(TabId),
    PageText(TabId, usize),
    #[cfg(windows)]
    Speak(TabId, usize),
    #[cfg(windows)]
    StopSpeech,
    Recognize(TabId, usize, u8, Option<(Point, Point)>),
    Undo(TabId, bool),
    Save(TabId, PathBuf),
    #[cfg(windows)]
    Print(TabId, crate::platform::PrintJob, String, u8),
    Stop,
}

#[derive(Clone)]
pub struct Gesture {
    pub tool: String,
    pub points: Vec<Point>,
    pub color: [u8; 4],
    pub thickness: f32,
    pub text: String,
}

pub struct Match {
    pub page: usize,
    pub position: Point,
    pub parts: Vec<Rect>,
}

pub enum Event {
    Opened(TabId, usize, (f32, f32)),
    OpenFailed(TabId, PdfError),
    Status(String),
    Dimensions(TabId, usize, Vec<(f32, f32)>),
    Tile(Raster, SharedPixelBuffer<Rgba8Pixel>),
    TileFailed(Raster, String),
    Outline(TabId, Vec<OutlineEntry>),
    Metadata(TabId, Vec<(String, String)>),
    Matches(TabId, u64, Vec<Match>, bool),
    Selected(TabId, usize, Vec<Rect>),
    Recognized(TabId, Result<String, String>),
    PageText(TabId, Result<String, String>),
    Coordinates(TabId, usize, u8, PageCoordinates),
    Edited(TabId, usize, bool),
    Saved(TabId, PathBuf),
    SaveFailed(TabId, String),
    Clipboard(String),
    AnnotationText(TabId, String),
    ActionDone(TabId),
}

fn deliver(event: Event) {
    let _ = slint::invoke_from_event_loop(move || app::deliver(event));
}

#[derive(Default)]
struct Requests {
    commands: VecDeque<Command>,
    plan: Option<(TabId, u64, Vec<Raster>)>,
}

pub struct Worker {
    queue: Arc<Mutex<Requests>>,
    epoch: Arc<AtomicU64>,
    wake: SyncSender<()>,
}

impl Worker {
    pub fn start() -> std::io::Result<Self> {
        let queue = Arc::new(Mutex::new(Requests::default()));
        let epoch = Arc::new(AtomicU64::new(0));
        let (wake, receiver) = sync_channel(1);
        let worker_queue = queue.clone();
        let worker_epoch = epoch.clone();
        std::thread::Builder::new()
            .name("TinyPDF document".into())
            .spawn(move || {
                let engine = std::cell::OnceCell::new();
                let mut documents: HashMap<TabId, OpenDocument<'_>> = HashMap::new();
                let mut active = 0;
                let mut inactive_to_trim = None;
                #[cfg(windows)]
                let mut speech = None;
                let mut generation = 0;
                let mut rasters = VecDeque::new();
                let mut coordinates = VecDeque::new();
                loop {
                    let command = {
                        let mut request = worker_queue
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some((id, current, jobs)) = request.plan.take() {
                            if active != id {
                                inactive_to_trim = Some(active);
                                coordinates.clear();
                            }
                            active = id;
                            generation = current;
                            rasters = jobs.into();
                        }
                        request.commands.pop_front()
                    };
                    if let Some(command) = command {
                        if matches!(command, Command::Stop) {
                            return;
                        }
                        #[cfg(windows)]
                        match command {
                            Command::Speak(tab, page) => {
                                if let Some(document) = documents.get(&tab) {
                                    let result = document
                                        .document
                                        .page_text(page as u32)
                                        .map_err(|error| error.to_string())
                                        .and_then(|text| speak(&mut speech, &text));
                                    deliver(Event::Status(
                                        result
                                            .err()
                                            .unwrap_or_else(|| "Started reading this page.".into()),
                                    ));
                                }
                                continue;
                            }
                            Command::StopSpeech => {
                                if let Some(voice) = &speech {
                                    let _ = voice.speak("");
                                }
                                continue;
                            }
                            _ => {}
                        }
                        process(command, &engine, &mut documents);
                        continue;
                    }
                    if let Some(key) = rasters.pop_front() {
                        if worker_epoch.load(Ordering::Relaxed) != generation {
                            continue;
                        }
                        if let Some(tab) = documents.get(&key.tab) {
                            if !key.thumbnail && !coordinates.contains(&(key.page, key.rotation)) {
                                if let Ok(position) =
                                    tab.document.page_coordinates(key.page as u32, key.rotation)
                                {
                                    deliver(Event::Coordinates(
                                        key.tab,
                                        key.page,
                                        key.rotation,
                                        position,
                                    ));
                                }
                                if coordinates.len() == 32 {
                                    coordinates.pop_front();
                                }
                                coordinates.push_back((key.page, key.rotation));
                            }
                            let region = key.region();
                            let mut pixels =
                                SharedPixelBuffer::<Rgba8Pixel>::new(region.width, region.height);
                            match tab.document.render_region_rgba(
                                key.page as u32,
                                region,
                                key.rotation,
                                pixels.make_mut_bytes(),
                                || {
                                    if worker_epoch.load(Ordering::Relaxed) == generation {
                                        return false;
                                    }
                                    // Keep rendering a tile that survived the viewport change.
                                    let requests = worker_queue.lock().unwrap();
                                    requests
                                        .plan
                                        .as_ref()
                                        .is_none_or(|(_, _, jobs)| !jobs.contains(&key))
                                },
                            ) {
                                Ok(()) => {
                                    if let Some((_, _, jobs)) =
                                        &mut worker_queue.lock().unwrap().plan
                                    {
                                        jobs.retain(|job| *job != key);
                                    }
                                    deliver(Event::Tile(key, pixels));
                                }
                                Err(PdfError::Cancelled) => {}
                                Err(error) => deliver(Event::TileFailed(key, error.to_string())),
                            }
                        }
                        continue;
                    }
                    // Recycle the previous pristine document after the new viewport is rendered.
                    if let Some(id) = inactive_to_trim.take()
                        && let Some(tab) = documents.get(&id)
                    {
                        tab.document.recycle_stream_cache(4 * 1024 * 1024);
                    }
                    if let Some(tab) = documents.get_mut(&active) {
                        if let Some(search) = &mut tab.search {
                            let count = tab.document.page_count();
                            if search.page < count {
                                let page = search.page;
                                match tab.document.search_page_up_to(
                                    page,
                                    &search.query,
                                    20_000usize.saturating_sub(search.count),
                                ) {
                                    Ok(matches) => {
                                        let coordinates = if matches.is_empty() {
                                            None
                                        } else {
                                            tab.document.page_coordinates(page, 0).ok()
                                        };
                                        for parts in matches {
                                            if let Some(rect) = bounds(&parts) {
                                                search.pending.push(Match {
                                                    page: page as usize,
                                                    position: coordinates.map_or(
                                                        Point::default(),
                                                        |coords| {
                                                            coords.from_pdf(Point {
                                                                x: rect.left,
                                                                y: rect.top,
                                                            })
                                                        },
                                                    ),
                                                    parts,
                                                });
                                                search.count += 1;
                                                if search.count >= 20_000 {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        deliver(Event::Status(error.to_string()));
                                        search.page = count;
                                    }
                                }
                                search.page = search.page.saturating_add(1);
                                let done = search.page >= count || search.count >= 20_000;
                                if done
                                    || search.pending.len() >= 64
                                    || (search.page <= 4 && !search.pending.is_empty())
                                {
                                    deliver(Event::Matches(
                                        active,
                                        search.token,
                                        std::mem::take(&mut search.pending),
                                        done,
                                    ));
                                }
                                if done {
                                    tab.search = None;
                                }
                                continue;
                            }
                        }
                        let count = tab.document.page_count() as usize;
                        if tab.next_size < count {
                            let page = tab.next_size;
                            tab.sizes.push(
                                tab.document
                                    .page_size(page as u32)
                                    .unwrap_or(tab.first_size),
                            );
                            tab.next_size += 1;
                            if tab.sizes.len() >= 48 || tab.next_size == count {
                                let first = tab.next_size - tab.sizes.len();
                                deliver(Event::Dimensions(
                                    active,
                                    first,
                                    std::mem::take(&mut tab.sizes),
                                ));
                            }
                            continue;
                        }
                    }
                    if receiver.recv().is_err() {
                        return;
                    }
                }
            })?;
        Ok(Self { queue, epoch, wake })
    }

    pub fn send(&self, command: Command) {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .commands
            .push_back(command);
        let _ = self.wake.try_send(());
    }

    pub fn render(&self, tab: TabId, jobs: Vec<Raster>) {
        let mut requests = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let epoch = self.epoch.fetch_add(1, Ordering::Relaxed) + 1;
        requests.plan = Some((tab, epoch, jobs));
        let _ = self.wake.try_send(());
    }
}

struct Search {
    token: u64,
    query: String,
    page: u32,
    count: usize,
    pending: Vec<Match>,
}

struct Edit {
    page: usize,
    before: Option<Annotation>,
    after: Option<Annotation>,
    previous: u64,
    revision: u64,
}

struct OpenDocument<'a> {
    document: Document<'a>,
    first_size: (f32, f32),
    next_size: usize,
    sizes: Vec<(f32, f32)>,
    search: Option<Search>,
    selection: String,
    undo: VecDeque<Edit>,
    redo: Vec<Edit>,
    sequence: u64,
    revision: u64,
    saved: u64,
}

fn process<'a>(
    command: Command,
    engine: &'a std::cell::OnceCell<Result<Pdfium, PdfError>>,
    tabs: &mut HashMap<TabId, OpenDocument<'a>>,
) {
    let id = match &command {
        Command::Open(id, ..)
        | Command::Close(id)
        | Command::Outline(id)
        | Command::Metadata(id)
        | Command::Search(id, ..)
        | Command::Gesture(id, ..)
        | Command::Copy(id)
        | Command::PageText(id, ..)
        | Command::Recognize(id, ..)
        | Command::Undo(id, ..)
        | Command::Save(id, ..) => *id,
        #[cfg(windows)]
        Command::Print(id, ..) => *id,
        #[cfg(windows)]
        Command::Speak(..) | Command::StopSpeech => return,
        Command::Stop => return,
    };
    if let Command::Open(_, path, password) = command {
        let result = match engine.get_or_init(Pdfium::load) {
            Ok(engine) => engine.open(&path, password.as_deref()),
            Err(error) => Err(PdfError::Library(error.to_string())),
        };
        match result {
            Ok(document) => {
                let count = document.page_count() as usize;
                let first_size = document.page_size(0).unwrap_or((612., 792.));
                tabs.insert(
                    id,
                    OpenDocument {
                        document,
                        first_size,
                        next_size: 1,
                        sizes: Vec::new(),
                        search: None,
                        selection: String::new(),
                        undo: VecDeque::new(),
                        redo: Vec::new(),
                        sequence: 0,
                        revision: 0,
                        saved: 0,
                    },
                );
                deliver(Event::Opened(id, count, first_size));
            }
            Err(error) => deliver(Event::OpenFailed(id, error)),
        }
        return;
    }
    if matches!(command, Command::Close(_)) {
        tabs.remove(&id);
        return;
    }
    let Some(tab) = tabs.get_mut(&id) else { return };
    let action = matches!(command, Command::Gesture(..) | Command::Undo(..));
    let result = match command {
        Command::Outline(_) => {
            deliver(Event::Outline(id, tab.document.outline()));
            Ok(())
        }
        Command::Metadata(_) => {
            deliver(Event::Metadata(id, tab.document.metadata()));
            Ok(())
        }
        Command::Search(_, token, query) => {
            if query.is_empty() {
                tab.search = None;
            } else {
                tab.search = Some(Search {
                    token,
                    query,
                    page: 0,
                    count: 0,
                    pending: Vec::new(),
                });
            }
            Ok(())
        }
        Command::Gesture(_, page, rotation, gesture) => tab.gesture(id, page, rotation, gesture),
        Command::Recognize(_, page, rotation, region) => {
            deliver(Event::Recognized(
                id,
                recognize(&tab.document, page as u32, rotation, region),
            ));
            Ok(())
        }
        Command::PageText(_, page) => {
            deliver(Event::PageText(
                id,
                tab.document
                    .page_text(page as u32)
                    .map_err(|error| error.to_string()),
            ));
            Ok(())
        }
        Command::Copy(_) => {
            if !tab.selection.is_empty() {
                deliver(Event::Clipboard(tab.selection.clone()));
            }
            Ok(())
        }
        Command::Undo(_, redo) => tab.replay(id, redo),
        Command::Save(_, path) => {
            match tab.document.save_as(&path) {
                Ok(()) => {
                    tab.saved = tab.revision;
                    deliver(Event::Saved(id, path));
                }
                Err(error) => deliver(Event::SaveFailed(id, error.to_string())),
            }
            Ok(())
        }
        #[cfg(windows)]
        Command::Print(_, job, title, rotation) => {
            let message = match job.print(&tab.document, &title, rotation) {
                Ok(()) => "Sent to printer.".into(),
                Err(error) => error,
            };
            deliver(Event::Status(message));
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(error) = result {
        deliver(Event::Status(error.to_string()));
    }
    if action {
        deliver(Event::ActionDone(id));
    }
}

impl OpenDocument<'_> {
    fn gesture(
        &mut self,
        id: TabId,
        page: usize,
        rotation: u8,
        gesture: Gesture,
    ) -> Result<(), PdfError> {
        let coordinates = self.document.page_coordinates(page as u32, rotation)?;
        deliver(Event::Coordinates(id, page, rotation, coordinates));
        let mut points: Vec<_> = gesture
            .points
            .into_iter()
            .map(|point| coordinates.to_pdf(point))
            .collect();
        let Some(&first) = points.first() else {
            return Ok(());
        };
        let last = *points.last().unwrap_or(&first);
        let kind = match gesture.tool.as_str() {
            "select" | "highlight" | "underline" | "strikeout" => {
                if gesture.tool == "select"
                    && (last.x - first.x).hypot(last.y - first.y) < 2.
                    && let Some(note) = self
                        .document
                        .annotations(page as u32)?
                        .into_iter()
                        .rev()
                        .find(|item| {
                            matches!(
                                item.annotation.kind,
                                AnnotationKind::Note | AnnotationKind::Comment
                            ) && (item.bounds.left..=item.bounds.right).contains(&first.x)
                                && (item.bounds.bottom..=item.bounds.top).contains(&first.y)
                        })
                {
                    deliver(Event::AnnotationText(id, note.annotation.text));
                    return Ok(());
                }
                let selection = if first == last {
                    Selection::default()
                } else {
                    self.document.select(page as u32, first, last)?
                };
                if gesture.tool == "select" || selection.rects.is_empty() {
                    self.selection = selection.text;
                    deliver(Event::Selected(id, page, selection.rects));
                    return Ok(());
                }
                let kind = match gesture.tool.as_str() {
                    "highlight" => AnnotationKind::Highlight,
                    "underline" => AnnotationKind::Underline,
                    _ => AnnotationKind::Strikeout,
                };
                self.record(
                    id,
                    page,
                    None,
                    Some(Annotation {
                        kind,
                        rects: selection.rects,
                        points: Vec::new(),
                        color: gesture.color,
                        thickness: gesture.thickness,
                        text: String::new(),
                    }),
                )?;
                self.selection.clear();
                deliver(Event::Selected(id, page, Vec::new()));
                return Ok(());
            }
            "move" | "delete" | "eraser" => {
                let candidates = self.document.annotations(page as u32)?;
                let target = candidates.into_iter().rev().find(|item| {
                    let tolerance = gesture.thickness.max(5.);
                    let rect = item.bounds;
                    (rect.left - tolerance..=rect.right + tolerance).contains(&first.x)
                        && (rect.bottom - tolerance..=rect.top + tolerance).contains(&first.y)
                });
                let Some(target) = target else {
                    return Err(PdfError::Invalid(
                        "No editable TinyPDF annotation at this position.",
                    ));
                };
                let before = target.annotation;
                let after = (gesture.tool == "move").then(|| {
                    let mut moved = before.clone();
                    let dx = last.x - first.x;
                    let dy = last.y - first.y;
                    for point in &mut moved.points {
                        point.x += dx;
                        point.y += dy;
                    }
                    for rect in &mut moved.rects {
                        rect.left += dx;
                        rect.right += dx;
                        rect.top += dy;
                        rect.bottom += dy;
                    }
                    moved
                });
                if after.as_ref() != Some(&before) {
                    self.record(id, page, Some(before), after)?;
                }
                return Ok(());
            }
            "ink" => AnnotationKind::Ink,
            "note" => AnnotationKind::Note,
            "text" => AnnotationKind::Comment,
            "line" => AnnotationKind::Line,
            "arrow" => AnnotationKind::Arrow,
            "rectangle" => AnnotationKind::Rectangle,
            "ellipse" => AnnotationKind::Circle,
            _ => return Ok(()),
        };
        let mut rects = Vec::new();
        match kind {
            AnnotationKind::Rectangle | AnnotationKind::Circle => {
                if (last.x - first.x).abs() < 1. || (last.y - first.y).abs() < 1. {
                    return Ok(());
                }
                rects.push(Rect {
                    left: first.x.min(last.x),
                    right: first.x.max(last.x),
                    bottom: first.y.min(last.y),
                    top: first.y.max(last.y),
                });
                points.clear();
            }
            AnnotationKind::Note | AnnotationKind::Comment => {
                let (width, height) = if kind == AnnotationKind::Comment {
                    (160., 54.)
                } else {
                    (18., 18.)
                };
                rects.push(Rect {
                    left: first.x,
                    top: first.y,
                    right: first.x + width,
                    bottom: first.y - height,
                });
                points.clear();
            }
            AnnotationKind::Ink => {
                if first == last && points.len() <= 2 {
                    points = vec![
                        first,
                        Point {
                            x: first.x + 0.2,
                            y: first.y + 0.2,
                        },
                    ];
                }
            }
            _ => {
                if first == last {
                    return Ok(());
                }
                points = vec![first, last];
            }
        }
        self.record(
            id,
            page,
            None,
            Some(Annotation {
                kind,
                color: gesture.color,
                thickness: gesture.thickness,
                rects,
                points,
                text: gesture.text,
            }),
        )
    }

    fn change(
        &self,
        page: usize,
        before: Option<&Annotation>,
        after: Option<&Annotation>,
    ) -> Result<(), PdfError> {
        if let Some(before) = before {
            let Some(item) = self
                .document
                .annotations(page as u32)?
                .into_iter()
                .rev()
                .find(|item| &item.annotation == before)
            else {
                return Err(PdfError::Invalid("This annotation could not be found."));
            };
            if let Some(after) = after {
                self.document
                    .replace_annotation(page as u32, item.index, after)?;
            } else {
                self.document.remove_annotation(page as u32, item.index)?;
            }
        } else if let Some(after) = after {
            self.document.add_annotation(page as u32, after)?;
        }
        Ok(())
    }

    fn record(
        &mut self,
        id: TabId,
        page: usize,
        before: Option<Annotation>,
        after: Option<Annotation>,
    ) -> Result<(), PdfError> {
        self.change(page, before.as_ref(), after.as_ref())?;
        self.sequence += 1;
        self.undo.push_back(Edit {
            page,
            before,
            after,
            previous: self.revision,
            revision: self.sequence,
        });
        if self.undo.len() > MAX_UNDO {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.revision = self.sequence;
        deliver(Event::Edited(id, page, self.saved != self.revision));
        Ok(())
    }

    fn replay(&mut self, id: TabId, redo: bool) -> Result<(), PdfError> {
        if redo {
            let Some(edit) = self.redo.last() else {
                return Ok(());
            };
            self.change(edit.page, edit.before.as_ref(), edit.after.as_ref())?;
            let edit = self.redo.pop().unwrap();
            self.revision = edit.revision;
            deliver(Event::Edited(id, edit.page, self.saved != self.revision));
            self.undo.push_back(edit);
        } else {
            let Some(edit) = self.undo.back() else {
                return Ok(());
            };
            self.change(edit.page, edit.after.as_ref(), edit.before.as_ref())?;
            let edit = self.undo.pop_back().unwrap();
            self.revision = edit.previous;
            deliver(Event::Edited(id, edit.page, self.saved != self.revision));
            self.redo.push(edit);
        }
        Ok(())
    }
}

fn bounds(parts: &[Rect]) -> Option<Rect> {
    let first = *parts.first()?;
    Some(parts.iter().skip(1).fold(first, |bounds, rect| Rect {
        left: bounds.left.min(rect.left),
        top: bounds.top.max(rect.top),
        right: bounds.right.max(rect.right),
        bottom: bounds.bottom.min(rect.bottom),
    }))
}
#[cfg(windows)]
fn recognize(
    document: &Document<'_>,
    page: u32,
    rotation: u8,
    selection: Option<(Point, Point)>,
) -> Result<String, String> {
    use windows::{
        Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap},
        Media::Ocr::OcrEngine,
        Storage::Streams::DataWriter,
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    };

    if !document.allows_copy() {
        return Err("This PDF restricts text extraction.".into());
    }
    // OCR runs only on explicit request, on the document worker.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|error| error.to_string())?;
    let result = (|| -> Result<String, Box<dyn std::error::Error>> {
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|_| "Install a Windows OCR language for your preferred language.")?;
        let (mut width, mut height) = document.page_size(page)?;
        if rotation % 2 != 0 {
            std::mem::swap(&mut width, &mut height);
        }
        let cap = OcrEngine::MaxImageDimension()?.min(2400) as f32;
        let scale = cap / width.max(height);
        let full_width = (width * scale).round().max(1.) as u32;
        let full_height = (height * scale).round().max(1.) as u32;
        let (first, last) = selection.unwrap_or((Point { x: 0., y: 0. }, Point { x: 1., y: 1. }));
        let span = |a: f32, b: f32, size: u32| {
            let position = |value: f32| (value.clamp(0., 1.) * size as f32).round() as u32;
            (position(a.min(b)), position(a.max(b)))
        };
        let (x, right) = span(first.x, last.x, full_width);
        let (y, bottom) = span(first.y, last.y, full_height);
        let (width, height) = (right - x, bottom - y);
        if width < 8 || height < 8 {
            return Err("Drag across a larger area to extract text.".into());
        }
        let region = RenderRegion {
            x,
            y,
            width,
            height,
            full_width,
            full_height,
        };
        let mut rgba = vec![0; (width * height * 4) as usize];
        document.render_region_rgba(page, region, rotation, &mut rgba, || false)?;
        // Grayscale keeps the WinRT input compact and avoids channel-order conversion.
        let gray: Vec<_> = rgba
            .chunks_exact(4)
            .map(|pixel| {
                ((u16::from(pixel[0]) * 77 + u16::from(pixel[1]) * 150 + u16::from(pixel[2]) * 29)
                    >> 8) as u8
            })
            .collect();
        let writer = DataWriter::new()?;
        writer.WriteBytes(&gray)?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &writer.DetachBuffer()?,
            BitmapPixelFormat::Gray8,
            width as i32,
            height as i32,
        )?;
        Ok(engine.RecognizeAsync(&bitmap)?.join()?.Text()?.to_string())
    })();
    unsafe { RoUninitialize() };
    result.map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn recognize(_: &Document<'_>, _: u32, _: u8, _: Option<(Point, Point)>) -> Result<String, String> {
    Err("Image text extraction is available on Windows.".into())
}
#[cfg(windows)]
struct Speech(Option<windows::Win32::Media::Speech::ISpVoice>);

#[cfg(windows)]
impl Speech {
    fn new() -> windows::core::Result<Self> {
        use windows::Win32::{
            Media::Speech::SpVoice,
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            },
        };
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        let mut speech = Self(None);
        speech.0 = Some(unsafe { CoCreateInstance(&SpVoice, None, CLSCTX_INPROC_SERVER)? });
        Ok(speech)
    }

    fn speak(&self, text: &str) -> windows::core::Result<()> {
        use windows::{
            Win32::Media::Speech::{SPF_ASYNC, SPF_IS_NOT_XML, SPF_PURGEBEFORESPEAK},
            core::PCWSTR,
        };
        // Plain text prevents embedded document text from being interpreted as speech XML.
        let mut wide: Vec<u16> = text.encode_utf16().filter(|unit| *unit != 0).collect();
        wide.push(0);
        unsafe {
            self.0.as_ref().unwrap().Speak(
                PCWSTR(wide.as_ptr()),
                (SPF_ASYNC.0 | SPF_IS_NOT_XML.0 | SPF_PURGEBEFORESPEAK.0) as u32,
                None,
            )
        }
    }
}

#[cfg(windows)]
impl Drop for Speech {
    fn drop(&mut self) {
        drop(self.0.take());
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

#[cfg(windows)]
fn speak(voice: &mut Option<Speech>, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("No selectable text was found. Use OCR for scanned pages.".into());
    }
    if voice.is_none() {
        *voice = Some(
            Speech::new().map_err(|error| format!("Windows speech could not start: {error}"))?,
        );
    }
    voice
        .as_ref()
        .unwrap()
        .speak(text)
        .map_err(|error| format!("Read Aloud failed: {error}"))
}
