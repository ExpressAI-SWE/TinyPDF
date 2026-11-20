/// Match the native caption to the reader theme on Windows versions supporting caption colors.
#[cfg(windows)]
pub fn set_caption_theme(window: &slint::Window, dark: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
    };

    let handle = window.window_handle();
    let Ok(handle) = handle.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let default = u32::MAX;
    let values = [
        (DWMWA_USE_IMMERSIVE_DARK_MODE, u32::from(dark)),
        // COLORREF stores RGB bytes in reverse order.
        (DWMWA_CAPTION_COLOR, if dark { 0x002a2522 } else { default }),
        (DWMWA_TEXT_COLOR, if dark { 0x00faf4ed } else { default }),
    ];
    for (attribute, value) in values {
        // Older Windows versions ignore unsupported caption attributes.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute as u32,
                (&raw const value).cast(),
                size_of::<u32>() as u32,
            );
        }
    }
}

#[cfg(windows)]
pub use printing::PrintJob;

#[cfg(windows)]
pub fn open_default_apps() -> Result<(), String> {
    use std::ptr::null;
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

    let uri: Vec<u16> = "ms-settings:defaultapps"
        .encode_utf16()
        .chain([0])
        .collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            null(),
            uri.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        return Err(format!(
            "Windows could not open Default apps (code {result})."
        ));
    }
    Ok(())
}

