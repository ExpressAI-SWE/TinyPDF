use std::{
    cell::{Cell, Ref, RefCell},
    collections::HashSet,
    env,
    ffi::{CString, c_char, c_int, c_uint, c_ulong, c_void},
    fmt::{self, Write as _},
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    marker::PhantomData,
    ops::Deref,
    path::{Path, PathBuf},
    ptr,
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::SystemTime,
};

use libloading::Library;

type Raw = *mut c_void;
type Close = unsafe extern "system" fn(Raw);
const ANNOT: c_int = 0x01;
const REVERSE_BYTE_ORDER: c_int = 0x10;
const LIMITED_IMAGE_CACHE: c_int = 0x200;
#[cfg(windows)]
const PRINTING: c_int = 0x800;
const BITMAP_BGRA: c_int = 4;
const RENDER_PAUSED: c_int = 1;
const RENDER_DONE: c_int = 2;
const SAVE_FULL_COPY: c_ulong = 1 << 1;
const ANNOTATION_PRINT: c_int = 1 << 2;
const PERMISSION_PRINT: c_ulong = 1 << 2;
const PERMISSION_COPY: c_ulong = 1 << 4;
const PERMISSION_ANNOTATE: c_ulong = 1 << 5;
const PERMISSION_PRINT_HIGH_QUALITY: c_ulong = 1 << 11;
const NATIVE_STREAM_BUDGET: u64 = 24 * 1024 * 1024;
const MAX_BITMAP_BYTES: usize = 256 * 1024 * 1024;
const MAX_ANNOTATION_POINTS: usize = 65_536;
const MAX_WIDE_BYTES: c_ulong = 512 * 1024;
static ENGINE_ACTIVE: AtomicBool = AtomicBool::new(false);
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum PdfError {
    Library(String),
    Io(io::Error),
    Invalid(&'static str),
    Pdf(&'static str),
    PasswordRequired,
    Cancelled,
}

impl fmt::Display for PdfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Library(message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
            Self::Invalid(message) | Self::Pdf(message) => formatter.write_str(message),
            Self::PasswordRequired => formatter.write_str("The PDF requires a valid password."),
            Self::Cancelled => formatter.write_str("The PDF operation was cancelled."),
        }
    }
}

