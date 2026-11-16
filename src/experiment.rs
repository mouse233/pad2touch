use crate::{
    decode::{Assembler, Contact},
    hid::{self, winerr, Device},
    injection::Injector,
    logging::Log,
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    mem::size_of,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    Security::*,
    System::{Console::*, LibraryLoader::GetModuleHandleW, Threading::*},
    UI::{
        HiDpi::*,
        Input::{Pointer::*, *},
        WindowsAndMessaging::*,
    },
};

pub static EXIT: AtomicBool = AtomicBool::new(false);
pub struct Options {
    pub mode: String,
    pub seconds: u64,
    pub out: PathBuf,
    pub device: usize,
    pub inject: bool,
}
#[derive(Default)]
pub struct Stats {
    pub device: String,
    pub device_label: String,
    pub mode: String,
    pub raw: u64,
    pub frames: u64,
    pub peak: usize,
    pub parse_errors: u64,
    pub incomplete: u64,
    pub down: u64,
    pub update: u64,
    pub up: u64,
    pub touch_info: u64,
    pub injected: u64,
    pub inject_errors: u64,
    pub retries: u64,
    pub last: String,
    pub error: String,
    pub points: VecDeque<(i32, i32, bool)>,
    pub api_us: Vec<u128>,
    pub capture_us: Vec<u128>,
    pub scans: Vec<u32>,
}
type Shared = Arc<Mutex<Stats>>;
struct Ui {
    shared: Shared,
    log: Log,
    stop: Arc<AtomicBool>,
    start: Instant,
    seconds: u64,
    rect: RECT,
}
thread_local! {static UI:RefCell<Option<Ui>>=const{RefCell::new(None)};static INPUT:RefCell<Option<Worker>>=const{RefCell::new(None)};}
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
unsafe extern "system" fn ctrl(_: u32) -> i32 {
    EXIT.store(true, Ordering::Relaxed);
    1
}
unsafe fn make_window(
    class: &str,
    title: &str,
    proc: WNDPROC,
    visible: bool,
) -> Result<HWND, String> {
    let name = wide(class);
    let title = wide(title);
    let module = GetModuleHandleW(null());
    if module.is_null() {
        return Err(winerr("GetModuleHandleW"));
    }
    let wc = WNDCLASSW {
        lpfnWndProc: proc,
        hInstance: module,
        lpszClassName: name.as_ptr(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
        ..Default::default()
    };
    if RegisterClassW(&wc) == 0 {
        return Err(winerr("RegisterClassW"));
    }
    let hwnd = CreateWindowExW(
        if visible { WS_EX_TOPMOST } else { 0 },
        name.as_ptr(),
        title.as_ptr(),
        if visible {
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX
        } else {
            0
        },
        80,
        80,
        1000,
        700,
        if visible { null_mut() } else { HWND_MESSAGE },
        null_mut(),
        module,
        null(),
    );
    if hwnd.is_null() {
        return Err(winerr("CreateWindowExW"));
    }
    Ok(hwnd)
}
pub fn run(opt: Options) -> Result<(), String> {
    let experiment_start = Instant::now();
    EXIT.store(false, Ordering::Relaxed);
    let (log, writer) = Log::open(&opt.out)?;
    log.event(format!(
        "RUN mode={} inject={} seconds={} source={}",
        opt.mode,
        opt.inject || opt.mode == "self-test",
        opt.seconds,
        if opt.mode == "self-test" {
            "SYNTHETIC_API_TEST_NOT_HARDWARE"
        } else {
            "PHYSICAL_RAW_INPUT"
        }
    ));
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0 {
            let mut elevation = TOKEN_ELEVATION::default();
            let mut bytes = 0;
            if GetTokenInformation(
                token,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut bytes,
            ) != 0
            {
                log.event(format!(
                    "PROCESS token_elevated={} pid={}",
                    elevation.TokenIsElevated,
                    GetCurrentProcessId()
                ));
            } else {
                log.event(winerr("GetTokenInformation(TokenElevation)"));
            }
            if CloseHandle(token) == 0 {
                log.event(winerr("CloseHandle(token)"));
            }
        } else {
            log.event(winerr("OpenProcessToken"));
        }
    }
    let shared: Shared = Arc::new(Mutex::new(Stats {
        mode: opt.mode.clone(),
        ..Default::default()
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let mut devices = hid::discover()?;
    for (i, d) in devices.iter().enumerate() {
        log.event(format!("DEVICE index={i} {}", d.summary()));
    }
    let device = if opt.mode == "capture" {
        if opt.device >= devices.len() {
            return Err(format!(
                "device index {} unavailable: {} Raw Input PTP collections",
                opt.device,
                devices.len()
            ));
        }
        Some(devices.remove(opt.device))
    } else {
        None
    };
    if let Some(d) = &device {
        let f = d.fields()?;
        std::fs::write(
            opt.out.join("descriptor-fields.txt"),
            f.iter().map(|f| format!("{f:?}\n")).collect::<String>(),
        )
        .map_err(|e| e.to_string())?;
        // Opaque Windows preparsed data, NOT the USB HID report descriptor.
        let pp = unsafe { std::slice::from_raw_parts(d.pp.as_ptr().cast::<u8>(), d.pp.len() * 8) };
        std::fs::write(opt.out.join("preparsed-data.bin"), pp).map_err(|e| e.to_string())?;
        let mut s = shared.lock().unwrap();
        s.device = d.summary();
        s.device_label = format!(
            "VID {:04X} / PID {:04X} | HID Touch Pad 0D:05 | {} bytes/report",
            d.vid, d.pid, d.caps.InputReportByteLength
        );
    }
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            return Err(winerr("SetProcessDpiAwarenessContext"));
        }
        if SetConsoleCtrlHandler(Some(ctrl), 1) == 0 {
            return Err(winerr("SetConsoleCtrlHandler"));
        }
        let hwnd = make_window(
            "PadBridgePhase0Receiver",
            &format!("PadBridge Phase 0 - {} - real PT_TOUCH receiver", opt.mode),
            Some(ui_proc),
            true,
        )?;
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client) == 0 {
            return Err(winerr("GetClientRect"));
        }
        let mut origin = POINT { x: 0, y: 0 };
        if ClientToScreen(hwnd, &mut origin) == 0 {
            return Err(winerr("ClientToScreen"));
        }
        let rect = RECT {
            left: origin.x + 40,
            top: origin.y + 220,
            right: origin.x + client.right - 40,
            bottom: origin.y + client.bottom - 40,
        };
        log.event(format!(
            "FIXED_RECT left={} top={} right={} bottom={}",
            rect.left, rect.top, rect.right, rect.bottom
        ));
        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                shared: shared.clone(),
                log: log.clone(),
                stop: stop.clone(),
                start: Instant::now(),
                seconds: opt.seconds,
                rect,
            })
        });
        // A second call also handles a launcher specifying SW_HIDE for the
        // console. This small receiver must remain visible for hit testing.
        ShowWindow(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        if SetForegroundWindow(hwnd) == 0 {
            log.event("SetForegroundWindow denied; receiver remains topmost");
        }
        if SetTimer(hwnd, 1, 50, None) == 0 {
            return Err(winerr("SetTimer(receiver)"));
        }
        let worker_shared = shared.clone();
        let worker_log = log.clone();
        let worker_stop = stop.clone();
        let target = hwnd as usize;
        let mode = opt.mode.clone();
        let inject = opt.inject || mode == "self-test";
        let worker = std::thread::spawn(move || {
            let result = worker_loop(
                device,
                mode,
                inject,
                (target, rect),
                worker_shared.clone(),
                worker_log.clone(),
                worker_stop,
            );
            if let Err(e) = &result {
                worker_log.event(format!("WORKER_ERROR {e}"));
                worker_shared.lock().unwrap().error = e.clone();
            }
            result
        });
        let mut msg = MSG::default();
        loop {
            let status = GetMessageW(&mut msg, null_mut(), 0, 0);
            if status == -1 {
                log.event(winerr("GetMessageW(receiver)"));
                break;
            }
            if status == 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        stop.store(true, Ordering::Relaxed);
        let result = worker.join().map_err(|_| "Raw Input worker panicked")?;
        if let Err(e) = &result {
            log.event(format!("WORKER_ERROR {e}"));
            shared.lock().unwrap().error = e.clone();
        }
        UI.with(|u| {
            u.borrow_mut().take();
        });
        if SetConsoleCtrlHandler(Some(ctrl), 0) == 0 {
            log.event(winerr("SetConsoleCtrlHandler(remove)"));
        }
        let s = shared.lock().unwrap();
        let dropped = log.dropped.load(Ordering::Relaxed);
        let summary = format!(
            "{}actual_duration_ms={}\n",
            summary(&s, dropped, opt.seconds),
            experiment_start.elapsed().as_millis()
        );
        println!("{summary}");
        log.event(format!("SUMMARY {}", summary.replace('\n', " | ")));
        std::fs::write(opt.out.join("summary.txt"), &summary).map_err(|e| e.to_string())?;
        drop(s);
        drop(log);
        writer.join().map_err(|_| "log writer panicked")??;
        result
    }
}
fn percentile(v: &[u128], p: usize) -> u128 {
    let mut v = v.to_vec();
    v.sort_unstable();
    if v.is_empty() {
        0
    } else {
        v[((v.len() - 1) * p) / 100]
    }
}
fn summary(s: &Stats, dropped: u64, seconds: u64) -> String {
    format!("mode={}\nsource={}\nrequested_duration_s={seconds}\ndevice={}\nraw_reports={}\ncomplete_frames={}\npeak_active_contacts={}\nparse_errors={}\nincomplete_frames={}\ninjected_frames={}\ninjection_errors={}\nnot_ready_retries={}\nreceiver_PT_TOUCH_down={}\nreceiver_PT_TOUCH_update={}\nreceiver_PT_TOUCH_up={}\nreceiver_GetPointerTouchInfo_ok={}\nlog_dropped={}\ninject_api_p95_us={}\nraw_to_inject_call_p95_us={}\nlatency_note=host software only; excludes firmware, OS delivery and physical input latency\nlast_error={}\nhardware_verdict=MANUAL_REVIEW_REQUIRED; synthetic tests never establish hardware compatibility\n",s.mode,if s.mode=="self-test"{"SYNTHETIC_API_TEST_NOT_HARDWARE"}else{"PHYSICAL_RAW_INPUT"},s.device,s.raw,s.frames,s.peak,s.parse_errors,s.incomplete,s.injected,s.inject_errors,s.retries,s.down,s.update,s.up,s.touch_info,dropped,percentile(&s.api_us,95),percentile(&s.capture_us,95),s.error)
}
unsafe extern "system" fn ui_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP => {
            UI.with(|u|{if let Some(u)=u.borrow().as_ref(){
                let mut info=POINTER_INFO::default();let id=(w&0xFFFF) as u32;
                if GetPointerInfo(id,&mut info)==0{u.log.event(winerr("GetPointerInfo"));return;}
                if info.pointerType!=PT_TOUCH{return;}
                let mut touch=POINTER_TOUCH_INFO::default();let touch_ok=GetPointerTouchInfo(id,&mut touch)!=0;
                let kind=match msg{WM_POINTERDOWN=>"DOWN",WM_POINTERUP=>"UP",_=>"UPDATE"};
                u.log.event(format!("RECEIVE_PT_TOUCH {kind} id={id} frame={} x={} y={} flags=0x{:X} touch_info={touch_ok}",info.frameId,info.ptPixelLocation.x,info.ptPixelLocation.y,info.pointerFlags));
                let mut s=u.shared.lock().unwrap();match msg{WM_POINTERDOWN=>s.down+=1,WM_POINTERUP=>s.up+=1,_=>s.update+=1};if touch_ok{s.touch_info+=1;}
                let mut p=info.ptPixelLocation;ScreenToClient(hwnd,&mut p);
                s.points.push_back((p.x,p.y,msg==WM_POINTERDOWN));if s.points.len()>5000{s.points.pop_front();}
            }});
            return 0;
        }
        WM_TIMER => {
            let close = UI.with(|u| {
                u.borrow().as_ref().is_some_and(|u| {
                    u.start.elapsed().as_secs() >= u.seconds || EXIT.load(Ordering::Relaxed)
                })
            });
            if close {
                DestroyWindow(hwnd);
            } else {
                InvalidateRect(hwnd, null(), 0);
            }
            return 0;
        }
        WM_KEYDOWN if w == 27 => {
            DestroyWindow(hwnd);
            return 0;
        }
        WM_DISPLAYCHANGE => {
            UI.with(|u| {
                if let Some(u) = u.borrow().as_ref() {
                    u.stop.store(true, Ordering::Relaxed);
                    u.shared.lock().unwrap().error = "display changed; experiment stopped".into();
                }
            });
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            return 0;
        }
        WM_DESTROY => {
            UI.with(|u| {
                if let Some(u) = u.borrow().as_ref() {
                    u.stop.store(true, Ordering::Relaxed);
                }
            });
            PostQuitMessage(0);
            return 0;
        }
        WM_PAINT => {
            paint(hwnd);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}
unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    GetClientRect(hwnd, &mut rc);
    FillRect(dc, &rc, (COLOR_WINDOW + 1) as HBRUSH);
    UI.with(|u|{if let Some(u)=u.borrow().as_ref(){let s=u.shared.lock().unwrap();
        let text=format!("PadBridge PHASE 0 | {}\n{}\nRaw reports {} | Frames {} | Peak fingers {} | Parse errors {}\nInject OK {} | Errors {} | Retry {} | SINGLE CONTACT ONLY\nReceived PT_TOUCH: DOWN {} / UPDATE {} / UP {} | TouchInfo {}\n{}\nESC: stop | Raw multi-finger capture; one injected touch | Native mouse remains active",if s.mode=="self-test"{"SYNTHETIC API TEST - NOT HARDWARE"}else{"PHYSICAL TOUCHPAD"},s.device_label,s.raw,s.frames,s.peak,s.parse_errors,s.injected,s.inject_errors,s.retries,s.down,s.update,s.up,s.touch_info,if s.error.is_empty(){&s.last}else{&s.error});
        let face=wide("Segoe UI");let font=CreateFontW(-20,0,0,0,FW_NORMAL as i32,0,0,0,DEFAULT_CHARSET as u32,0,0,CLEARTYPE_QUALITY as u32,0,face.as_ptr());
        let old_font=if !font.is_null(){SelectObject(dc,font)}else{null_mut()};
        let mut tr=RECT{left:20,top:12,right:rc.right-20,bottom:215};let t=wide(&text);SetBkMode(dc,TRANSPARENT as i32);DrawTextW(dc,t.as_ptr(),-1,&mut tr,DT_LEFT|DT_WORDBREAK|DT_NOPREFIX);
        let mut org=POINT{x:0,y:0};ClientToScreen(hwnd,&mut org);
        let boxrect=RECT{left:u.rect.left-org.x,top:u.rect.top-org.y,right:u.rect.right-org.x,bottom:u.rect.bottom-org.y};FrameRect(dc,&boxrect,GetStockObject(GRAY_BRUSH) as HBRUSH);
        let pen=CreatePen(PS_SOLID,3,0x00709010);let old=SelectObject(dc,pen);
        let mut first=true;for &(x,y,down) in &s.points {if first||down{MoveToEx(dc,x,y,null_mut());first=false;}else{LineTo(dc,x,y);}}
        SelectObject(dc,old);DeleteObject(pen);
        if !font.is_null(){SelectObject(dc,old_font);DeleteObject(font);}
    }});
    EndPaint(hwnd, &ps);
}

