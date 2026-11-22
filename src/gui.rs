//! Native Win32 shell with a touch preview and an embedded test area.
use crate::{
    control::{Controller, TakeoverMode},
    diagram,
    experiment::{self, wide, Session, Shared, Stats},
    hid::{self, winerr, Device},
    mapping::{self, MappingTarget, NormRect, WindowEntry},
    overlay,
    settings::{self, Area, Fit, Settings, Target as SavedTarget, TargetKind},
};
use std::{
    cell::RefCell,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            InitCommonControlsEx, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, TOOLTIPS_CLASSW,
            TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETMAXTIPWIDTH, TTM_UPDATETIPTEXTW,
            TTS_NOPREFIX, TTTOOLINFOW,
        },
        HiDpi::*,
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::*,
    },
};

const DEVICE: usize = 101;
const REFRESH: usize = 102;
const MODE: usize = 103;
const KEY: usize = 104;
const PAD_GESTURES: usize = 105;
const INJECT: usize = 106;
const SCREEN_GESTURES: usize = 107;
const TARGET: usize = 108;
const START: usize = 109;
const STOP: usize = 110;
const HOTKEY: i32 = 1;
const EMERGENCY: i32 = 2;
const PREVIEW_GROUP: usize = 111;
const LOG_PATH: usize = 112;
const STATUS: usize = 113;
const STOP_HINT: usize = 114;
const WINDOW: usize = 115;
const AREA: usize = 116;
const TARGET_INFO: usize = 117;
const PAD_FIT: usize = 118;
const FEEDBACK: usize = 119;
const PAD_AREA: usize = 120;
const MAPPING_GROUP: usize = 125;
const SCREEN_AREA_LABEL: usize = 126;
const PAD_AREA_LABEL: usize = 127;
const WINDOW_LABEL: usize = 128;
const STATUS_DETAIL: usize = 129;
const START_MODES: [(TakeoverMode, &str); 3] = [
    (TakeoverMode::Manual, "Manual start / stop"),
    (TakeoverMode::Hold, "Hold Alt to capture"),
    (TakeoverMode::Toggle, "Toggle with shortcut"),
];
const SIDEBAR_TOP: i32 = 8;
const SIDEBAR_WIDTH: i32 = 254;
const SIDEBAR_ROW: i32 = 26;
const GROUP_FIRST_ROW: i32 = SIDEBAR_ROW;
const CAPTURE_TOP: i32 = 76;
const GESTURES_TOP: i32 = 218;
const MAPPING_TOP: i32 = 308;
const SETTINGS_BOTTOM: i32 = MAPPING_TOP + 166;
const LABEL_LEFT: i32 = 18;
const LABEL_WIDTH: i32 = 84;
const CONTROL_WIDTH: i32 = SIDEBAR_WIDTH - 20;
const COMBO_LEFT: i32 = 104;
const COMBO_WIDTH: i32 = LABEL_LEFT + CONTROL_WIDTH - COMBO_LEFT;
const FOOTER_TOP: i32 = SETTINGS_BOTTOM + 10;
const FOOTER_HEIGHT: i32 = 34;
const LOG_ROW_HEIGHT: i32 = 28;
const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;
const SS_NOTIFY: u32 = 0x0100;
const DROPDOWN_STYLE: u32 = CBS_DROPDOWNLIST as u32 | WS_TABSTOP;
const DEFAULT_CLIENT: (i32, i32) = (820, FOOTER_TOP + FOOTER_HEIGHT + 10);
const PREVIEW_MS: u32 = 1500;
const PREVIEW_LEFT: i32 = SIDEBAR_TOP + SIDEBAR_WIDTH + 8;
const PANEL_LEFT: i32 = PREVIEW_LEFT + 8;
const PANEL_TOP: i32 = SIDEBAR_TOP + GROUP_FIRST_ROW;
const DIAGRAM_MAX_SIZE: (i32, i32) = (420, 318);
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
    Preview,
    FullWidth,
    Footer,
    Action,
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
    status_tooltip: HWND,
    status_tip_text: RefCell<Vec<u16>>,
    log_path: HWND,
    diagnostics: bool,
    font: HFONT,
    dpi: u32,
    devices: Vec<Device>,
    geometry: Option<crate::device_info::Geometry>,
    controller: Controller,
    hotkey_registered: bool,
    emergency_registered: bool,
    session: Option<Session>,
    prewarmed: Option<crate::background_guard::Backend>,
    shared: Shared,
    rect: RECT,
    buffer: Option<crate::render::BackBuffer>,
    closing: bool,
    restart_blocked: bool,
    last_status: String,
    settings_path: Option<PathBuf>,
    windows: Vec<WindowEntry>,
    screen_rect: Option<RECT>,
    window_area: Option<NormRect>,
    pending_window_area: Option<NormRect>,
    pad_area: Option<NormRect>,
    pending_selection: Option<Pending>,
    session_diagnostic: bool,
    monitors: Vec<mapping::MonitorEntry>,
    choices: Vec<Choice>,
    thumbnail: Option<diagram::Thumbnail>,
    thumbnail_next: Instant,
    thumbnail_job: Option<mpsc::Receiver<Option<diagram::Thumbnail>>>,
    #[cfg(test)]
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
    /// Pad2Touch's own receiver, for checking touch delivery.
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
        Choice::Diagnostic => "Test area".into(),
    }
}
/// What an in-progress overlay selection will configure.
#[derive(Clone, Copy)]
enum Pending {
    Screen,
    WindowArea { handle: usize, client: RECT },
    Touchpad(RECT),
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

pub fn run(diagnostics: bool) -> Result<(), String> {
    let result = run_window(diagnostics);
    if let Err(error) = &result {
        unsafe {
            MessageBoxW(
                null_mut(),
                wide(error).as_ptr(),
                wide("Pad2Touch could not start").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    result
}

fn run_window(diagnostics: bool) -> Result<(), String> {
    experiment::EXIT.store(false, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            return Err(winerr("SetProcessDpiAwarenessContext(GUI)"));
        }
        register("Pad2TouchGui", Some(window_proc), 0)?;
        register(
            "Pad2TouchDiagnosticPanel",
            Some(panel_proc),
            CS_HREDRAW | CS_VREDRAW,
        )?;
        let hwnd = CreateWindowExW(
            0,
            wide("Pad2TouchGui").as_ptr(),
            wide("Pad2Touch Control Panel").as_ptr(),
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
        let result = Gui::new(hwnd, diagnostics);
        let gui = match result {
            Ok(gui) => gui,
            Err(e) => {
                DestroyWindow(hwnd);
                return Err(e);
            }
        };
        GUI.with(|slot| *slot.borrow_mut() = Some(gui));
        // Devices come first: a restored selection is matched by device path.
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            let gui = slot.as_mut().unwrap();
            gui.refresh_devices();
            gui.settings_path = settings::user_path();
            if let Some(stored) = gui.settings_path.as_deref().and_then(settings::load) {
                gui.apply_settings(&stored);
            }
            gui.layout();
            gui.update_controls();
        });
        if SetTimer(hwnd, 1, 30, None) == 0 {
            DestroyWindow(hwnd);
            return Err(winerr("SetTimer(GUI)"));
        }
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
    unsafe fn new(hwnd: HWND, diagnostics: bool) -> Result<Self, String> {
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
            status_tooltip: null_mut(),
            status_tip_text: RefCell::new(vec![0]),
            log_path: null_mut(),
            diagnostics,
            font: null_mut(),
            dpi,
            devices: Vec::new(),
            geometry: None,
            controller: Controller::default(),
            hotkey_registered: false,
            emergency_registered: false,
            session: None,
            prewarmed: None,
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
            last_status: String::new(),
            settings_path: None,
            windows: Vec::new(),
            screen_rect: None,
            window_area: None,
            pending_window_area: None,
            pad_area: None,
            pending_selection: None,
            session_diagnostic: true,
            monitors: Vec::new(),
            choices: Vec::new(),
            thumbnail: None,
            thumbnail_next: Instant::now(),
            thumbnail_job: None,
            #[cfg(test)]
            thumbnail_captures: 0,
            preview: Err(String::new()),
            preview_at: Instant::now(),
        };
        g.add(
            "BUTTON",
            "Input device",
            0,
            BS_GROUPBOX as u32,
            [8, SIDEBAR_TOP, SIDEBAR_WIDTH, 60],
        )?;
        g.add(
            "COMBOBOX",
            "",
            DEVICE,
            DROPDOWN_STYLE | WS_VSCROLL,
            [
                LABEL_LEFT,
                SIDEBAR_TOP + GROUP_FIRST_ROW,
                CONTROL_WIDTH - 64,
                180,
            ],
        )?;
        g.add(
            "BUTTON",
            "Refresh",
            REFRESH,
            WS_TABSTOP,
            [
                LABEL_LEFT + CONTROL_WIDTH - 58,
                SIDEBAR_TOP + GROUP_FIRST_ROW,
                58,
                24,
            ],
        )?;
        g.add(
            "BUTTON",
            "Capture",
            0,
            BS_GROUPBOX as u32,
            [8, CAPTURE_TOP, SIDEBAR_WIDTH, 134],
        )?;
        g.add(
            "STATIC",
            "Start mode",
            0,
            SS_CENTERIMAGE,
            [LABEL_LEFT, CAPTURE_TOP + GROUP_FIRST_ROW, LABEL_WIDTH, 22],
        )?;
        g.add(
            "COMBOBOX",
            "",
            MODE,
            DROPDOWN_STYLE,
            [COMBO_LEFT, CAPTURE_TOP + GROUP_FIRST_ROW, COMBO_WIDTH, 160],
        )?;
        g.fill_combo(MODE, &START_MODES.map(|(_, label)| label));
        g.add(
            "STATIC",
            "Toggle shortcut",
            0,
            SS_CENTERIMAGE,
            [
                LABEL_LEFT,
                CAPTURE_TOP + GROUP_FIRST_ROW + SIDEBAR_ROW,
                LABEL_WIDTH,
                22,
            ],
        )?;
        g.add(
            "COMBOBOX",
            "",
            KEY,
            DROPDOWN_STYLE,
            [
                COMBO_LEFT,
                CAPTURE_TOP + GROUP_FIRST_ROW + SIDEBAR_ROW,
                COMBO_WIDTH,
                160,
            ],
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
            [
                LABEL_LEFT,
                CAPTURE_TOP + GROUP_FIRST_ROW + 2 * SIDEBAR_ROW,
                CONTROL_WIDTH,
                22,
            ],
        )?;
        SendMessageW(g.item(INJECT), BM_SETCHECK, 1, 0);
        g.add(
            "BUTTON",
            "Show Windows touch feedback",
            FEEDBACK,
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            [
                LABEL_LEFT,
                CAPTURE_TOP + GROUP_FIRST_ROW + 3 * SIDEBAR_ROW,
                CONTROL_WIDTH,
                22,
            ],
        )?;
        SendMessageW(g.item(FEEDBACK), BM_SETCHECK, 1, 0);
        g.add(
            "BUTTON",
            "Windows system gestures",
            0,
            BS_GROUPBOX as u32,
            [8, GESTURES_TOP, SIDEBAR_WIDTH, 82],
        )?;
        g.add(
            "BUTTON",
            "Block touchpad gestures",
            PAD_GESTURES,
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            [
                LABEL_LEFT,
                GESTURES_TOP + GROUP_FIRST_ROW,
                CONTROL_WIDTH,
                22,
            ],
        )?;
        g.add(
            "BUTTON",
            "Block touchscreen gestures",
            SCREEN_GESTURES,
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            [
                LABEL_LEFT,
                GESTURES_TOP + GROUP_FIRST_ROW + SIDEBAR_ROW,
                CONTROL_WIDTH,
                22,
            ],
        )?;
        let gesture_default = usize::from(crate::gesture_backend::overrides_enabled_by_default());
        SendMessageW(g.item(PAD_GESTURES), BM_SETCHECK, gesture_default, 0);
        SendMessageW(g.item(SCREEN_GESTURES), BM_SETCHECK, gesture_default, 0);
        g.add(
            "BUTTON",
            "Mapping",
            MAPPING_GROUP,
            BS_GROUPBOX as u32,
            [8, MAPPING_TOP, SIDEBAR_WIDTH, SETTINGS_BOTTOM - MAPPING_TOP],
        )?;
        g.add(
            "STATIC",
            "Screen area",
            SCREEN_AREA_LABEL,
            SS_CENTERIMAGE,
            [LABEL_LEFT, MAPPING_TOP + GROUP_FIRST_ROW, LABEL_WIDTH, 22],
        )?;
        g.add(
            "COMBOBOX",
            "",
            TARGET,
            DROPDOWN_STYLE | WS_VSCROLL,
            [COMBO_LEFT, MAPPING_TOP + GROUP_FIRST_ROW, COMBO_WIDTH, 240],
        )?;
        g.fill_targets();
        g.add(
            "STATIC",
            "Window",
            WINDOW_LABEL,
            SS_CENTERIMAGE,
            [LABEL_LEFT, MAPPING_TOP + 52, LABEL_WIDTH, 22],
        )?;
        g.add(
            "COMBOBOX",
            "",
            WINDOW,
            DROPDOWN_STYLE | WS_VSCROLL,
            [COMBO_LEFT, MAPPING_TOP + 52, COMBO_WIDTH, 240],
        )?;
        g.add(
            "BUTTON",
            "Select area…",
            AREA,
            WS_TABSTOP,
            [LABEL_LEFT, MAPPING_TOP + 78, 100, 24],
        )?;
        g.add(
            "STATIC",
            "",
            TARGET_INFO,
            SS_CENTERIMAGE,
            [LABEL_LEFT + 106, MAPPING_TOP + 78, CONTROL_WIDTH - 106, 24],
        )?;
        g.add(
            "STATIC",
            "Touchpad area",
            PAD_AREA_LABEL,
            SS_CENTERIMAGE,
            [LABEL_LEFT, MAPPING_TOP + 108, LABEL_WIDTH, 22],
        )?;
        g.add(
            "COMBOBOX",
            "",
            PAD_FIT,
            DROPDOWN_STYLE,
            [COMBO_LEFT, MAPPING_TOP + 108, COMBO_WIDTH, 160],
        )?;
        g.fill_combo(PAD_FIT, &["Force proportions", "Full", "Portion…"]);
        SendMessageW(g.item(PAD_FIT), CB_SETCURSEL, 0, 0);
        g.add(
            "BUTTON",
            "Select pad area…",
            PAD_AREA,
            WS_TABSTOP,
            [COMBO_LEFT, MAPPING_TOP + 134, COMBO_WIDTH, 24],
        )?;
        g.add(
            "STATIC",
            "",
            STATUS,
            SS_CENTERIMAGE,
            [8, FOOTER_TOP, 84, FOOTER_HEIGHT],
        )?;
        g.add(
            "STATIC",
            "",
            STATUS_DETAIL,
            SS_CENTERIMAGE | SS_ENDELLIPSIS | SS_NOTIFY,
            [100, FOOTER_TOP, 310, FOOTER_HEIGHT],
        )?;
        g.create_status_tooltip();
        let emergency_label = g.add(
            "STATIC",
            "Stop: Ctrl + Alt + Esc",
            STOP_HINT,
            SS_CENTERIMAGE,
            [422, FOOTER_TOP, 132, FOOTER_HEIGHT],
        )?;
        if diagnostics {
            g.log_path = g.add(
                "EDIT",
                "Logs: no session started",
                LOG_PATH,
                ES_READONLY as u32 | ES_AUTOHSCROLL as u32 | WS_TABSTOP,
                [8, FOOTER_TOP, 0, 22],
            )?;
        }
        g.add(
            "BUTTON",
            "Start capture",
            START,
            BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
            [136, 0, 122, 26],
        )?;
        g.add("BUTTON", "Stop / disarm", STOP, WS_TABSTOP, [8, 0, 122, 26])?;
        g.add(
            "BUTTON",
            if diagnostics {
                "Diagnostics / touch receiver"
            } else {
                "Touch preview"
            },
            PREVIEW_GROUP,
            BS_GROUPBOX as u32,
            [PREVIEW_LEFT, SIDEBAR_TOP, 0, 0],
        )?;
        g.panel = CreateWindowExW(
            0,
            wide("Pad2TouchDiagnosticPanel").as_ptr(),
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
            set_text(emergency_label, "Stop: Esc here");
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
        let size = outer_size(DEFAULT_CLIENT.0, g.client_height(), g.dpi);
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

    fn client_height(&self) -> i32 {
        DEFAULT_CLIENT.1 + if self.diagnostics { LOG_ROW_HEIGHT } else { 0 }
    }

    fn status_tool(&self, text: &mut [u16]) -> TTTOOLINFOW {
        TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: self.hwnd,
            uId: unsafe { self.item(STATUS_DETAIL) } as usize,
            lpszText: text.as_mut_ptr(),
            ..Default::default()
        }
    }

    unsafe fn create_status_tooltip(&mut self) {
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        });
        self.status_tooltip = CreateWindowExW(
            0,
            TOOLTIPS_CLASSW,
            null(),
            WS_POPUP | TTS_NOPREFIX,
            0,
            0,
            0,
            0,
            self.hwnd,
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if !self.status_tooltip.is_null() {
            let tool = self.status_tool(&mut self.status_tip_text.borrow_mut());
            SendMessageW(
                self.status_tooltip,
                TTM_ADDTOOLW,
                0,
                &tool as *const _ as LPARAM,
            );
        }
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
                PREVIEW_GROUP => Anchor::Preview,
                LOG_PATH => Anchor::FullWidth,
                STATUS | STATUS_DETAIL | STOP_HINT => Anchor::Footer,
                START | STOP => Anchor::Action,
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
    unsafe fn selected_mode(&self) -> TakeoverMode {
        START_MODES
            .get(self.selection(MODE))
            .map_or(TakeoverMode::Manual, |&(mode, _)| mode)
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
        if !self.status_tooltip.is_null() {
            SendMessageW(
                self.status_tooltip,
                TTM_SETMAXTIPWIDTH,
                0,
                self.px(420) as LPARAM,
            );
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
                MappingTarget::TestArea {
                    diagnostics: self.diagnostics,
                }
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
        SendMessageW(
            self.item(DEVICE),
            CB_SETDROPPEDWIDTH,
            self.px(340) as WPARAM,
            0,
        );
        // Only controls whose bounds change are moved, and Windows' own
        // invalidation is used: only uncovered background is erased, and resized
        // group boxes and the panel repaint from back buffers.
        let flags = SWP_NOZORDER | SWP_NOACTIVATE;
        let preview_bottom = self.px(SETTINGS_BOTTOM);
        let preview_right = rc.right - self.px(8);
        let footer_offset = self.client_height() - DEFAULT_CLIENT.1;
        let mut placements = Vec::with_capacity(self.controls.len() + 1);
        for c in &self.controls {
            let (x, y, w, h) = match c.anchor {
                Anchor::Fixed => (
                    self.px(c.x),
                    self.px(c.y),
                    self.px(c.width),
                    self.px(c.height),
                ),
                Anchor::Preview => (
                    self.px(c.x),
                    self.px(c.y),
                    preview_right - self.px(c.x),
                    preview_bottom - self.px(c.y),
                ),
                Anchor::FullWidth => (
                    self.px(c.x),
                    self.px(c.y),
                    preview_right - self.px(c.x),
                    self.px(c.height),
                ),
                Anchor::Footer => (
                    self.px(c.x),
                    self.px(c.y + footer_offset),
                    self.px(c.width),
                    self.px(c.height),
                ),
                Anchor::Action => (
                    rc.right - self.px(c.x) - self.px(c.width),
                    self.px(FOOTER_TOP + footer_offset + (FOOTER_HEIGHT - c.height) / 2),
                    self.px(c.width),
                    self.px(c.height),
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
            (preview_right - self.px(8) - x).max(1),
            (preview_bottom - self.px(12) - y).max(1),
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
        self.refresh_preview_rect();
        // Painting stays queued: a synchronous WM_PAINT here would find GUI's
        // RefCell borrowed. See WM_SIZE in `window_proc` for the immediate update.
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
                let labels: Vec<_> = self
                    .devices
                    .iter()
                    .map(|device| match device.name() {
                        Some(name) if self.diagnostics => {
                            format!("{name} ({:04X}:{:04X})", device.vid, device.pid)
                        }
                        Some(name) => name,
                        None => format!("Touchpad ({:04X}:{:04X})", device.vid, device.pid),
                    })
                    .collect();
                for (i, label) in labels.iter().enumerate() {
                    let label = if labels
                        .iter()
                        .filter(|other| *other == label)
                        .nth(1)
                        .is_some()
                    {
                        format!("{} · {label}", i + 1)
                    } else {
                        label.clone()
                    };
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
                    self.last_status.clear();
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
            Choice::Diagnostic => Ok(MappingTarget::TestArea {
                diagnostics: self.diagnostics,
            }),
        }
    }
    fn aspect(&self) -> Option<f64> {
        self.geometry.and_then(|g| g.aspect().ok())
    }
    unsafe fn pad_fit(&self) -> mapping::PadFit {
        match self.selection(PAD_FIT) {
            1 => mapping::PadFit::Stretch,
            2 => mapping::PadFit::Portion(self.pad_area),
            _ => mapping::PadFit::FillArea,
        }
    }
    /// Resolve the configured target exactly as a session would.
    unsafe fn resolve_target(&self) -> Result<mapping::ResolvedTarget, String> {
        let target = self.target()?;
        let fit = self.pad_fit();
        if self.aspect().is_none() && self.checked(INJECT) && fit.needs_shape() {
            return Err(
                "Touchpad shape unavailable: choose Touchpad area Full or clear injection.".into(),
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
    fn diagram_area(&self, rc: &RECT) -> RECT {
        let mut area = RECT {
            left: self.px(16),
            top: self.px(if self.diagnostics {
                mapping::DIAGNOSTICS_HEADER_HEIGHT
            } else {
                mapping::TEST_AREA_INSET
            }),
            right: rc.right - self.px(16),
            bottom: rc.bottom - self.px(12),
        };
        let width = (area.right - area.left).min(self.px(DIAGRAM_MAX_SIZE.0));
        let height = (area.bottom - area.top).min(self.px(DIAGRAM_MAX_SIZE.1));
        area.left += (area.right - area.left - width) / 2;
        area.top += (area.bottom - area.top - height) / 2;
        area.right = area.left + width;
        area.bottom = area.top + height;
        area
    }

    unsafe fn paint_panel(&mut self, hwnd: HWND, dc: HDC, rc: &RECT) {
        FillRect(dc, rc, GetSysColorBrush(COLOR_BTNFACE));
        let diagram = match &self.session {
            Some(_) => !self.session_diagnostic,
            None => self.choice() != Choice::Diagnostic,
        };
        if !diagram {
            if self.diagnostics {
                experiment::paint_diagnostics(hwnd, dc, rc, &self.shared, self.rect, self.dpi);
            }
            experiment::paint_touch_canvas(hwnd, dc, &self.shared, self.rect, self.diagnostics);
            return;
        }
        let area = self.diagram_area(rc);
        if self.diagnostics {
            let mut origin = POINT::default();
            ClientToScreen(hwnd, &mut origin);
            let text_limit = RECT {
                left: area.left + origin.x,
                top: area.top + origin.y,
                right: area.right + origin.x,
                bottom: area.bottom + origin.y,
            };
            experiment::paint_diagnostics(hwnd, dc, rc, &self.shared, text_limit, self.dpi);
        }
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
            self.thumbnail.as_ref(),
        );
    }
    unsafe fn tick_thumbnail(&mut self) {
        let foreground = GetForegroundWindow() == self.hwnd
            && IsIconic(self.hwnd) == 0
            && !overlay::active()
            && self.choice() != Choice::Diagnostic;
        if let Some(job) = &self.thumbnail_job {
            let result = job.try_recv();
            if matches!(result, Err(mpsc::TryRecvError::Empty)) {
                return;
            }
            self.thumbnail_job = None;
            if let Ok(Some(thumbnail)) = result {
                if foreground {
                    self.thumbnail = Some(thumbnail);
                    InvalidateRect(self.panel, null(), 0);
                }
            }
        }
        let now = Instant::now();
        if foreground && now >= self.thumbnail_next {
            self.thumbnail_next = now + Duration::from_secs(1);
            let virt = mapping::virtual_screen();
            let hwnd = self.hwnd as usize;
            let (tx, rx) = mpsc::channel();
            if std::thread::Builder::new()
                .spawn(move || {
                    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                    let result = (GetForegroundWindow() == hwnd as HWND)
                        .then(|| diagram::Thumbnail::capture(virt).ok())
                        .flatten();
                    let _ = tx.send(result);
                })
                .is_ok()
            {
                self.thumbnail_job = Some(rx);
                #[cfg(test)]
                {
                    self.thumbnail_captures += 1;
                }
            }
        }
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
    unsafe fn window_selection_changed(&mut self) {
        self.window_area = None;
        if let Some(handle) = self.selected_window() {
            if let Some(area) = self.pending_window_area.take() {
                let target = MappingTarget::WindowRectangle { handle, area };
                if target.resolve(self.panel).is_ok() {
                    self.window_area = Some(area);
                }
            } else {
                self.save_setting(|stored| stored.window_area = None);
            }
        }
        self.refresh_diagram_preview();
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
                self.save_setting(|stored| stored.window_area = None);
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
        if let Some(Pending::Touchpad(surface)) = pending {
            match NormRect::from_screen(surface, selected) {
                Ok(area) => {
                    self.pad_area = Some(area);
                    self.save_setting(|stored| stored.pad_area = Some(area.into()));
                    self.last_status = "Touchpad area selected.".into();
                    self.refresh_preview_rect();
                }
                Err(error) => self.last_status = error,
            }
            return;
        }
        let applied = match pending {
            Some(Pending::Screen) => mapping::validate_screen_target(selected).map(|r| {
                self.screen_rect = Some(r);
                self.save_setting(|stored| stored.screen_rect = Some(r.into()));
            }),
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
                        self.save_setting(|stored| stored.window_area = Some(area.into()));
                        Ok(())
                    })
                }
            }
            Some(Pending::Touchpad(_)) | None => Ok(()),
        };
        if let Err(e) = applied.and_then(|_| self.show_target_preview()) {
            self.last_status = e;
        }
    }
    unsafe fn select_pad_area(&mut self) -> Result<(), String> {
        if self.session.is_some()
            || self.controller.enabled
            || overlay::active()
            || self.choice() == Choice::Diagnostic
        {
            return Err("The touchpad area cannot be selected right now.".into());
        }
        let mut rc = RECT::default();
        GetClientRect(self.panel, &mut rc);
        let monitors: Vec<RECT> = self.monitors.iter().map(|monitor| monitor.rect).collect();
        let layout = diagram::layout(
            self.diagram_area(&rc),
            mapping::virtual_screen(),
            &monitors,
            self.aspect().unwrap_or(1.6),
            self.dpi,
        )
        .ok_or("The touchpad diagram is unavailable.")?;
        let mut origin = POINT::default();
        if ClientToScreen(self.panel, &mut origin) == 0 {
            return Err(winerr("Locate the touchpad diagram"));
        }
        let mut bounds = layout.surface;
        OffsetRect(&mut bounds, origin.x, origin.y);
        overlay::begin(
            self.hwnd,
            bounds,
            "Drag to select the active touchpad area.",
        )?;
        self.pending_selection = Some(Pending::Touchpad(bounds));
        self.last_status = "Selecting touchpad area…".into();
        Ok(())
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
            Choice::Window if self.pending_window_area.is_some() => "Saved part".into(),
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
        self.controller.mode = self.selected_mode();
        if self.controller.mode == TakeoverMode::Toggle {
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
        self.controller
            .enable(self.controller.mode == TakeoverMode::Hold && Self::alt_down());
        if self.controller.mode == TakeoverMode::Hold
            && (self.checked(PAD_GESTURES) || self.checked(SCREEN_GESTURES))
        {
            self.prewarmed =
                crate::background_guard::Backend::spawn(self.checked(PAD_GESTURES)).ok();
        }
        self.last_status = if self.controller.mode == TakeoverMode::Hold {
            "Hold Alt to capture. Release Alt to stop.".into()
        } else {
            "Shortcut enabled. Press it to capture.".into()
        };
    }
    unsafe fn key(&self) -> u32 {
        VK_F8 as u32 + self.selection(KEY) as u32
    }
    unsafe fn alt_down() -> bool {
        GetAsyncKeyState(VK_MENU as i32) < 0
    }
    unsafe fn disarm(&mut self, reason: &str) {
        self.controller.disarm();
        self.prewarmed = None;
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
                    "Cannot preserve the physical touchpad aspect: {error}. Choose Touchpad area Full to inject without it."
                ))
            }
            Err(_) => spec.resolve_fitted(self.panel, None, fit)?,
        };
        overlay::hide_outline();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let out = PathBuf::from(format!("artifacts/runs/gui-{}-{stamp}", std::process::id()));
        let session = Session::start(
            device,
            target,
            experiment::SessionSettings {
                inject: self.checked(INJECT),
                suppress_touchpad: self.checked(PAD_GESTURES),
                suppress_touchscreen: self.checked(SCREEN_GESTURES),
                touch_feedback: self.checked(FEEDBACK),
                diagnostics: self.diagnostics,
                prewarmed: self.prewarmed.clone(),
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
        if self.diagnostics {
            set_text(self.log_path, &format!("Logs: {}", out.display()));
        }
        self.last_status = if spec.is_diagnostic() {
            "Starting capture.".into()
        } else {
            let warning = spec
                .window_handle()
                .and_then(mapping::integrity_warning)
                .map(|w| format!(" {w}"))
                .unwrap_or_default();
            format!("Mapping to {}.{warning}", mapping::describe(target.bounds))
        };
        Ok(())
    }
    unsafe fn tick(&mut self) {
        if self.controller.enabled && self.controller.mode == TakeoverMode::Hold {
            self.controller.sample_alt(Self::alt_down());
        }
        if let Some(session) = self.session.as_ref() {
            if !self.controller.requested && !session.stopping() {
                session.request_stop("Capture stopped.");
            }
            if session.stopped_unexpectedly() {
                self.disarm("Capture stopped unexpectedly.");
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
                    "Unresolved contacts or settings recovery. New sessions are blocked.".into();
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
        self.tick_thumbnail();
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
        if !focus.is_null() && IsWindowEnabled(focus) != 0 && IsWindowVisible(focus) != 0 {
            return;
        }
        let next = [STOP, START]
            .into_iter()
            .map(|id| self.item(id))
            .find(|&h| IsWindowEnabled(h) != 0)
            .unwrap_or(self.hwnd);
        SetFocus(next);
    }
    /// Apply a stored snapshot to the controls. Values that no longer fit this
    /// machine (a display that is gone, a missing pad area) keep their defaults
    /// instead of failing the launch.
    unsafe fn apply_settings(&mut self, stored: &Settings) {
        for (id, value) in [
            (INJECT, stored.inject),
            (FEEDBACK, stored.touch_feedback),
            (PAD_GESTURES, stored.suppress_touchpad),
            (SCREEN_GESTURES, stored.suppress_touchscreen),
        ] {
            if let Some(value) = value {
                SendMessageW(self.item(id), BM_SETCHECK, usize::from(value), 0);
            }
        }
        if let Some(mode) = stored.start_mode {
            let mode = TakeoverMode::from(mode);
            if let Some(index) = START_MODES.iter().position(|&(entry, _)| entry == mode) {
                SendMessageW(self.item(MODE), CB_SETCURSEL, index, 0);
            }
        }
        // Only the four validated chords exist; anything else is ignored.
        if let Some(shortcut) = stored
            .shortcut
            .filter(|vk| (VK_F8 as u32..=VK_F11 as u32).contains(vk))
        {
            SendMessageW(
                self.item(KEY),
                CB_SETCURSEL,
                (shortcut - VK_F8 as u32) as usize,
                0,
            );
        }
        if let Some(index) = stored
            .device
            .as_ref()
            .and_then(|path| self.devices.iter().position(|d| &d.path == path))
        {
            SendMessageW(self.item(DEVICE), CB_SETCURSEL, index, 0);
            self.select_device_info();
        }
        self.screen_rect = stored
            .screen_rect
            .and_then(|r| mapping::validate_screen_target(r.into()).ok());
        self.pending_window_area = stored.window_area.and_then(Area::norm_rect);
        self.pad_area = stored.pad_area.and_then(Area::norm_rect);
        if let Some(fit) = stored.pad_fit {
            let fit = match fit {
                // Portion without a usable pad area cannot map anything.
                Fit::Portion if self.pad_area.is_none() => Fit::FillArea,
                fit => fit,
            };
            SendMessageW(self.item(PAD_FIT), CB_SETCURSEL, fit.selection(), 0);
        }
        if let Some(target) = &stored.target {
            let choice = match (target.kind, &target.monitor) {
                (TargetKind::Desktop, _) => Some(Choice::Desktop),
                (TargetKind::Monitor, Some(device)) => self
                    .monitors
                    .iter()
                    .position(|m| &m.device == device)
                    .map(Choice::Monitor),
                (TargetKind::Monitor, None) => None,
                (TargetKind::ScreenPortion, _) => self.screen_rect.map(|_| Choice::ScreenPortion),
                // A window handle cannot survive a restart, so only the kind is
                // restored and the list starts empty.
                (TargetKind::Window, _) => Some(Choice::Window),
                (TargetKind::TestArea, _) => Some(Choice::Diagnostic),
            };
            if let Some(choice) = choice {
                self.select_choice(choice);
            }
            if choice == Some(Choice::Window) {
                SendMessageW(self.item(WINDOW), CB_SETCURSEL, usize::MAX, 0);
            }
        }
        self.refresh_preview_rect();
        self.refresh_diagram_preview();
    }
    unsafe fn saved_target(&self) -> SavedTarget {
        match self.choice() {
            Choice::Desktop => SavedTarget::kind(TargetKind::Desktop),
            Choice::Monitor(i) => SavedTarget {
                kind: TargetKind::Monitor,
                monitor: self.monitors.get(i).map(|m| m.device.clone()),
            },
            Choice::ScreenPortion => SavedTarget::kind(TargetKind::ScreenPortion),
            Choice::Window => SavedTarget::kind(TargetKind::Window),
            Choice::Diagnostic => SavedTarget::kind(TargetKind::TestArea),
        }
    }
    fn save_setting(&self, edit: impl FnOnce(&mut Settings)) {
        if let Some(path) = &self.settings_path {
            let _ = settings::update(path, edit);
        }
    }
    unsafe fn update_controls(&self) {
        let locked =
            self.session.is_some() || self.controller.enabled || self.closing || overlay::active();
        for id in [
            DEVICE,
            REFRESH,
            MODE,
            INJECT,
            PAD_GESTURES,
            SCREEN_GESTURES,
            TARGET,
            PAD_FIT,
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
        let portion = matches!(self.pad_fit(), mapping::PadFit::Portion(_));
        let mode = self.selected_mode();
        EnableWindow(
            self.item(PAD_AREA),
            i32::from(!locked && portion && choice != Choice::Diagnostic),
        );
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
        let target_error = if portion && self.pad_area.is_none() {
            Some(if choice == Choice::Diagnostic {
                "Choose a screen or window to define a touchpad portion first.".into()
            } else {
                "Click Select pad area… and drag on the touchpad diagram.".into()
            })
        } else {
            self.target().err()
        };
        let target_error = target_error.or_else(|| {
            (!self.devices.is_empty()
                && self.checked(INJECT)
                && self.geometry.is_none()
                && self.pad_fit().needs_shape())
            .then(|| "Touchpad dimensions unavailable. Choose Touchpad area Full.".into())
        });
        EnableWindow(
            self.item(KEY),
            i32::from(!locked && mode == TakeoverMode::Toggle),
        );
        EnableWindow(
            self.item(START),
            i32::from(
                !locked
                    && !self.restart_blocked
                    && !self.devices.is_empty()
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
            if mode == TakeoverMode::Manual {
                "Start capture"
            } else {
                "Enable shortcut"
            },
        );
        let state = if self.closing {
            "Closing"
        } else if self.session.as_ref().is_some_and(|s| s.stopping()) {
            "Stopping"
        } else if self.session.is_some() && !self.checked(INJECT) {
            "Raw capture"
        } else if self.session.is_some() {
            let s = self.shared.lock().unwrap();
            if s.armed {
                "Capturing"
            } else if s.guard_status.starts_with("Preparing") {
                "Preparing capture"
            } else if s.raw == 0 {
                "Capturing"
            } else {
                "Lift all fingers"
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
        set_text(self.item(STATUS), state);
        let single_line;
        let info = if info.contains(['\r', '\n']) {
            single_line = info.split_whitespace().collect::<Vec<_>>().join(" ");
            &single_line
        } else {
            info.as_str()
        };
        if set_text(self.item(STATUS_DETAIL), info) && !self.status_tooltip.is_null() {
            let mut text = wide(info);
            let tool = self.status_tool(&mut text);
            SendMessageW(
                self.status_tooltip,
                TTM_UPDATETIPTEXTW,
                0,
                &tool as *const _ as LPARAM,
            );
            *self.status_tip_text.borrow_mut() = text;
        }
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
            if !self.status_tooltip.is_null() {
                DestroyWindow(self.status_tooltip);
            }
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
                                let path =
                                    g.devices.get(g.selection(DEVICE)).map(|d| d.path.clone());
                                g.save_setting(|stored| stored.device = path);
                            }
                            TARGET if notification == CBN_SELCHANGE => {
                                if g.choice().uses_window() {
                                    g.refresh_windows();
                                }
                                g.refresh_preview_rect();
                                g.refresh_diagram_preview();
                                g.save_setting(|stored| stored.target = Some(g.saved_target()));
                            }
                            WINDOW if notification == CBN_DROPDOWN => g.refresh_windows(),
                            WINDOW if notification == CBN_SELCHANGE => {
                                g.window_selection_changed();
                            }
                            AREA if notification == BN_CLICKED => g.area_action(),
                            PAD_AREA if notification == BN_CLICKED => {
                                if let Err(error) = g.select_pad_area() {
                                    g.last_status = error;
                                }
                            }
                            PAD_FIT if notification == CBN_SELCHANGE => {
                                g.refresh_preview_rect();
                                g.refresh_diagram_preview();
                                g.save_setting(|stored| {
                                    stored.pad_fit =
                                        Some(Fit::from_selection(g.selection(PAD_FIT)));
                                });
                            }
                            MODE if notification == CBN_SELCHANGE => {
                                g.save_setting(|stored| {
                                    stored.start_mode = Some(g.selected_mode().into());
                                });
                            }
                            KEY if notification == CBN_SELCHANGE => {
                                g.save_setting(|stored| stored.shortcut = Some(g.key()));
                            }
                            INJECT if notification == BN_CLICKED => {
                                g.save_setting(|stored| stored.inject = Some(g.checked(INJECT)));
                            }
                            FEEDBACK if notification == BN_CLICKED => {
                                g.save_setting(|stored| {
                                    stored.touch_feedback = Some(g.checked(FEEDBACK));
                                });
                            }
                            PAD_GESTURES if notification == BN_CLICKED => {
                                g.save_setting(|stored| {
                                    stored.suppress_touchpad = Some(g.checked(PAD_GESTURES));
                                });
                            }
                            SCREEN_GESTURES if notification == BN_CLICKED => {
                                g.save_setting(|stored| {
                                    stored.suppress_touchscreen = Some(g.checked(SCREEN_GESTURES));
                                });
                            }
                            _ => {}
                        }
                        g.update_controls();
                    }
                    WM_HOTKEY if w == EMERGENCY as usize => {
                        g.disarm("Global emergency stop requested.")
                    }
                    WM_HOTKEY if w == HOTKEY as usize => {
                        g.controller.hotkey();
                        g.tick();
                    }
                    WM_TIMER => g.tick(),
                    WM_CLOSE => {
                        g.closing = true;
                        g.disarm("Closing after contact cleanup.");
                        if g.session.is_none() {
                            destroy = true;
                        }
                    }
                    overlay::WM_SELECTION_DONE => {
                        g.selection_done();
                        g.refresh_diagram_preview();
                        g.update_controls();
                    }
                    WM_DISPLAYCHANGE => {
                        if g.session.is_some() && g.checked(INJECT) {
                            g.disarm("Display configuration changed. Capture stopped.");
                        }
                        if g.session.is_none() && !g.controller.enabled {
                            g.fill_targets();
                        }
                        g.update_controls();
                        g.thumbnail = None;
                        g.refresh_diagram_preview();
                    }
                    WM_ENTERSIZEMOVE => {
                        // External targets do not depend on Pad2Touch's own window.
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
                        let size = outer_size(DEFAULT_CLIENT.0, g.client_height(), g.dpi);
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
mod tests;
