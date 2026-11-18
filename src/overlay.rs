//! Desktop rectangle selection overlay and a click-through target outline.
//!
//! Both windows use physical desktop pixels (the process is Per-Monitor DPI
//! Aware V2). Selection completes asynchronously: the owner receives
//! `WM_SELECTION_DONE` and calls `take_result`, so no nested message loop runs
//! while GUI state is borrowed.
use crate::{experiment::wide, hid::winerr, mapping::rect_from_drag};
use std::{
    cell::RefCell,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

pub const WM_SELECTION_DONE: u32 = WM_APP + 1;
const SELECT_CLASS: &str = "Pad2TouchSelectionOverlay";
const OUTLINE_CLASS: &str = "Pad2TouchTargetOutline";
const OUTLINE_TIMER: usize = 1;
const OUTLINE_WIDTH: i32 = 4;

struct Selection {
    hwnd: HWND,
    owner: HWND,
    origin: POINT,
    prompt: String,
    anchor: Option<POINT>,
    current: POINT,
}

thread_local! {
    static SELECTION: RefCell<Option<Selection>> = const { RefCell::new(None) };
    static RESULT: RefCell<Option<Option<RECT>>> = const { RefCell::new(None) };
    static OUTLINE: RefCell<HWND> = const { RefCell::new(null_mut()) };
}

unsafe fn register(
    class: &str,
    proc: WNDPROC,
    cursor: windows_sys::core::PCWSTR,
) -> Result<(), String> {
    let name = wide(class);
    let wc = WNDCLASSW {
        lpfnWndProc: proc,
        hInstance: GetModuleHandleW(null()),
        lpszClassName: name.as_ptr(),
        hCursor: LoadCursorW(null_mut(), cursor),
        ..Default::default()
    };
    if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
        return Err(winerr("RegisterClassW(overlay)"));
    }
    Ok(())
}

pub fn active() -> bool {
    SELECTION.with(|s| s.borrow().is_some())
}

/// Begin selecting a rectangle inside `bounds` (physical screen pixels).
pub fn begin(owner: HWND, bounds: RECT, prompt: &str) -> Result<HWND, String> {
    if active() {
        return Err("A selection is already in progress.".into());
    }
    hide_outline();
    unsafe {
        register(SELECT_CLASS, Some(select_proc), IDC_CROSS)?;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            wide(SELECT_CLASS).as_ptr(),
            wide("Pad2Touch area selection").as_ptr(),
            WS_POPUP,
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            owner,
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if hwnd.is_null() {
            return Err(winerr("CreateWindowExW(selection overlay)"));
        }
        SetLayeredWindowAttributes(hwnd, 0, 120, LWA_ALPHA);
        RESULT.with(|r| r.borrow_mut().take());
        SELECTION.with(|s| {
            *s.borrow_mut() = Some(Selection {
                hwnd,
                owner,
                origin: POINT {
                    x: bounds.left,
                    y: bounds.top,
                },
                prompt: prompt.into(),
                anchor: None,
                current: POINT::default(),
            })
        });
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        Ok(hwnd)
    }
}

/// Selected rectangle (`Some(None)` = cancelled), available after WM_SELECTION_DONE.
pub fn take_result() -> Option<Option<RECT>> {
    RESULT.with(|r| r.borrow_mut().take())
}

pub fn cancel() {
    finish(None, true);
}

fn finish(result: Option<RECT>, destroy: bool) {
    // try_with: may run from a destructor during thread teardown.
    let Some(selection) = SELECTION.try_with(|s| s.borrow_mut().take()).ok().flatten() else {
        return;
    };
    let _ = RESULT.try_with(|r| *r.borrow_mut() = Some(result));
    unsafe {
        if GetCapture() == selection.hwnd {
            ReleaseCapture();
        }
        if destroy {
            DestroyWindow(selection.hwnd);
        }
        PostMessageW(selection.owner, WM_SELECTION_DONE, 0, 0);
    }
}

fn screen_point(l: LPARAM, origin: POINT) -> POINT {
    POINT {
        x: origin.x + (l & 0xffff) as u16 as i16 as i32,
        y: origin.y + ((l >> 16) & 0xffff) as u16 as i16 as i32,
    }
}