struct Worker {
    device: Option<Device>,
    fields: Vec<crate::decode::Field>,
    assembler: Assembler,
    shared: Shared,
    log: Log,
    stop: Arc<AtomicBool>,
    injector: Option<Injector>,
    start: Instant,
    last_raw: Instant,
    mode: String,
    target: usize,
    rect: RECT,
    synthetic_step: u32,
}
fn worker_loop(
    device: Option<Device>,
    mode: String,
    inject: bool,
    fixed_target: (usize, RECT),
    shared: Shared,
    log: Log,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let (target, rect) = fixed_target;
    let fields = device
        .as_ref()
        .map(|d| d.fields())
        .transpose()?
        .unwrap_or_default();
    let injector = if inject {
        Some(Injector::new(target, rect, log.clone())?)
    } else {
        None
    };
    let worker = Worker {
        device,
        fields,
        assembler: Assembler::default(),
        shared,
        log: log.clone(),
        stop,
        injector,
        start: Instant::now(),
        last_raw: Instant::now(),
        mode,
        target,
        rect,
        synthetic_step: 0,
    };
    unsafe {
        let hwnd = make_window("PadBridgePhase0RawInput", "", Some(input_proc), false)?;
        INPUT.with(|i| *i.borrow_mut() = Some(worker));
        let rid = RAWINPUTDEVICE {
            usUsagePage: 0x0D,
            usUsage: 0x05,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        };
        if RegisterRawInputDevices(&rid, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            INPUT.with(|i| {
                i.borrow_mut().take();
            });
            DestroyWindow(hwnd);
            return Err(winerr("RegisterRawInputDevices"));
        }
        log.event("REGISTER_RAW_INPUT success usage=0D:05 INPUTSINK DEVNOTIFY");
        if SetTimer(hwnd, 1, 16, None) == 0 {
            return Err(winerr("SetTimer(raw)"));
        }
        let mut msg = MSG::default();
        loop {
            let n = GetMessageW(&mut msg, null_mut(), 0, 0);
            if n == -1 {
                return Err(winerr("GetMessageW(raw)"));
            }
            if n == 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        INPUT.with(|i| {
            if let Some(mut worker) = i.borrow_mut().take() {
                worker.finish();
            }
        });
        let remove = RAWINPUTDEVICE {
            dwFlags: RIDEV_REMOVE,
            hwndTarget: null_mut(),
            ..rid
        };
        if RegisterRawInputDevices(&remove, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            log.event(winerr("UnregisterRawInputDevices"));
        }
        DestroyWindow(hwnd);
    }
    Ok(())
}
impl Worker {
    fn error(&mut self, e: String) {
        self.log.event(format!("ERROR {e}"));
        self.shared.lock().unwrap().error = e;
        if let Some(i) = &mut self.injector {
            if let Err(e) = i.release(true) {
                self.log.event(format!("RELEASE_ERROR {e}"));
            }
        }
        self.assembler.reset();
    }
    fn accept(&mut self, contacts: Vec<Contact>, start: Instant) {
        let live = contacts.iter().filter(|c| c.tip && c.confidence).count();
        self.log.event(format!(
            "FRAME active={live} records={} contacts={contacts:?}",
            contacts.len()
        ));
        {
            let mut s = self.shared.lock().unwrap();
            s.frames += 1;
            s.peak = s.peak.max(live);
            s.last = contacts
                .iter()
                .map(|c| {
                    format!(
                        "id={} tip={} conf={} ({},{})",
                        c.id, c.tip, c.confidence, c.x, c.y
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
        }
        if let Some(i) = &mut self.injector {
            let before = i.successes;
            if let Err(e) = i.frame(&contacts) {
                self.error(e);
            }
            let elapsed = start.elapsed().as_micros();
            if self.injector.as_ref().is_some_and(|i| i.successes > before) {
                let mut s = self.shared.lock().unwrap();
                if s.capture_us.len() < 100000 {
                    s.capture_us.push(elapsed);
                }
            }
        }
        self.sync_stats();
    }
    fn sync_stats(&self) {
        if let Some(i) = &self.injector {
            let mut s = self.shared.lock().unwrap();
            s.injected = i.successes;
            s.inject_errors = i.failures;
            s.retries = i.retries;
        }
    }
    fn raw(&mut self, raw: HRAWINPUT) {
        let start = Instant::now();
        let (handle, reports) = match hid::raw_reports(raw) {
            Ok(r) => r,
            Err(e) => {
                self.error(e);
                return;
            }
        };
        if self.mode != "capture" || self.device.as_ref().is_none_or(|d| d.handle != handle) {
            return;
        }
        for report in reports {
            self.last_raw = Instant::now();
            self.shared.lock().unwrap().raw += 1;
            self.log.event(format!(
                "RAW handle=0x{handle:X} hex={}",
                report
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<String>()
            ));
            match self.device.as_ref().unwrap().parse(&self.fields, &report) {
                Ok(Some(r)) => {
                    if let Some(scan) = r.scan {
                        let mut s = self.shared.lock().unwrap();
                        if s.scans.len() < 100000 {
                            s.scans.push(scan);
                        }
                    }
                    match self.assembler.push(r) {
                        Ok(Some(frame)) => self.accept(frame, start),
                        Ok(None) => {}
                        Err(e) => {
                            self.shared.lock().unwrap().parse_errors += 1;
                            self.error(e);
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    self.shared.lock().unwrap().parse_errors += 1;
                    self.error(e);
                }
            }
        }
    }
    fn tick(&mut self) {
        if self.stop.load(Ordering::Relaxed) || EXIT.load(Ordering::Relaxed) {
            unsafe { PostQuitMessage(0) };
            return;
        }
        if self.injector.is_some() {
            unsafe {
                let mut origin = POINT { x: 40, y: 220 };
                if IsWindow(self.target as HWND) == 0
                    || IsIconic(self.target as HWND) != 0
                    || ClientToScreen(self.target as HWND, &mut origin) == 0
                    || origin.x != self.rect.left
                    || origin.y != self.rect.top
                {
                    self.error(
                        "test window moved/minimized/closed; fixed mapping experiment stopped"
                            .into(),
                    );
                    self.stop.store(true, Ordering::Relaxed);
                    PostQuitMessage(0);
                    return;
                }
            }
        }
        if self.mode == "self-test" {
            let elapsed = self.start.elapsed().as_millis();
            // Three independent synthetic sequences: drag, 2s hold, rapid taps.
            // Runs through InjectTouchInput, never sends WM_POINTER directly.
            if elapsed >= 1200 && self.synthetic_step < 360 {
                let n = self.synthetic_step;
                self.synthetic_step += 1;
                let active = match n {
                    0 | 121 | 242 | 359 => false,
                    1..=120 => true,
                    122..=241 => true,
                    _ => n % 4 < 2,
                };
                let x = if n <= 120 {
                    100 + (n as i32) * 6
                } else if n < 242 {
                    600
                } else {
                    800
                };
                let c = Contact {
                    id: 1,
                    tip: true,
                    confidence: true,
                    x: x.clamp(0, 1000),
                    y: if n <= 120 { 200 + (n as i32) * 4 } else { 700 },
                    xmin: 0,
                    xmax: 1000,
                    ymin: 0,
                    ymax: 1000,
                };
                self.accept(if active { vec![c] } else { vec![] }, Instant::now());
            }
        } else if self.last_raw.elapsed() > Duration::from_millis(500) {
            if self.assembler.is_pending() {
                self.shared.lock().unwrap().incomplete += 1;
                self.error("hybrid frame timeout >500ms".into());
            }
            if let Some(i) = &mut self.injector {
                if i.has_active() {
                    if let Err(e) = i.release(true) {
                        self.log.event(e);
                    }
                }
            }
        }
        if let Some(i) = &mut self.injector {
            if let Err(e) = i.keepalive() {
                self.error(e);
            }
        }
        self.sync_stats();
    }
    fn finish(&mut self) {
        if self.assembler.is_pending() {
            self.shared.lock().unwrap().incomplete += 1;
        }
        if let Some(i) = &mut self.injector {
            if let Err(e) = i.release(true) {
                self.log.event(format!("RELEASE_ERROR {e}"));
            }
            self.shared.lock().unwrap().api_us = i.durations.clone();
        }
        self.sync_stats();
        self.log.event("WORKER_STOP released active touch");
    }
}
unsafe extern "system" fn input_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            INPUT.with(|i| {
                if let Some(worker) = i.borrow_mut().as_mut() {
                    worker.raw(l as HRAWINPUT);
                }
            }); /* DefWindowProc required for foreground RAWINPUT cleanup. */
        }
        WM_TIMER => {
            INPUT.with(|i| {
                if let Some(worker) = i.borrow_mut().as_mut() {
                    worker.tick();
                }
            });
            return 0;
        }
        WM_INPUT_DEVICE_CHANGE if w == GIDC_REMOVAL as usize => {
            INPUT.with(|i| {
                if let Some(worker) = i.borrow_mut().as_mut() {
                    if worker
                        .device
                        .as_ref()
                        .is_some_and(|d| d.handle == l as usize)
                    {
                        worker.error("selected Raw Input device removed".into());
                        worker.stop.store(true, Ordering::Relaxed);
                        PostQuitMessage(0);
                    }
                }
            });
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}
