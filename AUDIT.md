# TinyPDF engineering audit

Baseline is main at cedbdd9 with Rust 2024, Slint 1.19/Winit and the software renderer, and PDFium 159.0.8152.0 loaded via Rust FFI. Cargo compiles the Slint UI and Windows icon resource and stages a checksum-verified PDFium DLL. No graphics API or network connection is required for reading.

## Pipeline and ownership

The Slint UI sends reader commands to a single native worker. PDFium owns one process-wide engine, worker-owned documents, streamed file sources, and scoped page/bitmap/render handles. Opening returns the page count and first dimensions. Other dimensions arrive in batches. Layout anchors position to a page and fraction. The UI calculates clipped 1024-pixel tiles, queues viewport tiles before speculative work and thumbnails, and displays shared RGBA buffers on virtual page slots. Annotations, selection, search, OCR, speech and printing also use the worker. Settings and recent documents are stored locally. The baseline has seven passing Rust tests covering geometry, settings, printing, annotation fallback, and a native PDF fixture.

## Findings and planned fixes

- The raster cache trims by recency alone. At a viewport transition, a still-visible bitmap can be evicted before the UI re-reads it, turning an already rendered region into a blank rectangle. Protect the incoming viewport keys during both capacity changes and insertions.
- A speculative tile that remains in a newer plan can keep the only PDFium worker busy after higher-priority visible work appears. Make cancellation sensitive to visible priority and current key identity. Already-rendered obsolete results should be rejected before taking cache space.
- A failed tile is excluded until the tab resets or the failure set overflows. Add a bounded retry for the active viewport and allow another attempt on returning to it.
- Prediction currently warms row zero on both sides regardless of scroll direction and uses a worst-case admission threshold. Prewarm the edge that will enter the viewport first, within the existing byte budget.
- Replacing the entire tab model on document switches removes the focused tab control. Windows desktop testing reproduced a lost Ctrl+F shortcut immediately afterward. Update the existing model rows so the focus target survives selection.
- The worker opens a native page to obtain its coordinate transform, closes it, and opens it again to render the first tile. Compute the transform from the page already held during rendering.
- An entirely unseen page can require decoding and rasterization after a jump. Keep the sheet geometry stable and distinguish pending tiles from white rendered paper. A large PDFium operation or a print/OCR command can still occupy the native worker.
- Fit modes use the first page as the continuous-mode reference, so later mixed-size pages can exceed the viewport. This maintains a uniform scale. Dimensions discovered after loading can shift layout. Large page counts also raise layout and sidebar costs.
- The application ships PDFium in-process. Its native parser is the largest security boundary. Current bindings do not dispatch PDF JavaScript or document actions. The DLL path is restricted to application locations or an explicit absolute override. An isolated parser process would need separate memory, lifecycle, and compatibility work.
- Cargo Audit scanned 549 lockfile dependencies and returned seven upstream maintenance warnings. Yank checking timed out and this scan does not cover the bundled PDFium DLL. The locked application stack remains in place.
- The NSIS recipe uses hardcoded metadata and has no optional desktop component. The SDK script emits an MSIX bundle. Add version-derived metadata, a standalone MSIX, and an x64 Windows Installer recipe. The working tree already has older artifacts in dist. Use a separate release directory.
