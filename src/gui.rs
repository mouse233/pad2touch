//! Native Win32 shell; the diagnostic receiver is a child of the main window.
use crate::{
    control::{Controller, TakeoverMode},
    diagram,
    experiment::{self, wide, Session, Shared, Stats},
    hid::{self, winerr, Device},
    mapping::{self, MappingTarget, NormRect, WindowEntry},
    overlay,
};
use std::{
    cell::RefCell,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const DEVICE: usize = 101;
const REFRESH: usize = 102;
const MODE: usize = 103;
const KEY: usize = 104;
const CONTACTS: usize = 105;
const INJECT: usize = 106;
const GUARD: usize = 107;
const TARGET: usize = 108;
const START: usize = 109;
const STOP: usize = 110;
const HOTKEY: i32 = 1;
const EMERGENCY: i32 = 2;
const DIAGNOSTICS_GROUP: usize = 111;
const LOG_PATH: usize = 112;
const STATUS: usize = 113;
const STOP_HINT: usize = 114;
const WINDOW: usize = 115;
const AREA: usize = 116;
const TARGET_INFO: usize = 117;
const PAD_FIT: usize = 118;
const FEEDBACK: usize = 119;
const DEFAULT_CLIENT: (i32, i32) = (820, 618);
const PREVIEW_MS: u32 = 1500;
const PANEL_LEFT: i32 = 274;
const PANEL_TOP: i32 = 28;
/// Posted to the panel by the capture worker after each touchpad frame.
const WM_PANEL_FRAME: u32 = WM_APP + 2;
/// Set while a WM_PANEL_FRAME is queued; see `experiment::FrameSignal`.
static PANEL_FRAME_PENDING: AtomicBool = AtomicBool::new(false);
/// Fixed-size window: no sizing border and no maximize. The layout is designed
/// for DEFAULT_CLIENT and only rescales with DPI. WS_CLIPCHILDREN keeps the
/// dialog background from being painted under the controls.
const MAIN_STYLE: WINDOW_STYLE =
    WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;

#[derive(Clone, Copy)]
enum Anchor {
    Fixed,
    Diagnostics,
    Log,
    Hint,
}

struct Control {
    hwnd: HWND,
    group: bool,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    anchor: Anchor,
}
struct Gui {
    hwnd: HWND,
    panel: HWND,
    controls: Vec<Control>,
    status: HWND,
    log_path: HWND,
    font: HFONT,
    dpi: u32,
    devices: Vec<Device>,
    geometry: Option<crate::device_info::Geometry>,
    controller: Controller,
    hotkey_registered: bool,
    emergency_registered: bool,
    session: Option<Session>,
    shared: Shared,
    rect: RECT,
    buffer: Option<crate::render::BackBuffer>,
    closing: bool,
    restart_blocked: bool,
    last_status: String,
    windows: Vec<WindowEntry>,
    screen_rect: Option<RECT>,
    window_area: Option<NormRect>,
    pending_selection: Option<Pending>,
    session_diagnostic: bool,
    monitors: Vec<mapping::MonitorEntry>,
    choices: Vec<Choice>,
    thumbnail: Option<diagram::Thumbnail>,
    /// When the next desktop thumbnail may be captured (None: keep current).
    thumbnail_due: Option<Instant>,
    thumbnail_captures: u64,
    /// Idle preview of the mapped region, or why there is none.
    /// (mapped bounds, selected area before fitting, used touchpad part).
    preview: Result<(RECT, RECT, NormRect), String>,
    preview_at: Instant,
}

/// Screen area choices, Wacom-style: presets first, then custom portions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Desktop,
    Monitor(usize),
    ScreenPortion,
    /// A window's whole client area, or a dragged part of it.
    Window,
    /// PadBridge's own receiver, for checking touch delivery.
    Diagnostic,
}
impl Choice {
    fn uses_window(self) -> bool {
        self == Self::Window
    }
}
/// Whole screen(s) first, then a portion, a window, and the test area last.
/// A single monitor is listed once (it is the whole desktop).
fn choices(monitors: usize) -> Vec<Choice> {
    let mut list = match monitors {
        0 => vec![Choice::Desktop],
        1 => vec![Choice::Monitor(0)],
        n => std::iter::once(Choice::Desktop)
            .chain((0..n).map(Choice::Monitor))
            .collect(),
    };
    list.extend([Choice::ScreenPortion, Choice::Window, Choice::Diagnostic]);
    list
}
fn choice_label(choice: Choice, monitors: &[mapping::MonitorEntry]) -> String {
    match choice {
        Choice::Desktop => "Full desktop".into(),
        Choice::Monitor(i) => match monitors.get(i) {
            Some(m) => {
                let (w, h) = mapping::size(m.rect);
                if monitors.len() == 1 {
                    format!("Screen · {w}×{h}")
                } else {
                    format!(
                        "Monitor {} · {w}×{h}{}",
                        i + 1,
                        if m.primary { " (main)" } else { "" }
                    )
                }
            }
            None => format!("Monitor {}", i + 1),
        },
        Choice::ScreenPortion => "Portion of screen…".into(),
        Choice::Window => "Application window".into(),
        Choice::Diagnostic => "Test area (inside PadBridge)".into(),
    }
}
/// What an in-progress overlay selection will configure.
#[derive(Clone, Copy)]
enum Pending {
    Screen,
    WindowArea { handle: usize, client: RECT },
}
thread_local! { static GUI: RefCell<Option<Gui>> = const { RefCell::new(None) }; }

unsafe fn outer_size(width: i32, height: i32, dpi: u32) -> POINT {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width * dpi as i32 / 96,
        bottom: height * dpi as i32 / 96,
    };
    AdjustWindowRectExForDpi(&mut rect, MAIN_STYLE, 0, 0, dpi);
    POINT {
        x: rect.right - rect.left,
        y: rect.bottom - rect.top,
    }
}

/// The main window registers without CS_HREDRAW/CS_VREDRAW: `layout` invalidates
/// only what moved, so fixed controls are not erased and repainted on every
/// resize step. The diagnostic panel keeps them; it paints through a back buffer.
unsafe fn register(class: &str, proc: WNDPROC, style: WNDCLASS_STYLES) -> Result<(), String> {
    let name = wide(class);
    let wc = WNDCLASSW {
        style,
        lpfnWndProc: proc,
        hInstance: GetModuleHandleW(null()),
        lpszClassName: name.as_ptr(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_BTNFACE + 1) as HBRUSH,
        ..Default::default()
    };
    if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
        return Err(winerr("RegisterClassW(GUI)"));
    }
    Ok(())
}