unsafe extern "system" fn select_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            SELECTION.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    let p = screen_point(l, s.origin);
                    s.anchor = Some(p);
                    s.current = p;
                }
            });
            SetCapture(hwnd);
            InvalidateRect(hwnd, null(), 0);
            return 0;
        }
        WM_MOUSEMOVE => {
            SELECTION.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    if s.anchor.is_some() {
                        s.current = screen_point(l, s.origin);
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
            });
            return 0;
        }
        WM_LBUTTONUP => {
            let rect = SELECTION.with(|s| {
                s.borrow().as_ref().and_then(|s| {
                    s.anchor
                        .map(|a| rect_from_drag(a, screen_point(l, s.origin)))
                })
            });
            if let Some(rect) = rect {
                finish(Some(rect), true);
            }
            return 0;
        }
        WM_RBUTTONDOWN => {
            finish(None, true);
            return 0;
        }
        WM_KEYDOWN if w == VK_ESCAPE as usize => {
            finish(None, true);
            return 0;
        }
        WM_CAPTURECHANGED => {
            // Another window took the mouse mid-drag: cancel instead of guessing.
            let dragging = SELECTION.with(|s| {
                s.borrow()
                    .as_ref()
                    .is_some_and(|s| s.hwnd == hwnd && s.anchor.is_some())
            });
            if dragging && l as HWND != hwnd {
                finish(None, true);
            }
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            FillRect(dc, &rc, GetStockObject(BLACK_BRUSH) as HBRUSH);
            let (prompt, selected) = SELECTION.with(|s| {
                s.borrow()
                    .as_ref()
                    .map(|s| {
                        (
                            s.prompt.clone(),
                            s.anchor.map(|a| {
                                let r = rect_from_drag(a, s.current);
                                RECT {
                                    left: r.left - s.origin.x,
                                    top: r.top - s.origin.y,
                                    right: r.right - s.origin.x,
                                    bottom: r.bottom - s.origin.y,
                                }
                            }),
                        )
                    })
                    .unwrap_or_default()
            });
            if let Some(r) = selected {
                FillRect(dc, &r, GetStockObject(WHITE_BRUSH) as HBRUSH);
            }
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, 0x00FFFFFF);
            let mut text_rect = RECT {
                top: rc.top + 24,
                ..rc
            };
            let text = wide(&prompt);
            DrawTextW(
                dc,
                text.as_ptr(),
                -1,
                &mut text_rect,
                DT_CENTER | DT_TOP | DT_NOPREFIX | DT_WORDBREAK,
            );
            EndPaint(hwnd, &ps);
            return 0;
        }
        WM_DESTROY => {
            // Destroyed externally (e.g. owner closed): report cancellation.
            let ours = SELECTION.with(|s| s.borrow().as_ref().is_some_and(|s| s.hwnd == hwnd));
            if ours {
                finish(None, false);
            }
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}

/// Outline drawn just outside `bounds` so it never covers the mapped pixels.
pub fn outline_rect(bounds: RECT) -> RECT {
    RECT {
        left: bounds.left - OUTLINE_WIDTH,
        top: bounds.top - OUTLINE_WIDTH,
        right: bounds.right + OUTLINE_WIDTH,
        bottom: bounds.bottom + OUTLINE_WIDTH,
    }
}