impl std::error::Error for PdfError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for PdfError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    fn is_valid(self) -> bool {
        let coordinates = [self.left, self.top, self.right, self.bottom];
        coordinates
            .iter()
            .all(|n| n.is_finite() && n.abs() <= 10_000_000.0)
            && self.left < self.right
            && self.bottom < self.top
    }

    fn from_points(points: &[Point]) -> Option<Self> {
        let first = points.first()?;
        let mut bounds = Self {
            left: first.x,
            right: first.x,
            bottom: first.y,
            top: first.y,
        };
        for point in &points[1..] {
            bounds.left = bounds.left.min(point.x);
            bounds.right = bounds.right.max(point.x);
            bounds.bottom = bounds.bottom.min(point.y);
            bounds.top = bounds.top.max(point.y);
        }
        Some(bounds)
    }

    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.max(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageCoordinates {
    origin: Point,
    horizontal: Point,
    vertical: Point,
}

impl PageCoordinates {
    pub fn to_pdf(self, position: Point) -> Point {
        Point {
            x: self.origin.x + self.horizontal.x * position.x + self.vertical.x * position.y,
            y: self.origin.y + self.horizontal.y * position.x + self.vertical.y * position.y,
        }
    }

    pub fn from_pdf(self, position: Point) -> Point {
        let x = position.x - self.origin.x;
        let y = position.y - self.origin.y;
        let determinant = self.horizontal.x * self.vertical.y - self.horizontal.y * self.vertical.x;
        Point {
            x: (x * self.vertical.y - y * self.vertical.x) / determinant,
            y: (y * self.horizontal.x - x * self.horizontal.y) / determinant,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RenderRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub full_width: u32,
    pub full_height: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub text: String,
    pub rects: Vec<Rect>,
}

#[derive(Clone, Debug)]
pub struct OutlineEntry {
    pub title: String,
    pub depth: u8,
    pub page: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationKind {
    Highlight,
    Underline,
    Strikeout,
    Ink,
    Note,
    Comment,
    Line,
    Arrow,
    Rectangle,
    Circle,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub kind: AnnotationKind,
    pub color: [u8; 4],
    pub thickness: f32,
    pub rects: Vec<Rect>,
    pub points: Vec<Point>,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct AnnotationInfo {
    pub index: i32,
    pub bounds: Rect,
    pub annotation: Annotation,
}

#[repr(C)]
struct FileAccess {
    length: c_ulong,
    read: unsafe extern "C" fn(Raw, c_ulong, *mut u8, c_ulong) -> c_int,
    context: Raw,
}

#[repr(C)]
struct FileWriter {
    version: c_int,
    write: unsafe extern "C" fn(*mut FileWriter, *const c_void, c_ulong) -> c_int,
}

#[repr(C)]
struct SaveWriter {
    api: FileWriter,
    file: BufWriter<File>,
    error: Option<io::Error>,
}

#[repr(C)]
struct Pause {
    version: c_int,
    check: unsafe extern "C" fn(*mut Pause) -> c_int,
    context: Raw,
}

struct PauseState<T> {
    cancelled: bool,
    predicate: T,
}

#[repr(C)]
#[derive(Default)]
struct PageSize {
    width: f32,
    height: f32,
}

#[repr(C)]
#[derive(Default)]
struct QuadPoints {
    points: [Point; 4],
}

// Version 1 prefix of FPDF_FORMFILLINFO, supported by TinyPDF's non-XFA build.
#[repr(C)]
struct FormInfo {
    version: c_int,
    release: Option<unsafe extern "C" fn(*mut FormInfo)>,
    invalidate: unsafe extern "C" fn(*mut FormInfo, Raw, f64, f64, f64, f64),
    selected_rect: Option<unsafe extern "C" fn(*mut FormInfo, Raw, f64, f64, f64, f64)>,
    set_cursor: unsafe extern "C" fn(*mut FormInfo, c_int),
    set_timer:
        unsafe extern "C" fn(*mut FormInfo, c_int, Option<unsafe extern "C" fn(c_int)>) -> c_int,
    kill_timer: unsafe extern "C" fn(*mut FormInfo, c_int),
    local_time: unsafe extern "C" fn(*mut FormInfo) -> FormSystemTime,
    on_change: Option<unsafe extern "C" fn(*mut FormInfo)>,
    get_page: unsafe extern "C" fn(*mut FormInfo, Raw, c_int) -> Raw,
    current_page: Option<unsafe extern "C" fn(*mut FormInfo, Raw) -> Raw>,
    rotation: unsafe extern "C" fn(*mut FormInfo, Raw) -> c_int,
    named_action: unsafe extern "C" fn(*mut FormInfo, *const c_char),
    text_focus: Option<unsafe extern "C" fn(*mut FormInfo, *const u16, c_ulong, c_int)>,
    uri_action: Option<unsafe extern "C" fn(*mut FormInfo, *const c_char)>,
    goto_action: Option<unsafe extern "C" fn(*mut FormInfo, c_int, c_int, *mut f32, c_int)>,
    js_platform: Raw,
}

#[repr(C)]
struct FormSystemTime {
    fields: [u16; 8],
}

#[repr(C)]
struct FormContext {
    info: FormInfo,
    page: Cell<(c_int, Raw)>,
}

// Matches PDFium's public fpdfview, fpdf_progressive, fpdf_text, fpdf_doc,
// fpdf_annot and fpdf_save headers.
macro_rules! pdfium_bindings {
    ($($(#[$attribute:meta])* $name:ident: $signature:ty = $symbol:literal;)*) => {
        struct Bindings {
            $($(#[$attribute])* $name: $signature,)*
            _library: Library,
        }

        impl Bindings {
            fn load(path: &Path) -> Result<Self, PdfError> {
                // Load only the bundled library or an explicitly configured absolute path.
                #[cfg(not(windows))]
                let loaded = unsafe { Library::new(path) };
                #[cfg(windows)]
                let loaded = unsafe {
                    use libloading::os::windows::{
                        Library as WindowsLibrary, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
                        LOAD_LIBRARY_SEARCH_SYSTEM32,
                    };
                    WindowsLibrary::load_with_flags(path,
                        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32)
                        .map(Library::from)
                };
                let library = loaded
                    .map_err(|error| PdfError::Library(format!("{}: {error}", path.display())))?;
                Ok(Self {
                    $($(#[$attribute])* $name: *unsafe { library.get::<$signature>($symbol) }
                        .map_err(|error| PdfError::Library(format!("{}: {error}", path.display())))?,)*
                    _library: library,
                })
            }
        }
    };
}

pdfium_bindings! {
    init: unsafe extern "system" fn() = b"FPDF_InitLibrary\0";
    destroy: unsafe extern "system" fn() = b"FPDF_DestroyLibrary\0";
    open: unsafe extern "system" fn(*mut FileAccess, *const c_char) -> Raw = b"FPDF_LoadCustomDocument\0";
    close_document: Close = b"FPDF_CloseDocument\0";
    last_error: unsafe extern "system" fn() -> c_ulong = b"FPDF_GetLastError\0";
    page_count: unsafe extern "system" fn(Raw) -> c_int = b"FPDF_GetPageCount\0";
    page_size: unsafe extern "system" fn(Raw, c_int, *mut PageSize) -> c_int = b"FPDF_GetPageSizeByIndexF\0";
    load_page: unsafe extern "system" fn(Raw, c_int) -> Raw = b"FPDF_LoadPage\0";
    close_page: Close = b"FPDF_ClosePage\0";
    device_to_page: unsafe extern "system" fn(Raw, c_int, c_int, c_int, c_int, c_int, c_int, c_int, *mut f64, *mut f64) -> c_int = b"FPDF_DeviceToPage\0";
    bitmap: unsafe extern "system" fn(c_int, c_int, c_int, Raw, c_int) -> Raw = b"FPDFBitmap_CreateEx\0";
    destroy_bitmap: Close = b"FPDFBitmap_Destroy\0";
    render_start: unsafe extern "system" fn(Raw, Raw, c_int, c_int, c_int, c_int, c_int, c_int, *mut Pause) -> c_int = b"FPDF_RenderPageBitmap_Start\0";
    render_continue: unsafe extern "system" fn(Raw, *mut Pause) -> c_int = b"FPDF_RenderPage_Continue\0";
    close_render: Close = b"FPDF_RenderPage_Close\0";
    form_type: unsafe extern "system" fn(Raw) -> c_int = b"FPDF_GetFormType\0";
    init_forms: unsafe extern "system" fn(Raw, *mut FormInfo) -> Raw = b"FPDFDOC_InitFormFillEnvironment\0";
    close_forms: Close = b"FPDFDOC_ExitFormFillEnvironment\0";
    form_after_load: unsafe extern "system" fn(Raw, Raw) = b"FORM_OnAfterLoadPage\0";
    form_before_close: unsafe extern "system" fn(Raw, Raw) = b"FORM_OnBeforeClosePage\0";
    draw_forms: unsafe extern "system" fn(Raw, Raw, Raw, c_int, c_int, c_int, c_int, c_int, c_int) = b"FPDF_FFLDraw\0";
    count_chars: unsafe extern "system" fn(Raw) -> c_int = b"FPDFText_CountChars\0";
    load_text: unsafe extern "system" fn(Raw) -> Raw = b"FPDFText_LoadPage\0";
    close_text: Close = b"FPDFText_ClosePage\0";
    char_index_at: unsafe extern "system" fn(Raw, f64, f64, f64, f64) -> c_int = b"FPDFText_GetCharIndexAtPos\0";
    get_text: unsafe extern "system" fn(Raw, c_int, c_int, *mut u16) -> c_int = b"FPDFText_GetText\0";
    count_rects: unsafe extern "system" fn(Raw, c_int, c_int) -> c_int = b"FPDFText_CountRects\0";
    text_rect: unsafe extern "system" fn(Raw, c_int, *mut f64, *mut f64, *mut f64, *mut f64) -> c_int = b"FPDFText_GetRect\0";
    find_start: unsafe extern "system" fn(Raw, *const u16, c_ulong, c_int) -> Raw = b"FPDFText_FindStart\0";
    find_next: unsafe extern "system" fn(Raw) -> c_int = b"FPDFText_FindNext\0";
    find_index: unsafe extern "system" fn(Raw) -> c_int = b"FPDFText_GetSchResultIndex\0";
    find_count: unsafe extern "system" fn(Raw) -> c_int = b"FPDFText_GetSchCount\0";
    find_close: Close = b"FPDFText_FindClose\0";
    bookmark_child: unsafe extern "system" fn(Raw, Raw) -> Raw = b"FPDFBookmark_GetFirstChild\0";
    bookmark_next: unsafe extern "system" fn(Raw, Raw) -> Raw = b"FPDFBookmark_GetNextSibling\0";
    bookmark_title: unsafe extern "system" fn(Raw, Raw, c_ulong) -> c_ulong = b"FPDFBookmark_GetTitle\0";
    bookmark_dest: unsafe extern "system" fn(Raw, Raw) -> Raw = b"FPDFBookmark_GetDest\0";
    bookmark_action: unsafe extern "system" fn(Raw) -> Raw = b"FPDFBookmark_GetAction\0";
    action_type: unsafe extern "system" fn(Raw) -> c_ulong = b"FPDFAction_GetType\0";
    action_dest: unsafe extern "system" fn(Raw, Raw) -> Raw = b"FPDFAction_GetDest\0";
    dest_index: unsafe extern "system" fn(Raw, Raw) -> c_int = b"FPDFDest_GetDestPageIndex\0";
    metadata: unsafe extern "system" fn(Raw, *const c_char, Raw, c_ulong) -> c_ulong = b"FPDF_GetMetaText\0";
    permissions: unsafe extern "system" fn(Raw) -> c_ulong = b"FPDF_GetDocPermissions\0";
    save: unsafe extern "system" fn(Raw, *mut FileWriter, c_ulong) -> c_int = b"FPDF_SaveAsCopy\0";
    create_annotation: unsafe extern "system" fn(Raw, c_int) -> Raw = b"FPDFPage_CreateAnnot\0";
    count_annotations: unsafe extern "system" fn(Raw) -> c_int = b"FPDFPage_GetAnnotCount\0";
    get_annotation: unsafe extern "system" fn(Raw, c_int) -> Raw = b"FPDFPage_GetAnnot\0";
    close_annotation: Close = b"FPDFPage_CloseAnnot\0";
    remove_annotation: unsafe extern "system" fn(Raw, c_int) -> c_int = b"FPDFPage_RemoveAnnot\0";
    annotation_subtype: unsafe extern "system" fn(Raw) -> c_int = b"FPDFAnnot_GetSubtype\0";
    set_annotation_rect: unsafe extern "system" fn(Raw, *const Rect) -> c_int = b"FPDFAnnot_SetRect\0";
    get_annotation_rect: unsafe extern "system" fn(Raw, *mut Rect) -> c_int = b"FPDFAnnot_GetRect\0";
    set_annotation_color: unsafe extern "system" fn(Raw, c_int, c_uint, c_uint, c_uint, c_uint) -> c_int = b"FPDFAnnot_SetColor\0";
    set_annotation_border: unsafe extern "system" fn(Raw, f32, f32, f32) -> c_int = b"FPDFAnnot_SetBorder\0";
    set_annotation_flags: unsafe extern "system" fn(Raw, c_int) -> c_int = b"FPDFAnnot_SetFlags\0";
    set_annotation_string: unsafe extern "system" fn(Raw, *const c_char, *const u16) -> c_int = b"FPDFAnnot_SetStringValue\0";
    get_annotation_string: unsafe extern "system" fn(Raw, *const c_char, *mut u16, c_ulong) -> c_ulong = b"FPDFAnnot_GetStringValue\0";
    set_appearance: unsafe extern "system" fn(Raw, c_int, *const u16) -> c_int = b"FPDFAnnot_SetAP\0";
    append_quad: unsafe extern "system" fn(Raw, *const QuadPoints) -> c_int = b"FPDFAnnot_AppendAttachmentPoints\0";
    count_quads: unsafe extern "system" fn(Raw) -> usize = b"FPDFAnnot_CountAttachmentPoints\0";
    get_quad: unsafe extern "system" fn(Raw, usize, *mut QuadPoints) -> c_int = b"FPDFAnnot_GetAttachmentPoints\0";
    add_stroke: unsafe extern "system" fn(Raw, *const Point, usize) -> c_int = b"FPDFAnnot_AddInkStroke\0";
    get_stroke: unsafe extern "system" fn(Raw, c_ulong, *mut Point, c_ulong) -> c_ulong = b"FPDFAnnot_GetInkListPath\0";
    #[cfg(windows)]
    print: unsafe extern "system" fn(Raw, Raw, c_int, c_int, c_int, c_int, c_int, c_int) -> c_int = b"FPDF_RenderPage\0";
}

struct Handle {
    raw: Raw,
    close: Close,
}

impl Handle {
    fn new(raw: Raw, close: Close, error: &'static str) -> Result<Self, PdfError> {
        if raw.is_null() {
            return Err(PdfError::Pdf(error));
        }
        Ok(Self { raw, close })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Each successful open is paired with its matching PDFium close function.
        unsafe { (self.close)(self.raw) }
    }
}

pub struct Pdfium {
    api: Bindings,
    _worker_thread: PhantomData<Rc<()>>,
}

impl Pdfium {
    pub fn load() -> Result<Self, PdfError> {
        let name = if cfg!(windows) {
            "pdfium.dll"
        } else if cfg!(target_os = "macos") {
            "libpdfium.dylib"
        } else {
            "libpdfium.so"
        };
        let mut candidates = Vec::new();
        if let Some(path) = env::var_os("PDFIUM_LIBRARY_PATH") {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(PdfError::Library(
                    "PDFIUM_LIBRARY_PATH must be an absolute path.".into(),
                ));
            }
            candidates.push(if path.is_dir() { path.join(name) } else { path });
        } else if let Ok(executable) = env::current_exe()
            && let Some(directory) = executable.parent()
        {
            candidates.extend([
                directory.join(name),
                directory.join("lib").join(name),
                directory.join("../Frameworks").join(name),
                directory.join("../lib/tinypdf").join(name),
            ]);
        }
        let path = candidates.into_iter().find(|candidate| candidate.is_file())
            .ok_or_else(|| PdfError::Library(format!(
                "Could not find {name}. Extract the full TinyPDF archive or rebuild TinyPDF, then reopen the app. If you set PDFIUM_LIBRARY_PATH, check that it points to the PDFium library for your system."
            )))?;
        let api = Bindings::load(&path)?;
        ENGINE_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                PdfError::Library("PDFium is already running on another thread.".into())
            })?;
        // PDFium's process-wide state is exclusively owned by this non-Send value.
        unsafe { (api.init)() };
        Ok(Self {
            api,
            _worker_thread: PhantomData,
        })
    }

    pub fn open<'a>(
        &'a self,
        path: &Path,
        password: Option<&str>,
    ) -> Result<Document<'a>, PdfError> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(PdfError::Invalid("Select a regular PDF file."));
        }
        #[cfg(unix)]
        let stamp = FileStamp::read(&metadata)?;
        #[cfg(windows)]
        let stamp = FileStamp::read(&file, &metadata)?;
        let length = native_file_length(stamp.length)?;
        let requested = std::path::absolute(path)?;
        let target = requested.canonicalize()?;
        let password = password
            .map(CString::new)
            .transpose()
            .map_err(|_| PdfError::Invalid("PDF passwords cannot contain a null character."))?;
        let source = Rc::new(FileSource {
            file,
            length,
            stamp,
            bytes_read: Cell::new(0),
        });
        let native = NativeDocument::open(&self.api, &source, password.as_deref())?;
        let pages = unsafe { (self.api.page_count)(native.handle.raw) };
        if pages <= 0 {
            return Err(PdfError::Pdf("This PDF has no readable pages."));
        }
        if pages > 1_000_000 {
            return Err(PdfError::Invalid(
                "This PDF exceeds the supported page count.",
            ));
        }
        let permissions = unsafe { (self.api.permissions)(native.handle.raw) };
        Ok(Document {
            native: RefCell::new(native),
            api: &self.api,
            pages: pages as u32,
            permissions,
            can_reopen: Cell::new(password.is_none() && metadata.len() > NATIVE_STREAM_BUDGET),
            annotated: Cell::new(false),
            destination: RefCell::new(SaveDestination {
                requested,
                target,
                stamp,
            }),
            source,
        })
    }
}

impl Drop for Pdfium {
    fn drop(&mut self) {
        // Borrowing the engine ensures all open documents have closed before global teardown.
        unsafe { (self.api.destroy)() };
        ENGINE_ACTIVE.store(false, Ordering::Release);
    }
}

unsafe extern "C" fn read_file(
    context: Raw,
    offset: c_ulong,
    output: *mut u8,
    size: c_ulong,
) -> c_int {
    let size = size as usize;
    if size == 0 {
        return 1;
    }
    if output.is_null() || context.is_null() || size > isize::MAX as usize {
        return 0;
    }
    // PDFium supplies a writable size-byte buffer and calls this only during file access.
    let source = unsafe { &*context.cast::<FileSource>() };
    let file = &source.file;
    let bytes = unsafe { std::slice::from_raw_parts_mut(output, size) };
    #[allow(clippy::unnecessary_cast)] // Windows unsigned long is 32-bit.
    let offset = offset as u64;
    #[cfg(unix)]
    let read = std::os::unix::fs::FileExt::read_exact_at(file, bytes, offset).is_ok();
    #[cfg(windows)]
    let read = {
        use std::os::windows::fs::FileExt;
        let mut filled = 0;
        while filled < bytes.len() {
            match file.seek_read(&mut bytes[filled..], offset + filled as u64) {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        filled == bytes.len()
    };
    if read {
        source
            .bytes_read
            .set(source.bytes_read.get().saturating_add(size as u64));
    }
    c_int::from(read)
}

unsafe extern "C" fn write_file(
    writer: *mut FileWriter,
    input: *const c_void,
    size: c_ulong,
) -> c_int {
    let size = size as usize;
    if size == 0 {
        return 1;
    }
    if writer.is_null() || input.is_null() || size > isize::MAX as usize {
        return 0;
    }
    // FileWriter is the repr(C) prefix of the SaveWriter passed to PDFium.
    let writer = unsafe { &mut *writer.cast::<SaveWriter>() };
    let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), size) };
    if let Err(error) = writer.file.write_all(bytes) {
        writer.error.get_or_insert(error);
        return 0;
    }
    1
}

unsafe extern "C" fn check_pause<T: Fn() -> bool>(pause: *mut Pause) -> c_int {
    // PDFium calls the hook synchronously with the Pause and state owned by the render.
    let state = unsafe { &mut *(*pause).context.cast::<PauseState<T>>() };
    state.cancelled |=
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(&state.predicate)).unwrap_or(true);
    c_int::from(state.cancelled)
}

struct FormEnvironment {
    handle: Handle,
    context: Rc<FormContext>,
}

impl FormEnvironment {
    fn new(api: &Bindings, document: Raw) -> Result<Self, PdfError> {
        let context = Rc::new(FormContext {
            info: FormInfo {
                version: 1,
                release: None,
                invalidate: form_invalidate,
                selected_rect: None,
                set_cursor: form_cursor,
                set_timer: form_timer,
                kill_timer: form_cursor,
                local_time: form_time,
                on_change: None,
                get_page: form_page,
                current_page: None,
                rotation: form_rotation,
                named_action: form_named_action,
                text_focus: None,
                uri_action: None,
                goto_action: None,
                js_platform: ptr::null_mut(),
            },
            page: Cell::new((-1, ptr::null_mut())),
        });
        // The non-XFA viewer supplies no actions, JavaScript runtime or timers.
        // The form engine uses the callbacks until the Handle closes.
        let raw = unsafe { (api.init_forms)(document, Rc::as_ptr(&context).cast_mut().cast()) };
        let handle = Handle::new(
            raw,
            api.close_forms,
            "PDFium could not load this PDF's form fields.",
        )?;
        Ok(Self { handle, context })
    }
}

unsafe extern "C" fn form_invalidate(_: *mut FormInfo, _: Raw, _: f64, _: f64, _: f64, _: f64) {}
unsafe extern "C" fn form_cursor(_: *mut FormInfo, _: c_int) {}
unsafe extern "C" fn form_timer(
    _: *mut FormInfo,
    _: c_int,
    _: Option<unsafe extern "C" fn(c_int)>,
) -> c_int {
    0
}
unsafe extern "C" fn form_time(_: *mut FormInfo) -> FormSystemTime {
    FormSystemTime { fields: [0; 8] }
}
unsafe extern "C" fn form_rotation(_: *mut FormInfo, _: Raw) -> c_int {
    0
}
unsafe extern "C" fn form_named_action(_: *mut FormInfo, _: *const c_char) {}

unsafe extern "C" fn form_page(info: *mut FormInfo, _: Raw, index: c_int) -> Raw {
    // FormInfo is the repr(C) prefix of FormContext, held by FormEnvironment.
    let (loaded_index, page) = unsafe { (&*info.cast::<FormContext>()).page.get() };
    if index == loaded_index {
        page
    } else {
        ptr::null_mut()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: SystemTime,
    identity: (u64, u64),
}

impl FileStamp {
    #[cfg(unix)]
    fn read(metadata: &fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            identity: (metadata.dev(), metadata.ino()),
        })
    }

    #[cfg(windows)]
    fn read(file: &File, metadata: &fs::Metadata) -> io::Result<Self> {
        use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // The live Windows file handle permits attribute queries. The API initializes
        // the result on success.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        let information = unsafe { information.assume_init() };
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            identity: (
                u64::from(information.dwVolumeSerialNumber),
                (u64::from(information.nFileIndexHigh) << 32)
                    | u64::from(information.nFileIndexLow),
            ),
        })
    }

    fn from_file(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        #[cfg(unix)]
        return Self::read(&metadata);
        #[cfg(windows)]
        Self::read(file, &metadata)
    }

    fn at(path: &Path) -> io::Result<Option<Self>> {
        #[cfg(unix)]
        let current = path.metadata().and_then(|metadata| Self::read(&metadata));
        #[cfg(windows)]
        let current = {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;

            OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .open(path)
                .and_then(|file| Self::from_file(&file))
        };
        match current {
            Ok(stamp) => Ok(Some(stamp)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

struct SaveDestination {
    requested: PathBuf,
    target: PathBuf,
    stamp: FileStamp,
}

fn resolve_destination(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A dangling symlink has an ambiguous destination and may have been
            // changed to redirect this save.
            if path.symlink_metadata().is_ok() {
                return Err(error);
            }
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("Missing output directory"))?;
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::other("Missing output filename"))?;
            Ok(parent.canonicalize()?.join(name))
        }
        Err(error) => Err(error),
    }
}
struct FileSource {
    file: File,
    length: c_ulong,
    stamp: FileStamp,
    bytes_read: Cell<u64>,
}

