# Windows x64 benchmark

## Memory after switching away from a scan

In two runs per build on November 19, 2026, TinyPDF visited all 13 pages of the same 119,288,725-byte image PDF, switched to the 501-page text fixture, and sampled process-private memory about one second later. Both builds used matching window sizes and default settings. Median private memory with the scan in the background fell from 22.9 MiB to 14.3 MiB. The change recycles native caches for eligible unedited documents after the new tab has rendered.

## Software renderer comparison

On November 19, 2026, the previous WGPU build and the current software build each opened the same generated 501-page PDF three times on this Windows host. Each run used a fresh profile and the build's default window size. The median figures below use a five-second warmup and approximately three seconds of stationary sampling.

| Metric | Previous WGPU build | Current software build |
| --- | ---: | ---: |
| Window startup | 94.1 ms | 47.7 ms |
| First PDF tile published | 2,047 ms | 83.7 ms |
| Working set | 175.3 MiB | 35.9 MiB |
| Private memory | 103.7 MiB | 14.9 MiB |
| Idle CPU (% of one core) | 0.50 | 0.00 |
| Windows shared GPU memory counter | 193.3 MiB | No process counter instance |

Windows also reported no dedicated GPU memory counter instance for the software build. The table covers a stationary window with one document. In a separate single run per build with two tabs, matching window sizes, and 30 wheel events at 100 ms intervals, scroll CPU was 5.1% of one core for WGPU and 3.6% for software. Other documents and memory attributed to the desktop compositor require separate measurements.

## Previous WGPU benchmark

Measurements from November 18, 2026 on Windows 11 build 26300 with an Intel Core i5-1155G7 and Intel Iris Xe. Chrome was version 157.0.8103.80 and Brave was 157.1.99.57.

### Stationary process measurements

Each row is the median of three fresh process launches with temporary profiles. The sample starts five seconds after the main window appears and lasts approximately three seconds. Startup measures process launch to a main window handle. TinyPDF separately measures time from its Rust entry point to publishing the first visible PDF tile. A short, stationary sample can still contain document loading and browser background activity.

Working set sums the resident pages attributed to each process in the reader's process tree, including repeated accounting of shared pages. Private memory reports private committed bytes. Windows GPU process counters reported zero dedicated bytes for these runs. Shared GPU memory is listed separately because it can overlap other accounting domains.

| Reader and workload | Window (ms) | First tile (ms) | Working set (MiB) | Sampled peak (MiB) | Private (MiB) | Shared GPU (MiB) | CPU (% of one core) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| TinyPDF default, empty | 111.8 | — | 160.0 | 160.4 | 90.7 | 185.0 | 0.00 |
| TinyPDF default, 501 pages | 58.2 | 2,389.9 | 167.8 | 167.8 | 96.9 | 185.2 | 0.00 |
| TinyPDF default, USGS | 68.1 | 2,217.9 | 166.9 | 169.1 | 97.9 | 196.4 | 0.98 |
| TinyPDF default, synthetic scan | 73.8 | 3,078.9 | 173.3 | 173.3 | 102.7 | 196.4 | 0.49 |
| Chrome, 501 pages | 466.9 | — | 822.4 | 822.4 | 421.4 | 39.8 | 0.49 |
| Brave, 501 pages | 663.0 | — | 666.1 | 666.1 | 351.8 | 41.0 | 16.68 |
| Chrome, USGS | 827.7 | — | 1,046.4 | 1,046.4 | 678.2 | 54.6 | 52.25 |
| Brave, USGS | 553.1 | — | 934.8 | 934.8 | 647.0 | 73.0 | 71.20 |

TinyPDF used one process in each sample. Its process working set was lower than both browsers on the matched documents, while shared GPU accounting was higher. The application remains above the provisional RAM targets. These warm launches establish window availability below one second on this host. The benchmark covered neither cold boots nor GPU evictions.

The generated 501-page text PDF is 186,906 bytes. The synthetic scan is 119,288,725 bytes. The USGS Circular 1296 PDF is 112,181,765 bytes. [USGS Circular 1296](https://pubs.usgs.gov/circ/2006/1296/)
## 0.2.0 hardening comparison

Measured on November 20, 2026 on Windows 11 build 26300, Intel Core i5-1155G7, with Rust 1.98.0 and the software renderer. Baseline was main at `cedbdd9`. The updated build was the 0.2.0 x64 installer payload. Each comparison below used one foreground run per build, a fresh profile, light theme, and the same 1024 × 720 window. Treat timing differences as observations from these runs.

The test sent 200 wheel events with seven reversals per workload, except the 501-page fixture, which received 400 events and 15 reversals. The starting and recovered page crops matched byte-for-byte in all eight compared runs. Recovery time measures from UI Automation observing page one again until the matching capture. It includes capture and polling overhead.

| Workload | Peak working set MiB, baseline → updated | Peak private commit MiB, baseline → updated | Private commit after stress MiB, baseline → updated | Scroll CPU seconds, baseline → updated |
|---|---:|---:|---:|---:|
| 3-page generated text, 1,408 bytes | 38.88 → 36.22 | 19.87 → 16.60 | 14.16 → 14.54 | 0.19 → 0.23 |
| 501-page generated text, 186,906 bytes | 40.85 → 40.91 | 19.05 → 20.70 | 17.18 → 16.27 | 2.28 → 1.89 |
| 13-page image scan, 119,288,725 bytes | 69.39 → 77.67 | 47.63 → 55.86 | 30.68 → 21.90 | 0.38 → 0.42 |
| USGS Circular 1296, 112,181,765 bytes | 85.82 → 83.11 | 63.44 → 60.72 | 37.71 → 33.38 | 2.06 → 1.98 |

The empty reader measured 7.43–7.53 MiB private commit across baseline runs and 7.45–7.50 MiB across updated runs. Empty-reader working set was approximately 27.7 MiB after warmup. The historical 30 MB scrolling peak did not reproduce as the total working set on these workloads. The scan run used less private commit after stress and more at its peak.

These counters cover the TinyPDF process, including Slint, the render worker, and PDFium. Working set includes resident private and shared pages. Private commit measures committed private memory. Peak counters cover the process lifetime. CPU time sums the reader's kernel and user time during wheel injection and checkpoint validation. The figures exclude the Python driver, desktop compositor, and separate GPU allocations. [Windows process memory counters](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex2)

Run in an interactive desktop using Python with `Pillow`, `pywinauto`, and `pywin32` installed in a test environment. Use the same fixture, zoom, window size, and Windows display scale for both builds. Keep the reader in the foreground.

```powershell
./scripts/new-benchmark-pdf.ps1 -Output ./target/benchmark-501.pdf -Pages 501
python ./scripts/stress-windows.py ./target/release/tinypdf.exe ./target/benchmark-501.pdf
```
