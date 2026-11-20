# TinyPDF

TinyPDF is a native desktop PDF reader built with Rust, Slint and PDFium. It renders locally using Slint’s software backend and needs no GPU graphics API.

## Build and run on Windows x64

Use PowerShell 7, the Rust MSVC toolchain, and the Visual Studio C++ build tools. Cargo verifies and stages the pinned PDFium DLL on Windows. The first build downloads the archive when it is missing. For an offline build, place the pinned `pdfium-win-x64.tgz` in `target/pdfium-download/` and make the locked Rust dependencies available locally.

```powershell
cargo build --release --locked
& ./target/release/tinypdf.exe "C:/path/to/document.pdf"
cargo test --locked
```

Use **+** or **Ctrl+T** to open a home tab. Open or drop a PDF there to fill it. **Ctrl+Tab** and **Ctrl+Shift+Tab** switch tabs. Each PDF keeps its own reading position. The toolbar provides page navigation, Find, zoom, page views and annotations. Use **View** for page modes and fit options. Save, Print, Rotate and OCR are in **More**.

Open **Settings** to change the interface theme, choose whether to restore tabs, and set the default layout and fit for PDFs TinyPDF has not seen before. Previously opened files use their saved positions and view settings. **Memory saver** reduces the raster cache reserve and skips nearby-page prefetch. Visible pages still render at their usual resolution. PDF page colors stay as rendered by PDFium.

Use **More > View page text** to open selectable native PDF text in a text field. On Windows, **Read page aloud** uses the installed Windows voice for the current page, and **Stop reading** stops playback. Select native PDF text and press **Ctrl+C**. On Windows, open **More** and choose **Extract page text (OCR)** or **Select area for OCR**. Area OCR uses a drag rectangle. OCR uses an installed Windows recognition language and runs on your device.

## Package

`./scripts/package.ps1` builds the host target and writes a portable archive to `dist/`. Use `-Binary` to package an existing release executable. The archive already includes PDFium and its notices. Extract it before starting TinyPDF.

```powershell
./scripts/package.ps1 -Binary ./target/release/tinypdf.exe
./scripts/package.ps1 -Binary ./target/release/tinypdf.exe -MakeNsi "C:/path/to/makensis.exe"
```

Build a Windows x64 MSIX bundle from the portable ZIP with the Windows SDK:

```powershell
./scripts/package-msix.ps1 -PortableZip ./dist/TinyPDF-win-x64.zip -MakeAppx "C:/path/to/makeappx.exe"
```

The MSIX output is unsigned by default. For installation, supply a trusted signing certificate whose subject matches `-Publisher`, and pass `-SignTool` and `-CertificateThumbprint`. The packaging script does not change the machine's trust settings.

The optional Windows installer uses NSIS 3.13. It installs for the current Windows user and adds TinyPDF to **Open with** for PDFs. Windows keeps the current default PDF app. The packager reports executable, library, installed payload, and archive sizes and produces SHA-256 checksums for the finished artifacts. The current local packages are unsigned.

`-Target` accepts Windows MSVC, Linux GNU, and macOS Rust targets for x64 and ARM64. Build each target with its native toolchain. Linux archives contain a portable executable. macOS archives contain an ad-hoc signed app bundle when packaged on macOS. Native behavior on each target requires testing on that platform.

TinyPDF displays Slint attribution in its About screen. Review dependency notices before distributing packages. [Slint licensing options](https://slint.dev/terms-and-conditions)

The packaging scripts use checksum-verified PDFium 159.0.8152.0. [Pinned PDFium release](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8152)

## Measure on Windows

See [Windows benchmark results](BENCHMARKS.md) for the tested workloads and measurement limits.

Run the benchmark from a desktop PowerShell session with access to Windows process metrics. The script uses an isolated profile for every run. Omit `-Pdf` to measure the empty reader.

```powershell
./scripts/new-benchmark-pdf.ps1 -Output ./target/benchmark-501.pdf
./scripts/bench-windows.ps1 -Executable ./target/release/tinypdf.exe -Pdf ./target/benchmark-501.pdf
./scripts/bench-pdfium.ps1 -Executable ./target/release/tinypdf.exe -Pdf ./target/benchmark-501.pdf
./scripts/bench-windows.ps1 -Browser -Executable "C:/path/to/browser.exe" -Pdf ./target/benchmark-501.pdf
```

Run each executable against the same local PDF and repeat on the same machine. Window startup runs from process launch to the first main window handle. TinyPDF also records elapsed time from its Rust entry point to publishing the first PDF tile to the interface. The stationary sample begins after the warmup and can still include document loading. Memory and CPU include the process tree. Working set counts shared pages in every process that uses them. GPU dedicated and shared memory come from Windows counters when available. The generated 501-page fixture exercises page count and text. The public USGS Circular 1296 download is a 112,181,765-byte real-world workload. [USGS Circular 1296](https://pubs.usgs.gov/circ/2006/1296/)