fn native_file_length(length: u64) -> Result<c_ulong, PdfError> {
    #[cfg(windows)]
    {
        c_ulong::try_from(length).map_err(|_| {
            PdfError::Invalid("The PDF exceeds the file size supported by this build.")
        })
    }
    #[cfg(not(windows))]
    Ok(length)
}

struct NativeDocument {
    forms: Option<FormEnvironment>,
    handle: Handle,
}

impl NativeDocument {
    fn open(
        api: &Bindings,
        source: &Rc<FileSource>,
        password: Option<&std::ffi::CStr>,
    ) -> Result<Self, PdfError> {
        let mut access = FileAccess {
            length: source.length,
            read: read_file,
            context: Rc::as_ptr(source).cast_mut().cast(),
        };
        let password = password.map_or(ptr::null(), |password| password.as_ptr());
        // PDFium copies the access descriptor. The Rc keeps its file context alive.
        let raw = unsafe { (api.open)(&mut access, password) };
        if raw.is_null() {
            return Err(match unsafe { (api.last_error)() } {
                2 => PdfError::Pdf("PDFium could not read the PDF file."),
                3 => PdfError::Pdf("This file is not a readable PDF."),
                4 => PdfError::PasswordRequired,
                5 => PdfError::Pdf("This PDF uses an unsupported encryption method."),
                _ => PdfError::Pdf("PDFium could not open this document."),
            });
        }
        let handle = Handle {
            raw,
            close: api.close_document,
        };
        let form_type = unsafe { (api.form_type)(raw) };
        let forms = if form_type == 1 || form_type == 3 {
            Some(FormEnvironment::new(api, raw)?)
        } else {
            None
        };
        Ok(Self { forms, handle })
    }
}

struct LoadedPage<'a> {
    handle: Handle,
    document: Ref<'a, NativeDocument>,
}

impl Deref for LoadedPage<'_> {
    type Target = Handle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

pub struct Document<'a> {
    native: RefCell<NativeDocument>,
    api: &'a Bindings,
    pages: u32,
    permissions: c_ulong,
    can_reopen: Cell<bool>,
    annotated: Cell<bool>,
    destination: RefCell<SaveDestination>,
    source: Rc<FileSource>,
}

