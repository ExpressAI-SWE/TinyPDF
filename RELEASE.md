# TinyPDF v1 preview for Windows x64

The v1 candidate tracks version `1.0.0-beta.1`. Its Windows Installer version is `0.99.1`, which allows a later Windows `1.0.0` package to supersede it. The v1 changes add PDF links, Windows Default Apps registration, new button states, and per-tab open and save recovery. Each download has a 128 MiB limit and a fixed timeout. The app keeps downloaded PDFs temporarily until you close the tab or save a permanent copy.

The v1 candidate passed 14 Rust tests and Windows desktop checks covering corrupt PDF retry, recovery after restoring a missing PDFium DLL, incorrect and correct passwords, HTTP redirects, a live HTTPS PDF, HTML rejection, and cleanup after closing a downloaded PDF. A 501-page run delivered 6,000 wheel events and 299 direction reversals. The final view matched the initial reference crop byte for byte. Peak total working set was 41.72 MiB and idle private commit was 7.57 MiB.

A Windows file-lock regression confirmed that TinyPDF leaves the original PDF intact if another reader keeps it locked. The desktop regression confirmed that a note saves successfully when the other reader releases a temporary lock. The page number field waits until the selected document is ready before exposing its page value.

Navigation and zoom use square 40-pixel targets in the desktop toolbar and 44-pixel targets in fullscreen. Buttons show a pressed color immediately and keep the entire surface clickable. The desktop suite verifies the square geometry, visible mouse and Space-key press feedback, and a single navigation action on release.

Build the v1 packages with `scripts/build-windows-candidate.ps1`. Local NSIS and MSIX payloads passed extraction and native PDF reading. The MSI passed installation, Default Apps and Start Menu registration, native PDF reading, and uninstallation. Hosted checks cover clean-account NSIS and disposable signed MSIX validation. A public MSIX still requires publisher signing.

The sections below record the earlier 0.2.0 hardening build and its evidence.

## 0.2.0 changes

- Keep visible PDF tiles in the cache during scrolling and reuse their rendered images as the viewport moves. Visible work supersedes obsolete prefetch work. Prefetch follows the entering edge of the page without increasing the existing cache reserve.
- Retry failed visible renders once, discard obsolete output, and show a shaded placeholder until a tile is available. Read PDF coordinates from the page already open for rendering.
- Add accessible previous and next toolbar controls, keep keyboard focus when switching document tabs, and use dark mode for new profiles. The reader retains the theme stored in an existing profile.
- Add an offline per-user Windows Installer package and a standalone x64 MSIX alongside the NSIS setup. Embed Windows version information and write artifacts to separate directories.

See the [engineering audit](AUDIT.md) for architecture and failure analysis, the [benchmark report](BENCHMARKS.md) for workload and memory details, and the [README](README.md) for build, signing, and install commands.

## Automated verification

The Windows baseline had 7 passing Rust tests. The updated suite passed 11 tests with zero failures and zero skips. It covers viewport boundaries, 2,000 direction changes in layout and scheduling, cancellation and stale-key rejection, protected cache eviction, invalid input, native resource reopening, and rendering of text, forms, rotation, images, vectors, transparency, and encrypted PDFs. The generated PDF fixture in `tests/fixtures/protected-graphics.pdf` uses the test password `classroom`. It contains a blue RGB image, a translucent red fill, and a green Bézier path. Pixel tests check interior samples including the composited color.

Rust checks are reproducible with `cargo fmt --check`, `cargo test --locked`, and `cargo build --release --locked`. The lockfile scan ran with `cargo audit --no-fetch --no-yanked`. It reported seven maintenance advisories and no vulnerability findings in the locked crates. This scan did not check the PDFium DLL or crate yanks.

Desktop checks passed for page navigation, independent tabs, Ctrl+F after switching tabs, native fullscreen and its controls, note creation and PDF saving, session restoration, thumbnail rendering, sidebar navigation, 24 repeated zoom changes, and injected single-finger touch panning. Windows rejected both attempts to inject two simultaneous touch contacts with error 87. Two-finger pinch and physical smartboard validation remain open.

## Package validation

The artifacts are under `dist/0.2.0-win-x64-hardened`. All contain the same x64 reader and PDFium DLL used in the completed tests.

| Artifact | Bytes | SHA-256 | Validation |
|---|---:|---|---|
| `TinyPDF-Setup.exe` | 7,334,077 | `E42DD346E97F16FD1F2D124F52DED2A70FA52CE60C23DE8F850C15D91D01F80E` | Verified the 0.2.0 metadata and the extracted payload. The extracted app passed scroll and rendering checks. |
| `TinyPDF.msi` | 9,412,608 | `63A4301A29EE2CF287BA63C60CE3D30CC67724E0B07FD6986811CB4A8C8FFF3B` | WiX validation, fresh installation and removal, silent upgrade from a synthetic 0.1.9 package, Start Menu shortcut, installed payload checks, launch, 768 wheel events and 47 reversals, and removal of the installed files and shortcut. |
| `TinyPDF.msix` | 9,564,874 | `7B584D2776C474FC53C907E72D0D1DC2B1FE0B9D5F174E24E9780770B525E74A` | MakeAppx packed and unpacked identity `TinyPDF.Reader`, version `0.2.0.0`, and architecture `x64`. The extracted app passed scroll and rendering checks. The package is unsigned. |
| `TinyPDF-win-x64.zip` | 9,587,041 | `4C8B56E8165EFD7D77EFE70F1080CD2531A401448F5A0013E1DB8C2694B52AFB` | Shared payload source for all three packaging workflows. |

The MSI ran on the development host using a separate installation directory and fresh app profiles. NSIS was checked through its extracted payload because this Windows account already has TinyPDF 0.1.0 installed. MSIX signature verification reported that the package has no signature, and a trusted publisher certificate was unavailable. Deployment approval is pending the clean Windows checks below.

## Clean-machine release gates

- Run each installer separately on a clean Windows x64 VM or test account. Start from a fresh snapshot for each installer. Launch from Start, open a local PDF, check page navigation and rapid scrolling, and confirm uninstall removes the package-owned files and shortcuts. Check NSIS and MSIX **Open with** registration. Verify the NSIS optional desktop shortcut in both states and upgrade the previous NSIS version in a disposable snapshot.
- Supply a trusted publisher certificate, rebuild the MSIX with the matching manifest publisher, verify its signature, and validate install, activation, PDF file association and removal on the clean machine. Test an upgrade using packages signed under the same identity. [Microsoft MSIX signing guide](https://learn.microsoft.com/windows/msix/package/sign-msix-package-guide)
- Confirm the supported display, DPI and touch conditions on the target school hardware. Exercise long reading sessions and large scans before deployment. PDFium runs inside the reader process and image-heavy documents can exceed the historical memory figures.
