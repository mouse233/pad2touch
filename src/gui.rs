//! Native Win32 shell; the diagnostic receiver is a child of the main window.
use crate::{
    control::{Controller, TakeoverMode},
    experiment::{self, wide, Session, Shared, Stats},
    hid::{self, winerr, Device},
    mapping::MappingTarget,
};
use std::{
    cell::RefCell,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
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
const DEFAULT_CLIENT: (i32, i32) = (820, 480);
const MIN_CLIENT: (i32, i32) = (760, 470);
const PANEL_LEFT: i32 = 274;
const PANEL_TOP: i32 = 28;

#[derive(Clone, Copy)]
enum Anchor {
    Fixed,
    Diagnostics,
    Log,
    Hint,
}

struct Control {
    hwnd: HWND,
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
}
thread_local! { static GUI: RefCell<Option<Gui>> = const { RefCell::new(None) }; }

unsafe fn outer_size(width: i32, height: i32, dpi: u32) -> POINT {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width * dpi as i32 / 96,
        bottom: height * dpi as i32 / 96,
    };
    AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, 0, 0, dpi);
    POINT {
        x: rect.right - rect.left,
        y: rect.bottom - rect.top,
    }
}

unsafe fn register(class: &str, proc: WNDPROC) -> Result<(), String> {
    let name = wide(class);
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
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
        register("PadBridgeGui", Some(window_proc))?;
        register("PadBridgeDiagnosticPanel", Some(panel_proc))?;
        let hwnd = CreateWindowExW(
            0,
            wide("PadBridgeGui").as_ptr(),
            wide("PadBridge Control Panel").as_ptr(),
            WS_OVERLAPPEDWINDOW,
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
            // Esc works even while a native child control has keyboard focus.
            if msg.message == WM_KEYDOWN && msg.wParam == VK_ESCAPE as usize {
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
            [8, 100, 250, 130],
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
        g.add(
            "BUTTON",
            "Windows system gestures",
            0,
            BS_GROUPBOX as u32,
            [8, 238, 250, 82],
        )?;
        g.add(
            "COMBOBOX",
            "",
            GUARD,
            CBS_DROPDOWNLIST as u32 | WS_TABSTOP,
            [18, 258, 230, 160],
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
            [18, 295, 230, 16],
        )?;
        g.add(
            "BUTTON",
            "Mapping target",
            0,
            BS_GROUPBOX as u32,
            [8, 328, 250, 56],
        )?;
        g.add(
            "COMBOBOX",
            "",
            TARGET,
            CBS_DROPDOWNLIST as u32 | WS_TABSTOP,
            [18, 348, 230, 160],
        )?;
        g.fill_combo(
            TARGET,
            &[
                "Diagnostic area",
                "Screen rectangle (planned)",
                "Application window (planned)",
                "Window rectangle (planned)",
            ],
        );
        g.add(
            "BUTTON",
            "Start capture",
            START,
            BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
            [8, 396, 122, 26],
        )?;
        g.add(
            "BUTTON",
            "Stop / disarm",
            STOP,
            WS_TABSTOP,
            [136, 396, 122, 26],
        )?;
        g.status = g.add("STATIC", "", STATUS, 0, [8, 432, 250, 32])?;
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
        g.set_font();
        let mut work = RECT::default();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
        let size = outer_size(DEFAULT_CLIENT.0, DEFAULT_CLIENT.1, g.dpi);
        let w = size.x.min((work.right - work.left - 32).max(1));
        let h = size.y.min((work.bottom - work.top - 32).max(1));
        SetWindowPos(
            hwnd,
            null_mut(),
            work.left + 20,
            work.top + 20,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
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
        let hwnd = CreateWindowExW(
            if class == "BUTTON" && style & 0xF == BS_GROUPBOX as u32 {
                WS_EX_TRANSPARENT
            } else {
                0
            },
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
        self.controls.push(Control {
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
        if let Some(target) = self
            .geometry
            .and_then(|g| g.aspect().ok())
            .and_then(|aspect| {
                MappingTarget::Diagnostics
                    .resolve_with_aspect(self.panel, aspect)
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
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | SWP_NOCOPYBITS;
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
        let mut batch = BeginDeferWindowPos(placements.len() as i32);
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
        self.refresh_preview_rect();
        // Queued redraw avoids reentrant WM_PAINT while GUI's RefCell is borrowed.
        RedrawWindow(
            self.hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
        );
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
    fn target(&self, index: usize) -> MappingTarget {
        match index {
            1 => MappingTarget::ScreenRectangle {
                bounds: RECT::default(),
            },
            2 => MappingTarget::Window { handle: 0 },
            3 => MappingTarget::WindowRectangle {
                handle: 0,
                client_bounds: RECT::default(),
            },
            _ => MappingTarget::Diagnostics,
        }
    }
    unsafe fn enable(&mut self) {
        if self.restart_blocked || self.closing {
            return;
        }
        if self.selection(TARGET) != 0 {
            self.last_status = "This mapping is planned. Choose Diagnostic area to capture.".into();
            return;
        }
        if self.devices.is_empty() {
            self.last_status = "No touchpad available. Refresh the device list.".into();
            return;
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
        if self.checked(INJECT) && (IsIconic(self.hwnd) != 0 || IsWindowVisible(self.hwnd) == 0) {
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
        let target = match geometry {
            Ok(g) => self
                .target(self.selection(TARGET))
                .resolve_with_aspect(self.panel, g.aspect()?)?,
            Err(error) if self.checked(INJECT) => {
                return Err(format!(
                    "Cannot preserve the physical touchpad aspect: {error}"
                ))
            }
            Err(_) => self.target(self.selection(TARGET)).resolve(self.panel)?,
        };
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let out = PathBuf::from(format!("artifacts/gui-{stamp}"));
        let session = Session::start(
            device,
            target,
            self.checked(INJECT),
            self.selection(CONTACTS) + 1,
            self.selection(GUARD) == 1,
            self.selection(GUARD) == 2,
            out.clone(),
        )?;
        self.geometry = geometry.ok();
        self.refresh_preview_rect();
        self.shared = session.shared.clone();
        self.session = Some(session);
        SetWindowTextW(
            self.log_path,
            wide(&format!("Logs: {}", out.display())).as_ptr(),
        );
        self.last_status = "Starting. Touch the pad, then lift all fingers to arm.".into();
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
        let mut s = self.shared.lock().unwrap();
        s.foreground = GetForegroundWindow() == self.hwnd;
        drop(s);
        self.update_controls();
        InvalidateRect(self.panel, null(), 0);
    }
    unsafe fn update_controls(&self) {
        let locked = self.session.is_some() || self.controller.enabled || self.closing;
        for id in [DEVICE, REFRESH, MODE, CONTACTS, INJECT, GUARD, TARGET] {
            EnableWindow(self.item(id), i32::from(!locked));
        }
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
                    && (!self.checked(INJECT) || self.geometry.is_some())
                    && self.selection(TARGET) == 0,
            ),
        );
        EnableWindow(
            self.item(STOP),
            i32::from(!self.closing && (self.controller.enabled || self.session.is_some())),
        );
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
        let info = if self.selection(TARGET) != 0 {
            "This mapping is planned. Choose Diagnostic area."
        } else {
            &self.last_status
        };
        set_text(self.status, &format!("{state}\r\n{info}"));
    }
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
        WM_COMMAND | WM_HOTKEY | WM_TIMER | WM_CLOSE | WM_SIZE | WM_MOVE | WM_ENTERSIZEMOVE
        | WM_EXITSIZEMOVE | WM_DISPLAYCHANGE | WM_DPICHANGED => {
            let mut destroy = false;
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
                            MODE | TARGET | KEY if notification == CBN_SELCHANGE => {}
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
                    WM_ENTERSIZEMOVE | WM_DISPLAYCHANGE => {
                        if g.session.is_some() && g.checked(INJECT) {
                            g.disarm("Window or display changed. Capture stopped.");
                        }
                    }
                    WM_MOVE => {
                        g.refresh_preview_rect();
                    }
                    WM_EXITSIZEMOVE => {
                        g.layout();
                    }
                    WM_SIZE => {
                        if g.session.is_some() && g.checked(INJECT) {
                            g.disarm("Window resized. Capture stopped.");
                        }
                        g.layout();
                    }
                    WM_DPICHANGED => {
                        if g.session.is_some() && g.checked(INJECT) {
                            g.disarm("Display scale changed. Capture stopped.");
                        }
                        g.dpi = (w & 0xffff) as u32;
                        g.set_font();
                        let r = &*(l as *const RECT);
                        SetWindowPos(
                            hwnd,
                            null_mut(),
                            r.left,
                            r.top,
                            r.right - r.left,
                            r.bottom - r.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        g.layout();
                    }
                    _ => {}
                }
            });
            if destroy {
                DestroyWindow(hwnd);
            }
            return 0;
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(l as *mut MINMAXINFO);
            info.ptMinTrackSize =
                outer_size(MIN_CLIENT.0, MIN_CLIENT.1, GetDpiForWindow(hwnd).max(96));
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
        WM_PRINTCLIENT => {
            GUI.with(|slot| {
                if let Ok(slot) = slot.try_borrow() {
                    if let Some(g) = slot.as_ref() {
                        let mut rc = RECT::default();
                        GetClientRect(hwnd, &mut rc);
                        experiment::paint_diagnostics(
                            hwnd, w as HDC, &rc, &g.shared, g.rect, g.dpi,
                        );
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
                let buffer = g.buffer.as_ref().unwrap();
                experiment::paint_diagnostics(hwnd, buffer.dc, &rc, &g.shared, g.rect, g.dpi);
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

    unsafe fn assert_live_layout(hwnd: HWND) {
        GUI.with(|slot| {
            let slot = slot.borrow();
            let g = slot.as_ref().unwrap();
            let dc = GetDC(hwnd);
            let background = GetSysColor(COLOR_BTNFACE);
            for (x, y) in [
                (140, 67),
                (244, 87),
                (250, 214),
                (240, 314),
                (240, 378),
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
            let aspect = g.geometry.unwrap().aspect().unwrap();
            assert!(
                ((g.rect.right - g.rect.left) as f64
                    - (g.rect.bottom - g.rect.top) as f64 * aspect)
                    .abs()
                    <= aspect.max(1.0)
            );
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
    #[ignore = "shows an idle native window briefly and checks actual displayed pixels after resizing/moving/maximizing; no capture or settings changes"]
    fn native_gui_live_resize_repaints_background_and_canvas() {
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register("PadBridgeGui", Some(window_proc)).unwrap();
            register("PadBridgeDiagnosticPanel", Some(panel_proc)).unwrap();
            let hwnd = CreateWindowExW(
                0,
                wide("PadBridgeGui").as_ptr(),
                wide("PadBridge resize regression").as_ptr(),
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
            GUI.with(|slot| *slot.borrow_mut() = Some(gui));
            GUI.with(|slot| {
                let mut slot = slot.borrow_mut();
                let g = slot.as_mut().unwrap();
                g.layout();
                g.update_controls();
            });
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
            for (index, (width, height)) in [(1000, 620), MIN_CLIENT, (900, 550), DEFAULT_CLIENT]
                .into_iter()
                .enumerate()
            {
                let size = outer_size(width, height, dpi);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    30 + index as i32 * 12,
                    30 + index as i32 * 8,
                    size.x,
                    size.y,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                pump_for(Duration::from_millis(100));
                assert_live_layout(hwnd);
            }
            let mut before = RECT::default();
            // Coalesce several resize invalidations before dispatching WM_PAINT,
            // as happens while dragging an edge quickly.
            for step in 0..12 {
                let size = outer_size(
                    DEFAULT_CLIENT.0 + step * 8,
                    DEFAULT_CLIENT.1 + step * 4,
                    dpi,
                );
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    40,
                    40,
                    size.x,
                    size.y,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            pump_for(Duration::from_millis(100));
            assert_live_layout(hwnd);
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
            ShowWindow(hwnd, SW_MAXIMIZE);
            pump_for(Duration::from_millis(100));
            assert_live_layout(hwnd);
            ShowWindow(hwnd, SW_RESTORE);
            pump_for(Duration::from_millis(100));
            assert_live_layout(hwnd);
            save_client_preview(hwnd);
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            GUI.with(|slot| {
                slot.borrow_mut().take();
            });
            println!("native_live_resize_sequences=8 background_and_canvas=passed dpi={dpi}");
        }
    }

    #[test]
    #[ignore = "temporarily changes four gesture settings via the GUI session, captures raw input only, and verifies restoration"]
    fn native_gui_background_guard_session_restores() {
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register("PadBridgeGui", Some(window_proc)).unwrap();
            register("PadBridgeDiagnosticPanel", Some(panel_proc)).unwrap();
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
        std::fs::write("artifacts/gui-preview.bmp", bmp).unwrap();
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
            register("PadBridgeGui", Some(window_proc)).unwrap();
            register("PadBridgeDiagnosticPanel", Some(panel_proc)).unwrap();
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