/// Show a click-through outline around `bounds` for `millis` milliseconds.
pub fn flash(bounds: RECT, millis: u32) -> Result<(), String> {
    hide_outline();
    unsafe {
        register(OUTLINE_CLASS, Some(outline_proc), IDC_ARROW)?;
        let outer = outline_rect(bounds);
        let (w, h) = (outer.right - outer.left, outer.bottom - outer.top);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            wide(OUTLINE_CLASS).as_ptr(),
            wide("Pad2Touch target outline").as_ptr(),
            WS_POPUP,
            outer.left,
            outer.top,
            w,
            h,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if hwnd.is_null() {
            return Err(winerr("CreateWindowExW(target outline)"));
        }
        SetLayeredWindowAttributes(hwnd, 0, 230, LWA_ALPHA);
        let frame = CreateRectRgn(0, 0, w, h);
        let hole = CreateRectRgn(
            OUTLINE_WIDTH,
            OUTLINE_WIDTH,
            w - OUTLINE_WIDTH,
            h - OUTLINE_WIDTH,
        );
        CombineRgn(frame, frame, hole, RGN_DIFF);
        DeleteObject(hole);
        // The system owns the region after a successful SetWindowRgn.
        if SetWindowRgn(hwnd, frame, 0) == 0 {
            DeleteObject(frame);
        }
        SetTimer(hwnd, OUTLINE_TIMER, millis, None);
        OUTLINE.with(|o| *o.borrow_mut() = hwnd);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

pub fn hide_outline() {
    let hwnd = OUTLINE
        .try_with(|o| std::mem::replace(&mut *o.borrow_mut(), null_mut()))
        .unwrap_or(null_mut());
    if !hwnd.is_null() {
        unsafe { DestroyWindow(hwnd) };
    }
}

#[cfg(test)]
pub fn outline_visible() -> bool {
    OUTLINE.with(|o| !o.borrow().is_null())
}

unsafe extern "system" fn outline_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => return HTTRANSPARENT as LRESULT,
        WM_TIMER => {
            KillTimer(hwnd, OUTLINE_TIMER);
            let ours = OUTLINE.with(|o| *o.borrow() == hwnd);
            if ours {
                hide_outline();
            } else {
                DestroyWindow(hwnd);
            }
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(0x002040E0);
            FillRect(dc, &rc, brush);
            DeleteObject(brush);
            EndPaint(hwnd, &ps);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::tuple;
    #[test]
    fn mouse_positions_are_signed_and_offset_by_overlay_origin() {
        let origin = POINT { x: -2560, y: -300 };
        let pack = |x: i16, y: i16| ((y as u16 as isize) << 16 | x as u16 as isize) as LPARAM;
        let p = screen_point(pack(10, 20), origin);
        assert_eq!((p.x, p.y), (-2550, -280));
        // Captured drags beyond the overlay report negative client coordinates.
        let p = screen_point(pack(-5, -7), origin);
        assert_eq!((p.x, p.y), (-2565, -307));
    }
    #[test]
    fn outline_surrounds_bounds_without_covering_them() {
        let o = outline_rect(RECT {
            left: -100,
            top: 10,
            right: 200,
            bottom: 300,
        });
        assert_eq!(tuple(o), (-104, 6, 204, 304));
    }

    /// Drives the real overlay window with synthetic window messages only;
    /// no physical mouse input, capture session or touch injection.
    #[test]
    #[ignore = "creates native overlay windows; run on an interactive Windows desktop"]
    fn native_selection_overlay_reports_physical_rectangle_and_cancel() {
        use windows_sys::Win32::UI::HiDpi::*;
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let owner = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("owner").as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!owner.is_null());
            let screen = crate::mapping::virtual_screen();
            let bounds = RECT {
                left: screen.left + 50,
                top: screen.top + 60,
                right: screen.left + 650,
                bottom: screen.top + 460,
            };
            let overlay = begin(owner, bounds, "test selection").unwrap();
            assert!(begin(owner, bounds, "second").is_err());
            let pack = |x: i32, y: i32| ((y as isize) << 16 | x as isize) as LPARAM;
            SendMessageW(overlay, WM_LBUTTONDOWN, 0x0001, pack(300, 200));
            SendMessageW(overlay, WM_MOUSEMOVE, 0x0001, pack(100, 50));
            SendMessageW(overlay, WM_LBUTTONUP, 0, pack(100, 50));
            assert!(!active());
            assert_eq!(IsWindow(overlay), 0);
            let r = take_result().unwrap().unwrap();
            assert_eq!(
                tuple(r),
                (
                    bounds.left + 100,
                    bounds.top + 50,
                    bounds.left + 301,
                    bounds.top + 201
                )
            );
            let mut msg = MSG::default();
            assert_ne!(
                PeekMessageW(
                    &mut msg,
                    owner,
                    WM_SELECTION_DONE,
                    WM_SELECTION_DONE,
                    PM_REMOVE
                ),
                0
            );
            let overlay = begin(owner, bounds, "cancel").unwrap();
            SendMessageW(overlay, WM_KEYDOWN, VK_ESCAPE as usize, 0);
            assert!(take_result().unwrap().is_none());
            let overlay = begin(owner, bounds, "destroy").unwrap();
            DestroyWindow(overlay);
            assert!(take_result().unwrap().is_none());
            flash(bounds, 60_000).unwrap();
            assert!(outline_visible());
            let outline = OUTLINE.with(|o| *o.borrow());
            let mut placed = RECT::default();
            GetWindowRect(outline, &mut placed);
            assert_eq!(tuple(placed), tuple(outline_rect(bounds)));
            // The outline must never become the hit-test owner of mapped pixels.
            let center = POINT {
                x: (bounds.left + bounds.right) / 2,
                y: (bounds.top + bounds.bottom) / 2,
            };
            assert_ne!(WindowFromPoint(center), outline);
            hide_outline();
            assert!(!outline_visible());
            assert_eq!(IsWindow(outline), 0);
            DestroyWindow(owner);
        }
    }
}
