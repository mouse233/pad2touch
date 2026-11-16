//! Reusable GDI back buffer. Only complete frames are copied to the window.
use crate::hid::winerr;
use std::ptr::null_mut;
use windows_sys::Win32::Graphics::Gdi::*;
pub struct BackBuffer {
    pub dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pub width: i32,
    pub height: i32,
}
impl BackBuffer {
    pub fn new(reference: HDC, width: i32, height: i32) -> Result<Self, String> {
        if width <= 0 || height <= 0 {
            return Err("invalid back-buffer size".into());
        }
        unsafe {
            let dc = CreateCompatibleDC(reference);
            if dc.is_null() {
                return Err(winerr("CreateCompatibleDC"));
            }
            let bitmap = CreateCompatibleBitmap(reference, width, height);
            if bitmap.is_null() {
                let error = winerr("CreateCompatibleBitmap");
                DeleteDC(dc);
                return Err(error);
            }
            let previous = SelectObject(dc, bitmap);
            if previous.is_null() || previous as isize == -1 {
                let error = winerr("SelectObject(back buffer)");
                DeleteObject(bitmap);
                DeleteDC(dc);
                return Err(error);
            }
            Ok(Self {
                dc,
                bitmap,
                previous,
                width,
                height,
            })
        }
    }
    pub fn present(&self, destination: HDC) -> Result<(), String> {
        if unsafe {
            BitBlt(
                destination,
                0,
                0,
                self.width,
                self.height,
                self.dc,
                0,
                0,
                SRCCOPY,
            )
        } == 0
        {
            Err(winerr("BitBlt"))
        } else {
            Ok(())
        }
    }
}
impl Drop for BackBuffer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
            self.dc = null_mut();
        }
    }
}