impl Document<'_> {
    pub fn page_count(&self) -> u32 {
        self.pages
    }

    fn page_index(&self, page: u32) -> Result<c_int, PdfError> {
        if page >= self.pages {
            return Err(PdfError::Invalid("Page number is outside this PDF."));
        }
        Ok(page as c_int)
    }

    fn load_page(&self, page: u32) -> Result<LoadedPage<'_>, PdfError> {
        let index = self.page_index(page)?;
        self.recycle_stream_cache(NATIVE_STREAM_BUDGET);
        let document = self.native.borrow();
        let raw = unsafe { (self.api.load_page)(document.handle.raw, index) };
        let handle = Handle::new(raw, self.api.close_page, "PDFium could not load this page.")?;
        Ok(LoadedPage { handle, document })
    }

    pub fn recycle_stream_cache(&self, budget: u64) {
        if !self.can_reopen.get() || self.annotated.get() || self.source.bytes_read.get() < budget {
            return;
        }
        // PDFium retains stream copies for the document lifetime. Reopen only a
        // pristine document backed by the same file, at a page boundary.
        let Ok(mut native) = self.native.try_borrow_mut() else {
            return;
        };
        let still_original =
            FileStamp::from_file(&self.source.file).is_ok_and(|stamp| stamp == self.source.stamp);
        if still_original {
            self.source.bytes_read.set(0);
            if let Ok(reopened) = NativeDocument::open(self.api, &self.source, None) {
                *native = reopened;
                return;
            }
        }
        self.can_reopen.set(false);
    }

    pub fn page_size(&self, page: u32) -> Result<(f32, f32), PdfError> {
        let index = self.page_index(page)?;
        let mut size = PageSize::default();
        let success =
            unsafe { (self.api.page_size)(self.native.borrow().handle.raw, index, &mut size) };
        if success == 0
            || !size.width.is_finite()
            || !size.height.is_finite()
            || size.width <= 0.0
            || size.height <= 0.0
        {
            return Err(PdfError::Pdf("This PDF page has invalid dimensions."));
        }
        Ok((size.width, size.height))
    }

    pub fn page_coordinates(&self, page: u32, rotation: u8) -> Result<PageCoordinates, PdfError> {
        let page = self.load_page(page)?;
        self.loaded_page_coordinates(&page, rotation)
    }

    fn loaded_page_coordinates(
        &self,
        page: &Handle,
        rotation: u8,
    ) -> Result<PageCoordinates, PdfError> {
        let rotation = c_int::from(rotation % 4);
        let map = |x, y| {
            let (mut pdf_x, mut pdf_y) = (0.0, 0.0);
            let result = unsafe {
                (self.api.device_to_page)(
                    page.raw, 0, 0, 1, 1, rotation, x, y, &mut pdf_x, &mut pdf_y,
                )
            };
            if result == 0 || !pdf_x.is_finite() || !pdf_y.is_finite() {
                return Err(PdfError::Pdf("This page has invalid coordinates."));
            }
            Ok(Point {
                x: pdf_x as f32,
                y: pdf_y as f32,
            })
        };
        let origin = map(0, 0)?;
        let right = map(1, 0)?;
        let bottom = map(0, 1)?;
        let horizontal = Point {
            x: right.x - origin.x,
            y: right.y - origin.y,
        };
        let vertical = Point {
            x: bottom.x - origin.x,
            y: bottom.y - origin.y,
        };
        let determinant = horizontal.x * vertical.y - horizontal.y * vertical.x;
        if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
            return Err(PdfError::Pdf("This page has invalid coordinates."));
        }
        Ok(PageCoordinates {
            origin,
            horizontal,
            vertical,
        })
    }

    pub fn render_rgba(
        &self,
        page: u32,
        width: u32,
        height: u32,
        rotation: u8,
        output: &mut [u8],
        cancel: impl Fn() -> bool,
    ) -> Result<(), PdfError> {
        self.render_region_rgba(
            page,
            RenderRegion {
                x: 0,
                y: 0,
                width,
                height,
                full_width: width,
                full_height: height,
            },
            rotation,
            output,
            false,
            cancel,
        )
        .map(|_| ())
    }

    pub fn render_region_rgba<T: Fn() -> bool>(
        &self,
        page: u32,
        region: RenderRegion,
        rotation: u8,
        output: &mut [u8],
        include_coordinates: bool,
        cancel: T,
    ) -> Result<Option<PageCoordinates>, PdfError> {
        let RenderRegion {
            x,
            y,
            width,
            height,
            full_width,
            full_height,
        } = region;
        let stride = width.checked_mul(4).filter(|n| *n <= c_int::MAX as u32);
        let size = stride.and_then(|stride| (stride as usize).checked_mul(height as usize));
        let valid_x = x
            .checked_add(width)
            .is_some_and(|right| right <= full_width);
        let valid_y = y
            .checked_add(height)
            .is_some_and(|bottom| bottom <= full_height);
        if width == 0
            || height == 0
            || !valid_x
            || !valid_y
            || full_width > c_int::MAX as u32
            || full_height > c_int::MAX as u32
            || !size.is_some_and(|size| size == output.len() && size <= MAX_BITMAP_BYTES)
        {
            return Err(PdfError::Invalid(
                "Invalid PDF bitmap dimensions or buffer size.",
            ));
        }
        if cancel() {
            return Err(PdfError::Cancelled);
        }
        let page_index = self.page_index(page)?;
        let page = self.load_page(page)?;
        output.fill(255);
        // FPDFBitmap_BGRA with REVERSE_BYTE_ORDER writes RGBA into the caller's buffer.
        let bitmap = unsafe {
            (self.api.bitmap)(
                width as c_int,
                height as c_int,
                BITMAP_BGRA,
                output.as_mut_ptr().cast(),
                stride.unwrap() as c_int,
            )
        };
        let bitmap = Handle::new(
            bitmap,
            self.api.destroy_bitmap,
            "PDFium could not allocate the bitmap.",
        )?;
        let mut state = PauseState {
            cancelled: false,
            predicate: cancel,
        };
        let mut pause = Pause {
            version: 1,
            check: check_pause::<T>,
            context: (&raw mut state).cast(),
        };
        let result = unsafe {
            let flags = ANNOT | REVERSE_BYTE_ORDER | LIMITED_IMAGE_CACHE;
            let mut result = (self.api.render_start)(
                bitmap.raw,
                page.raw,
                -(x as c_int),
                -(y as c_int),
                full_width as c_int,
                full_height as c_int,
                c_int::from(rotation % 4),
                flags,
                &mut pause,
            );
            let _render = Handle {
                raw: page.raw,
                close: self.api.close_render,
            };
            while result == RENDER_PAUSED && !state.cancelled && !(state.predicate)() {
                result = (self.api.render_continue)(page.raw, &mut pause);
            }
            result
        };
        if state.cancelled || (state.predicate)() {
            return Err(PdfError::Cancelled);
        }
        if result != RENDER_DONE {
            return Err(PdfError::Pdf("PDFium could not render this page."));
        }
        if let Some(forms) = &page.document.forms {
            forms.context.page.set((page_index, page.raw));
            unsafe {
                let form = forms.handle.raw;
                (self.api.form_after_load)(page.raw, form);
                // The progressive page pass already painted non-widget annotations.
                (self.api.draw_forms)(
                    form,
                    bitmap.raw,
                    page.raw,
                    -(x as c_int),
                    -(y as c_int),
                    full_width as c_int,
                    full_height as c_int,
                    c_int::from(rotation % 4),
                    REVERSE_BYTE_ORDER,
                );
                (self.api.form_before_close)(page.raw, form);
            }
            forms.context.page.set((-1, ptr::null_mut()));
        }
        Ok(if include_coordinates {
            self.loaded_page_coordinates(&page, rotation).ok()
        } else {
            None
        })
    }

    fn text_page(&self, page: &Handle) -> Result<Handle, PdfError> {
        let raw = unsafe { (self.api.load_text)(page.raw) };
        Handle::new(
            raw,
            self.api.close_text,
            "PDFium could not read the text on this page.",
        )
    }

    fn text_rects(&self, text_page: &Handle, start: c_int, count: c_int) -> Vec<Rect> {
        let rects = unsafe { (self.api.count_rects)(text_page.raw, start, count) };
        (0..rects)
            .filter_map(|index| {
                let (mut left, mut top, mut right, mut bottom) = (0.0, 0.0, 0.0, 0.0);
                let success = unsafe {
                    (self.api.text_rect)(
                        text_page.raw,
                        index,
                        &mut left,
                        &mut top,
                        &mut right,
                        &mut bottom,
                    )
                };
                let rect = Rect {
                    left: left as f32,
                    top: top as f32,
                    right: right as f32,
                    bottom: bottom as f32,
                };
                (success != 0 && rect.is_valid()).then_some(rect)
            })
            .collect()
    }

    pub fn search_page(&self, page: u32, query: &str) -> Result<Vec<Vec<Rect>>, PdfError> {
        self.search_page_up_to(page, query, usize::MAX)
    }

    pub fn search_page_up_to(
        &self,
        page: u32,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Vec<Rect>>, PdfError> {
        if limit == 0 || query.is_empty() {
            return Ok(Vec::new());
        }
        if query.contains('\0') || query.len() > 16_384 {
            return Err(PdfError::Invalid("The search text is invalid or too long."));
        }
        let query = wide(query);
        let page = self.load_page(page)?;
        let text = self.text_page(&page)?;
        let search = unsafe { (self.api.find_start)(text.raw, query.as_ptr(), 0, 0) };
        let search = Handle::new(
            search,
            self.api.find_close,
            "PDFium could not search this page.",
        )?;
        let mut matches = Vec::new();
        while matches.len() < limit && unsafe { (self.api.find_next)(search.raw) } != 0 {
            let index = unsafe { (self.api.find_index)(search.raw) };
            let count = unsafe { (self.api.find_count)(search.raw) };
            let rects = self.text_rects(&text, index, count);
            if !rects.is_empty() {
                matches.push(rects);
            }
        }
        Ok(matches)
    }

    pub fn page_text(&self, page: u32) -> Result<String, PdfError> {
        if !self.allows_copy() {
            return Err(PdfError::Invalid("This PDF restricts text extraction."));
        }
        let page = self.load_page(page)?;
        let text = self.text_page(&page)?;
        let count = unsafe { (self.api.count_chars)(text.raw) };
        if count < 0 || count > 1024 * 1024 {
            return Err(PdfError::Invalid(
                "This page exceeds the text extraction limit.",
            ));
        }
        Ok(self.read_text(&text, 0, count))
    }

    // PDFium reserves up to two UTF-16 units per character plus a terminator.
    fn read_text(&self, text: &Handle, first: c_int, count: c_int) -> String {
        let mut buffer = vec![0_u16; count as usize * 2 + 1];
        let length = unsafe { (self.api.get_text)(text.raw, first, count, buffer.as_mut_ptr()) };
        buffer.truncate(length.saturating_sub(1).max(0) as usize);
        String::from_utf16_lossy(&buffer)
    }

    pub fn allows_copy(&self) -> bool {
        self.permissions & PERMISSION_COPY != 0
    }

    pub fn select(&self, page: u32, start: Point, end: Point) -> Result<Selection, PdfError> {
        if !valid_point(start) || !valid_point(end) {
            return Err(PdfError::Invalid(
                "This text selection is outside valid PDF coordinates.",
            ));
        }
        let page = self.load_page(page)?;
        let text = self.text_page(&page)?;
        let index = |point: Point| unsafe {
            (self.api.char_index_at)(text.raw, f64::from(point.x), f64::from(point.y), 8.0, 8.0)
        };
        let (start, end) = (index(start), index(end));
        if start < 0 || end < 0 {
            return Ok(Selection::default());
        }
        let first = start.min(end);
        let count = (start.max(end) - first).saturating_add(1);
        if count > 1024 * 1024 {
            return Err(PdfError::Invalid(
                "This text selection is too large to copy.",
            ));
        }
        let string = if self.permissions & PERMISSION_COPY != 0 {
            self.read_text(&text, first, count)
        } else {
            String::new()
        };
        Ok(Selection {
            text: string,
            rects: self.text_rects(&text, first, count),
        })
    }

    pub fn outline(&self) -> Vec<OutlineEntry> {
        let mut levels = vec![(
            unsafe { (self.api.bookmark_child)(self.native.borrow().handle.raw, ptr::null_mut()) },
            0_u8,
        )];
        let mut visited = HashSet::new();
        let mut entries = Vec::new();
        while let Some((bookmark, depth)) = levels.pop() {
            if bookmark.is_null() || !visited.insert(bookmark) {
                continue;
            }
            let title = wide_string(|buffer, length| unsafe {
                (self.api.bookmark_title)(bookmark, buffer.cast(), length)
            });
            let mut target =
                unsafe { (self.api.bookmark_dest)(self.native.borrow().handle.raw, bookmark) };
            if target.is_null() {
                let action = unsafe { (self.api.bookmark_action)(bookmark) };
                if !action.is_null() && unsafe { (self.api.action_type)(action) } == 1 {
                    target =
                        unsafe { (self.api.action_dest)(self.native.borrow().handle.raw, action) };
                }
            }
            let index = if target.is_null() {
                -1
            } else {
                unsafe { (self.api.dest_index)(self.native.borrow().handle.raw, target) }
            };
            let page = u32::try_from(index).ok().filter(|page| *page < self.pages);
            entries.push(OutlineEntry { title, depth, page });
            levels.push((
                unsafe { (self.api.bookmark_next)(self.native.borrow().handle.raw, bookmark) },
                depth,
            ));
            if depth < u8::MAX {
                levels.push((
                    unsafe { (self.api.bookmark_child)(self.native.borrow().handle.raw, bookmark) },
                    depth + 1,
                ));
            }
        }
        entries
    }

    pub fn metadata(&self) -> Vec<(String, String)> {
        [
            c"Title",
            c"Author",
            c"Subject",
            c"Keywords",
            c"Creator",
            c"Producer",
            c"CreationDate",
            c"ModDate",
        ]
        .into_iter()
        .filter_map(|tag| {
            let value = wide_string(|buffer, length| unsafe {
                (self.api.metadata)(
                    self.native.borrow().handle.raw,
                    tag.as_ptr(),
                    buffer.cast(),
                    length,
                )
            });
            (!value.is_empty()).then(|| (tag.to_string_lossy().into_owned(), value))
        })
        .collect()
    }

    pub fn save_as(&self, path: &Path) -> Result<(), PdfError> {
        let requested = std::path::absolute(path)?;
        let path = resolve_destination(&requested)?;
        let expected = FileStamp::at(&path)?;
        {
            let last = self.destination.borrow();
            if (last.requested == requested || last.target == path)
                && (last.target != path || expected != Some(last.stamp))
            {
                return Err(PdfError::Pdf(
                    "The output PDF changed outside TinyPDF. Choose a different Save As path.",
                ));
            }
        }
        self.verify_source()?;
        let parent = path
            .parent()
            .ok_or(PdfError::Invalid("Select a valid output path."))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let (temporary, file) = loop {
            let sequence = SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary = parent.join(format!(".tinypdf-{}-{sequence}.tmp", std::process::id()));
            match options.open(&temporary) {
                Ok(file) => break (temporary, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let result = (|| {
            let mut writer = SaveWriter {
                api: FileWriter {
                    version: 1,
                    write: write_file,
                },
                file: BufWriter::new(file),
                error: None,
            };
            // The synchronous writer streams a complete copy without discarding encryption.
            let saved = unsafe {
                (self.api.save)(
                    self.native.borrow().handle.raw,
                    (&raw mut writer).cast(),
                    SAVE_FULL_COPY,
                )
            };
            if let Some(error) = writer.error.take() {
                return Err(error.into());
            }
            if saved == 0 {
                return Err(PdfError::Pdf("PDFium could not save this document."));
            }
            writer.file.flush()?;
            if let Ok(metadata) = path.metadata() {
                writer
                    .file
                    .get_ref()
                    .set_permissions(metadata.permissions())?;
            }
            writer.file.get_ref().sync_all()?;
            drop(writer);
            let stamp = FileStamp::at(&temporary)?
                .ok_or(PdfError::Pdf("The temporary PDF is no longer available."))?;
            self.verify_source()?;
            if resolve_destination(&requested)? != path || FileStamp::at(&path)? != expected {
                return Err(PdfError::Pdf(
                    "The output PDF changed while TinyPDF was saving. Choose a different Save As path.",
                ));
            }
            fs::rename(&temporary, &path)?;
            *self.destination.borrow_mut() = SaveDestination {
                requested,
                target: path,
                stamp,
            };
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn verify_source(&self) -> Result<(), PdfError> {
        if FileStamp::from_file(&self.source.file)? != self.source.stamp {
            return Err(PdfError::Pdf(
                "The PDF source was modified while open. Reopen it before saving changes.",
            ));
        }
        Ok(())
    }

    #[cfg(windows)]
    pub fn has_form_fields(&self) -> bool {
        self.native.borrow().forms.is_some()
    }

    pub fn can_print(&self) -> bool {
        self.permissions & PERMISSION_PRINT != 0
    }

    pub fn can_print_high_quality(&self) -> bool {
        self.can_print() && self.permissions & PERMISSION_PRINT_HIGH_QUALITY != 0
    }

    /// # Safety
    /// The caller must supply a valid Windows printer device context during an active print page.
    #[cfg(windows)]
    pub unsafe fn print_page(
        &self,
        page: u32,
        dc: Raw,
        area: [i32; 4],
        rotation: u8,
    ) -> Result<(), PdfError> {
        if !self.can_print() {
            return Err(PdfError::Pdf("Printing is restricted by this PDF."));
        }
        let [x, y, width, height] = area;
        if dc.is_null() || width <= 0 || height <= 0 {
            return Err(PdfError::Invalid(
                "The printer returned an invalid page area.",
            ));
        }
        let page = self.load_page(page)?;
        let success = unsafe {
            (self.api.print)(
                dc,
                page.raw,
                x,
                y,
                width,
                height,
                c_int::from(rotation % 4),
                ANNOT | PRINTING,
            )
        };
        if success == 0 {
            return Err(PdfError::Pdf(
                "PDFium could not send this page to the printer.",
            ));
        }
        Ok(())
    }
}

fn wide(input: &str) -> Vec<u16> {
    input.encode_utf16().chain([0]).collect()
}

fn wide_string(read: impl Fn(*mut u16, c_ulong) -> c_ulong) -> String {
    let bytes = read(ptr::null_mut(), 0);
    if !(2..=MAX_WIDE_BYTES).contains(&bytes) || !bytes.is_multiple_of(2) {
        return String::new();
    }
    let mut buffer = vec![0u16; (bytes / 2) as usize];
    let read = read(buffer.as_mut_ptr(), bytes);
    if read != bytes {
        return String::new();
    }
    buffer.pop();
    String::from_utf16_lossy(&buffer)
}

impl AnnotationKind {
    const ALL: [Self; 10] = [
        Self::Highlight,
        Self::Underline,
        Self::Strikeout,
        Self::Ink,
        Self::Note,
        Self::Comment,
        Self::Line,
        Self::Arrow,
        Self::Rectangle,
        Self::Circle,
    ];

    fn subtype(self) -> c_int {
        match self {
            Self::Note | Self::Comment => 1, // Text
            Self::Rectangle => 5,
            Self::Circle => 6,
            Self::Highlight => 9,
            Self::Underline => 10,
            Self::Strikeout => 12,
            // PDFium cannot create Line annotations. Ink stores the same paths as editable vectors.
            Self::Ink | Self::Line | Self::Arrow => 15,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Highlight => "highlight",
            Self::Underline => "underline",
            Self::Strikeout => "strikeout",
            Self::Ink => "ink",
            Self::Note => "note",
            Self::Comment => "comment",
            Self::Line => "line",
            Self::Arrow => "arrow",
            Self::Rectangle => "rectangle",
            Self::Circle => "circle",
        }
    }
}

const ANNOTATION_TOOL_KEY: &std::ffi::CStr = c"TinyPDFTool";

impl Document<'_> {
    fn check_annotation_permissions(&self) -> Result<(), PdfError> {
        if self.permissions & PERMISSION_ANNOTATE == 0 {
            return Err(PdfError::Pdf(
                "Editing annotations is restricted by this PDF.",
            ));
        }
        Ok(())
    }

    fn annotation_value(&self, annotation: Raw, key: &std::ffi::CStr) -> String {
        wide_string(|buffer, length| unsafe {
            (self.api.get_annotation_string)(annotation, key.as_ptr(), buffer, length)
        })
    }

    fn annotation_handle(&self, page: &Handle, index: c_int) -> Result<Handle, PdfError> {
        let handle = unsafe { (self.api.get_annotation)(page.raw, index) };
        Handle::new(
            handle,
            self.api.close_annotation,
            "This annotation is no longer available.",
        )
    }

    fn set_annotation_value(&self, annotation: Raw, key: &std::ffi::CStr, input: &str) -> bool {
        let value = wide(input);
        unsafe { (self.api.set_annotation_string)(annotation, key.as_ptr(), value.as_ptr()) != 0 }
    }

    pub fn annotations(&self, page: u32) -> Result<Vec<AnnotationInfo>, PdfError> {
        let page = self.load_page(page)?;
        let count = unsafe { (self.api.count_annotations)(page.raw) };
        Ok((0..count)
            .filter_map(|index| {
                let handle = self.annotation_handle(&page, index).ok()?;
                let raw = handle.raw;
                let tool = self.annotation_value(raw, ANNOTATION_TOOL_KEY);
                let mut fields = tool.split_ascii_whitespace();
                let name = fields.next()?;
                let kind = AnnotationKind::ALL
                    .into_iter()
                    .find(|kind| kind.key() == name)?;
                if kind.subtype() != unsafe { (self.api.annotation_subtype)(raw) } {
                    return None;
                }
                let mut color = [0u8; 4];
                for value in &mut color {
                    *value = fields.next()?.parse().ok()?;
                }
                let thickness = fields.next()?.parse::<f32>().ok()?;
                if !valid_thickness(thickness) {
                    return None;
                }
                let mut bounds = Rect::default();
                if unsafe { (self.api.get_annotation_rect)(raw, &mut bounds) } == 0
                    || !bounds.is_valid()
                {
                    return None;
                }
                let mut rects = Vec::new();
                let mut points = Vec::new();
                match kind.subtype() {
                    9 | 10 | 12 => {
                        let count =
                            unsafe { (self.api.count_quads)(raw) }.min(MAX_ANNOTATION_POINTS);
                        rects.extend((0..count).filter_map(|quad_index| {
                            let mut quad = QuadPoints::default();
                            if unsafe { (self.api.get_quad)(raw, quad_index, &mut quad) } == 0 {
                                return None;
                            }
                            Rect::from_points(&quad.points).filter(|rect| rect.is_valid())
                        }));
                        if rects.is_empty() {
                            return None;
                        }
                    }
                    15 => {
                        let length = unsafe { (self.api.get_stroke)(raw, 0, ptr::null_mut(), 0) };
                        if !(1..=MAX_ANNOTATION_POINTS as c_ulong).contains(&length) {
                            return None;
                        }
                        points.resize(length as usize, Point::default());
                        if unsafe { (self.api.get_stroke)(raw, 0, points.as_mut_ptr(), length) }
                            != length
                            || !points.iter().copied().all(valid_point)
                        {
                            return None;
                        }
                    }
                    _ => rects.push(bounds),
                }
                let text = self.annotation_value(raw, c"Contents");
                Some(AnnotationInfo {
                    index,
                    bounds,
                    annotation: Annotation {
                        kind,
                        color,
                        thickness,
                        rects,
                        points,
                        text,
                    },
                })
            })
            .collect())
    }

    pub fn add_annotation(&self, page: u32, annotation: &Annotation) -> Result<i32, PdfError> {
        self.check_annotation_permissions()?;
        let page = self.load_page(page)?;
        self.add_annotation_on(&page, annotation)
    }

    fn add_annotation_on(&self, page: &Handle, annotation: &Annotation) -> Result<i32, PdfError> {
        let draft = AnnotationDraft::make(annotation)?;
        let index = unsafe { (self.api.count_annotations)(page.raw) };
        let raw = unsafe { (self.api.create_annotation)(page.raw, annotation.kind.subtype()) };
        let handle = Handle::new(
            raw,
            self.api.close_annotation,
            "PDFium could not create the annotation.",
        )?;
        let success = self.configure_annotation(handle.raw, annotation, &draft);
        drop(handle);
        if !success {
            unsafe { (self.api.remove_annotation)(page.raw, index) };
            return Err(PdfError::Pdf("PDFium could not write the annotation."));
        }
        self.annotated.set(true);
        Ok(index)
    }

    pub fn replace_annotation(
        &self,
        page: u32,
        index: i32,
        annotation: &Annotation,
    ) -> Result<i32, PdfError> {
        self.check_annotation_permissions()?;
        let page = self.load_page(page)?;
        let old = self.annotation_handle(&page, index)?;
        self.check_owned_annotation(&old)?;
        let appended = self.add_annotation_on(&page, annotation)?;
        if unsafe { (self.api.remove_annotation)(page.raw, index) } == 0 {
            unsafe { (self.api.remove_annotation)(page.raw, appended) };
            return Err(PdfError::Pdf("PDFium could not replace the annotation."));
        }
        Ok(appended - 1)
    }

    pub fn remove_annotation(&self, page: u32, index: i32) -> Result<(), PdfError> {
        self.check_annotation_permissions()?;
        let page = self.load_page(page)?;
        let annotation = self.annotation_handle(&page, index)?;
        self.check_owned_annotation(&annotation)?;
        drop(annotation);
        if unsafe { (self.api.remove_annotation)(page.raw, index) } == 0 {
            return Err(PdfError::Pdf("PDFium could not remove the annotation."));
        }
        self.annotated.set(true);
        Ok(())
    }

    fn check_owned_annotation(&self, annotation: &Handle) -> Result<(), PdfError> {
        if self
            .annotation_value(annotation.raw, ANNOTATION_TOOL_KEY)
            .is_empty()
        {
            return Err(PdfError::Pdf("Only TinyPDF annotations can be edited."));
        }
        Ok(())
    }

    fn configure_annotation(
        &self,
        raw: Raw,
        annotation: &Annotation,
        draft: &AnnotationDraft,
    ) -> bool {
        let [red, green, blue, opacity] = annotation.color.map(c_uint::from);
        let tool = format!(
            "{} {red} {green} {blue} {opacity} {}",
            annotation.kind.key(),
            annotation.thickness
        );
        if !unsafe {
            (self.api.set_annotation_rect)(raw, &draft.bounds) != 0
                && (self.api.set_annotation_flags)(raw, ANNOTATION_PRINT) != 0
                && (self.api.set_annotation_border)(raw, 0.0, 0.0, annotation.thickness) != 0
                && (self.api.set_annotation_color)(raw, 0, red, green, blue, opacity) != 0
        } || !self.set_annotation_value(raw, ANNOTATION_TOOL_KEY, &tool)
            || (!annotation.text.is_empty()
                && !self.set_annotation_value(raw, c"Contents", &annotation.text))
        {
            return false;
        }
        if annotation.kind.subtype() == 15 {
            // The first stroke holds the user-editable points. Arrow wings, if any, are a second path.
            if unsafe {
                (self.api.add_stroke)(raw, annotation.points.as_ptr(), annotation.points.len())
            } < 0
            {
                return false;
            }
            if let Some(wing) = draft.arrow
                && unsafe { (self.api.add_stroke)(raw, wing.as_ptr(), wing.len()) } < 0
            {
                return false;
            }
        }
        if annotation.kind.subtype() == 1 && !self.set_annotation_value(raw, c"T", "TinyPDF") {
            return false;
        }
        if matches!(
            annotation.kind,
            AnnotationKind::Highlight | AnnotationKind::Underline | AnnotationKind::Strikeout
        ) {
            for &rect in &annotation.rects {
                let quad = QuadPoints {
                    points: [
                        Point {
                            x: rect.left,
                            y: rect.top,
                        },
                        Point {
                            x: rect.right,
                            y: rect.top,
                        },
                        Point {
                            x: rect.left,
                            y: rect.bottom,
                        },
                        Point {
                            x: rect.right,
                            y: rect.bottom,
                        },
                    ],
                };
                if unsafe { (self.api.append_quad)(raw, &quad) } == 0 {
                    return false;
                }
            }
            // PDFium generates appearance streams for text markup from the standard quad points.
            return true;
        }
        let appearance = wide(&draft.appearance);
        unsafe { (self.api.set_appearance)(raw, 0, appearance.as_ptr()) != 0 }
    }
}

fn valid_point(point: Point) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && point.x.abs() <= 10_000_000.0
        && point.y.abs() <= 10_000_000.0
}

fn valid_thickness(value: f32) -> bool {
    value.is_finite() && (0.0..=144.0).contains(&value)
}

struct AnnotationDraft {
    bounds: Rect,
    arrow: Option<[Point; 3]>,
    appearance: String,
}

impl AnnotationDraft {
    fn make(annotation: &Annotation) -> Result<Self, PdfError> {
        use AnnotationKind::*;
        let invalid = PdfError::Invalid("The annotation has invalid or oversized geometry.");
        if !valid_thickness(annotation.thickness)
            || annotation.rects.len() > MAX_ANNOTATION_POINTS
            || annotation.points.len() > MAX_ANNOTATION_POINTS
            || annotation.text.len() > 65_536
            || annotation.text.contains('\0')
            || !annotation.rects.iter().copied().all(Rect::is_valid)
            || !annotation.points.iter().copied().all(valid_point)
        {
            return Err(invalid);
        }
        let mut arrow = None;
        let bounds = match annotation.kind {
            Highlight | Underline | Strikeout => {
                annotation.rects.iter().copied().reduce(Rect::union)
            }
            Ink | Line | Arrow => {
                if annotation.kind != Ink && annotation.points.len() != 2 {
                    return Err(invalid);
                }
                let mut bounds = Rect::from_points(&annotation.points).ok_or(invalid)?;
                if annotation.kind == Arrow {
                    let start = annotation.points[0];
                    let end = annotation.points[1];
                    let dx = end.x - start.x;
                    let dy = end.y - start.y;
                    let length = dx.hypot(dy);
                    if length > 0.0 {
                        let size = (annotation.thickness * 4.0).max(8.0).min(length * 0.75);
                        let along = Point {
                            x: dx / length * size,
                            y: dy / length * size,
                        };
                        let back = Point {
                            x: end.x - along.x,
                            y: end.y - along.y,
                        };
                        let wing = [
                            Point {
                                x: back.x - 0.5 * along.y,
                                y: back.y + 0.5 * along.x,
                            },
                            end,
                            Point {
                                x: back.x + 0.5 * along.y,
                                y: back.y - 0.5 * along.x,
                            },
                        ];
                        bounds = bounds.union(Rect::from_points(&wing).unwrap());
                        arrow = Some(wing);
                    }
                }
                let padding = annotation.thickness * 0.5 + 1.0;
                Some(Rect {
                    left: bounds.left - padding,
                    top: bounds.top + padding,
                    right: bounds.right + padding,
                    bottom: bounds.bottom - padding,
                })
            }
            Note | Comment | Rectangle | Circle => annotation
                .rects
                .first()
                .copied()
                .or_else(|| Rect::from_points(&annotation.points)),
        };
        let bounds = bounds
            .filter(|rect| rect.is_valid())
            .ok_or(PdfError::Invalid(
                "The annotation needs an area or a stroke on the page.",
            ))?;
        let appearance = match annotation.kind {
            Highlight | Underline | Strikeout => String::new(),
            kind => vector_appearance(kind, annotation, bounds, arrow.as_ref()),
        };
        Ok(Self {
            bounds,
            arrow,
            appearance,
        })
    }
}

fn vector_appearance(
    kind: AnnotationKind,
    annotation: &Annotation,
    bounds: Rect,
    arrow: Option<&[Point; 3]>,
) -> String {
    use AnnotationKind::*;
    let [r, g, b, opacity] = annotation.color;
    let [r, g, b] = [r, g, b].map(|component| f32::from(component) / 255.0);
    let mut output = String::from("q\n");
    if opacity < 255 {
        output.push_str("/GS gs\n");
    }
    let width = annotation.thickness.max(0.1);
    let _ = writeln!(
        output,
        "{r:.6} {g:.6} {b:.6} RG {r:.6} {g:.6} {b:.6} rg {width:.4} w 1 J 1 j"
    );
    match kind {
        Ink | Line | Arrow => {
            stroke(&mut output, &annotation.points);
            if let Some(wing) = arrow {
                stroke(&mut output, wing);
            }
        }
        Rectangle | Circle => {
            let padding = (width * 0.5)
                .min((bounds.right - bounds.left).min(bounds.top - bounds.bottom) * 0.2);
            let (left, right) = (bounds.left + padding, bounds.right - padding);
            let (bottom, top) = (bounds.bottom + padding, bounds.top - padding);
            if kind == Rectangle {
                let _ = writeln!(
                    output,
                    "{left:.4} {bottom:.4} {:.4} {:.4} re S",
                    right - left,
                    top - bottom
                );
            } else {
                // Four cubic Bezier arcs approximate the ellipse inside the annotation's bounds.
                let (x, y) = ((left + right) * 0.5, (bottom + top) * 0.5);
                let dx = (right - x) * 0.552_284_8;
                let dy = (top - y) * 0.552_284_8;
                let _ = writeln!(
                    output,
                    "{right:.4} {y:.4} m {right:.4} {:.4} {:.4} {top:.4} {x:.4} {top:.4} c\n\
                    {:.4} {top:.4} {left:.4} {:.4} {left:.4} {y:.4} c\n\
                    {left:.4} {:.4} {:.4} {bottom:.4} {x:.4} {bottom:.4} c\n\
                    {:.4} {bottom:.4} {right:.4} {:.4} {right:.4} {y:.4} c h S",
                    y + dy,
                    x + dx,
                    x - dx,
                    y + dy,
                    y - dy,
                    x - dx,
                    x + dx,
                    y - dy
                );
            }
        }
        Note | Comment => {
            let Rect {
                left,
                right,
                bottom,
                top,
            } = bounds;
            let width = right - left;
            let height = top - bottom;
            let _ = writeln!(output, "{left:.4} {bottom:.4} {width:.4} {height:.4} re f");
            output.push_str("0.12 0.15 0.19 RG 1 w\n");
            for row in [0.35, 0.55, 0.75] {
                let y = bottom + row * height;
                let _ = writeln!(
                    output,
                    "{:.4} {y:.4} m {:.4} {y:.4} l S",
                    left + width * 0.21,
                    right - width * 0.21
                );
            }
        }
        _ => unreachable!(),
    }
    output.push_str("Q\n");
    output
}

fn stroke(output: &mut String, points: &[Point]) {
    let first = points[0];
    let _ = writeln!(output, "{:.4} {:.4} m", first.x, first.y);
    if points.len() == 1 {
        let _ = writeln!(output, "{:.4} {:.4} l", first.x + 0.001, first.y);
    } else {
        for point in &points[1..] {
            let _ = writeln!(output, "{:.4} {:.4} l", point.x, point.y);
        }
    }
    output.push_str("S\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    static PDFIUM_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn fixture() -> String {
        let contents =
            "1 0 0 rg 25 35 20 10 re f\n0 g BT /F1 16 Tf 55 100 Td (tiny reader tiny) Tj ET\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R /AcroForm << /Fields [10 0 R] >> >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 180] /CropBox [20 30 220 150] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 180] /CropBox [20 30 220 150] /Rotate 90 /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R /Annots [10 0 R] >>".to_owned(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{contents}endstream", contents.len()),
            "<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 1 >>".to_owned(),
            "<< /Title (Second page) /Parent 7 0 R /Dest [4 0 R /Fit] >>".to_owned(),
            "<< /Title (TinyPDF engine test) >>".to_owned(),
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (field) /V (existing value) /F 4 /Rect [70 72 100 88] /AP << /N 11 0 R >> /P 4 0 R >>".to_owned(),
            {
                let field = "0 1 0 rg 0 0 30 16 re f\n";
                format!("<< /Type /XObject /Subtype /Form /BBox [0 0 30 16] /Length {} >>\nstream\n{field}endstream", field.len())
            },
        ];
        let mut text = "%PDF-1.7\n".to_owned();
        let mut offsets = Vec::new();
        for (number, object) in objects.into_iter().enumerate() {
            offsets.push(text.len());
            let _ = writeln!(text, "{} 0 obj\n{object}\nendobj", number + 1);
        }
        let xref = text.len();
        let count = offsets.len() + 1;
        let _ = writeln!(text, "xref\n0 {count}\n0000000000 65535 f ");
        for offset in offsets {
            let _ = writeln!(text, "{offset:010} 00000 n ");
        }
        let _ = writeln!(
            text,
            "trailer\n<< /Size {count} /Root 1 0 R /Info 9 0 R >>\nstartxref\n{xref}\n%%EOF"
        );
        text
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0.join("caf\u{e9}.pdf"));
            let _ = fs::remove_file(self.0.join("invalid.pdf"));
            let _ = fs::remove_file(self.0.join("other.pdf"));
            let _ = fs::remove_file(self.0.join("replacement.pdf"));
            let _ = fs::remove_file(self.0.join("edited-elsewhere.pdf"));
            let _ = fs::remove_file(self.0.join("protected-graphics.pdf"));
            let _ = fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn coordinate_transform_inverts_a_nonzero_rotated_crop() {
        let transform = PageCoordinates {
            origin: Point { x: 30.0, y: 70.0 },
            horizontal: Point { x: 0.0, y: 140.0 },
            vertical: Point { x: 100.0, y: 0.0 },
        };
        let unit = Point { x: 0.25, y: 0.5 };
        let page = Point { x: 80.0, y: 105.0 };
        assert_eq!(transform.to_pdf(unit), page);
        assert_eq!(transform.from_pdf(page), unit);
    }

    #[test]
    #[cfg_attr(
        not(windows),
        ignore = "requires bundled PDFium or PDFIUM_LIBRARY_PATH"
    )]
    fn pdfium_renders_selects_searches_and_reopens_embedded_annotations() {
        let _exclusive = PDFIUM_TEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scratch = Scratch(env::temp_dir().join(format!(
            "tinypdf-engine-{}-{}",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        )));
        fs::create_dir(&scratch.0).unwrap();
        let path = scratch.0.join("caf\u{e9}.pdf");
        fs::write(&path, fixture()).unwrap();
        let invalid = scratch.0.join("invalid.pdf");
        fs::write(&invalid, b"not a PDF").unwrap();
        let engine = Pdfium::load().unwrap();
        assert!(engine.open(&invalid, None).is_err());

        let pdf = engine.open(&path, None).unwrap();
        // Force the large-document threshold to exercise reopening on this tiny fixture.
        pdf.can_reopen.set(true);
        let original = pdf.native.borrow().handle.raw;
        pdf.source.bytes_read.set(NATIVE_STREAM_BUDGET);
        assert!(pdf.page_text(0).unwrap().to_lowercase().contains("tiny"));
        assert_eq!(pdf.search_page(0, "tiny").unwrap().len(), 2);
        assert_ne!(pdf.native.borrow().handle.raw, original);
        assert_eq!(pdf.page_count(), 2);
        assert_eq!(pdf.page_size(0).unwrap(), (200.0, 120.0));
        assert_eq!(pdf.page_size(1).unwrap(), (120.0, 200.0));
        assert_eq!(
            pdf.metadata(),
            vec![("Title".into(), "TinyPDF engine test".into())]
        );
        assert_eq!(pdf.outline()[0].title, "Second page");
        assert_eq!(pdf.outline()[0].page, Some(1));
        let text = pdf.search_page(0, "TINY").unwrap();
        assert_eq!(text.len(), 2);
        assert_eq!(pdf.search_page_up_to(0, "tiny", 1).unwrap(), text[..1]);
        assert!(pdf.search_page_up_to(0, "tiny", 0).unwrap().is_empty());
        let center = |rect: Rect| Point {
            x: (rect.left + rect.right) * 0.5,
            y: (rect.bottom + rect.top) * 0.5,
        };
        let selection = pdf
            .select(0, center(text[0][0]), center(text[1][0]))
            .unwrap();
        assert!(selection.text.contains("reader"));

        let mut full = vec![0; 200 * 120 * 4];
        pdf.render_rgba(0, 200, 120, 0, &mut full, || false)
            .unwrap();
        let mut tile = vec![0; 10 * 5 * 4];
        let region = RenderRegion {
            x: 10,
            y: 107,
            width: 10,
            height: 5,
            full_width: 200,
            full_height: 120,
        };
        let coordinates = pdf
            .render_region_rgba(0, region, 0, &mut tile, true, || false)
            .unwrap();
        assert_eq!(coordinates, Some(pdf.page_coordinates(0, 0).unwrap()));
        let index = (107 * 200 + 10) * 4;
        assert_eq!(&full[index..index + 4], &[255, 0, 0, 255]);
        assert!(
            tile.as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [255, 0, 0, 255])
        );
        assert!(matches!(
            pdf.render_region_rgba(0, region, 0, &mut tile, true, || true),
            Err(PdfError::Cancelled)
        ));
        assert!(
            pdf.render_rgba(0, 200, 120, 0, &mut tile, || false)
                .is_err()
        );
        for (page, rotation, y) in [(0, 0, 150.0), (0, 1, 30.0), (1, 0, 30.0)] {
            let point = pdf
                .page_coordinates(page, rotation)
                .unwrap()
                .to_pdf(Point::default());
            assert!((point.x - 20.0).abs() < 0.001);
            assert!((point.y - y).abs() < 0.001);
        }
        let mut with_form = vec![0; 120 * 200 * 4];
        pdf.render_rgba(1, 120, 200, 0, &mut with_form, || false)
            .unwrap();
        let pos = pdf
            .page_coordinates(1, 0)
            .unwrap()
            .from_pdf(Point { x: 85.0, y: 80.0 });
        let form_pixel = ((pos.y * 200.0) as usize * 120 + (pos.x * 120.0) as usize) * 4;
        assert_eq!(&with_form[form_pixel..form_pixel + 4], &[0, 255, 0, 255]);

        let rect = Rect {
            left: 55.0,
            top: 80.0,
            right: 105.0,
            bottom: 55.0,
        };
        let marks: Vec<_> = AnnotationKind::ALL
            .into_iter()
            .map(|kind| {
                let (rects, points) = match kind {
                    AnnotationKind::Highlight
                    | AnnotationKind::Underline
                    | AnnotationKind::Strikeout => (text[0].clone(), Vec::new()),
                    AnnotationKind::Ink | AnnotationKind::Line | AnnotationKind::Arrow => (
                        Vec::new(),
                        vec![Point { x: 80.0, y: 50.0 }, Point { x: 150.0, y: 50.0 }],
                    ),
                    _ => (vec![rect], Vec::new()),
                };
                Annotation {
                    kind,
                    color: [0, 90, 198, 200],
                    thickness: 3.0,
                    rects,
                    points,
                    text: "Annotation - \u{1f4cc}".into(),
                }
            })
            .collect();
        for (index, annotation) in marks.iter().enumerate() {
            assert_eq!(
                pdf.add_annotation(0, annotation).unwrap(),
                index as i32,
                "{:?}",
                annotation.kind
            );
        }
        assert_eq!(pdf.annotations(0).unwrap().len(), marks.len());
        let edited = pdf.native.borrow().handle.raw;
        pdf.source.bytes_read.set(NATIVE_STREAM_BUDGET);
        assert_eq!(pdf.annotations(0).unwrap().len(), marks.len());
        assert_eq!(pdf.native.borrow().handle.raw, edited);
        let mut with_marks = full.clone();
        pdf.render_rgba(0, 200, 120, 0, &mut with_marks, || false)
            .unwrap();
        let pen = (100 * 200 + 95) * 4;
        assert_ne!(&with_marks[pen..pen + 4], &[255, 255, 255, 255]);
        assert!(
            with_marks
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
        // Saving to the active source path must preserve the current reader and embedded comments.
        pdf.save_as(&path).unwrap();
        pdf.save_as(&path).unwrap();
        assert_eq!(pdf.search_page(0, "tiny").unwrap().len(), 2);

        let other = scratch.0.join("other.pdf");
        fs::write(&other, b"existing Save As target").unwrap();
        pdf.save_as(&other).unwrap();
        pdf.save_as(&other).unwrap();
        assert_eq!(
            engine
                .open(&other, None)
                .unwrap()
                .annotations(0)
                .unwrap()
                .len(),
            marks.len()
        );

        // An atomic replacement with identical size and timestamp still has a different identity.
        let replacement = scratch.0.join("replacement.pdf");
        let original_stamp = FileStamp::at(&other).unwrap().unwrap();
        fs::copy(&other, &replacement).unwrap();
        let replacement_file = OpenOptions::new().write(true).open(&replacement).unwrap();
        replacement_file
            .set_modified(original_stamp.modified)
            .unwrap();
        drop(replacement_file);
        fs::rename(&replacement, &other).unwrap();
        let modified_stamp = FileStamp::at(&other).unwrap().unwrap();
        assert_eq!(
            (modified_stamp.length, modified_stamp.modified),
            (original_stamp.length, original_stamp.modified)
        );
        assert_ne!(modified_stamp.identity, original_stamp.identity);
        assert!(
            matches!(pdf.save_as(&other), Err(PdfError::Pdf(message)) if message.contains("outside TinyPDF"))
        );
        assert_eq!(FileStamp::at(&other).unwrap(), Some(modified_stamp));

        // An in-place third-party write also invalidates the backing PDFium reader.
        let originally_open = scratch.0.join("edited-elsewhere.pdf");
        fs::write(&originally_open, fixture()).unwrap();
        let original = engine.open(&originally_open, None).unwrap();
        fs::write(&originally_open, b"an external writer's new contents").unwrap();
        assert!(original.save_as(&originally_open).is_err());
        assert!(
            matches!(original.save_as(&replacement), Err(PdfError::Pdf(message)) if message.contains("source was modified"))
        );
        assert_eq!(
            fs::read(&originally_open).unwrap(),
            b"an external writer's new contents"
        );
        assert!(!replacement.exists());

        drop(pdf);
        let reopened = engine.open(&path, None).unwrap();
        let reopened_marks = reopened.annotations(0).unwrap();
        assert_eq!(
            reopened_marks
                .iter()
                .map(|entry| &entry.annotation)
                .collect::<Vec<_>>(),
            marks.iter().collect::<Vec<_>>()
        );
        let note = reopened_marks
            .iter()
            .find(|entry| entry.annotation.kind == AnnotationKind::Note)
            .unwrap();
        let mut moved = note.annotation.clone();
        moved.rects[0].left += 5.0;
        let new_index = reopened.replace_annotation(0, note.index, &moved).unwrap();
        assert_eq!(
            reopened.annotations(0).unwrap()[new_index as usize].annotation,
            moved
        );
        reopened.remove_annotation(0, new_index).unwrap();
        reopened.save_as(&path).unwrap();
        drop(reopened);
        assert_eq!(
            engine
                .open(&path, None)
                .unwrap()
                .annotations(0)
                .unwrap()
                .len(),
            marks.len() - 1
        );
    }

    #[test]
    #[cfg_attr(
        not(windows),
        ignore = "requires bundled PDFium or PDFIUM_LIBRARY_PATH"
    )]
    fn protected_pdf_can_retry_password_and_render_image_vector_and_transparency() {
        let _exclusive = PDFIUM_TEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scratch = Scratch(env::temp_dir().join(format!(
            "tinypdf-protected-{}-{}",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        )));
        fs::create_dir(&scratch.0).unwrap();
        let path = scratch.0.join("protected-graphics.pdf");
        fs::write(
            &path,
            include_bytes!("../tests/fixtures/protected-graphics.pdf"),
        )
        .unwrap();
        let engine = Pdfium::load().unwrap();
        for password in [None, Some("wrong")] {
            assert!(matches!(
                engine.open(&path, password),
                Err(PdfError::PasswordRequired)
            ));
        }
        let pdf = engine.open(&path, Some("classroom")).unwrap();
        assert_eq!(pdf.page_count(), 1);
        let mut rgba = vec![0; 120 * 80 * 4];
        pdf.render_rgba(0, 120, 80, 0, &mut rgba, || false).unwrap();
        let pixel = |x: usize, y: usize| &rgba[(y * 120 + x) * 4..(y * 120 + x) * 4 + 4];
        assert_eq!(pixel(10, 15), &[0, 0, 255, 255]);
        assert_eq!(pixel(90, 20), &[0, 255, 0, 255]);
        let blended = pixel(50, 60);
        assert_eq!((blended[0], blended[3]), (255, 255));
        assert!(blended[1].abs_diff(128) <= 1 && blended[2].abs_diff(128) <= 1);
        drop(pdf);
        fs::remove_file(path).unwrap();
    }
}
