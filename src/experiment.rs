use crate::{
    decode::{Assembler, Contact},
    hid::{self, winerr, Device},
    injection::Injector,
    logging::Log,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    mem::size_of,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    pub contacts: usize,
    pub suppress_gestures: bool,
    pub background: bool,
}
#[derive(Clone, Copy)]
pub struct TracePoint {
    id: u32,
    x: i32,
    y: i32,
    down: bool,
    color: usize,
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
    pub points: VecDeque<TracePoint>,
    pub received_active: BTreeMap<u32, (i32, i32)>,
    pub received_colors: BTreeMap<u32, usize>,
    pub received_peak: usize,
    pub receive_errors: u64,
    pub injected_active: usize,
    pub capacity: usize,
    pub armed: bool,
    pub cleanup_pending: bool,
    pub pinch_pair: Option<(u32, u32, f64)>,
    pub pinch_ratio: f64,
    pub api_us: Vec<u128>,
    pub capture_us: Vec<u128>,
    pub scans: Vec<u32>,
    pub gesture_guard: bool,
    pub gesture_events: u64,
    pub capture_stopped: bool,
    pub stop_reason: String,
    pub foreground: bool,
    pub foreground_losses: u64,
    pub minimizations: u64,
    pub capture: crate::capture::Counters,
}
type Shared = Arc<Mutex<Stats>>;
struct Ui {
    shared: Shared,
    log: Log,
    stop: Arc<AtomicBool>,
    start: Instant,
    seconds: u64,
    rect: RECT,
    worker_done: Arc<AtomicBool>,
    drain_started: Option<Instant>,
    back_buffer: Option<crate::render::BackBuffer>,
    close_requested: bool,
}
struct WorkerFinished(Arc<AtomicBool>);
impl Drop for WorkerFinished {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
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
        hbrBackground: null_mut(),
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
        "RUN format=2 mode={} inject={} seconds={} source={} capacity={} gesture_guard_requested={}",
        opt.mode,
        opt.inject || opt.mode == "self-test",
        opt.seconds,
        if opt.mode == "self-test" {
            "SYNTHETIC_API_TEST_NOT_HARDWARE"
        } else {
            "PHYSICAL_RAW_INPUT"
        },
        opt.contacts,opt.suppress_gestures
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
        capacity: opt.contacts,
        pinch_ratio: 1.0,
        ..Default::default()
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let worker_done = Arc::new(AtomicBool::new(false));
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
            "PadBridgeReceiver",
            &format!("PadBridge Phase 1+2 - {} - PT_TOUCH receiver", opt.mode),
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
                worker_done: worker_done.clone(),
                drain_started: None,
                back_buffer: None,
                close_requested: false,
            })
        });
        // A second call also handles a launcher specifying SW_HIDE for the
        // console. This small receiver must remain visible for hit testing.
        if opt.background {
            ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            log.event(
                "BACKGROUND_DIAGNOSTIC raw_only=true start_minimized=true settings_changed=false",
            );
        } else {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
            if SetForegroundWindow(hwnd) == 0 {
                log.event("SetForegroundWindow denied; receiver remains topmost");
            }
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
        let capacity = opt.contacts;
        let suppress_gestures = opt.suppress_gestures;
        let worker = std::thread::spawn(move || {
            let _finished = WorkerFinished(worker_done);
            let result = worker_loop(
                device,
                (mode, inject, capacity, suppress_gestures),
                (target, rect),
                worker_shared.clone(),
                worker_log.clone(),
                worker_stop,
            );
            if let Err(e) = &result {
                worker_log.event(format!("WORKER_ERROR {e}"));
                let mut s = worker_shared.lock().unwrap();
                s.error = e.clone();
                s.stop_reason = e.clone();
                s.capture_stopped = true;
            }
            result
        });
        let mut msg = MSG::default();
        let mut receiver_result = Ok(());
        loop {
            let status = GetMessageW(&mut msg, null_mut(), 0, 0);
            if status == -1 {
                let error = winerr("GetMessageW(receiver)");
                log.event(&error);
                receiver_result = Err(error);
                break;
            }
            if status == 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        stop.store(true, Ordering::Relaxed);
        let result = worker
            .join()
            .map_err(|_| "Raw Input worker panicked")?
            .and(receiver_result);
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
        let result = result.and(
            if dropped > 0
                || s.cleanup_pending
                || s.injected_active > 0
                || !s.received_active.is_empty()
            {
                Err(
                    "experiment ended with log loss or unresolved touch contacts; inspect summary"
                        .into(),
                )
            } else {
                Ok(())
            },
        );
        let summary = format!(
            "{}{}actual_duration_ms={}\nforeground_gesture_guard_used={}\ngesture_events_received={}\nforeground_losses={}\nwindow_minimizations={}\nrun_status={}\nstop_reason={}\n",
            summary(&s, dropped, opt.seconds),
            s.capture.summary(),
            experiment_start.elapsed().as_millis(),
            s.gesture_guard,
            s.gesture_events,
            s.foreground_losses,
            s.minimizations,
            if result.is_ok() && s.error.is_empty() { "COMPLETED" } else { "COMPLETED_WITH_ERRORS" },
            s.stop_reason
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
    format!("phase=1+2\nvalidation_status=PENDING_MANUAL_HARDWARE_TEST\nmode={}\nsource={}\nrequested_duration_s={seconds}\ndevice={}\ncapacity={}\nraw_reports={}\ncomplete_frames={}\npeak_active_contacts={}\nparse_errors={}\nincomplete_frames={}\ninjected_frames={}\ninjection_errors={}\nnot_ready_retries={}\ninjected_contacts_remaining={}\ncleanup_pending={}\nreceiver_PT_TOUCH_down={}\nreceiver_PT_TOUCH_update={}\nreceiver_PT_TOUCH_up={}\nreceiver_GetPointerTouchInfo_ok={}\nreceiver_peak_contacts={}\nreceiver_contacts_remaining={}\nreceiver_lifecycle_errors={}\nlog_dropped={}\ninject_api_p95_us={}\nraw_to_inject_call_p95_us={}\nlatency_note=host software only; excludes firmware, OS delivery and physical input latency\nlast_error={}\n",s.mode,if s.mode=="self-test"{"SYNTHETIC_API_TEST_NOT_HARDWARE"}else{"PHYSICAL_RAW_INPUT"},s.device,s.capacity,s.raw,s.frames,s.peak,s.parse_errors,s.incomplete,s.injected,s.inject_errors,s.retries,s.injected_active,s.cleanup_pending,s.down,s.update,s.up,s.touch_info,s.received_peak,s.received_active.len(),s.receive_errors,dropped,percentile(&s.api_us,95),percentile(&s.capture_us,95),s.error)
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
                if msg==WM_POINTERDOWN {let color=(0..5).find(|c|!s.received_colors.values().any(|v|v==c)).unwrap_or(id as usize%5);s.received_colors.insert(id,color);if s.received_active.insert(id,(p.x,p.y)).is_some(){s.receive_errors+=1;}}
                else if msg==WM_POINTERUP {if s.received_active.remove(&id).is_none(){s.receive_errors+=1;}}
                else if let Some(point)=s.received_active.get_mut(&id){*point=(p.x,p.y);}else{s.receive_errors+=1;}
                s.received_peak=s.received_peak.max(s.received_active.len());
                if s.received_active.len()==2 {
                    let pair:Vec<_>=s.received_active.iter().collect();let (&a,&(ax,ay))=pair[0];let (&b,&(bx,by))=pair[1];
                    let distance=((ax as f64-bx as f64).powi(2)+(ay as f64-by as f64).powi(2)).sqrt();
                    if let Some((old_a,old_b,baseline))=s.pinch_pair {if old_a==a&&old_b==b&&baseline>0.0{s.pinch_ratio=distance/baseline;}else{s.pinch_pair=Some((a,b,distance));}}
                    else{s.pinch_pair=Some((a,b,distance));s.pinch_ratio=1.0;}
                }else{s.pinch_pair=None;}
                let color=s.received_colors.get(&id).copied().unwrap_or(0);
                s.points.push_back(TracePoint{id,x:p.x,y:p.y,down:msg==WM_POINTERDOWN,color});if s.points.len()>5000{s.points.pop_front();}
                if msg==WM_POINTERUP{s.received_colors.remove(&id);}
            }});
            return 0;
        }
        WM_TIMER => {
            let close = UI.with(|u| {
                let mut ui = u.borrow_mut();
                let Some(u) = ui.as_mut() else {
                    return false;
                };
                u.shared.lock().unwrap().foreground = GetForegroundWindow() == hwnd;
                if u.start.elapsed().as_secs() >= u.seconds || EXIT.load(Ordering::Relaxed) {
                    u.close_requested = true;
                    let mut s = u.shared.lock().unwrap();
                    if s.stop_reason.is_empty() {
                        s.stop_reason = if EXIT.load(Ordering::Relaxed) {
                            "console stop requested"
                        } else {
                            "duration elapsed"
                        }
                        .into();
                    }
                    u.stop.store(true, Ordering::Relaxed);
                }
                if u.close_requested && u.worker_done.load(Ordering::Acquire) {
                    let since = u.drain_started.get_or_insert_with(Instant::now);
                    since.elapsed() >= Duration::from_millis(300)
                } else {
                    false
                }
            });
            if close {
                DestroyWindow(hwnd);
            } else {
                InvalidateRect(hwnd, null(), 0);
            }
            return 0;
        }
        WM_KEYDOWN if w == 27 => {
            UI.with(|u| {
                if let Some(u) = u.borrow_mut().as_mut() {
                    u.close_requested = true;
                    let mut s = u.shared.lock().unwrap();
                    if s.stop_reason.is_empty() {
                        s.stop_reason = "Esc requested".into();
                    }
                    u.stop.store(true, Ordering::Relaxed);
                }
            });
            return 0;
        }
        WM_DISPLAYCHANGE => {
            UI.with(|u| {
                if let Some(u) = u.borrow().as_ref() {
                    u.stop.store(true, Ordering::Relaxed);
                    let mut s = u.shared.lock().unwrap();
                    s.error = "display changed; experiment stopped".into();
                    s.stop_reason = s.error.clone();
                }
            });
        }
        WM_CLOSE => {
            UI.with(|u| {
                if let Some(u) = u.borrow_mut().as_mut() {
                    u.close_requested = true;
                    let mut s = u.shared.lock().unwrap();
                    if s.stop_reason.is_empty() {
                        s.stop_reason = "window close requested".into();
                    }
                    u.stop.store(true, Ordering::Relaxed);
                }
            });
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
        WM_ERASEBKGND => return 1,
        WM_SIZE | WM_ACTIVATEAPP => {
            UI.with(|u| {
                if let Some(u) = u.borrow().as_ref() {
                    let mut s = u.shared.lock().unwrap();
                    s.foreground = GetForegroundWindow() == hwnd;
                    if msg == WM_ACTIVATEAPP && w == 0 {
                        s.foreground_losses += 1;
                    }
                    if msg == WM_SIZE && w == SIZE_MINIMIZED as usize {
                        s.minimizations += 1;
                    }
                    u.log.event(format!(
                        "WINDOW_STATE message={msg} value={w} minimized={} foreground={}",
                        IsIconic(hwnd) != 0,
                        GetForegroundWindow() == hwnd
                    ));
                }
            });
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}
unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let screen = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    if GetClientRect(hwnd, &mut rc) == 0 || screen.is_null() || rc.right <= 0 || rc.bottom <= 0 {
        EndPaint(hwnd, &ps);
        return;
    }
    UI.with(|cell| {
        let mut state = cell.borrow_mut();
        let Some(ui) = state.as_mut() else {
            return;
        };
        if ui
            .back_buffer
            .as_ref()
            .is_none_or(|b| b.width != rc.right || b.height != rc.bottom)
        {
            match crate::render::BackBuffer::new(screen, rc.right, rc.bottom) {
                Ok(buffer) => ui.back_buffer = Some(buffer),
                Err(error) => {
                    ui.log.event(format!("RENDER_ERROR {error}"));
                    FillRect(screen, &rc, (COLOR_WINDOW + 1) as HBRUSH);
                    return;
                }
            }
        }
        let buffer = ui.back_buffer.as_ref().unwrap();
        paint_contents(hwnd, buffer.dc, &rc, ui);
        if let Err(error) = buffer.present(screen) {
            ui.log.event(format!("RENDER_ERROR {error}"));
        }
    });
    EndPaint(hwnd, &ps);
}
unsafe fn paint_contents(hwnd: HWND, dc: HDC, rc: &RECT, u: &Ui) {
    FillRect(dc, rc, (COLOR_WINDOW + 1) as HBRUSH);
    let (text, points, active, active_colors, pinch_ratio) = {
        let s = u.shared.lock().unwrap();
        let text=format!("PadBridge PHASE 1+2 | {} | max {} touches\n{}\nRaw {} / Frames {} / Peak fingers {} / Parse errors {}\nInjected batches {} / Errors {} / Retry {} / Active {} / {}\nPT_TOUCH: DOWN {} / UPDATE {} / UP {} / Active {} / Peak {}\nTwo-finger ratio {:.2} | 3/4-finger guard: {} / events {}\n{}",if s.mode=="self-test"{"SYNTHETIC API TEST"}else{"PHYSICAL TOUCHPAD"},s.capacity,s.device_label,s.raw,s.frames,s.peak,s.parse_errors,s.injected,s.inject_errors,s.retries,s.injected_active,if s.cleanup_pending{"CLEANUP FAILED"}else if s.capture_stopped{"STOPPED"}else if s.armed{"ARMED"}else{"WAIT ALL FINGERS UP"},s.down,s.update,s.up,s.received_active.len(),s.received_peak,s.pinch_ratio,if !s.gesture_guard || s.capture_stopped{"OFF"}else if s.foreground{"ACTIVE (foreground)"}else{"INACTIVE (background)"},s.gesture_events,if !s.stop_reason.is_empty(){format!("Stopped: {} | Esc to close",s.stop_reason)}else if !s.error.is_empty(){s.error.clone()}else{"ESC: release and stop | Native mouse remains active".into()});
        (
            text,
            s.points.clone(),
            s.received_active.clone(),
            s.received_colors.clone(),
            s.pinch_ratio,
        )
    };
    // Release the statistics lock before GDI painting.
    let face = wide("Segoe UI");
    let font = CreateFontW(
        -20,
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
        face.as_ptr(),
    );
    let old_font = if !font.is_null() {
        SelectObject(dc, font)
    } else {
        null_mut()
    };
    let mut tr = RECT {
        left: 20,
        top: 12,
        right: rc.right - 20,
        bottom: 215,
    };
    let t = wide(&text);
    SetBkMode(dc, TRANSPARENT as i32);
    DrawTextW(
        dc,
        t.as_ptr(),
        -1,
        &mut tr,
        DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
    );
    let mut org = POINT { x: 0, y: 0 };
    ClientToScreen(hwnd, &mut org);
    let boxrect = RECT {
        left: u.rect.left - org.x,
        top: u.rect.top - org.y,
        right: u.rect.right - org.x,
        bottom: u.rect.bottom - org.y,
    };
    FrameRect(dc, &boxrect, GetStockObject(GRAY_BRUSH) as HBRUSH);
    let colors = [0x00709010, 0x002070D0, 0x00C06050, 0x009040A0, 0x0020A0A0];
    let pens: Vec<_> = colors
        .iter()
        .map(|&color| CreatePen(PS_SOLID, 3, color))
        .collect();
    let old_pen = SelectObject(dc, pens[0]);
    let old_brush = SelectObject(dc, GetStockObject(HOLLOW_BRUSH));
    let mut previous = BTreeMap::new();
    for p in &points {
        SelectObject(dc, pens[p.color % pens.len()]);
        if !p.down {
            if let Some(&(x, y)) = previous.get(&p.id) {
                MoveToEx(dc, x, y, null_mut());
                LineTo(dc, p.x, p.y);
            }
        }
        previous.insert(p.id, (p.x, p.y));
    }
    for (&id, &(x, y)) in &active {
        SelectObject(
            dc,
            pens[active_colors.get(&id).copied().unwrap_or(0) % pens.len()],
        );
        Ellipse(dc, x - 10, y - 10, x + 10, y + 10);
        let label = wide(&format!("{id}"));
        TextOutW(dc, x + 12, y - 12, label.as_ptr(), (label.len() - 1) as i32);
    }
    if active.len() == 2 {
        let cx = (boxrect.left + boxrect.right) / 2;
        let cy = (boxrect.top + boxrect.bottom) / 2;
        let scale = pinch_ratio.clamp(0.25, 2.0);
        let half_w = (80.0 * scale) as i32;
        let half_h = (50.0 * scale) as i32;
        Rectangle(dc, cx - half_w, cy - half_h, cx + half_w, cy + half_h);
    }
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    for pen in pens {
        DeleteObject(pen);
    }
    if !font.is_null() {
        SelectObject(dc, old_font);
        DeleteObject(font);
    }
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
    capacity: usize,
    pending_since: Option<Instant>,
    scenarios: Vec<crate::scenarios::Step>,
    gesture_events: Option<Arc<AtomicU64>>,
    capture: crate::capture::Evidence,
    last_scope: Option<crate::capture::Scope>,
}
fn worker_loop(
    device: Option<Device>,
    settings: (String, bool, usize, bool),
    fixed_target: (usize, RECT),
    shared: Shared,
    log: Log,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let (target, rect) = fixed_target;
    let (mode, inject, capacity, suppress_gestures) = settings;
    let fields = device
        .as_ref()
        .map(|d| d.fields())
        .transpose()?
        .unwrap_or_default();
    let injector = if inject {
        Some(Injector::new(target, rect, capacity, log.clone())?)
    } else {
        None
    };
    let mut worker = Worker {
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
        capacity,
        pending_since: None,
        scenarios: crate::scenarios::steps(capacity),
        gesture_events: None,
        capture: crate::capture::Evidence::default(),
        last_scope: None,
    };
    unsafe {
        let hwnd = make_window("PadBridgePhase0RawInput", "", Some(input_proc), false)?;
        let mut gesture_guard = if suppress_gestures {
            let guard = crate::gestures::Guard::create(log.clone())?;
            worker.shared.lock().unwrap().gesture_guard = true;
            worker.gesture_events = Some(guard.event_counter());
            Some(guard)
        } else {
            None
        };
        INPUT.with(|i| *i.borrow_mut() = Some(worker));
        let rid = RAWINPUTDEVICE {
            usUsagePage: 0x0D,
            usUsage: 0x05,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        };
        if RegisterRawInputDevices(&rid, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            let error = winerr("RegisterRawInputDevices");
            INPUT.with(|i| {
                i.borrow_mut().take();
            });
            DestroyWindow(hwnd);
            return Err(error);
        }
        log.event("REGISTER_RAW_INPUT success usage=0D:05 INPUTSINK DEVNOTIFY");
        let mut result = if SetTimer(hwnd, 1, 16, None) == 0 {
            Err(winerr("SetTimer(raw)"))
        } else {
            Ok(())
        };
        let mut msg = MSG::default();
        while result.is_ok() {
            let n = GetMessageW(&mut msg, null_mut(), 0, 0);
            if n == -1 {
                result = Err(winerr("GetMessageW(raw)"));
                break;
            }
            if n == 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let cleanup = INPUT.with(|i| {
            if let Some(mut worker) = i.borrow_mut().take() {
                worker.finish()
            } else {
                Ok(())
            }
        });
        result = result.and(cleanup);
        if let Some(guard) = &mut gesture_guard {
            result = result.and(guard.disable());
        }
        let remove = RAWINPUTDEVICE {
            dwFlags: RIDEV_REMOVE,
            hwndTarget: null_mut(),
            ..rid
        };
        if RegisterRawInputDevices(&remove, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            log.event(winerr("UnregisterRawInputDevices"));
        }
        DestroyWindow(hwnd);
        result
    }
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
        self.pending_since = None;
        self.capture.reset_fragments();
    }
    fn accept(&mut self, contacts: Vec<Contact>, start: Instant) {
        let scope = self.capture.frame(&contacts);
        let live = contacts.iter().filter(|c| c.tip && c.confidence).count();
        self.log.event(format!(
            "FRAME active={live} records={} contacts={contacts:?} foreground_scope={}",
            contacts.len(),
            scope.label()
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
            let before = i.successes();
            if let Err(e) = i.frame(&contacts) {
                self.error(e);
            }
            let elapsed = start.elapsed().as_micros();
            if self
                .injector
                .as_ref()
                .is_some_and(|i| i.successes() > before)
            {
                let mut s = self.shared.lock().unwrap();
                if s.capture_us.len() < 100000 {
                    s.capture_us.push(elapsed);
                }
            }
        }
        self.sync_stats();
    }
    fn sync_stats(&self) {
        self.shared.lock().unwrap().capture = self.capture.counts.clone();
        if let Some(events) = &self.gesture_events {
            self.shared.lock().unwrap().gesture_events = events.load(Ordering::Relaxed);
        }
        if let Some(i) = &self.injector {
            let mut s = self.shared.lock().unwrap();
            s.injected = i.successes();
            s.inject_errors = i.failures();
            s.retries = i.retries();
            s.injected_active = i.active_count();
            s.armed = i.is_armed();
            s.cleanup_pending = i.cleanup_pending();
        }
    }
    fn sample_scope(&mut self) -> crate::capture::Scope {
        use crate::capture::Scope;
        let scope = unsafe {
            let hwnd = GetForegroundWindow();
            let mut pid = 0;
            if hwnd.is_null() || GetWindowThreadProcessId(hwnd, &mut pid) == 0 || pid == 0 {
                Scope::Unknown
            } else if pid == GetCurrentProcessId() {
                Scope::Foreground
            } else {
                Scope::Background
            }
        };
        self.capture.focus(scope);
        if self.last_scope != Some(scope) {
            self.log
                .event(format!("CAPTURE_FOCUS scope={}", scope.label()));
            self.last_scope = Some(scope);
        }
        scope
    }
    fn raw(&mut self, raw: HRAWINPUT, delivery_code: usize) {
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
        let scope = self.sample_scope();
        for report in reports {
            self.capture
                .raw(scope, delivery_code == RIM_INPUTSINK as usize);
            self.last_raw = Instant::now();
            self.shared.lock().unwrap().raw += 1;
            self.log.event(format!(
                "RAW handle=0x{handle:X} foreground_scope={} delivery_code={delivery_code} hex={}",
                scope.label(),
                report
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<String>()
            ));
            match self.device.as_ref().unwrap().parse(&self.fields, &report) {
                Ok(Some(r)) => {
                    self.capture.fragment(scope);
                    if let Some(scan) = r.scan {
                        let mut s = self.shared.lock().unwrap();
                        if s.scans.len() < 100000 {
                            s.scans.push(scan);
                        }
                    }
                    match self.assembler.push(r) {
                        Ok(Some(frame)) => {
                            self.pending_since = None;
                            self.accept(frame, start)
                        }
                        Ok(None) => {
                            self.pending_since.get_or_insert_with(Instant::now);
                        }
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
        self.sample_scope();
        if self.stop.load(Ordering::Relaxed) || EXIT.load(Ordering::Relaxed) {
            unsafe { PostQuitMessage(0) };
            return;
        }
        if self.injector.is_some() {
            unsafe {
                let mut origin = POINT { x: 40, y: 220 };
                let reason = if IsWindow(self.target as HWND) == 0 {
                    Some("receiver closed; fixed mapping stopped")
                } else if IsIconic(self.target as HWND) != 0 {
                    Some("receiver minimized (possible Windows system gesture); injection stopped")
                } else if ClientToScreen(self.target as HWND, &mut origin) == 0 {
                    Some("receiver position unavailable; fixed mapping stopped")
                } else if origin.x != self.rect.left || origin.y != self.rect.top {
                    Some("receiver moved; fixed mapping stopped")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    self.error(reason.into());
                    self.shared.lock().unwrap().stop_reason = reason.into();
                    self.stop.store(true, Ordering::Relaxed);
                    PostQuitMessage(0);
                    return;
                }
            }
        }
        if self
            .pending_since
            .is_some_and(|t| t.elapsed() > Duration::from_millis(100))
        {
            self.shared.lock().unwrap().incomplete += 1;
            self.error("hybrid assembly timeout >100ms".into());
        }
        if self.mode == "self-test" {
            let elapsed = self.start.elapsed().as_millis();
            if elapsed >= 1200 && (self.synthetic_step as usize) < self.scenarios.len() {
                let step = &self.scenarios[self.synthetic_step as usize];
                let contacts = step.contacts.clone();
                self.log.event(format!(
                    "SCENARIO {} capacity={}",
                    step.label, self.capacity
                ));
                self.synthetic_step += 1;
                self.accept(contacts, Instant::now());
            }
        } else if self.last_raw.elapsed() > Duration::from_millis(500) {
            if self.assembler.is_pending() {
                self.shared.lock().unwrap().incomplete += 1;
                self.error("hybrid frame timeout >500ms".into());
            }
            if let Some(i) = &mut self.injector {
                if i.has_active() {
                    if let Err(e) = i.release(true) {
                        self.error(e);
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
    fn finish(&mut self) -> Result<(), String> {
        let mut result = Ok(());
        if self.assembler.is_pending() {
            self.shared.lock().unwrap().incomplete += 1;
        }
        if let Some(i) = &mut self.injector {
            if let Err(e) = i.release(true) {
                self.log.event(format!("RELEASE_ERROR {e}"));
                result = Err(e);
            }
            self.shared.lock().unwrap().api_us = i.durations().to_vec();
        }
        self.sync_stats();
        self.shared.lock().unwrap().capture_stopped = true;
        self.log.event(format!(
            "WORKER_STOP active_contacts={} cleanup_pending={}",
            self.injector.as_ref().map_or(0, |i| i.active_count()),
            self.injector.as_ref().is_some_and(|i| i.cleanup_pending())
        ));
        result
    }
}
unsafe extern "system" fn input_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            INPUT.with(|i| {
                if let Some(worker) = i.borrow_mut().as_mut() {
                    worker.raw(l as HRAWINPUT, w & 0xFF);
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
                        worker.shared.lock().unwrap().stop_reason =
                            "selected Raw Input device removed".into();
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
