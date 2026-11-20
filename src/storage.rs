use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const VERSION: u32 = 1;
const MAX_RECENT: usize = 32;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Continuous,
    Single,
    Spread,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Continuous => "continuous",
            Self::Single => "single",
            Self::Spread => "spread",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Zoom {
    Fixed(f32),
    #[default]
    FitPage,
    FitWidth,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Reading {
    pub page: usize,
    pub fraction: f32,
    pub zoom: Zoom,
    pub mode: Mode,
    pub rotation: u8,
    pub bookmarks: Vec<usize>,
}

impl Reading {
    fn validate(&mut self) {
        self.fraction = self.fraction.clamp(0., 1.);
        if let Zoom::Fixed(scale) = &mut self.zoom {
            *scale = scale.clamp(0.1, 8.);
        }
        self.rotation %= 4;
        self.bookmarks.sort_unstable();
        self.bookmarks.dedup();
        self.bookmarks.truncate(1024);
    }
}

#[derive(Deserialize, Serialize)]
pub struct Recent {
    pub path: PathBuf,
    pub reading: Reading,
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    version: u32,
    pub open: Vec<PathBuf>,
    pub active: Option<PathBuf>,
    pub recent: Vec<Recent>,
    pub sidebar: bool,
    pub dark: bool,
    pub restore_session: bool,
    pub default_zoom: Zoom,
    pub default_mode: Mode,
    pub memory_saver: bool,
    pub pen_color: String,
    pub pen_width: f32,
    pub automatic_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            open: Vec::new(),
            active: None,
            recent: Vec::new(),
            sidebar: false,
            dark: false,
            restore_session: true,
            default_zoom: Zoom::FitPage,
            default_mode: Mode::Continuous,
            memory_saver: false,
            pen_color: "#e65761".into(),
            pen_width: 3.,
            automatic_updates: false,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = fs::read(path) else {
            return Self::default();
        };
        if let Ok(mut settings) = serde_json::from_slice::<Self>(&bytes)
            && settings.version == VERSION
        {
            settings.open.truncate(MAX_RECENT);
            settings.recent.truncate(MAX_RECENT);
            settings.pen_width = settings.pen_width.clamp(1., 16.);
            for item in &mut settings.recent {
                item.reading.validate();
            }
            return settings;
        }
        let backup = path.with_extension(format!("{}.recovered.json", std::process::id()));
        let _ = fs::rename(path, backup);
        Self::default()
    }

    pub fn record(&mut self, path: PathBuf, reading: Reading) {
        self.recent.retain(|item| item.path != path);
        self.recent.insert(0, Recent { path, reading });
        self.recent.truncate(MAX_RECENT);
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let Some(parent) = path.parent() else {
            return Err(io::Error::other("Missing settings directory"));
        };
        fs::create_dir_all(parent)?;
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| {
            let mut file = fs::File::create(&temp)?;
            serde_json::to_writer(&mut file, self)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            replace(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}

#[cfg(not(windows))]
fn replace(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<_> = source.as_os_str().encode_wide().chain([0]).collect();
    let target: Vec<_> = target.as_os_str().encode_wide().chain([0]).collect();
    // Both buffers are NUL-terminated UTF-16 paths and live through the call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn path() -> PathBuf {
    if let Some(path) = env::var_os("TINYPDF_DATA_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(path).join("session.json");
    }
    let (base, subdirectory) = if cfg!(windows) {
        (env::var_os("LOCALAPPDATA").map(PathBuf::from), "TinyPDF")
    } else if cfg!(target_os = "macos") {
        (
            env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support")),
            "TinyPDF",
        )
    } else {
        (
            env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
                }),
            "tinypdf",
        )
    };
    base.unwrap_or_else(env::temp_dir)
        .join(subdirectory)
        .join("session.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_sessions_receive_compatible_reading_defaults() {
        let old: Settings = serde_json::from_str(r#"{"version":1,"dark":true}"#).unwrap();
        assert!(old.dark && old.restore_session);
        assert!(!old.memory_saver);
        assert_eq!(old.default_zoom, Zoom::FitPage);
        assert_eq!(old.default_mode, Mode::Continuous);
    }

    #[test]
    fn settings_replace_and_recover() {
        let base = env::temp_dir().join(format!("tinypdf-settings-test-{}", std::process::id()));
        let path = base.join("test.json");
        let doc = PathBuf::from("book.pdf");
        let mut settings = Settings::default();
        settings.record(
            doc.clone(),
            Reading {
                page: 7,
                ..Default::default()
            },
        );
        settings.save(&path).unwrap();
        settings.record(
            doc,
            Reading {
                page: 42,
                ..Default::default()
            },
        );
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).recent[0].reading.page, 42);
        fs::write(&path, b"torn").unwrap();
        assert!(Settings::load(&path).recent.is_empty());
        fs::remove_file(path.with_extension(format!("{}.recovered.json", std::process::id())))
            .unwrap();
        fs::remove_dir(base).unwrap();
    }
}