pub fn run() -> Result<(), String> {
    // Detach this process only; command-line capture and verification keep stdout.
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
    let result = run_window();
    if let Err(error) = &result {
        unsafe {
            MessageBoxW(
                null_mut(),
                wide(error).as_ptr(),
                wide("PadBridge could not start").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    result
}

fn run_window() -> Result<(), String> {
    experiment::EXIT.store(false, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            return Err(winerr("SetProcessDpiAwarenessContext(GUI)"));
        }
        register("PadBridgeGui", Some(window_proc), 0)?;
        register(
            "PadBridgeDiagnosticPanel",
            Some(panel_proc),
            CS_HREDRAW | CS_VREDRAW,
        )?;
        let hwnd = CreateWindowExW(
            0,
            wide("PadBridgeGui").as_ptr(),
            wide("PadBridge Control Panel").as_ptr(),
            MAIN_STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            860,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if hwnd.is_null() {
            return Err(winerr("CreateWindowExW(GUI)"));
        }
        let result = Gui::new(hwnd);
        let gui = match result {
            Ok(gui) => gui,
            Err(e) => {
                DestroyWindow(hwnd);
                return Err(e);
            }
        };
        GUI.with(|slot| *slot.borrow_mut() = Some(gui));
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let gui = slot.as_mut().unwrap();
            gui.refresh_devices();
            gui.layout();
            gui.update_controls();
        });
        if SetTimer(hwnd, 1, 30, None) == 0 {
            DestroyWindow(hwnd);
            return Err(winerr("SetTimer(GUI)"));
        }
        ShowWindow(hwnd, SW_SHOW);
        // A launcher may set STARTUPINFO.wShowWindow=SW_HIDE for the console.
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        let mut msg = MSG::default();
        let mut result = Ok(());
        loop {
            let status = GetMessageW(&mut msg, null_mut(), 0, 0);
            if status == 0 {
                break;
            }
            if status == -1 {
                result = Err(winerr("GetMessageW(GUI)"));
                break;
            }
            // Esc works even while a native child control has keyboard focus. With no
            // focus window, Windows delivers keys to the active window as WM_SYSKEYDOWN.
            if is_plain_escape(&msg) {
                GUI.with(|slot| {
                    if let Some(g) = slot.borrow_mut().as_mut() {
                        g.disarm("Emergency stop requested.");
                    }
                });
                continue;
            }
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        GUI.with(|slot| {
            slot.borrow_mut().take();
        });
        result
    }
}

impl Gui {
    unsafe fn new(hwnd: HWND) -> Result<Self, String> {
        // Flicker is an intermediate paint state reaching the screen: the dialog
        // background erased in one paint, the control drawn over it in a later
        // one. The background is therefore never painted under child windows;
        // every child paints its whole rectangle itself (group boxes included,
        // see `group_proc`). WS_EX_COMPOSITED is not an alternative: with DWM
        // it leaves controls blank while the window changes size.
        // Applied here as well as at creation so every caller gets the fixed frame.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            ((style & !(WS_THICKFRAME | WS_MAXIMIZEBOX)) | MAIN_STYLE) as isize,
        );
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut g = Self {
            hwnd,
            panel: null_mut(),
            controls: Vec::new(),
            status: null_mut(),
            log_path: null_mut(),
            font: null_mut(),
            dpi,
            devices: Vec::new(),
            geometry: None,
            controller: Controller::default(),
            hotkey_registered: false,
            emergency_registered: false,
            session: None,
            shared: Arc::new(Mutex::new(Stats {
                capacity: 5,
                mode: "capture".into(),
                pinch_ratio: 1.0,
                capture_stopped: true,
                ..Default::default()
            })),
            rect: RECT::default(),
            buffer: None,
            closing: false,
            restart_blocked: false,
            last_status: "Choose a device to get started.".into(),
            windows: Vec::new(),
            screen_rect: None,
            window_area: None,
            pending_selection: None,
            session_diagnostic: true,
            monitors: Vec::new(),
            choices: Vec::new(),
            thumbnail: None,
            thumbnail_due: Some(Instant::now()),
            thumbnail_captures: 0,
            preview: Err(String::new()),
            preview_at: Instant::now(),
        };
        g.add(
            "BUTTON",
            "Input device",
            0,
            BS_GROUPBOX as u32,
            [8, 8, 250, 84],
        )?;
        g.add(
            "COMBOBOX",
            "",
            DEVICE,
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP,
            [18, 28, 166, 180],
        )?;
        g.add("BUTTON", "Refresh", REFRESH, WS_TABSTOP, [190, 27, 58, 25])?;
        g.add("STATIC", "Contact capacity:", 0, 0, [18, 65, 126, 16])?;
        g.add(
            "COMBOBOX",
            "",
            CONTACTS,
            CBS_DROPDOWNLIST as u32 | WS_TABSTOP,
            [154, 59, 94, 160],
        )?;
        g.fill_combo(
            CONTACTS,
            &[
                "1 touch",
                "2 touches",
                "3 touches",
                "4 touches",
                "5 touches",
            ],
        );
        SendMessageW(g.item(CONTACTS), CB_SETCURSEL, 4, 0);
        g.add(
            "BUTTON",
            "Capture activation",
            0,
            BS_GROUPBOX as u32,
            [8, 100, 250, 154],
        )?;
        g.add(
            "COMBOBOX",
            "",
            MODE,
            CBS_DROPDOWNLIST as u32 | WS_TABSTOP,
            [18, 120, 230, 160],
        )?;
        g.fill_combo(
            MODE,
            &[
                "Manual start / stop",
                "Toggle with shortcut",
                "Hold shortcut to capture",
            ],
        );
        g.add("STATIC", "Global shortcut:", 0, 0, [18, 159, 100, 16])?;
        g.add(
            "COMBOBOX",
            "",
            KEY,
            CBS_DROPDOWNLIST as u32 | WS_TABSTOP,
            [116, 153, 132, 160],
        )?;
        g.fill_combo(
            KEY,
            &[
                "Ctrl + Alt + F8",
                "Ctrl + Alt + F9",
                "Ctrl + Alt + F10",
                "Ctrl + Alt + F11",
            ],
        );
        g.add(
            "BUTTON",
            "Inject native touch input",
            INJECT,
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            [18, 194, 230, 20],
        )?;
        SendMessageW(g.item(INJECT), BM_SETCHECK, 1, 0);
        // Off by default: injected touches draw no Windows touch visuals.
        g.add(
            "BUTTON",
            "Show Windows touch feedback",
            FEEDBACK,
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            [34, 216, 214, 20],
        )?;
        g.add(
            "BUTTON",
            "Windows system gestures",
            0,
            BS_GROUPBOX as u32,
            [8, 262, 250, 82],
        )?;
        g.add(
            "COMBOBOX",
            "",
            GUARD,
            CBS_DROPDOWNLIST as u32 | WS_TABSTOP,
            [18, 282, 230, 160],
        )?;
        g.fill_combo(
            GUARD,
            &[
                "Keep system gestures",
                "Foreground guard",
                "Background guard (experimental)",
            ],
        );
        g.add(
            "STATIC",
            "Background: 120 s, restores on stop.",
            0,
            0,
            [18, 319, 230, 16],
        )?;
        g.add(
            "BUTTON",
            "Screen area",
            0,
            BS_GROUPBOX as u32,
            [8, 352, 250, 142],
        )?;
        g.add(
            "COMBOBOX",
            "",
            TARGET,
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP,
            [18, 372, 230, 240],
        )?;
        g.fill_targets();
        g.add(
            "COMBOBOX",
            "",
            WINDOW,
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP,
            [18, 402, 230, 240],
        )?;
        g.add(
            "BUTTON",
            "Select area…",
            AREA,
            WS_TABSTOP,
            [18, 431, 96, 25],
        )?;
        g.add("STATIC", "Touchpad:", 0, 0, [18, 467, 64, 16])?;
        g.add(
            "COMBOBOX",
            "",
            PAD_FIT,
            CBS_DROPDOWNLIST as u32 | WS_TABSTOP,
            [84, 461, 164, 160],
        )?;
        g.fill_combo(
            PAD_FIT,
            &[
                "Whole pad · keep shape",
                "Whole pad · stretch",
                "Part of pad · fill area",
            ],
        );
        SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, 0, 0);
        g.add("STATIC", "", TARGET_INFO, 0x4000, [120, 436, 128, 16])?;
        g.add(
            "BUTTON",
            "Start capture",
            START,
            BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
            [8, 502, 122, 26],
        )?;
        g.add(
            "BUTTON",
            "Stop / disarm",
            STOP,
            WS_TABSTOP,
            [136, 502, 122, 26],
        )?;
        // Four lines: the state plus up to three wrapped lines of detail.
        g.status = g.add("STATIC", "", STATUS, 0, [8, 538, 250, 64])?;
        let emergency_label = g.add(
            "STATIC",
            "Emergency stop: Ctrl + Alt + Esc (or Esc here)",
            STOP_HINT,
            0,
            [PANEL_LEFT, 0, 0, 16],
        )?;
        g.log_path = g.add(
            "EDIT",
            "Logs: no session started",
            LOG_PATH,
            ES_READONLY as u32 | ES_AUTOHSCROLL as u32 | WS_TABSTOP,
            [PANEL_LEFT, 0, 0, 22],
        )?;
        g.add(
            "BUTTON",
            "Diagnostics / touch receiver",
            DIAGNOSTICS_GROUP,
            BS_GROUPBOX as u32,
            [266, 8, 0, 0],
        )?;
        g.panel = CreateWindowExW(
            0,
            wide("PadBridgeDiagnosticPanel").as_ptr(),
            wide("Touch receiver").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            0,
            0,
            1,
            1,
            hwnd,
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if g.panel.is_null() {
            return Err(winerr("CreateWindowExW(diagnostic panel)"));
        }
        g.emergency_registered = RegisterHotKey(
            hwnd,
            EMERGENCY,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            VK_ESCAPE as u32,
        ) != 0;
        if !g.emergency_registered {
            set_text(
                emergency_label,
                "Global emergency shortcut unavailable; use Stop / disarm or Esc in this window.",
            );
        }
        // Group boxes are opaque (`group_proc`) and must stay below every
        // control and the panel: WS_CLIPSIBLINGS then clips those out of the
        // group box's own paint, so it never draws over them.
        for c in g.controls.iter().filter(|c| c.group) {
            SetWindowPos(
                c.hwnd,
                HWND_BOTTOM,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOREDRAW,
            );
        }
        g.set_font();
        let mut work = RECT::default();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
        // The window cannot be resized, so it is never made smaller than its
        // layout; SWP_FRAMECHANGED applies the fixed frame set above.
        let size = outer_size(DEFAULT_CLIENT.0, DEFAULT_CLIENT.1, g.dpi);
        SetWindowPos(
            hwnd,
            null_mut(),
            work.left + 20,
            work.top + 20,
            size.x,
            size.y,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        Ok(g)
    }
    fn px(&self, n: i32) -> i32 {
        ((n as i64 * self.dpi as i64) / 96) as i32
    }
    unsafe fn add(
        &mut self,
        class: &str,
        text: &str,
        id: usize,
        style: u32,
        bounds: [i32; 4],
    ) -> Result<HWND, String> {
        let group = class == "BUTTON" && style & 0xF == BS_GROUPBOX as u32;
        let hwnd = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | style,
            0,
            0,
            1,
            1,
            self.hwnd,
            id as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        if hwnd.is_null() {
            return Err(winerr("CreateWindowExW(control)"));
        }
        if group {
            let original = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, group_proc as *const () as isize);
            GROUP_PROC.store(original as usize, Ordering::Relaxed);
        }
        self.controls.push(Control {
            group,
            hwnd,
            x: bounds[0],
            y: bounds[1],
            width: bounds[2],
            height: bounds[3],
            anchor: match id {
                DIAGNOSTICS_GROUP => Anchor::Diagnostics,
                LOG_PATH => Anchor::Log,
                STOP_HINT => Anchor::Hint,
                _ => Anchor::Fixed,
            },
        });
        Ok(hwnd)
    }
    unsafe fn item(&self, id: usize) -> HWND {
        GetDlgItem(self.hwnd, id as i32)
    }
    unsafe fn selection(&self, id: usize) -> usize {
        SendMessageW(self.item(id), CB_GETCURSEL, 0, 0).max(0) as usize
    }
    unsafe fn checked(&self, id: usize) -> bool {
        SendMessageW(self.item(id), BM_GETCHECK, 0, 0) == 1
    }
    unsafe fn fill_combo(&self, id: usize, items: &[&str]) {
        for label in items {
            SendMessageW(
                self.item(id),
                CB_ADDSTRING,
                0,
                wide(label).as_ptr() as LPARAM,
            );
        }
        SendMessageW(self.item(id), CB_SETCURSEL, 0, 0);
    }
    unsafe fn set_font(&mut self) {
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let font = if SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
            0,
            self.dpi,
        ) != 0
        {
            CreateFontIndirectW(&metrics.lfMessageFont)
        } else {
            CreateFontW(
                -self.px(12),
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                wide("Segoe UI").as_ptr(),
            )
        };
        if font.is_null() {
            return;
        }
        for c in &self.controls {
            SendMessageW(c.hwnd, WM_SETFONT, font as WPARAM, 0);
        }
        if !self.font.is_null() {
            DeleteObject(self.font);
        }
        self.font = font;
    }
    unsafe fn refresh_preview_rect(&mut self) {
        // This is only the displayed canvas. Session keeps its own immutable target.
        let diagnostic = match &self.session {
            Some(_) => self.session_diagnostic,
            None => self.choice() == Choice::Diagnostic,
        };
        if !diagnostic {
            self.rect = RECT::default();
        } else if let Some(target) = self
            .aspect()
            .map(Some)
            // Without geometry only Stretch can map the canvas.
            .or((!self.pad_fit().needs_shape()).then_some(None))
            .and_then(|aspect| {
                MappingTarget::Diagnostics
                    .resolve_fitted(self.panel, aspect, self.pad_fit())
                    .ok()
            })
        {
            self.rect = target.bounds;
        } else {
            self.rect = RECT::default();
        }
        InvalidateRect(self.panel, null(), 0);
    }
    unsafe fn layout(&mut self) {
        let mut rc = RECT::default();
        GetClientRect(self.hwnd, &mut rc);
        if rc.right <= 0 || rc.bottom <= 0 {
            return;
        }
        // Only controls whose bounds change are moved, and Windows' own
        // invalidation is used: a moved control's pixels are copied to the new
        // position, only the uncovered background is erased (WS_CLIPCHILDREN),
        // and resized group boxes and the panel repaint from back buffers. Fixed
        // controls, i.e. the whole left column, are not repainted at all.
        let flags = SWP_NOZORDER | SWP_NOACTIVATE;
        let mut placements = Vec::with_capacity(self.controls.len() + 1);
        for c in &self.controls {
            let (x, y, w, h) = match c.anchor {
                Anchor::Fixed => (
                    self.px(c.x),
                    self.px(c.y),
                    self.px(c.width),
                    self.px(c.height),
                ),
                Anchor::Diagnostics => (
                    self.px(266),
                    self.px(8),
                    rc.right - self.px(274),
                    rc.bottom - self.px(72),
                ),
                Anchor::Log => (
                    self.px(PANEL_LEFT),
                    rc.bottom - self.px(56),
                    rc.right - self.px(PANEL_LEFT + 10),
                    self.px(22),
                ),
                Anchor::Hint => (
                    self.px(PANEL_LEFT),
                    rc.bottom - self.px(26),
                    rc.right - self.px(PANEL_LEFT + 10),
                    self.px(16),
                ),
            };
            placements.push((c.hwnd, x, y, w.max(1), h.max(1)));
        }
        let x = self.px(PANEL_LEFT);
        let y = self.px(PANEL_TOP);
        placements.push((
            self.panel,
            x,
            y,
            (rc.right - x - self.px(10)).max(1),
            (rc.bottom - y - self.px(76)).max(1),
        ));
        placements.retain(|&(hwnd, x, y, w, h)| {
            let old = child_rect(self.hwnd, hwnd);
            (
                old.left,
                old.top,
                old.right - old.left,
                old.bottom - old.top,
            ) != (x, y, w, h)
        });
        let mut batch = if placements.is_empty() {
            null_mut()
        } else {
            BeginDeferWindowPos(placements.len() as i32)
        };
        for &(hwnd, x, y, w, h) in &placements {
            if batch.is_null() {
                break;
            }
            batch = DeferWindowPos(batch, hwnd, null_mut(), x, y, w, h, flags);
        }
        let committed = !batch.is_null() && EndDeferWindowPos(batch) != 0;
        if !committed {
            for &(hwnd, x, y, w, h) in &placements {
                SetWindowPos(hwnd, null_mut(), x, y, w, h, flags);
            }
        }
        // The group box overlaps this child. Keep the receiver above it so
        // WindowFromPoint and native touch delivery reach the receiver itself
        // (and the opaque group box's paint stays clipped out of it).
        SetWindowPos(
            self.panel,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        // Painting stays queued: a synchronous WM_PAINT here would find GUI's
        // RefCell borrowed. See WM_SIZE in `window_proc` for the immediate update.
        self.refresh_preview_rect();
    }
    unsafe fn refresh_devices(&mut self) {
        let previous = self
            .devices
            .get(self.selection(DEVICE))
            .map(|d| d.path.clone());
        match hid::discover() {
            Ok(devices) => {
                self.devices = devices;
                SendMessageW(self.item(DEVICE), CB_RESETCONTENT, 0, 0);
                for (i, d) in self.devices.iter().enumerate() {
                    let label = format!("{} · VID {:04X} / PID {:04X}", i + 1, d.vid, d.pid);
                    SendMessageW(
                        self.item(DEVICE),
                        CB_ADDSTRING,
                        0,
                        wide(&label).as_ptr() as LPARAM,
                    );
                }
                let index = self
                    .devices
                    .iter()
                    .position(|d| Some(&d.path) == previous.as_ref())
                    .unwrap_or(0);
                SendMessageW(self.item(DEVICE), CB_SETCURSEL, index, 0);
                if self.devices.is_empty() {
                    self.last_status =
                        "No Precision Touchpad found. Connect a device and refresh.".into();
                } else {
                    self.last_status =
                        format!("{} touchpad collection(s) available.", self.devices.len());
                }
            }
            Err(e) => {
                self.devices.clear();
                SendMessageW(self.item(DEVICE), CB_RESETCONTENT, 0, 0);
                self.last_status = e;
            }
        }
        self.select_device_info();
    }
    unsafe fn select_device_info(&mut self) {
        let result = self
            .devices
            .get(self.selection(DEVICE))
            .ok_or("No input device".to_string())
            .and_then(Device::geometry);
        self.geometry = result.as_ref().ok().copied();
        if self.session.is_none() {
            let mut stats = self.shared.lock().unwrap();
            stats.device_label = match result {
                Ok(g) => g.label().unwrap_or_default(),
                Err(error) => format!("Physical geometry unavailable: {error}"),
            };
        }
        self.refresh_preview_rect();
    }
    /// No selection (CB_ERR) is the diagnostic default.
    unsafe fn choice(&self) -> Choice {
        let index = SendMessageW(self.item(TARGET), CB_GETCURSEL, 0, 0);
        usize::try_from(index)
            .ok()
            .and_then(|i| self.choices.get(i).copied())
            .or_else(|| self.choices.first().copied())
            .unwrap_or(Choice::Diagnostic)
    }
    unsafe fn select_choice(&self, choice: Choice) {
        let index = self.choices.iter().position(|&c| c == choice).unwrap_or(0);
        SendMessageW(self.item(TARGET), CB_SETCURSEL, index, 0);
    }
    /// (Re)build the screen area list from the connected monitors, keeping the
    /// current choice. The first fill and a disconnected monitor select the
    /// first entry (the whole screen).
    unsafe fn fill_targets(&mut self) {
        let first_fill = self.choices.is_empty();
        let previous = self.choice();
        let previous_device = match previous {
            Choice::Monitor(i) => self.monitors.get(i).map(|m| m.device.clone()),
            _ => None,
        };
        self.monitors = mapping::monitors();
        self.choices = choices(self.monitors.len());
        let combo = self.item(TARGET);
        SendMessageW(combo, CB_RESETCONTENT, 0, 0);
        for &c in &self.choices {
            let label = wide(&choice_label(c, &self.monitors));
            SendMessageW(combo, CB_ADDSTRING, 0, label.as_ptr() as LPARAM);
        }
        let next = match (previous, previous_device) {
            (Choice::Monitor(_), Some(device)) => {
                match self.monitors.iter().position(|m| m.device == device) {
                    Some(i) => Choice::Monitor(i),
                    None => {
                        self.last_status = "The selected monitor was disconnected.".into();
                        self.choices[0]
                    }
                }
            }
            _ if first_fill => self.choices[0],
            // Full desktop and the single "Screen" entry are the same area.
            (Choice::Desktop, _) | (Choice::Monitor(_), _) if self.monitors.len() == 1 => {
                Choice::Monitor(0)
            }
            (other, _) => other,
        };
        self.select_choice(next);
    }
    unsafe fn selected_window(&self) -> Option<usize> {
        // No selection (CB_ERR) must never fall back to the first listed window.
        let index = SendMessageW(self.item(WINDOW), CB_GETCURSEL, 0, 0);
        usize::try_from(index)
            .ok()
            .and_then(|i| self.windows.get(i))
            .map(|w| w.handle)
    }
    /// The configured target, or why it cannot be used yet.
    unsafe fn target(&self) -> Result<MappingTarget, String> {
        match self.choice() {
            Choice::Desktop => Ok(MappingTarget::ScreenRectangle {
                bounds: mapping::virtual_screen(),
            }),
            Choice::Monitor(i) => self
                .monitors
                .get(i)
                .map(|m| MappingTarget::ScreenRectangle { bounds: m.rect })
                .ok_or_else(|| "The selected monitor is no longer connected.".into()),
            Choice::ScreenPortion => self
                .screen_rect
                .map(|bounds| MappingTarget::ScreenRectangle { bounds })
                .ok_or_else(|| "Click Select area… and drag a screen rectangle.".into()),
            Choice::Window => {
                let handle = self
                    .selected_window()
                    .ok_or("Choose an application window from the list.")?;
                Ok(match self.window_area {
                    Some(area) => MappingTarget::WindowRectangle { handle, area },
                    None => MappingTarget::Window { handle },
                })
            }
            Choice::Diagnostic => Ok(MappingTarget::Diagnostics),
        }
    }
    fn aspect(&self) -> Option<f64> {
        self.geometry.and_then(|g| g.aspect().ok())
    }
    unsafe fn pad_fit(&self) -> mapping::PadFit {
        match self.selection(PAD_FIT) {
            1 => mapping::PadFit::Stretch,
            2 => mapping::PadFit::FillArea,
            _ => mapping::PadFit::KeepShape,
        }
    }
    /// Resolve the configured target exactly as a session would.
    unsafe fn resolve_target(&self) -> Result<mapping::ResolvedTarget, String> {
        let target = self.target()?;
        let fit = self.pad_fit();
        if self.aspect().is_none() && self.checked(INJECT) && fit.needs_shape() {
            return Err(
                "Touchpad shape unavailable: choose Touchpad area \"Whole pad · stretch\" or clear injection."
                    .into(),
            );
        }
        target.resolve_fitted(self.panel, self.aspect(), fit)
    }
    /// Region shown in the diagram while idle: the target exactly as a session
    /// would map it, without requiring physical geometry for raw-only use.
    unsafe fn refresh_diagram_preview(&mut self) {
        self.preview_at = Instant::now();
        self.preview = self.target().and_then(|target| {
            let selected = target.resolve(self.panel)?.bounds;
            let fitted = target.resolve_fitted(self.panel, self.aspect(), self.pad_fit())?;
            Ok((fitted.bounds, selected, fitted.input))
        });
    }
    /// Diagnostic text, then (for screen/window targets) the mapping diagram.
    unsafe fn paint_panel(&mut self, hwnd: HWND, dc: HDC, rc: &RECT) {
        let diagram = match &self.session {
            Some(_) => !self.session_diagnostic,
            None => self.choice() != Choice::Diagnostic,
        };
        if !diagram {
            experiment::paint_diagnostics(hwnd, dc, rc, &self.shared, self.rect, self.dpi, true);
            return;
        }
        let area = RECT {
            left: self.px(16),
            top: self.px(190),
            right: rc.right - self.px(16),
            bottom: rc.bottom - self.px(12),
        };
        let mut origin = POINT::default();
        ClientToScreen(hwnd, &mut origin);
        let text_limit = RECT {
            left: area.left + origin.x,
            top: area.top + origin.y,
            right: area.right + origin.x,
            bottom: area.bottom + origin.y,
        };
        experiment::paint_diagnostics(hwnd, dc, rc, &self.shared, text_limit, self.dpi, false);
        let (mapped, mapped_input, live) = {
            let s = self.shared.lock().unwrap();
            let live = if s.capture_stopped {
                Vec::new()
            } else {
                s.live.clone()
            };
            (s.mapped, s.mapped_input, live)
        };
        let (region, selected, pad_input, message) = if self.session.is_some() {
            let (w, h) = mapping::size(mapped);
            (
                (w > 0 && h > 0).then_some(mapped),
                None,
                mapped_input.unwrap_or(NormRect::FULL),
                String::new(),
            )
        } else {
            match &self.preview {
                Ok((r, selected, input)) => (Some(*r), Some(*selected), *input, String::new()),
                Err(e) => (None, None, NormRect::FULL, e.clone()),
            }
        };
        let monitors: Vec<RECT> = self.monitors.iter().map(|m| m.rect).collect();
        let capture =
            self.session.is_none() && self.thumbnail_due.is_some_and(|due| Instant::now() >= due);
        if capture {
            self.thumbnail_due = None;
            self.thumbnail_captures += 1;
        }
        diagram::paint(
            dc,
            area,
            &diagram::Scene {
                virt: mapping::virtual_screen(),
                monitors: &monitors,
                pad_aspect: self.aspect(),
                region,
                selected,
                pad_input,
                contacts: &live,
                message: &message,
                dpi: self.dpi,
            },
            &mut self.thumbnail,
            capture,
        );
    }
    /// Ask for a fresh desktop thumbnail shortly (after overlays or window
    /// animations have left the screen). Never captured during a session.
    fn request_thumbnail(&mut self) {
        let due = Instant::now() + Duration::from_millis(150);
        self.thumbnail_due = Some(self.thumbnail_due.map_or(due, |d| d.min(due)));
    }
    unsafe fn refresh_windows(&mut self) {
        let previous = self.selected_window();
        self.windows = mapping::list_windows();
        let combo = self.item(WINDOW);
        SendMessageW(combo, CB_RESETCONTENT, 0, 0);
        for w in &self.windows {
            let mut title: String = w.title.chars().take(70).collect();
            if w.minimized {
                title.push_str(" (minimized)");
            }
            SendMessageW(combo, CB_ADDSTRING, 0, wide(&title).as_ptr() as LPARAM);
        }
        match self.windows.iter().position(|w| Some(w.handle) == previous) {
            Some(index) => {
                SendMessageW(combo, CB_SETCURSEL, index, 0);
            }
            None => {
                SendMessageW(combo, CB_SETCURSEL, usize::MAX, 0);
                if previous.is_some() {
                    self.window_area = None;
                    self.last_status = "The selected window closed. Choose another.".into();
                }
            }
        }
    }
    /// Select area… / Show area. Selection completes later via WM_SELECTION_DONE,
    /// so no nested message loop runs while GUI state is borrowed.
    unsafe fn area_action(&mut self) {
        if self.session.is_some() || self.controller.enabled || overlay::active() {
            return;
        }
        let result = match self.choice() {
            Choice::ScreenPortion => overlay::begin(
                self.hwnd,
                mapping::virtual_screen(),
                "Drag to select the screen area to map. Esc or right-click cancels.",
            )
            .map(|_| self.pending_selection = Some(Pending::Screen)),
            Choice::Window if self.window_area.is_some() => {
                self.window_area = None;
                self.refresh_diagram_preview();
                self.show_target_preview()
            }
            Choice::Window => self
                .selected_window()
                .ok_or("Choose an application window first.".to_string())
                .and_then(|handle| mapping::window_client(handle).map(|client| (handle, client)))
                .and_then(|(handle, client)| {
                    SetForegroundWindow(handle as HWND);
                    overlay::begin(
                        self.hwnd,
                        client,
                        "Drag inside the window to map only that part. Esc or right-click cancels.",
                    )
                    .map(|_| self.pending_selection = Some(Pending::WindowArea { handle, client }))
                }),
            _ => self.show_target_preview(),
        };
        if let Err(e) = result {
            self.pending_selection = None;
            self.last_status = e;
        } else if self.pending_selection.is_some() {
            self.last_status = "Selecting area…".into();
        }
    }
    unsafe fn selection_done(&mut self) {
        let Some(result) = overlay::take_result() else {
            return;
        };
        let pending = self.pending_selection.take();
        SetForegroundWindow(self.hwnd);
        let Some(selected) = result else {
            self.last_status = "Area selection cancelled.".into();
            return;
        };
        let applied = match pending {
            Some(Pending::Screen) => {
                mapping::validate_screen_rect(selected, mapping::virtual_screen())
                    .map(|r| self.screen_rect = Some(r))
            }
            Some(Pending::WindowArea { handle, client }) => {
                if self.selected_window() != Some(handle) {
                    Err("The window selection changed during area selection.".into())
                } else {
                    NormRect::from_screen(client, selected).and_then(|area| {
                        let (w, h) = mapping::size(area.apply(client));
                        if w < mapping::MIN_TARGET_PX as i64 || h < mapping::MIN_TARGET_PX as i64 {
                            return Err("Selected area is too small.".into());
                        }
                        self.window_area = Some(area);
                        Ok(())
                    })
                }
            }
            None => Ok(()),
        };
        if let Err(e) = applied.and_then(|_| self.show_target_preview()) {
            self.last_status = e;
        }
    }
    /// Flash the resolved (aspect-fitted) injection bounds on the desktop.
    unsafe fn show_target_preview(&mut self) -> Result<(), String> {
        if self.choice() == Choice::Diagnostic {
            return Ok(());
        }
        let target = self.resolve_target()?;
        overlay::flash(target.bounds, PREVIEW_MS)?;
        self.last_status = format!("Mapped region: {}.", mapping::describe(target.bounds));
        Ok(())
    }
    unsafe fn target_info(&self) -> String {
        let at = |r: RECT| {
            let (w, h) = mapping::size(r);
            format!("{w}×{h} at {},{}", r.left, r.top)
        };
        match self.choice() {
            Choice::Diagnostic => "Inside this window".into(),
            Choice::Desktop => at(mapping::virtual_screen()),
            Choice::Monitor(i) => self
                .monitors
                .get(i)
                .map_or("Disconnected".into(), |m| at(m.rect)),
            Choice::ScreenPortion => self.screen_rect.map_or("Not selected".into(), at),
            Choice::Window => self.window_area.map_or("Whole window".into(), |a| {
                format!(
                    "{:.0}–{:.0}% × {:.0}–{:.0}%",
                    a.left * 100.0,
                    a.right * 100.0,
                    a.top * 100.0,
                    a.bottom * 100.0
                )
            }),
        }
    }
    unsafe fn enable(&mut self) {
        if self.restart_blocked || self.closing {
            return;
        }
        if self.devices.is_empty() {
            self.last_status = "No touchpad available. Refresh the device list.".into();
            return;
        }
        // Validate the target before arming a shortcut.
        match self.resolve_target() {
            Ok(target) => {
                if let Some(warning) = target
                    .spec
                    .window_handle()
                    .and_then(mapping::integrity_warning)
                {
                    self.last_status = warning;
                }
            }
            Err(e) => {
                self.last_status = e;
                return;
            }
        }
        self.controller.mode = match self.selection(MODE) {
            1 => TakeoverMode::Toggle,
            2 => TakeoverMode::Hold,
            _ => TakeoverMode::Manual,
        };
        if self.controller.mode != TakeoverMode::Manual {
            if RegisterHotKey(
                self.hwnd,
                HOTKEY,
                MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                self.key(),
            ) == 0
            {
                self.last_status = format!(
                    "Shortcut unavailable. Choose another shortcut. {}",
                    winerr("RegisterHotKey")
                );
                return;
            }
            self.hotkey_registered = true;
        }
        self.controller.enable();
        self.last_status = "Shortcut enabled. Press it to capture.".into();
    }
    unsafe fn key(&self) -> u32 {
        VK_F8 as u32 + self.selection(KEY) as u32
    }
    unsafe fn held(&self) -> bool {
        [VK_CONTROL as i32, VK_MENU as i32, self.key() as i32]
            .iter()
            .all(|&key| GetAsyncKeyState(key) < 0)
    }
    unsafe fn disarm(&mut self, reason: &str) {
        self.controller.disarm();
        overlay::cancel();
        if self.hotkey_registered {
            UnregisterHotKey(self.hwnd, HOTKEY);
            self.hotkey_registered = false;
        }
        if let Some(session) = &self.session {
            session.request_stop(reason);
        }
        self.last_status = reason.into();
        self.update_controls();
    }
    unsafe fn start_session(&mut self) -> Result<(), String> {
        let spec = self.target()?;
        if spec.is_diagnostic()
            && self.checked(INJECT)
            && (IsIconic(self.hwnd) != 0 || IsWindowVisible(self.hwnd) == 0)
        {
            return Err("Restore and show the window before starting touch injection.".into());
        }
        let selected = self
            .devices
            .get(self.selection(DEVICE))
            .ok_or("Select an input device first.")?;
        // Rediscover and match identity, never reuse a potentially reordered index.
        let device = hid::discover()?
            .into_iter()
            .find(|d| d.path == selected.path)
            .ok_or("The selected device disconnected. Refresh the list.")?;
        let geometry = device.geometry();
        let fit = self.pad_fit();
        let target = match geometry {
            Ok(g) => spec.resolve_fitted(self.panel, Some(g.aspect()?), fit)?,
            Err(error) if self.checked(INJECT) && fit.needs_shape() => {
                return Err(format!(
                    "Cannot preserve the physical touchpad aspect: {error}. Choose Touchpad area \"Whole pad · stretch\" to inject without it."
                ))
            }
            Err(_) => spec.resolve_fitted(self.panel, None, fit)?,
        };
        overlay::hide_outline();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let out = PathBuf::from(format!("artifacts/gui-{stamp}"));
        let session = Session::start(
            device,
            target,
            experiment::SessionSettings {
                inject: self.checked(INJECT),
                capacity: self.selection(CONTACTS) + 1,
                suppress_gestures: self.selection(GUARD) == 1,
                background_guard: self.selection(GUARD) == 2,
                touch_feedback: self.checked(FEEDBACK),
            },
            out.clone(),
        )?;
        session.shared.lock().unwrap().frame_signal = Some(experiment::FrameSignal::new(
            self.panel,
            WM_PANEL_FRAME,
            &PANEL_FRAME_PENDING,
        ));
        self.geometry = geometry.ok();
        self.shared = session.shared.clone();
        self.session = Some(session);
        self.session_diagnostic = spec.is_diagnostic();
        self.refresh_preview_rect();
        SetWindowTextW(
            self.log_path,
            wide(&format!("Logs: {}", out.display())).as_ptr(),
        );
        self.last_status = if spec.is_diagnostic() {
            "Starting. Touch the pad, then lift all fingers to arm.".into()
        } else {
            let warning = spec
                .window_handle()
                .and_then(mapping::integrity_warning)
                .map(|w| format!(" {w}"))
                .unwrap_or_default();
            format!(
                "Mapping to {}. Lift all fingers to arm.{warning}",
                mapping::describe(target.bounds)
            )
        };
        Ok(())
    }
    unsafe fn tick(&mut self) {
        if self.controller.enabled && self.controller.mode == TakeoverMode::Hold && !self.held() {
            self.controller.release();
        }
        if let Some(session) = self.session.as_ref() {
            if !self.controller.requested && !session.stopping() {
                session.request_stop("Capture stopped.");
            }
            if session.stopped_unexpectedly() {
                self.disarm("Capture stopped. Review diagnostics before enabling again.");
            }
        }
        if self.session.as_mut().is_some_and(|s| s.ready_to_finish()) {
            let session = self.session.take().unwrap();
            match session.finish() {
                Ok(()) => {
                    self.last_status = if self.controller.enabled {
                        "Contacts released. Ready for the next shortcut.".into()
                    } else {
                        "Stopped. All contacts released.".into()
                    }
                }
                Err(e) => {
                    self.disarm(&e);
                    self.last_status = e;
                }
            }
            let s = self.shared.lock().unwrap();
            self.restart_blocked = s.cleanup_pending
                || s.recovery_pending
                || s.injected_active > 0
                || !s.received_active.is_empty();
            drop(s);
            if self.restart_blocked {
                self.last_status =
                    "Unresolved contacts. New sessions are blocked; keep the logs for review."
                        .into();
            }
            self.layout();
        }
        if self.closing && self.session.is_none() {
            DestroyWindow(self.hwnd);
            return;
        }
        if self.controller.requested && self.session.is_none() && !self.restart_blocked {
            if let Err(e) = self.start_session() {
                self.disarm(&e);
            }
        }
        if self.session.is_none()
            && self.choice() != Choice::Diagnostic
            && self.preview_at.elapsed() >= Duration::from_millis(250)
        {
            self.refresh_diagram_preview();
        }
        let mut s = self.shared.lock().unwrap();
        s.foreground = GetForegroundWindow() == self.hwnd;
        drop(s);
        self.update_controls();
        InvalidateRect(self.panel, null(), 0);
    }
    /// Disabling the focused control (e.g. Start right after it is clicked) sets
    /// focus to NULL, which leaves the window without keyboard navigation.
    /// Move focus to the next usable button, or the window itself.
    unsafe fn keep_focus(&self) {
        if GetActiveWindow() != self.hwnd {
            return;
        }
        let focus = GetFocus();
        if !focus.is_null() && IsWindowEnabled(focus) != 0 {
            return;
        }
        let next = [STOP, START]
            .into_iter()
            .map(|id| self.item(id))
            .find(|&h| IsWindowEnabled(h) != 0)
            .unwrap_or(self.hwnd);
        SetFocus(next);
    }
    unsafe fn update_controls(&self) {
        let locked =
            self.session.is_some() || self.controller.enabled || self.closing || overlay::active();
        for id in [
            DEVICE, REFRESH, MODE, CONTACTS, INJECT, GUARD, TARGET, PAD_FIT,
        ] {
            EnableWindow(self.item(id), i32::from(!locked));
        }
        // Feedback is fixed when injection initializes, so it only applies
        // to a session that injects.
        EnableWindow(
            self.item(FEEDBACK),
            i32::from(!locked && self.checked(INJECT)),
        );
        let choice = self.choice();
        EnableWindow(
            self.item(WINDOW),
            i32::from(!locked && choice.uses_window()),
        );
        EnableWindow(
            self.item(AREA),
            i32::from(
                !locked
                    && match choice {
                        Choice::Diagnostic => false,
                        c if c.uses_window() => self.selected_window().is_some(),
                        _ => true,
                    },
            ),
        );
        set_text(
            self.item(AREA),
            match choice {
                Choice::ScreenPortion => "Select area…",
                Choice::Window if self.window_area.is_some() => "Whole window",
                Choice::Window => "Select part…",
                _ => "Show area",
            },
        );
        set_text(self.item(TARGET_INFO), &self.target_info());
        let target_error = self.target().err();
        EnableWindow(
            self.item(KEY),
            i32::from(!locked && self.selection(MODE) != 0),
        );
        EnableWindow(
            self.item(START),
            i32::from(
                !locked
                    && !self.restart_blocked
                    && !self.devices.is_empty()
                    && (!self.checked(INJECT)
                        || self.geometry.is_some()
                        || !self.pad_fit().needs_shape())
                    && target_error.is_none(),
            ),
        );
        EnableWindow(
            self.item(STOP),
            i32::from(!self.closing && (self.controller.enabled || self.session.is_some())),
        );
        self.keep_focus();
        set_text(
            self.item(START),
            if self.selection(MODE) == 0 {
                "Start capture"
            } else {
                "Enable shortcut"
            },
        );
        let state = if self.closing {
            "Closing · releasing contacts"
        } else if self.session.as_ref().is_some_and(|s| s.stopping()) {
            "Stopping · releasing contacts"
        } else if self.session.is_some() && !self.checked(INJECT) {
            "Capturing · raw input only"
        } else if self.session.is_some() {
            let s = self.shared.lock().unwrap();
            if s.armed {
                "Capturing"
            } else {
                "Lift all fingers to arm"
            }
        } else if self.controller.enabled {
            "Shortcut ready"
        } else {
            "Ready"
        };
        let info = match &target_error {
            Some(e) if self.session.is_none() && !self.controller.enabled => e,
            _ => &self.last_status,
        };
        set_text(self.status, &format!("{state}\r\n{info}"));
    }
}

/// Esc as WM_KEYDOWN, or as WM_SYSKEYDOWN without Alt (lParam bit 29), which is
/// how it arrives when no window has focus. Alt+Esc stays the system window switch.
fn is_plain_escape(msg: &MSG) -> bool {
    msg.wParam == VK_ESCAPE as usize
        && (msg.message == WM_KEYDOWN
            || (msg.message == WM_SYSKEYDOWN && msg.lParam & (1 << 29) == 0))
}

/// The system Button procedure that group boxes were subclassed from.
static GROUP_PROC: AtomicUsize = AtomicUsize::new(0);

/// Group boxes do not paint their interior; they rely on the parent erasing
/// under them first, which shows as a blank frame between the two paints. This
/// paints the dialog background and the system-drawn frame and caption into a
/// back buffer and presents them together in a single paint.
unsafe extern "system" fn group_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: set from SetWindowLongPtrW(GWLP_WNDPROC) before any subclassed message.
    let original: WNDPROC = std::mem::transmute(GROUP_PROC.load(Ordering::Relaxed));
    match msg {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            if !dc.is_null() && rc.right > 0 && rc.bottom > 0 {
                match crate::render::BackBuffer::new(dc, rc.right, rc.bottom) {
                    Ok(buffer) => {
                        FillRect(buffer.dc, &rc, GetSysColorBrush(COLOR_BTNFACE));
                        CallWindowProcW(
                            original,
                            hwnd,
                            WM_PRINTCLIENT,
                            buffer.dc as WPARAM,
                            PRF_CLIENT as LPARAM,
                        );
                        let _ = buffer.present(dc);
                    }
                    Err(_) => {
                        FillRect(dc, &rc, GetSysColorBrush(COLOR_BTNFACE));
                        CallWindowProcW(
                            original,
                            hwnd,
                            WM_PRINTCLIENT,
                            dc as WPARAM,
                            PRF_CLIENT as LPARAM,
                        );
                    }
                }
            }
            EndPaint(hwnd, &ps);
            return 0;
        }
        // The frame follows the size, so the whole box repaints.
        WM_SIZE => {
            InvalidateRect(hwnd, null(), 0);
        }
        _ => {}
    }
    CallWindowProcW(original, hwnd, msg, w, l)
}

