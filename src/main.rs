#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod layout;
mod links;
mod pdf;
mod platform;
mod storage;
mod worker;

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

slint::include_modules!();

fn main() -> std::process::ExitCode {
    let started = Instant::now();
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    #[cfg(windows)]
    let use_dialog = first
        .as_deref()
        .is_none_or(|arg| arg != "--benchmark" && arg != "--timing");
    let result = match first.as_deref().and_then(|arg| arg.to_str()) {
        Some("--benchmark") => match args.next() {
            Some(path) if args.next().is_none() => benchmark(Path::new(&path)),
            _ => Err("Usage: tinypdf --benchmark <document.pdf>".into()),
        },
        _ => {
            let timing = first.as_deref().is_some_and(|arg| arg == "--timing");
            let paths: Vec<_> = first
                .filter(|_| !timing)
                .into_iter()
                .chain(args)
                .map(PathBuf::from)
                .collect();
            app::run(paths, timing.then_some(started))
        }
    };
    if let Err(error) = result {
        eprintln!("TinyPDF: {error}");
        #[cfg(windows)]
        if use_dialog {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("TinyPDF could not start")
                .set_description(error.to_string())
                .show();
        }
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn benchmark(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let engine = pdf::Pdfium::load()?;
    let engine_ms = start.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    let document = engine.open(path, None)?;
    let open_ms = start.elapsed().as_secs_f64() * 1000.;
    let pages = document.page_count();
    if pages == 0 {
        return Err("The PDF has no pages.".into());
    }
    let mut render_ms = Vec::new();
    let mut pixels = Vec::new();
    for sample in 0..pages.min(16) {
        let page = sample as u64 * (pages - 1) as u64 / (pages.min(16) - 1).max(1) as u64;
        let start = Instant::now();
        let (width, height) = document.page_size(page as u32)?;
        let scale = (900. / width).min(1400. / height);
        let (width, height) = (
            (width * scale).round().max(1.) as u32,
            (height * scale).round().max(1.) as u32,
        );
        pixels.resize((width * height * 4) as usize, 0);
        document.render_rgba(page as u32, width, height, 0, &mut pixels, || false)?;
        render_ms.push(start.elapsed().as_secs_f64() * 1000.);
    }
    let search_start = Instant::now();
    let mut matches = 0;
    for page in 0..pages {
        matches += document.search_page(page, "survey")?.len();
    }
    println!(
        "{}",
        serde_json::json!({
            "pages": pages, "engine_ms": engine_ms, "open_ms": open_ms, "render_ms": render_ms,
            "search_ms": search_start.elapsed().as_secs_f64() * 1000., "search_matches": matches
        })
    );
    Ok(())
}