#[cfg(windows)]
mod printing {
    use crate::pdf::Document;
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ops::Range;
    use std::ptr;
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteDC, GetDeviceCaps, HDC,
        HORZRES, LOGPIXELSX, LOGPIXELSY, SRCCOPY, StretchDIBits, VERTRES,
    };
    use windows_sys::Win32::Storage::Xps::{
        AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, PD_NOSELECTION, PD_PAGENUMS, PD_PRINTTOFILE, PD_RETURNDC,
        PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW, PrintDlgW,
    };

    pub struct PrintJob {
        device: HDC,
        pages: Range<u32>,
        to_file: bool,
    }

    // A job owns its printer DC and only its current thread uses or releases it.
    unsafe impl Send for PrintJob {}

    impl Drop for PrintJob {
        fn drop(&mut self) {
            unsafe { DeleteDC(self.device) };
        }
    }

    impl PrintJob {
        pub fn choose(
            owner: Option<isize>,
            pages: u32,
            current: u32,
        ) -> Result<Option<Self>, String> {
            if pages == 0 {
                return Err("This document has no pages to print.".into());
            }
            // shortcut: custom ranges stop at 65,535, use PrintDlgExW if larger ranges are needed.
            let max_page = pages.min(u32::from(u16::MAX)) as u16;
            let current_page = current.saturating_add(1).clamp(1, u32::from(max_page)) as u16;
            let mut dialog = PRINTDLGW {
                lStructSize: size_of::<PRINTDLGW>() as u32,
                hwndOwner: owner.unwrap_or_default() as *mut c_void,
                Flags: PD_RETURNDC | PD_NOSELECTION | PD_USEDEVMODECOPIESANDCOLLATE,
                nMinPage: 1,
                nMaxPage: max_page,
                nFromPage: current_page,
                nToPage: current_page,
                nCopies: 1,
                ..Default::default()
            };
            // The dialog may return caller-owned resources even when it fails.
            let (selected, error) = unsafe {
                let selected = PrintDlgW(&mut dialog) != 0;
                let error = if selected { 0 } else { CommDlgExtendedError() };
                if !dialog.hDevMode.is_null() {
                    GlobalFree(dialog.hDevMode);
                }
                if !dialog.hDevNames.is_null() {
                    GlobalFree(dialog.hDevNames);
                }
                (selected, error)
            };
            if !selected {
                if !dialog.hDC.is_null() {
                    unsafe { DeleteDC(dialog.hDC) };
                }
                return if error == 0 {
                    Ok(None)
                } else {
                    Err(format!("The Windows print dialog failed (0x{error:04x})."))
                };
            }
            if dialog.hDC.is_null() {
                return Err("Windows returned no printer for this job.".into());
            }

            let pages = if dialog.Flags & PD_PAGENUMS != 0 {
                let start = u32::from(dialog.nFromPage.clamp(1, max_page) - 1);
                let end = u32::from(dialog.nToPage.clamp(1, max_page));
                start..end
            } else {
                0..pages
            };
            let job = Self {
                device: dialog.hDC,
                pages,
                to_file: dialog.Flags & PD_PRINTTOFILE != 0,
            };
            if job.pages.is_empty() {
                return Err("Choose a valid page range.".into());
            }
            Ok(Some(job))
        }

        pub fn print(
            self,
            document: &Document<'_>,
            title: &str,
            rotation: u8,
        ) -> Result<(), String> {
            if !document.can_print() {
                return Err("This PDF does not allow printing.".into());
            }
            let high_quality = document.can_print_high_quality();
            let draw_vector = high_quality && !document.has_form_fields();
            let raster_dpi = if high_quality { 300.0 } else { 150.0 };
            // These caps use the printable area and can report different X and Y resolutions.
            let width = unsafe { GetDeviceCaps(self.device, HORZRES as i32) };
            let height = unsafe { GetDeviceCaps(self.device, VERTRES as i32) };
            let dpi_x = unsafe { GetDeviceCaps(self.device, LOGPIXELSX as i32) };
            let dpi_y = unsafe { GetDeviceCaps(self.device, LOGPIXELSY as i32) };
            if [width, height, dpi_x, dpi_y]
                .into_iter()
                .any(|value| value <= 0)
            {
                return Err("The printer reported an invalid printable area.".into());
            }

            let title: Vec<u16> = title
                .encode_utf16()
                .filter(|unit| *unit != 0)
                .chain([0])
                .collect();
            let file = [
                b'F' as u16,
                b'I' as u16,
                b'L' as u16,
                b'E' as u16,
                b':' as u16,
                0,
            ];
            let info = DOCINFOW {
                cbSize: size_of::<DOCINFOW>() as i32,
                lpszDocName: title.as_ptr(),
                lpszOutput: if self.to_file {
                    file.as_ptr()
                } else {
                    ptr::null()
                },
                ..Default::default()
            };
            if unsafe { StartDocW(self.device, &info) } <= 0 {
                return Err("The printer could not start the job.".into());
            }

            let result = (|| {
                for page in self.pages.clone() {
                    let (mut page_width, mut page_height) =
                        document.page_size(page).map_err(|err| err.to_string())?;
                    if !rotation.is_multiple_of(2) {
                        std::mem::swap(&mut page_width, &mut page_height);
                    }
                    let area = fit_page(
                        [f64::from(page_width), f64::from(page_height)],
                        [width, height],
                        [dpi_x, dpi_y],
                    )?;
                    if unsafe { StartPage(self.device) } <= 0 {
                        return Err(format!("The printer could not start page {}.", page + 1));
                    }
                    if draw_vector {
                        // The job owns a valid printer DC until EndDoc or AbortDoc completes.
                        unsafe { document.print_page(page, self.device, area, rotation) }
                            .map_err(|err| err.to_string())?;
                    } else {
                        print_raster(
                            self.device,
                            document,
                            page,
                            rotation,
                            area,
                            [dpi_x, dpi_y],
                            raster_dpi,
                        )?;
                    }
                    if unsafe { EndPage(self.device) } <= 0 {
                        return Err(format!("The printer could not spool page {}.", page + 1));
                    }
                }
                if unsafe { EndDoc(self.device) } <= 0 {
                    return Err("The printer could not finish the job.".into());
                }
                Ok(())
            })();
            if result.is_err() {
                unsafe { AbortDoc(self.device) };
            }
            result
        }
    }

    fn print_raster(
        device: HDC,
        document: &Document<'_>,
        page: u32,
        rotation: u8,
        [x, y, width, height]: [i32; 4],
        [dpi_x, dpi_y]: [i32; 2],
        max_dpi: f64,
    ) -> Result<(), String> {
        const MAX_PIXELS: f64 = 16.0 * 1024.0 * 1024.0;
        let raster_width = (f64::from(width) * (max_dpi / f64::from(dpi_x)).min(1.0)).max(1.0);
        let raster_height = (f64::from(height) * (max_dpi / f64::from(dpi_y)).min(1.0)).max(1.0);
        let scale = (MAX_PIXELS / (raster_width * raster_height))
            .sqrt()
            .min(MAX_PIXELS / raster_width.max(raster_height))
            .min(1.0);
        let raster_width = (raster_width * scale).floor().max(1.0) as i32;
        let raster_height = (raster_height * scale).floor().max(1.0) as i32;
        let length = raster_width as usize * raster_height as usize * 4;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(length)
            .map_err(|_| "There is not enough memory to rasterize this print page.")?;
        pixels.resize(length, 0);
        document
            .render_rgba(
                page,
                raster_width as u32,
                raster_height as u32,
                rotation,
                &mut pixels,
                || false,
            )
            .map_err(|err| err.to_string())?;
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let bitmap = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: raster_width,
                biHeight: -raster_height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        // A negative height describes the PDFium bitmap's top-down row order.
        let copied = unsafe {
            StretchDIBits(
                device,
                x,
                y,
                width,
                height,
                0,
                0,
                raster_width,
                raster_height,
                pixels.as_ptr().cast(),
                &bitmap,
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
        if copied <= 0 {
            return Err(format!("The printer could not draw page {}.", page + 1));
        }
        Ok(())
    }

    fn fit_page(
        [page_width, page_height]: [f64; 2],
        [width, height]: [i32; 2],
        [dpi_x, dpi_y]: [i32; 2],
    ) -> Result<[i32; 4], String> {
        if !page_width.is_finite()
            || !page_height.is_finite()
            || page_width <= 0.0
            || page_height <= 0.0
        {
            return Err("A PDF page has invalid dimensions.".into());
        }
        let printer_width = page_width * f64::from(dpi_x);
        let printer_height = page_height * f64::from(dpi_y);
        let scale = (f64::from(width) / printer_width).min(f64::from(height) / printer_height);
        let drawn_width = ((printer_width * scale).round() as i32).clamp(1, width);
        let drawn_height = ((printer_height * scale).round() as i32).clamp(1, height);
        Ok([
            (width - drawn_width) / 2,
            (height - drawn_height) / 2,
            drawn_width,
            drawn_height,
        ])
    }

    #[cfg(test)]
    mod tests {
        use super::fit_page;

        #[test]
        fn print_area_preserves_physical_aspect_ratio() {
            assert_eq!(
                fit_page([612.0, 792.0], [2400, 3000], [600, 300]).unwrap(),
                [0, 723, 2400, 1553]
            );
            assert!(fit_page([0.0, 792.0], [2400, 3000], [600, 300]).is_err());
        }
    }
}