/// A child window's bounds in its parent's client coordinates.
unsafe fn child_rect(parent: HWND, child: HWND) -> RECT {
    let mut rect = RECT::default();
    GetWindowRect(child, &mut rect);
    MapWindowPoints(null_mut(), parent, (&mut rect as *mut RECT).cast(), 2);
    rect
}

unsafe fn set_text(hwnd: HWND, text: &str) -> bool {
    let mut current = vec![0u16; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
    let len = GetWindowTextW(hwnd, current.as_mut_ptr(), current.len() as i32);
    if String::from_utf16_lossy(&current[..len.max(0) as usize]) != text {
        SetWindowTextW(hwnd, wide(text).as_ptr());
        true
    } else {
        false
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        if let Some(session) = &self.session {
            session.request_stop("GUI closed unexpectedly");
        }
        overlay::cancel();
        overlay::hide_outline();
        unsafe {
            if self.hotkey_registered {
                UnregisterHotKey(self.hwnd, HOTKEY);
            }
            if self.emergency_registered {
                UnregisterHotKey(self.hwnd, EMERGENCY);
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORDLG => {
            SetBkColor(w as HDC, GetSysColor(COLOR_BTNFACE));
            SetTextColor(w as HDC, GetSysColor(COLOR_WINDOWTEXT));
            SetBkMode(w as HDC, TRANSPARENT as i32);
            return GetSysColorBrush(COLOR_BTNFACE) as LRESULT;
        }
        WM_ERASEBKGND => {
            let mut clip = RECT::default();
            GetClipBox(w as HDC, &mut clip);
            FillRect(w as HDC, &clip, GetSysColorBrush(COLOR_BTNFACE));
            return 1;
        }
        WM_PRINTCLIENT => {
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            FillRect(w as HDC, &rc, (COLOR_BTNFACE + 1) as HBRUSH);
            return 0;
        }
        // Returning to PadBridge: the desktop may have changed. Default
        // processing still runs so keyboard focus is restored normally.
        WM_ACTIVATE if (w & 0xffff) as u32 != WA_INACTIVE => {
            GUI.with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut() {
                    if let Some(g) = slot.as_mut() {
                        g.request_thumbnail();
                    }
                }
            });
        }
        WM_COMMAND
        | WM_HOTKEY
        | WM_TIMER
        | WM_CLOSE
        | WM_SIZE
        | WM_MOVE
        | WM_ENTERSIZEMOVE
        | WM_EXITSIZEMOVE
        | WM_DISPLAYCHANGE
        | WM_DPICHANGED
        | overlay::WM_SELECTION_DONE => {
            let mut destroy = false;
            let mut laid_out = false;
            GUI.with(|slot| {
                let Ok(mut slot) = slot.try_borrow_mut() else {
                    return;
                };
                let Some(g) = slot.as_mut() else {
                    if msg == WM_CLOSE {
                        destroy = true;
                    }
                    return;
                };
                match msg {
                    WM_COMMAND => {
                        let id = w & 0xffff;
                        let notification = (w >> 16) as u32;
                        match id {
                            START if notification == BN_CLICKED => g.enable(),
                            STOP if notification == BN_CLICKED => {
                                g.disarm("Capture and shortcut activation stopped.")
                            }
                            REFRESH if notification == BN_CLICKED => g.refresh_devices(),
                            DEVICE if notification == CBN_SELCHANGE => {
                                g.select_device_info();
                            }
                            TARGET if notification == CBN_SELCHANGE => {
                                if g.choice().uses_window() {
                                    g.refresh_windows();
                                }
                                g.refresh_preview_rect();
                                g.refresh_diagram_preview();
                                g.request_thumbnail();
                            }
                            WINDOW if notification == CBN_DROPDOWN => g.refresh_windows(),
                            WINDOW if notification == CBN_SELCHANGE => {
                                g.window_area = None;
                                g.refresh_diagram_preview();
                                g.request_thumbnail();
                            }
                            AREA if notification == BN_CLICKED => g.area_action(),
                            PAD_FIT if notification == CBN_SELCHANGE => {
                                g.refresh_preview_rect();
                                g.refresh_diagram_preview();
                            }
                            MODE | KEY if notification == CBN_SELCHANGE => {}
                            _ => {}
                        }
                        g.update_controls();
                    }
                    WM_HOTKEY if w == EMERGENCY as usize => {
                        g.disarm("Global emergency stop requested.")
                    }
                    WM_HOTKEY if w == HOTKEY as usize => {
                        let held = g.held();
                        g.controller.hotkey(held);
                        g.tick();
                    }
                    WM_TIMER => g.tick(),
                    WM_CLOSE => {
                        g.closing = true;
                        g.disarm("Closing after contact cleanup and log flush.");
                        if g.session.is_none() {
                            destroy = true;
                        }
                    }
                    overlay::WM_SELECTION_DONE => {
                        g.selection_done();
                        g.refresh_diagram_preview();
                        g.request_thumbnail();
                        g.update_controls();
                    }
                    WM_DISPLAYCHANGE => {
                        if g.session.is_some() && g.checked(INJECT) {
                            g.disarm("Display configuration changed. Capture stopped.");
                        }
                        if g.session.is_none() && !g.controller.enabled {
                            g.fill_targets();
                        }
                        g.thumbnail = None;
                        g.request_thumbnail();
                        g.refresh_diagram_preview();
                    }
                    WM_ENTERSIZEMOVE => {
                        // External targets do not depend on PadBridge's own window.
                        if g.session.is_some() && g.checked(INJECT) && g.session_diagnostic {
                            g.disarm("Window or display changed. Capture stopped.");
                        }
                    }
                    WM_MOVE => {
                        g.refresh_preview_rect();
                    }
                    WM_EXITSIZEMOVE => {
                        g.layout();
                        laid_out = true;
                    }
                    WM_SIZE => {
                        if g.session.is_some() && g.checked(INJECT) && g.session_diagnostic {
                            g.disarm("Window resized. Capture stopped.");
                        }
                        g.layout();
                        laid_out = true;
                    }
                    WM_DPICHANGED => {
                        if g.session.is_some() && g.checked(INJECT) && g.session_diagnostic {
                            g.disarm("Display scale changed. Capture stopped.");
                        }
                        g.dpi = (w & 0xffff) as u32;
                        g.set_font();
                        // Keep the suggested position, but size the fixed
                        // client area exactly for the new DPI.
                        let r = &*(l as *const RECT);
                        let size = outer_size(DEFAULT_CLIENT.0, DEFAULT_CLIENT.1, g.dpi);
                        SetWindowPos(
                            hwnd,
                            null_mut(),
                            r.left,
                            r.top,
                            size.x,
                            size.y,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        g.layout();
                        laid_out = true;
                    }
                    _ => {}
                }
            });
            if laid_out {
                // Paint the new layout now rather than when the message queue
                // next goes idle. The RefCell is released here, so the panel can
                // paint.
                RedrawWindow(hwnd, null(), null_mut(), RDW_UPDATENOW | RDW_ALLCHILDREN);
            }
            if destroy {
                DestroyWindow(hwnd);
            }
            return 0;
        }
        WM_DESTROY => {
            KillTimer(hwnd, 1);
            PostQuitMessage(0);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        // Repaint as soon as a frame arrives rather than on the next UI tick.
        // Clearing first lets the next frame post again; WM_PAINT coalesces
        // frames that arrive faster than the panel paints.
        WM_PANEL_FRAME => {
            PANEL_FRAME_PENDING.store(false, Ordering::Release);
            InvalidateRect(hwnd, null(), 0);
            return 0;
        }
        WM_PRINTCLIENT => {
            GUI.with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut() {
                    if let Some(g) = slot.as_mut() {
                        let mut rc = RECT::default();
                        GetClientRect(hwnd, &mut rc);
                        g.paint_panel(hwnd, w as HDC, &rc);
                    }
                }
            });
            return 0;
        }
        WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP => {
            GUI.with(|slot| {
                if let Some(g) = slot.borrow().as_ref() {
                    if let Some(session) = &g.session {
                        experiment::receive_touch(hwnd, msg, w, &g.shared, &session.log);
                    }
                }
            });
            InvalidateRect(hwnd, null(), 0);
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            GUI.with(|slot| {
                let Ok(mut slot) = slot.try_borrow_mut() else {
                    return;
                };
                let Some(g) = slot.as_mut() else {
                    return;
                };
                if dc.is_null() || rc.right <= 0 || rc.bottom <= 0 {
                    return;
                }
                if g.buffer
                    .as_ref()
                    .is_none_or(|b| b.width != rc.right || b.height != rc.bottom)
                {
                    match crate::render::BackBuffer::new(dc, rc.right, rc.bottom) {
                        Ok(buffer) => g.buffer = Some(buffer),
                        Err(e) => {
                            g.last_status = e;
                            FillRect(dc, &rc, (COLOR_WINDOW + 1) as HBRUSH);
                            return;
                        }
                    }
                }
                let buffer_dc = g.buffer.as_ref().unwrap().dc;
                g.paint_panel(hwnd, buffer_dc, &rc);
                let buffer = g.buffer.as_ref().unwrap();
                if let Err(e) = buffer.present(dc) {
                    g.last_status = e;
                }
            });
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
    use std::time::{Duration, Instant};

    #[test]
    fn escape_is_recognized_without_focus_but_not_alt_escape() {
        let key = |message, lparam| MSG {
            message,
            wParam: VK_ESCAPE as usize,
            lParam: lparam,
            ..Default::default()
        };
        assert!(is_plain_escape(&key(WM_KEYDOWN, 0)));
        // No focus window: Esc reaches the active window as WM_SYSKEYDOWN.
        assert!(is_plain_escape(&key(WM_SYSKEYDOWN, 0)));
        // Alt+Esc is the system window switch.
        assert!(!is_plain_escape(&key(WM_SYSKEYDOWN, 1 << 29)));
        assert!(!is_plain_escape(&key(WM_KEYUP, 0)));
        let mut other = key(WM_KEYDOWN, 0);
        other.wParam = VK_RETURN as usize;
        assert!(!is_plain_escape(&other));
    }

    #[test]
    #[ignore = "shows and activates an idle native window; no device, hotkey, capture or settings changes"]
    fn native_gui_focus_moves_off_disabled_controls() {
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register("PadBridgeGui", Some(window_proc), 0).unwrap();
            register(
                "PadBridgeDiagnosticPanel",
                Some(panel_proc),
                CS_HREDRAW | CS_VREDRAW,
            )
            .unwrap();
            let hwnd = CreateWindowExW(
                0,
                wide("PadBridgeGui").as_ptr(),
                wide("PadBridge focus regression").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                40,
                40,
                100,
                100,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            let mut g = Gui::new(hwnd).unwrap();
            g.refresh_devices();
            SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
            SendMessageW(g.item(MODE), CB_SETCURSEL, 1, 0);
            let (start, stop) = (g.item(START), g.item(STOP));
            GUI.with(|slot| *slot.borrow_mut() = Some(g));
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                g.layout();
                g.update_controls();
            });
            ShowWindow(hwnd, SW_SHOW);
            SetActiveWindow(hwnd);
            pump_for(Duration::from_millis(100));
            assert_eq!(GetActiveWindow(), hwnd, "window could not be activated");
            // Start when a device is present, else another control the lock disables.
            let first = if IsWindowEnabled(start) != 0 {
                start
            } else {
                GetDlgItem(hwnd, MODE as i32)
            };
            SetFocus(first);
            assert_eq!(GetFocus(), first);
            // Same lock state as an enabled shortcut, without registering one.
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                g.controller.mode = TakeoverMode::Toggle;
                g.controller.enabled = true;
                g.update_controls();
            });
            assert_eq!(IsWindowEnabled(first), 0);
            assert_eq!(GetFocus(), stop, "focus was left on a disabled control");
            SendMessageW(hwnd, WM_COMMAND, STOP, 0);
            assert_eq!(IsWindowEnabled(stop), 0);
            let expected = if IsWindowEnabled(start) != 0 {
                start
            } else {
                hwnd
            };
            assert_eq!(
                GetFocus(),
                expected,
                "focus was left on the disabled Stop button"
            );
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            GUI.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }

    /// Phase 3 target configuration through real controls and the real
    /// selection overlay, against a window of another process. No device,
    /// capture session, hotkey registration or touch injection is involved.
    #[test]
    #[ignore = "creates native windows and a helper process; run on an interactive Windows desktop"]
    fn native_gui_phase3_target_selection_without_capture() {
        use crate::{mapping::tuple, target_tests::Host};
        unsafe fn with<R>(f: impl FnOnce(&mut Gui) -> R) -> R {
            GUI.with(|slot| f(slot.borrow_mut().as_mut().unwrap()))
        }
        unsafe fn choose(hwnd: HWND, combo: usize, index: usize) {
            let item = GetDlgItem(hwnd, combo as i32);
            SendMessageW(item, CB_SETCURSEL, index, 0);
            SendMessageW(
                hwnd,
                WM_COMMAND,
                (CBN_SELCHANGE as usize) << 16 | combo,
                item as LPARAM,
            );
        }
        unsafe fn index_of(choice: Choice) -> usize {
            with(|g| g.choices.iter().position(|&c| c == choice).unwrap())
        }
        unsafe fn click(hwnd: HWND, id: usize) {
            SendMessageW(hwnd, WM_COMMAND, id, GetDlgItem(hwnd, id as i32) as LPARAM);
        }
        /// Drag inside the live overlay using client coordinates.
        unsafe fn drag(from: (i32, i32), to: (i32, i32)) {
            let overlay = FindWindowW(wide("PadBridgeSelectionOverlay").as_ptr(), null());
            assert!(!overlay.is_null(), "selection overlay not shown");
            let pack = |(x, y): (i32, i32)| ((y as isize) << 16 | x as isize) as LPARAM;
            SendMessageW(overlay, WM_LBUTTONDOWN, 1, pack(from));
            SendMessageW(overlay, WM_MOUSEMOVE, 1, pack(to));
            SendMessageW(overlay, WM_LBUTTONUP, 0, pack(to));
            pump_for(Duration::from_millis(100));
        }
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register("PadBridgeGui", Some(window_proc), 0).unwrap();
            register(
                "PadBridgeDiagnosticPanel",
                Some(panel_proc),
                CS_HREDRAW | CS_VREDRAW,
            )
            .unwrap();
            let hwnd = CreateWindowExW(
                0,
                wide("PadBridgeGui").as_ptr(),
                wide("PadBridge Phase 3 GUI check").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                40,
                40,
                100,
                100,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            let g = Gui::new(hwnd).unwrap();
            // Raw-only: previews need no physical geometry.
            SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
            GUI.with(|slot| *slot.borrow_mut() = Some(g));
            with(|g| {
                g.layout();
                g.update_controls();
            });
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            pump_for(Duration::from_millis(100));
            with(|g| {
                assert_eq!(g.choice(), g.choices[0], "the whole screen is the default");
                assert_ne!(g.choice(), Choice::Diagnostic);
                assert!(matches!(
                    g.target().unwrap(),
                    MappingTarget::ScreenRectangle { .. }
                ));
                assert_eq!(IsWindowEnabled(g.item(WINDOW)), 0);
                assert_ne!(IsWindowEnabled(g.item(AREA)), 0, "Show area");
                assert_eq!(
                    *g.choices.last().unwrap(),
                    Choice::Diagnostic,
                    "test area is listed last"
                );
            });

            let host = Host::spawn("pmv2");
            host.place(900, 120, 760, 520);

            // Application window: list, select, preview outline.
            choose(hwnd, TARGET, index_of(Choice::Window));
            let index = with(|g| {
                assert!(g.target().is_err(), "no window chosen yet");
                assert_eq!(tuple(g.rect), (0, 0, 0, 0), "no diagnostic canvas");
                assert_ne!(IsWindowEnabled(g.item(WINDOW)), 0);
                g.windows
                    .iter()
                    .position(|w| w.handle == host.hwnd)
                    .expect("host offered in window list")
            });
            choose(hwnd, WINDOW, index);
            with(|g| {
                // Choosing a window maps its whole client area right away.
                assert_eq!(
                    g.target().unwrap(),
                    MappingTarget::Window { handle: host.hwnd }
                );
                assert_eq!(g.target_info(), "Whole window");
                assert_ne!(IsWindowEnabled(g.item(AREA)), 0);
            });

            // Emergency stop cancels an in-progress selection.
            click(hwnd, AREA);
            assert!(crate::overlay::active());
            SendMessageW(hwnd, WM_HOTKEY, EMERGENCY as usize, 0);
            pump_for(Duration::from_millis(100));
            with(|g| {
                assert!(!crate::overlay::active());
                assert!(g.pending_selection.is_none());
                assert!(g.window_area.is_none(), "cancel keeps the whole window");
            });

            // Select part…: drag the central half of the client area.
            click(hwnd, AREA);
            with(|g| {
                assert!(g.pending_selection.is_some());
                // Controls stay locked while the overlay is active.
                assert_eq!(IsWindowEnabled(g.item(TARGET)), 0);
            });
            let client = mapping::window_client(host.hwnd).unwrap();
            let (w, h) = mapping::size(client);
            let (w, h) = (w as i32, h as i32);
            drag((w / 4, h / 4), (w * 3 / 4 - 1, h * 3 / 4 - 1));
            with(|g| {
                let area = g.window_area.expect("window area selected");
                for (value, expected) in [
                    (area.left, 0.25),
                    (area.top, 0.25),
                    (area.right, 0.75),
                    (area.bottom, 0.75),
                ] {
                    assert!((value - expected).abs() < 0.01, "{area:?}");
                }
                assert_eq!(
                    g.target().unwrap(),
                    MappingTarget::WindowRectangle {
                        handle: host.hwnd,
                        area
                    }
                );
                assert_ne!(IsWindowEnabled(g.item(TARGET)), 0);
                assert!(g.target_info().contains('%'));
                assert!(crate::overlay::outline_visible(), "preview outline");
            });
            pump_for(Duration::from_millis(300));
            std::fs::create_dir_all("artifacts").unwrap();
            save_client_bitmap(hwnd, "artifacts/diagram-window-portion.bmp");

            // The same button returns to the whole window.
            click(hwnd, AREA);
            with(|g| {
                assert!(g.window_area.is_none());
                assert_eq!(
                    g.target().unwrap(),
                    MappingTarget::Window { handle: host.hwnd }
                );
                assert!(
                    g.last_status.starts_with("Mapped region"),
                    "{}",
                    g.last_status
                );
            });

            // Screen rectangle: drag in virtual-desktop coordinates.
            choose(hwnd, TARGET, index_of(Choice::ScreenPortion));
            with(|g| assert!(g.target().is_err()));
            click(hwnd, AREA);
            drag((100, 80), (419, 279));
            with(|g| {
                let screen = mapping::virtual_screen();
                let r = g.screen_rect.expect("screen area selected");
                assert_eq!(
                    tuple(r),
                    (
                        screen.left + 100,
                        screen.top + 80,
                        screen.left + 420,
                        screen.top + 280
                    )
                );
                assert!(g.target().is_ok());
            });
            // A too-small screen selection is rejected and keeps the old one.
            click(hwnd, AREA);
            drag((10, 10), (12, 12));
            with(|g| {
                assert!(g.screen_rect.is_some());
                assert!(g.last_status.contains("at least"), "{}", g.last_status);
            });

            // A closed target window is dropped from the selection.
            choose(hwnd, TARGET, index_of(Choice::Window));
            host.close();
            SendMessageW(
                hwnd,
                WM_COMMAND,
                (CBN_DROPDOWN as usize) << 16 | WINDOW,
                g_item(hwnd, WINDOW) as LPARAM,
            );
            with(|g| {
                assert!(g.selected_window().is_none());
                assert!(g.window_area.is_none());
                assert!(g.target().is_err());
            });
            crate::overlay::hide_outline();
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            GUI.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }
    unsafe fn g_item(hwnd: HWND, id: usize) -> HWND {
        GetDlgItem(hwnd, id as i32)
    }

    /// Renders the panel exactly as WM_PAINT does and returns a memory DC.
    unsafe fn render_panel(panel: HWND) -> (HDC, HBITMAP, HGDIOBJ) {
        let mut rc = RECT::default();
        GetClientRect(panel, &mut rc);
        let screen = GetDC(panel);
        let dc = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, rc.right, rc.bottom);
        let previous = SelectObject(dc, bitmap);
        ReleaseDC(panel, screen);
        SendMessageW(panel, WM_PRINTCLIENT, dc as WPARAM, PRF_CLIENT as LPARAM);
        (dc, bitmap, previous)
    }
    unsafe fn release_render((dc, bitmap, previous): (HDC, HBITMAP, HGDIOBJ)) {
        SelectObject(dc, previous);
        DeleteObject(bitmap);
        DeleteDC(dc);
    }

    #[test]
    fn screen_area_choices_list_presets_before_custom_portions() {
        let monitor = |left: i32, primary: bool| mapping::MonitorEntry {
            rect: RECT {
                left,
                top: 0,
                right: left + 2560,
                bottom: 1440,
            },
            primary,
            device: String::new(),
        };
        // One monitor: listed once, as the whole screen.
        let single = [monitor(0, true)];
        let list = choices(single.len());
        assert_eq!(
            list,
            [
                Choice::Monitor(0),
                Choice::ScreenPortion,
                Choice::Window,
                Choice::Diagnostic
            ]
        );
        let labels: Vec<_> = list.iter().map(|&c| choice_label(c, &single)).collect();
        assert_eq!(
            labels,
            [
                "Screen · 2560×1440",
                "Portion of screen…",
                "Application window",
                "Test area (inside PadBridge)"
            ]
        );
        // Several monitors: full desktop, then each monitor, main first.
        let monitors = [monitor(0, true), monitor(-2560, false)];
        let list = choices(monitors.len());
        assert_eq!(
            list,
            [
                Choice::Desktop,
                Choice::Monitor(0),
                Choice::Monitor(1),
                Choice::ScreenPortion,
                Choice::Window,
                Choice::Diagnostic
            ]
        );
        let labels: Vec<_> = list.iter().map(|&c| choice_label(c, &monitors)).collect();
        assert_eq!(labels[1], "Monitor 1 · 2560×1440 (main)");
        assert_eq!(labels[2], "Monitor 2 · 2560×1440");
        assert_eq!(choices(0)[0], Choice::Desktop);
        assert!(Choice::Window.uses_window() && !Choice::ScreenPortion.uses_window());
    }

    /// The Wacom-style diagram is drawn for screen targets, with the red
    /// mapped region and touchpad surface where the pure layout puts them.
    #[test]
    #[ignore = "creates an idle native window and reads its rendering; no device, capture or injection"]
    fn native_gui_mapping_diagram_draws_region_and_touchpad() {
        unsafe fn with<R>(f: impl FnOnce(&mut Gui) -> R) -> R {
            GUI.with(|slot| f(slot.borrow_mut().as_mut().unwrap()))
        }
        unsafe fn choose(hwnd: HWND, choice: Choice) {
            let index = with(|g| g.choices.iter().position(|&c| c == choice).unwrap());
            let item = GetDlgItem(hwnd, TARGET as i32);
            SendMessageW(item, CB_SETCURSEL, index, 0);
            SendMessageW(
                hwnd,
                WM_COMMAND,
                (CBN_SELCHANGE as usize) << 16 | TARGET,
                item as LPARAM,
            );
        }
        const RED: u32 = 0x002020E0;
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register("PadBridgeGui", Some(window_proc), 0).unwrap();
            register(
                "PadBridgeDiagnosticPanel",
                Some(panel_proc),
                CS_HREDRAW | CS_VREDRAW,
            )
            .unwrap();
            let hwnd = CreateWindowExW(
                0,
                wide("PadBridgeGui").as_ptr(),
                wide("PadBridge mapping diagram check").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                40,
                40,
                100,
                100,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            let g = Gui::new(hwnd).unwrap();
            SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
            let panel = g.panel;
            GUI.with(|slot| *slot.borrow_mut() = Some(g));
            with(|g| {
                g.layout();
                g.update_controls();
            });
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            pump_for(Duration::from_millis(100));
            // The decorative group must not intercept the receiver's area.
            let mut receiver_point = POINT { x: 100, y: 100 };
            assert_ne!(ClientToScreen(panel, &mut receiver_point), 0);
            assert_ne!(ScreenToClient(hwnd, &mut receiver_point), 0);
            assert_eq!(ChildWindowFromPointEx(hwnd, receiver_point, 0), panel);
            let monitors = mapping::monitors();
            assert!(!monitors.is_empty());
            with(|g| {
                assert_eq!(g.monitors.len(), monitors.len());
                assert_eq!(g.choices, choices(monitors.len()));
            });

            choose(hwnd, Choice::Monitor(0));
            // Let the requested thumbnail capture (150 ms) happen.
            pump_for(Duration::from_millis(250));
            InvalidateRect(panel, null(), 0);
            pump_for(Duration::from_millis(100));
            let (area, dpi) = with(|g| {
                assert_eq!(
                    g.target().unwrap(),
                    MappingTarget::ScreenRectangle {
                        bounds: monitors[0].rect
                    }
                );
                assert!(g.target_info().starts_with(&format!(
                    "{}×",
                    monitors[0].rect.right - monitors[0].rect.left
                )));
                let mut rc = RECT::default();
                GetClientRect(g.panel, &mut rc);
                (
                    RECT {
                        left: g.px(16),
                        top: g.px(190),
                        right: rc.right - g.px(16),
                        bottom: rc.bottom - g.px(12),
                    },
                    g.dpi,
                )
            });
            let rects: Vec<RECT> = monitors.iter().map(|m| m.rect).collect();
            let l = crate::diagram::layout(area, mapping::virtual_screen(), &rects, 1.6, dpi)
                .expect("panel large enough for the diagram");
            let region = l.screen.rect(monitors[0].rect);
            let render = render_panel(panel);
            let mid_y = (region.top + region.bottom) / 2;
            let mid_x = (l.surface.left + l.surface.right) / 2;
            assert_eq!(GetPixel(render.0, region.left, mid_y), RED, "region frame");
            assert_eq!(GetPixel(render.0, region.right - 1, mid_y), RED);
            assert_eq!(
                GetPixel(render.0, mid_x, l.surface.top),
                RED,
                "surface frame"
            );
            assert_eq!(
                GetPixel(render.0, mid_x, l.pad.top + 1),
                0x00404040,
                "pad body"
            );
            release_render(render);
            std::fs::create_dir_all("artifacts").unwrap();
            save_client_bitmap(hwnd, "artifacts/diagram-monitor.bmp");

            // The thumbnail is captured on demand, not on every repaint.
            let captured = with(|g| {
                assert!(g.thumbnail.is_some(), "first diagram captures a thumbnail");
                assert!(g.thumbnail_due.is_none());
                g.thumbnail_captures
            });
            for _ in 0..20 {
                InvalidateRect(panel, null(), 0);
                pump_for(Duration::from_millis(75));
            }
            assert_eq!(with(|g| g.thumbnail_captures), captured, "idle repaints");
            // Resizing reuses (scales) the existing thumbnail.
            let mut original = RECT::default();
            GetWindowRect(hwnd, &mut original);
            SetWindowPos(hwnd, null_mut(), 0, 0, 1100, 760, SWP_NOMOVE | SWP_NOZORDER);
            pump_for(Duration::from_millis(300));
            assert_eq!(with(|g| g.thumbnail_captures), captured, "resize");
            // Changing the screen area requests exactly one new capture.
            choose(hwnd, Choice::ScreenPortion);
            pump_for(Duration::from_millis(300));
            InvalidateRect(panel, null(), 0);
            pump_for(Duration::from_millis(100));
            assert_eq!(
                with(|g| g.thumbnail_captures),
                captured + 1,
                "selection change"
            );
            // Restore the size the remaining pixel checks were computed for.
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                original.right - original.left,
                original.bottom - original.top,
                SWP_NOMOVE | SWP_NOZORDER,
            );
            choose(hwnd, Choice::Monitor(0));
            pump_for(Duration::from_millis(100));

            // An unconfigured target shows guidance and no mapping connectors.
            choose(hwnd, Choice::ScreenPortion);
            pump_for(Duration::from_millis(100));
            let render = render_panel(panel);
            assert_ne!(GetPixel(render.0, mid_x, l.surface.top), RED);
            release_render(render);
            save_client_bitmap(hwnd, "artifacts/diagram-unselected.bmp");

            // Touchpad area: without physical geometry, injection can only
            // stretch; raw-only previews still resolve every mode.
            choose(hwnd, Choice::Monitor(0));
            with(|g| {
                g.geometry = None;
                SendMessageW(g.item(INJECT), BM_SETCHECK, 1, 0);
                for (index, ok) in [(0, false), (1, true), (2, false)] {
                    SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, index, 0);
                    let resolved = g.resolve_target();
                    assert_eq!(
                        resolved.is_ok(),
                        ok,
                        "pad fit {index}: {:?}",
                        resolved.err()
                    );
                    if let Ok(t) = resolved {
                        assert_eq!(t.fit, mapping::PadFit::Stretch);
                        assert_eq!(mapping::tuple(t.bounds), mapping::tuple(monitors[0].rect));
                        assert_eq!(t.input, NormRect::FULL);
                    } else {
                        assert!(resolved.err().unwrap().contains("stretch"));
                    }
                }
                SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
                SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, 0, 0);
            });

            // Back to the diagnostic receiver: no diagram.
            choose(hwnd, Choice::Diagnostic);
            pump_for(Duration::from_millis(100));
            let render = render_panel(panel);
            assert_ne!(GetPixel(render.0, mid_x, l.pad.top + 1), 0x00404040);
            release_render(render);

            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            GUI.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }

    unsafe fn assert_live_layout(hwnd: HWND) {
        GUI.with(|slot| {
            let slot = slot.borrow();
            let g = slot.as_ref().unwrap();
            // Without DCX_CLIPCHILDREN: some sample points lie inside group
            // boxes, which are child windows that paint the same background.
            let dc = GetDCEx(hwnd, null_mut(), DCX_CACHE);
            let background = GetSysColor(COLOR_BTNFACE);
            for (x, y) in [
                (140, 67),
                (244, 87),
                (250, 214),
                (240, 314),
                (252, 378),
                (260, 230),
                (600, 12),
            ] {
                assert_eq!(
                    GetPixel(dc, g.px(x), g.px(y)),
                    background,
                    "live dialog background differs at ({x}, {y})"
                );
            }
            ReleaseDC(hwnd, dc);
            // The receiver canvas exists only for the test area with a known
            // touchpad shape; without a touchpad only the dialog is checked.
            if let Some(geometry) = g.geometry.filter(|_| g.rect.right > g.rect.left) {
                let panel_dc = GetDC(g.panel);
                let mut canvas = POINT {
                    x: g.rect.left,
                    y: g.rect.top,
                };
                ScreenToClient(g.panel, &mut canvas);
                assert_eq!(
                    GetPixel(panel_dc, canvas.x, canvas.y + 1),
                    0x00808080,
                    "canvas border moved or was not repainted"
                );
                ReleaseDC(g.panel, panel_dc);
                let mut origin = POINT::default();
                ClientToScreen(g.panel, &mut origin);
                assert!(g.rect.left - origin.x >= g.px(16));
                assert!(g.rect.top - origin.y >= g.px(190));
                let aspect = geometry.aspect().unwrap();
                assert!(
                    ((g.rect.right - g.rect.left) as f64
                        - (g.rect.bottom - g.rect.top) as f64 * aspect)
                        .abs()
                        <= aspect.max(1.0)
                );
            }
            let mut client = RECT::default();
            GetClientRect(hwnd, &mut client);
            for c in &g.controls {
                let mut bounds = RECT::default();
                GetWindowRect(c.hwnd, &mut bounds);
                let mut top = POINT {
                    x: bounds.left,
                    y: bounds.top,
                };
                let mut bottom = POINT {
                    x: bounds.right,
                    y: bounds.bottom,
                };
                ScreenToClient(hwnd, &mut top);
                ScreenToClient(hwnd, &mut bottom);
                assert!(
                    top.x >= 0
                        && top.y >= 0
                        && bottom.x <= client.right
                        && bottom.y <= client.bottom,
                    "control clipped at {}/{}, {}/{}",
                    top.x,
                    top.y,
                    bottom.x,
                    bottom.y
                );
            }
        });
    }

    #[test]
    #[ignore = "shows an idle native window briefly and checks actual displayed pixels after moving/minimizing/restoring; no capture or settings changes"]
    fn native_gui_fixed_size_repaints_background_and_canvas() {
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register("PadBridgeGui", Some(window_proc), 0).unwrap();
            register(
                "PadBridgeDiagnosticPanel",
                Some(panel_proc),
                CS_HREDRAW | CS_VREDRAW,
            )
            .unwrap();
            let hwnd = CreateWindowExW(
                0,
                wide("PadBridgeGui").as_ptr(),
                wide("PadBridge fixed-size regression").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                20,
                20,
                100,
                100,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            let mut gui = Gui::new(hwnd).unwrap();
            gui.refresh_devices();
            let dpi = gui.dpi;
            let expected = (gui.px(DEFAULT_CLIENT.0), gui.px(DEFAULT_CLIENT.1));
            GUI.with(|slot| *slot.borrow_mut() = Some(gui));
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                let test_area = g.choices.iter().position(|&c| c == Choice::Diagnostic);
                SendMessageW(g.item(TARGET), CB_SETCURSEL, test_area.unwrap(), 0);
                g.layout();
                g.update_controls();
            });
            // Even when created with a sizable frame, the window becomes fixed.
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            assert_eq!(
                style & (WS_THICKFRAME | WS_MAXIMIZEBOX),
                0,
                "window is resizable"
            );
            assert_ne!(
                style & WS_CLIPCHILDREN,
                0,
                "background paints under controls"
            );
            let client = || {
                let mut rc = RECT::default();
                GetClientRect(hwnd, &mut rc);
                (rc.right, rc.bottom)
            };
            assert_eq!(client(), expected, "client size");
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                20,
                20,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
            pump_for(Duration::from_millis(150));
            assert_live_layout(hwnd);
            save_client_preview(hwnd);
            let mut before = RECT::default();
            GetWindowRect(hwnd, &mut before);
            SetWindowPos(
                hwnd,
                null_mut(),
                before.left + 70,
                before.top + 35,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            pump_for(Duration::from_millis(100));
            assert_live_layout(hwnd);
            ShowWindow(hwnd, SW_MINIMIZE);
            pump_for(Duration::from_millis(100));
            ShowWindow(hwnd, SW_RESTORE);
            pump_for(Duration::from_millis(150));
            assert_eq!(client(), expected, "client size after restore");
            assert_live_layout(hwnd);
            save_client_preview(hwnd);
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            GUI.with(|slot| {
                slot.borrow_mut().take();
            });
            println!("native_fixed_size=passed client={expected:?} dpi={dpi}");
        }
    }

    #[test]
    #[ignore = "temporarily changes four gesture settings via the GUI session, captures raw input only, and verifies restoration"]
    fn native_gui_background_guard_session_restores() {
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register("PadBridgeGui", Some(window_proc), 0).unwrap();
            register(
                "PadBridgeDiagnosticPanel",
                Some(panel_proc),
                CS_HREDRAW | CS_VREDRAW,
            )
            .unwrap();
            let hwnd = CreateWindowExW(
                0,
                wide("PadBridgeGui").as_ptr(),
                wide("PadBridge background integration check").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                80,
                80,
                1120,
                760,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            let mut g = Gui::new(hwnd).unwrap();
            g.refresh_devices();
            SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
            SendMessageW(g.item(GUARD), CB_SETCURSEL, 2, 0);
            GUI.with(|slot| *slot.borrow_mut() = Some(g));
            GUI.with(|slot| slot.borrow_mut().as_mut().unwrap().layout());
            SetTimer(hwnd, 1, 30, None);
            SendMessageW(hwnd, WM_COMMAND, START, 0);
            pump_for(Duration::from_secs(4));
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(g.session.is_some(), "{}", g.last_status);
                let s = g.shared.lock().unwrap();
                assert!(
                    s.guard_status.contains("ACTIVE"),
                    "{} / {}",
                    s.guard_status,
                    s.error
                );
            });
            SendMessageW(hwnd, WM_COMMAND, STOP, 0);
            pump_for(Duration::from_secs(4));
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(g.session.is_none(), "guard did not finish");
                assert!(!g.restart_blocked, "{}", g.last_status);
                let s = g.shared.lock().unwrap();
                assert_eq!(s.guard_status, "Windows gestures restored");
                assert!(!s.recovery_pending);
                assert!(s.error.is_empty(), "{}", s.error);
            });
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            GUI.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }

    unsafe fn save_client_preview(hwnd: HWND) {
        save_client_bitmap(hwnd, "artifacts/gui-preview.bmp");
    }
    unsafe fn save_client_bitmap(hwnd: HWND, path: &str) {
        let mut rc = RECT::default();
        GetClientRect(hwnd, &mut rc);
        let screen = GetDC(hwnd);
        let dc = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, rc.right, rc.bottom);
        let previous = SelectObject(dc, bitmap);
        SendMessageW(hwnd, WM_PRINTCLIENT, dc as WPARAM, 0);
        let mut child = GetWindow(hwnd, GW_CHILD);
        while !child.is_null() {
            let mut child_rect = RECT::default();
            GetWindowRect(child, &mut child_rect);
            let mut at = POINT {
                x: child_rect.left,
                y: child_rect.top,
            };
            ScreenToClient(hwnd, &mut at);
            let state = SaveDC(dc);
            SetViewportOrgEx(dc, at.x, at.y, null_mut());
            SendMessageW(
                child,
                WM_PRINT,
                dc as WPARAM,
                (PRF_CLIENT | PRF_CHILDREN | PRF_ERASEBKGND) as LPARAM,
            );
            RestoreDC(dc, state);
            child = GetWindow(child, GW_HWNDNEXT);
        }
        SelectObject(dc, previous);
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = rc.right;
        info.bmiHeader.biHeight = -rc.bottom;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut pixels = vec![0u8; (rc.right * rc.bottom * 4) as usize];
        assert_ne!(
            GetDIBits(
                dc,
                bitmap,
                0,
                rc.bottom as u32,
                pixels.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS
            ),
            0
        );
        let mut bmp = vec![0u8; 54];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&((54 + pixels.len()) as u32).to_le_bytes());
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&rc.right.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-rc.bottom).to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&pixels);
        std::fs::create_dir_all("artifacts").unwrap();
        std::fs::write(path, bmp).unwrap();
        DeleteObject(bitmap);
        DeleteDC(dc);
        ReleaseDC(hwnd, screen);
    }

    unsafe fn pump_for(duration: Duration) {
        let start = Instant::now();
        while start.elapsed() < duration {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message != WM_QUIT {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "creates native controls and raw-only capture sessions; run alone on an interactive Windows desktop"]
    fn native_gui_sessions_restart_and_hotkey_messages_stop_cleanly() {
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register("PadBridgeGui", Some(window_proc), 0).unwrap();
            register(
                "PadBridgeDiagnosticPanel",
                Some(panel_proc),
                CS_HREDRAW | CS_VREDRAW,
            )
            .unwrap();
            let hwnd = CreateWindowExW(
                0,
                wide("PadBridgeGui").as_ptr(),
                wide("PadBridge GUI integration check").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                80,
                80,
                1280,
                860,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!hwnd.is_null());
            let mut g = Gui::new(hwnd).unwrap();
            g.refresh_devices();
            assert!(!g.devices.is_empty(), "requires a local Precision Touchpad");
            SendMessageW(g.item(INJECT), BM_SETCHECK, 0, 0);
            GUI.with(|slot| *slot.borrow_mut() = Some(g));
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                g.layout();
                g.update_controls();
            });
            SetTimer(hwnd, 1, 30, None);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            pump_for(Duration::from_millis(100));
            save_client_preview(hwnd);
            // Real WM_COMMAND dispatch, actual Raw Input worker/window and logger.
            // These checks never initialize or submit native touch injection.
            for _ in 0..2 {
                SendMessageW(hwnd, WM_COMMAND, START, 0);
                pump_for(Duration::from_millis(250));
                GUI.with(|slot| {
                    let slot = slot.borrow();
                    let g = slot.as_ref().unwrap();
                    assert!(g.session.is_some());
                    assert!(!g.session.as_ref().unwrap().worker_done());
                    assert_eq!(IsWindowEnabled(g.item(DEVICE)), 0);
                });
                SendMessageW(hwnd, WM_COMMAND, STOP, 0);
                pump_for(Duration::from_millis(650));
                GUI.with(|slot| {
                    let slot = slot.borrow();
                    let g = slot.as_ref().unwrap();
                    assert!(g.session.is_none());
                    assert!(!g.controller.enabled);
                    assert!(!g.restart_blocked);
                    let mut path = vec![0u16; 1024];
                    let len = GetWindowTextW(g.log_path, path.as_mut_ptr(), path.len() as i32);
                    let label = String::from_utf16_lossy(&path[..len as usize]);
                    let out = label.strip_prefix("Logs: ").unwrap();
                    let log =
                        std::fs::read_to_string(PathBuf::from(out).join("events.log")).unwrap();
                    assert!(log.contains("REGISTER_RAW_INPUT success"));
                    assert!(log.contains("WORKER_STOP active_contacts=0 cleanup_pending=false"));
                    assert!(log.contains("SUMMARY"));
                    assert!(PathBuf::from(out).join("summary.txt").exists());
                });
            }
            // Reserve a chord to verify that a registration conflict cannot arm.
            let key = VK_F8 as u32;
            if RegisterHotKey(hwnd, 99, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, key) != 0 {
                GUI.with(|slot| {
                    let mut slot = slot.borrow_mut();
                    let g = slot.as_mut().unwrap();
                    SendMessageW(g.item(MODE), CB_SETCURSEL, 1, 0);
                });
                SendMessageW(hwnd, WM_COMMAND, START, 0);
                GUI.with(|slot| assert!(!slot.borrow().as_ref().unwrap().controller.enabled));
                UnregisterHotKey(hwnd, 99);
            }
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                SendMessageW(g.item(MODE), CB_SETCURSEL, 1, 0);
            });
            SendMessageW(hwnd, WM_COMMAND, START, 0);
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(g.controller.enabled, "Toggle failed: {}", g.last_status);
            });
            SendMessageW(hwnd, WM_HOTKEY, HOTKEY as usize, 0);
            pump_for(Duration::from_millis(250));
            GUI.with(|slot| assert!(slot.borrow().as_ref().unwrap().session.is_some()));
            SendMessageW(hwnd, WM_HOTKEY, HOTKEY as usize, 0);
            pump_for(Duration::from_millis(650));
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(g.session.is_none());
                assert!(g.controller.enabled);
            });
            SendMessageW(hwnd, WM_HOTKEY, EMERGENCY as usize, 0);
            GUI.with(|slot| {
                let slot = slot.borrow();
                let g = slot.as_ref().unwrap();
                assert!(!g.controller.enabled);
                assert!(!g.hotkey_registered);
            });
            // A late Hold notification after the chord has been released cannot start.
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                SendMessageW(g.item(MODE), CB_SETCURSEL, 2, 0);
            });
            SendMessageW(hwnd, WM_COMMAND, START, 0);
            SendMessageW(hwnd, WM_HOTKEY, HOTKEY as usize, 0);
            pump_for(Duration::from_millis(100));
            GUI.with(|slot| assert!(slot.borrow().as_ref().unwrap().session.is_none()));
            SendMessageW(hwnd, WM_HOTKEY, EMERGENCY as usize, 0);
            // Closing an active session must keep the receiver alive for drain.
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                SendMessageW(g.item(MODE), CB_SETCURSEL, 0, 0);
            });
            SendMessageW(hwnd, WM_COMMAND, START, 0);
            pump_for(Duration::from_millis(250));
            GUI.with(|slot| assert!(slot.borrow().as_ref().unwrap().session.is_some()));
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            assert_ne!(IsWindow(hwnd), 0);
            pump_for(Duration::from_millis(650));
            assert_eq!(IsWindow(hwnd), 0);
            GUI.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }
}
