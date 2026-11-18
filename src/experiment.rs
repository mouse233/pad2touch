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
/// Wakes a window when the capture worker publishes a new frame, so the live
/// preview repaints at the touchpad's report rate instead of a UI timer's.
/// At most one message is in flight: the window clears `pending` when it
/// handles the message, and frames arriving before then share that repaint.
#[derive(Clone, Copy)]
pub struct FrameSignal {
    hwnd: usize,
    msg: u32,
    pending: &'static AtomicBool,
}
impl FrameSignal {
    pub fn new(hwnd: HWND, msg: u32, pending: &'static AtomicBool) -> Self {
        Self {
            hwnd: hwnd as usize,
            msg,
            pending,
        }
    }
    fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel)
            // SAFETY: PostMessageW only queues; an invalid HWND just fails.
            && unsafe { PostMessageW(self.hwnd as HWND, self.msg, 0, 0) } == 0
        {
            self.pending.store(false, Ordering::Release);
        }
    }
}
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
    pub(crate) id: u32,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) down: bool,
    pub(crate) color: usize,
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
    pub report_rate: crate::report_rate::Snapshot,
    pub gesture_guard: bool,
    pub background_guard: bool,
    pub gesture_events: u64,
    pub guard_status: String,
    pub recovery_pending: bool,
    pub capture_stopped: bool,
    pub stop_reason: String,
    pub foreground: bool,
    pub foreground_losses: u64,
    pub minimizations: u64,
    pub capture: crate::capture::Counters,
    pub target_label: String,
    pub target_state: String,
    pub target_updates: u64,
    pub target_pauses: u64,
    pub injection_blocked: u64,
    /// Current mapped bounds in physical desktop pixels (empty if none).
    pub mapped: RECT,
    /// Used touchpad region of the current mapping.
    pub mapped_input: Option<crate::mapping::NormRect>,
    /// Live confident contacts, normalized to the touchpad's logical range.
    pub live: Vec<(f64, f64)>,
    /// Notified after each frame updates `live`.
    pub frame_signal: Option<FrameSignal>,
}
pub(crate) type Shared = Arc<Mutex<Stats>>;
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
    if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
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
        s.device_label = d
            .geometry()
            .and_then(|g| g.label())
            .unwrap_or_else(|e| format!("Physical geometry unavailable: {e}"));
    }
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            return Err(winerr("SetProcessDpiAwarenessContext"));
        }
        if SetConsoleCtrlHandler(Some(ctrl), 1) == 0 {
            return Err(winerr("SetConsoleCtrlHandler"));
        }
        let hwnd = make_window(
            "Pad2TouchReceiver",
            &format!("Pad2Touch Phase 1+2 - {} - PT_TOUCH receiver", opt.mode),
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
        let area = RECT {
            left: origin.x + 40,
            top: origin.y + 220,
            right: origin.x + client.right - 40,
            bottom: origin.y + client.bottom - 40,
        };
        let rect = if let Some(device) = &device {
            match device.geometry() {
                Ok(g) => crate::mapping::fit_aspect(area, g.aspect()?)?,
                Err(e) if opt.inject => {
                    return Err(format!("Cannot preserve physical touchpad aspect: {e}"))
                }
                Err(_) => RECT::default(),
            }
        } else {
            area
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
        let target = crate::mapping::ResolvedTarget {
            spec: crate::mapping::MappingTarget::Diagnostics,
            window: hwnd as usize,
            bounds: rect,
            hit: crate::mapping::HitPolicy::Exact(hwnd as usize),
            aspect: None,
            // The CLI receiver keeps the physical shape (fit_aspect above).
            fit: crate::mapping::PadFit::KeepShape,
            input: crate::mapping::NormRect::FULL,
        };
        let mode = opt.mode.clone();
        let inject = opt.inject || mode == "self-test";
        let capacity = opt.contacts;
        let suppress_gestures = opt.suppress_gestures;
        let worker = std::thread::spawn(move || {
            let _finished = WorkerFinished(worker_done);
            let result = worker_loop(
                device,
                (mode, inject, capacity, suppress_gestures, false),
                target,
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
    let report = format!("phase=1+2\nvalidation_status=PENDING_MANUAL_HARDWARE_TEST\nmode={}\nsource={}\nrequested_duration_s={seconds}\ndevice={}\ncapacity={}\nraw_reports={}\ncomplete_frames={}\npeak_active_contacts={}\nparse_errors={}\nincomplete_frames={}\ninjected_frames={}\ninjection_errors={}\nnot_ready_retries={}\ninjected_contacts_remaining={}\ncleanup_pending={}\nreceiver_PT_TOUCH_down={}\nreceiver_PT_TOUCH_update={}\nreceiver_PT_TOUCH_up={}\nreceiver_GetPointerTouchInfo_ok={}\nreceiver_peak_contacts={}\nreceiver_contacts_remaining={}\nreceiver_lifecycle_errors={}\nlog_dropped={}\ninject_api_p95_us={}\nraw_to_inject_call_p95_us={}\nlatency_note=host software only; excludes firmware, OS delivery and physical input latency\nlast_error={}\n",s.mode,if s.mode=="self-test"{"SYNTHETIC_API_TEST_NOT_HARDWARE"}else{"PHYSICAL_RAW_INPUT"},s.device,s.capacity,s.raw,s.frames,s.peak,s.parse_errors,s.incomplete,s.injected,s.inject_errors,s.retries,s.injected_active,s.cleanup_pending,s.down,s.update,s.up,s.touch_info,s.received_peak,s.received_active.len(),s.receive_errors,dropped,percentile(&s.api_us,95),percentile(&s.capture_us,95),s.error);
    format!("{report}device_geometry={}\nobserved_rate_snapshot={}\nrate_note=observed host delivery and PTP scan cadence, not nominal bus polling\ntarget={}\ntarget_updates={}\ntarget_pauses={}\ninjection_blocked={}\n", s.device_label.replace('\n', " | "), s.report_rate.label(), if s.target_label.is_empty() { "diagnostic receiver" } else { &s.target_label }, s.target_updates, s.target_pauses, s.injection_blocked)
}

/// Machine-readable target record. Window titles are deliberately omitted.
pub(crate) fn target_record(
    kind: &str,
    target: &crate::mapping::ResolvedTarget,
    r: RECT,
) -> String {
    format!(
        "{kind} mode={} window=0x{:X} receiver={} left={} top={} right={} bottom={} pad_fit={} pad_input={:.4},{:.4},{:.4},{:.4}",
        target.spec.label(),
        target.window,
        if target.spec.is_diagnostic() {
            "embedded"
        } else {
            "external"
        },
        r.left,
        r.top,
        r.right,
        r.bottom,
        target.fit.label(),
        target.input.left,
        target.input.top,
        target.input.right,
        target.input.bottom
    )
}
pub(crate) fn target_label(target: &crate::mapping::ResolvedTarget) -> String {
    let fit = match target.fit {
        crate::mapping::PadFit::KeepShape => "whole pad, keep shape",
        crate::mapping::PadFit::Stretch => "whole pad, stretched",
        crate::mapping::PadFit::FillArea => "part of pad, fill area",
        crate::mapping::PadFit::Portion(_) => "selected portion of pad",
    };
    let place: String = match target.spec {
        crate::mapping::MappingTarget::Diagnostics => "diagnostic receiver".into(),
        crate::mapping::MappingTarget::ScreenRectangle { .. } => "screen rectangle".into(),
        crate::mapping::MappingTarget::Window { handle } => {
            format!("window 0x{handle:X} client area")
        }
        crate::mapping::MappingTarget::WindowRectangle { handle, area } => format!(
            "window 0x{handle:X} area {:.0}-{:.0}% × {:.0}-{:.0}%",
            area.left * 100.0,
            area.right * 100.0,
            area.top * 100.0,
            area.bottom * 100.0
        ),
    };
    format!("{place} ({fit})")
}

/// A GUI-owned session. The caller keeps pumping receiver messages during stop
/// and only finalizes after the worker and the receiver drain have completed.
pub(crate) struct Session {
    pub shared: Shared,
    pub log: Log,
    pub out: PathBuf,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    worker: std::thread::JoinHandle<Result<(), String>>,
    writer: std::thread::JoinHandle<Result<(), String>>,
    drain_started: Option<Instant>,
    started: Instant,
    explicit_stop: AtomicBool,
}
/// User choices for a GUI session, fixed for its lifetime.
pub struct SessionSettings {
    pub inject: bool,
    pub suppress_touchpad: bool,
    pub suppress_touchscreen: bool,
    /// Draw Windows touch visuals for injected contacts.
    pub touch_feedback: bool,
    pub prewarmed: Option<crate::background_guard::Backend>,
}
impl Session {
    pub fn start(
        device: Device,
        target: crate::mapping::ResolvedTarget,
        settings: SessionSettings,
        out: PathBuf,
    ) -> Result<Self, String> {
        let SessionSettings {
            inject,
            suppress_touchpad,
            suppress_touchscreen,
            touch_feedback,
            prewarmed,
        } = settings;
        let capacity = 5;
        let background_guard = suppress_touchpad || suppress_touchscreen;
        // Resolve parser support before starting either thread.
        device.fields()?;
        if inject && target.fit.needs_shape() {
            // The used part of the pad must have the target's physical shape;
            // only Stretch deliberately distorts.
            let pad = device.geometry()?.aspect()?;
            let i = target.input;
            let ratio = pad * (i.right - i.left) / (i.bottom - i.top);
            let width = target.bounds.right as f64 - target.bounds.left as f64;
            let height = target.bounds.bottom as f64 - target.bounds.top as f64;
            if width < 2.0 || height < 2.0 || (width - height * ratio).abs() > ratio.max(1.0) {
                return Err(
                    "Injection target does not preserve the physical touchpad aspect".into(),
                );
            }
        }
        let shared = Arc::new(Mutex::new(Stats {
            device: device.summary(),
            device_label: device
                .geometry()
                .and_then(|g| g.label())
                .unwrap_or_else(|e| format!("Physical geometry unavailable: {e}")),
            mode: "capture".into(),
            capacity,
            pinch_ratio: 1.0,
            ..Default::default()
        }));
        let (log, writer) = Log::open(&out)?;
        log.event(format!("RUN format=2 mode=capture inject={inject} seconds=0 source=PHYSICAL_RAW_INPUT capacity={capacity} suppress_touchpad={suppress_touchpad} suppress_touchscreen={suppress_touchscreen} touch_feedback={touch_feedback} frontend=GUI"));
        log.event(format!("DEVICE {}", device.summary()));
        let r = target.bounds;
        log.event(format!(
            "FIXED_RECT left={} top={} right={} bottom={}",
            r.left, r.top, r.right, r.bottom
        ));
        log.event(target_record("TARGET", &target, r));
        {
            let mut s = shared.lock().unwrap();
            s.background_guard = background_guard;
            if background_guard {
                s.guard_status = "Preparing background guard and recovery watchdog...".into();
            }
            s.target_label = target_label(&target);
            s.target_state = format!("mapped {}", crate::mapping::describe(r));
            s.mapped = r;
            s.mapped_input = Some(target.input);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let worker_shared = shared.clone();
        let worker_log = log.clone();
        let worker_stop = stop.clone();
        let worker_done = done.clone();
        let worker_out = out.clone();
        let worker = std::thread::spawn(move || {
            let _finished = WorkerFinished(worker_done);
            let guard = if background_guard {
                crate::background_guard::Guard::acquire(
                    &worker_out,
                    worker_shared.clone(),
                    worker_log.clone(),
                    worker_stop.clone(),
                    crate::gesture_backend::Scope {
                        touchpad: suppress_touchpad,
                        touchscreen: suppress_touchscreen,
                    },
                    prewarmed,
                )
            } else {
                Ok(None)
            };
            let result = guard.and_then(|guard| {
                let result = if worker_stop.load(Ordering::Relaxed) {
                    Ok(())
                } else {
                    worker_loop(
                        Some(device),
                        ("capture".into(), inject, capacity, false, touch_feedback),
                        target,
                        worker_shared.clone(),
                        worker_log.clone(),
                        worker_stop,
                    )
                };
                if let Some(guard) = guard {
                    result.and(guard.release())
                } else {
                    result
                }
            });
            worker_shared.lock().unwrap().capture_stopped = true;
            if let Err(error) = &result {
                worker_log.event(format!("WORKER_ERROR {error}"));
                let mut s = worker_shared.lock().unwrap();
                s.error = error.clone();
                s.stop_reason = error.clone();
                s.capture_stopped = true;
            }
            result
        });
        Ok(Self {
            shared,
            log,
            out,
            stop,
            done,
            worker,
            writer,
            drain_started: None,
            started: Instant::now(),
            explicit_stop: AtomicBool::new(false),
        })
    }
    pub fn request_stop(&self, reason: &str) {
        self.explicit_stop.store(true, Ordering::Relaxed);
        let mut s = self.shared.lock().unwrap();
        if s.stop_reason.is_empty() {
            s.stop_reason = reason.into();
        }
        self.stop.store(true, Ordering::Relaxed);
    }
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    pub fn stopped_unexpectedly(&self) -> bool {
        !self.explicit_stop.load(Ordering::Relaxed) && (self.stopping() || self.worker_done())
    }
    pub fn worker_done(&self) -> bool {
        self.done.load(Ordering::Acquire) && self.worker.is_finished()
    }
    pub fn ready_to_finish(&mut self) -> bool {
        if !self.worker_done() {
            return false;
        }
        self.drain_started
            .get_or_insert_with(Instant::now)
            .elapsed()
            >= Duration::from_millis(300)
    }
    pub fn finish(self) -> Result<(), String> {
        let result = self
            .worker
            .join()
            .map_err(|_| "Raw Input worker panicked".to_string())
            .and_then(|r| r);
        let s = self.shared.lock().unwrap();
        let dropped = self.log.dropped.load(Ordering::Relaxed);
        let result = result.and(
            if dropped > 0
                || s.cleanup_pending
                || s.recovery_pending
                || s.injected_active > 0
                || !s.received_active.is_empty()
                || !s.error.is_empty()
            {
                Err("Session ended with errors or unresolved contacts. Review diagnostics and logs.".into())
            } else {
                Ok(())
            },
        );
        let report = format!(
            "{}{}actual_duration_ms={}\nrun_status={}\nstop_reason={}\nbackground_guard_status={}\nsettings_recovery_pending={}\n",
            summary(&s, dropped, 0),
            s.capture.summary(),
            self.started.elapsed().as_millis(),
            if result.is_ok() {
                "COMPLETED"
            } else {
                "COMPLETED_WITH_ERRORS"
            },
            s.stop_reason,
            s.guard_status,
            s.recovery_pending
        );
        self.log
            .event(format!("SUMMARY {}", report.replace('\n', " | ")));
        let written =
            std::fs::write(self.out.join("summary.txt"), report).map_err(|e| e.to_string());
        drop(s);
        drop(self.log);
        let flushed = self
            .writer
            .join()
            .map_err(|_| "log writer panicked".to_string())
            .and_then(|r| r);
        result.and(written).and(flushed)
    }
}
unsafe extern "system" fn ui_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP => {
            UI.with(|u| {
                if let Some(u) = u.borrow().as_ref() {
                    receive_touch(hwnd, msg, w, &u.shared, &u.log);
                }
            });
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
pub(crate) unsafe fn receive_touch(hwnd: HWND, msg: u32, w: WPARAM, shared: &Shared, log: &Log) {
    let mut info = POINTER_INFO::default();
    let id = (w & 0xFFFF) as u32;
    if GetPointerInfo(id, &mut info) == 0 {
        log.event(winerr("GetPointerInfo"));
        return;
    }
    if info.pointerType != PT_TOUCH {
        return;
    }
    let mut touch = POINTER_TOUCH_INFO::default();
    let touch_ok = GetPointerTouchInfo(id, &mut touch) != 0;
    let kind = match msg {
        WM_POINTERDOWN => "DOWN",
        WM_POINTERUP => "UP",
        _ => "UPDATE",
    };
    log.event(format!(
        "RECEIVE_PT_TOUCH {kind} id={id} frame={} x={} y={} flags=0x{:X} touch_info={touch_ok}",
        info.frameId, info.ptPixelLocation.x, info.ptPixelLocation.y, info.pointerFlags
    ));
    let mut s = shared.lock().unwrap();
    match msg {
        WM_POINTERDOWN => s.down += 1,
        WM_POINTERUP => s.up += 1,
        _ => s.update += 1,
    };
    if touch_ok {
        s.touch_info += 1;
    }
    let mut p = info.ptPixelLocation;
    ScreenToClient(hwnd, &mut p);
    if msg == WM_POINTERDOWN {
        let color = (0..5)
            .find(|c| !s.received_colors.values().any(|v| v == c))
            .unwrap_or(id as usize % 5);
        s.received_colors.insert(id, color);
        if s.received_active.insert(id, (p.x, p.y)).is_some() {
            s.receive_errors += 1;
        }
    } else if msg == WM_POINTERUP {
        if s.received_active.remove(&id).is_none() {
            s.receive_errors += 1;
        }
    } else if let Some(point) = s.received_active.get_mut(&id) {
        *point = (p.x, p.y);
    } else {
        s.receive_errors += 1;
    }
    s.received_peak = s.received_peak.max(s.received_active.len());
    if s.received_active.len() == 2 {
        let pair: Vec<_> = s.received_active.iter().collect();
        let (&a, &(ax, ay)) = pair[0];
        let (&b, &(bx, by)) = pair[1];
        let distance = ((ax as f64 - bx as f64).powi(2) + (ay as f64 - by as f64).powi(2)).sqrt();
        if let Some((old_a, old_b, baseline)) = s.pinch_pair {
            if old_a == a && old_b == b && baseline > 0.0 {
                s.pinch_ratio = distance / baseline;
            } else {
                s.pinch_pair = Some((a, b, distance));
            }
        } else {
            s.pinch_pair = Some((a, b, distance));
            s.pinch_ratio = 1.0;
        }
    } else {
        s.pinch_pair = None;
    }
    let color = s.received_colors.get(&id).copied().unwrap_or(0);
    s.points.push_back(TracePoint {
        id,
        x: p.x,
        y: p.y,
        down: msg == WM_POINTERDOWN,
        color,
    });
    if s.points.len() > 5000 {
        s.points.pop_front();
    }
    if msg == WM_POINTERUP {
        s.received_colors.remove(&id);
    }
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
        paint_diagnostics(hwnd, buffer.dc, &rc, &ui.shared, ui.rect, 96, true);
        if let Err(error) = buffer.present(screen) {
            ui.log.event(format!("RENDER_ERROR {error}"));
        }
    });
    EndPaint(hwnd, &ps);
}
fn gesture_guard_label(s: &Stats) -> &str {
    if s.background_guard {
        // This mode has its own lifecycle and does not depend on foreground
        // controller ownership or receive that controller's gesture events.
        if s.guard_status.contains("ACTIVE") {
            if s.guard_status.contains("touchpad + touchscreen") {
                "ACTIVE (pad + screen)"
            } else if s.guard_status.ends_with(": touchscreen") {
                "ACTIVE (screen)"
            } else {
                "ACTIVE (background)"
            }
        } else if s.guard_status == "Windows gestures restored" {
            "RESTORED (background)"
        } else if s.guard_status.starts_with("Stopping") {
            "STOPPING (background)"
        } else if s.guard_status.starts_with("Restoring") {
            "RESTORING (background)"
        } else {
            "PREPARING (background)"
        }
    } else if !s.gesture_guard || s.capture_stopped {
        "OFF"
    } else if s.foreground {
        "ACTIVE (foreground)"
    } else {
        "INACTIVE (background)"
    }
}
pub(crate) fn diagnostic_text(s: &Stats) -> String {
    let text = format!("Pad2Touch PHASE 1+2 | {} | max {} touches\n{}\nRaw {} / Frames {} / Peak fingers {} / Parse errors {}\nInjected batches {} / Errors {} / Retry {} / Active {} / {}\nPT_TOUCH: DOWN {} / UPDATE {} / UP {} / Active {} / Peak {}\nTwo-finger ratio {:.2} | 3/4-finger guard: {}{}\n{}",if s.mode=="self-test"{"SYNTHETIC API TEST"}else{"PHYSICAL TOUCHPAD"},s.capacity,s.device_label,s.raw,s.frames,s.peak,s.parse_errors,s.injected,s.inject_errors,s.retries,s.injected_active,if s.cleanup_pending{"CLEANUP FAILED"}else if s.capture_stopped{"STOPPED"}else if s.armed{"ARMED"}else{"WAIT ALL FINGERS UP"},s.down,s.update,s.up,s.received_active.len(),s.received_peak,s.pinch_ratio,gesture_guard_label(s),if s.background_guard { String::new() } else { format!(" / events {}", s.gesture_events) },if !s.stop_reason.is_empty(){format!("Stopped: {} | Esc to close",s.stop_reason)}else if !s.error.is_empty(){s.error.clone()}else{"ESC: release and stop | Native mouse remains active".into()});
    let text = format!("{text}\n{}", s.report_rate.label());
    let text = if s.target_label.is_empty() {
        text
    } else {
        format!("{text}\nTarget: {} | {}", s.target_label, s.target_state)
    };
    if s.guard_status.is_empty() || s.background_guard {
        text
    } else {
        format!("{text}\n{}", s.guard_status)
    }
}

pub(crate) unsafe fn paint_diagnostics(
    hwnd: HWND,
    dc: HDC,
    rc: &RECT,
    shared: &Shared,
    rect: RECT,
    dpi: u32,
    canvas: bool,
) {
    let px = |n: i32| n * dpi as i32 / 96;
    FillRect(dc, rc, (COLOR_BTNFACE + 1) as HBRUSH);
    let (text, points, active, active_colors, pinch_ratio) = {
        let s = shared.lock().unwrap();
        let text = diagnostic_text(&s);
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
        -px(12),
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
    let mut header_origin = POINT::default();
    ClientToScreen(hwnd, &mut header_origin);
    let mut tr = RECT {
        left: px(20),
        top: px(12),
        right: rc.right - px(20),
        bottom: if rect.right > rect.left && rect.bottom > rect.top {
            rect.top - header_origin.y - px(8)
        } else {
            rc.bottom - px(12)
        },
    };
    let t = wide(&text);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, GetSysColor(COLOR_WINDOWTEXT));
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
        left: rect.left - org.x,
        top: rect.top - org.y,
        right: rect.right - org.x,
        bottom: rect.bottom - org.y,
    };
    // Without a canvas, `rect` only limits the text (e.g. above a diagram).
    let boxrect = if canvas { boxrect } else { RECT::default() };
    if boxrect.right > boxrect.left && boxrect.bottom > boxrect.top {
        FrameRect(dc, &boxrect, GetStockObject(GRAY_BRUSH) as HBRUSH);
    }
    let saved = SaveDC(dc);
    if boxrect.right > boxrect.left && boxrect.bottom > boxrect.top {
        IntersectClipRect(
            dc,
            boxrect.left + 1,
            boxrect.top + 1,
            boxrect.right - 1,
            boxrect.bottom - 1,
        );
    } else {
        IntersectClipRect(dc, 0, 0, 0, 0);
    }
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
    RestoreDC(dc, saved);
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
    target: crate::mapping::ResolvedTarget,
    target_origin: POINT,
    target_size: (i32, i32),
    tracker: Option<crate::mapping::Tracker>,
    last_occlusion_check: Instant,
    last_occlusion: Option<(usize, usize)>,
    synthetic_step: u32,
    capacity: usize,
    pending_since: Option<Instant>,
    scenarios: Vec<crate::scenarios::Step>,
    gesture_events: Option<Arc<AtomicU64>>,
    capture: crate::capture::Evidence,
    last_scope: Option<crate::capture::Scope>,
    rates: crate::report_rate::Meter,
}
fn worker_loop(
    device: Option<Device>,
    settings: (String, bool, usize, bool, bool),
    target: crate::mapping::ResolvedTarget,
    shared: Shared,
    log: Log,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    // Target resolution needs physical pixels on this thread as well.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let rect = target.bounds;
    let (mode, inject, capacity, suppress_gestures, touch_feedback) = settings;
    let fields = device
        .as_ref()
        .map(|d| d.fields())
        .transpose()?
        .unwrap_or_default();
    let injector = if inject {
        Some(Injector::new(
            target.hit,
            rect,
            target.input,
            capacity,
            touch_feedback,
            log.clone(),
        )?)
    } else {
        None
    };
    let mut target_origin = POINT::default();
    let mut target_client = RECT::default();
    if target.spec.is_diagnostic() {
        unsafe {
            if ClientToScreen(target.window as HWND, &mut target_origin) == 0
                || GetClientRect(target.window as HWND, &mut target_client) == 0
            {
                return Err(winerr("resolve diagnostic target"));
            }
        }
    }
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
        target_origin,
        target_size: (target_client.right, target_client.bottom),
        tracker: target.dynamic().then(|| crate::mapping::Tracker::new(rect)),
        last_occlusion_check: Instant::now(),
        last_occlusion: None,
        synthetic_step: 0,
        capacity,
        pending_since: None,
        scenarios: crate::scenarios::steps(capacity),
        gesture_events: None,
        capture: crate::capture::Evidence::default(),
        last_scope: None,
        rates: crate::report_rate::Meter::default(),
    };
    unsafe {
        let mut gesture_guard = if suppress_gestures {
            let guard = crate::gestures::Guard::create(log.clone())?;
            worker.shared.lock().unwrap().gesture_guard = true;
            worker.gesture_events = Some(guard.event_counter());
            Some(guard)
        } else {
            None
        };
        let hwnd = make_window("Pad2TouchPhase0RawInput", "", Some(input_proc), false)?;
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
    fn accept(&mut self, contacts: Vec<Contact>, start: Instant, scan: Option<u32>) {
        if self.stop.load(Ordering::Relaxed) || EXIT.load(Ordering::Relaxed) {
            return;
        }
        let scope = self.capture.frame(&contacts);
        let live = contacts.iter().filter(|c| c.tip && c.confidence).count();
        if self.mode == "capture" {
            self.rates.frame(start, scan, live > 0);
        }
        self.log.event(format!(
            "FRAME active={live} records={} contacts={contacts:?} foreground_scope={} scan_time={}",
            contacts.len(),
            scope.label(),
            scan.map(|v| v.to_string())
                .unwrap_or_else(|| "unavailable".into())
        ));
        let signal = {
            let mut s = self.shared.lock().unwrap();
            s.frames += 1;
            s.peak = s.peak.max(live);
            s.live = contacts
                .iter()
                .filter(|c| c.tip && c.confidence)
                .map(|c| {
                    let norm = |v: i32, min: i32, max: i32| {
                        (v as f64 - min as f64) / (max as f64 - min as f64).max(1.0)
                    };
                    (norm(c.x, c.xmin, c.xmax), norm(c.y, c.ymin, c.ymax))
                })
                .collect();
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
            s.frame_signal
        };
        if let Some(signal) = signal {
            signal.notify();
        }
        let paused = self.tracker.as_ref().is_some_and(|t| t.paused());
        if let Some(i) = self.injector.as_mut().filter(|_| !paused) {
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
        self.shared.lock().unwrap().report_rate = self.rates.snapshot(Instant::now());
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
            s.injection_blocked = i.blocked();
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
            self.rates.raw(Instant::now());
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
                    let frame_scan = r.scan;
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
                            self.accept(frame, start, frame_scan)
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
        if self.tracker.is_some() {
            if !self.follow_target() {
                return;
            }
        } else if self.injector.is_some() && self.target.spec.is_diagnostic() {
            unsafe {
                let target = self.target.window;
                let mut origin = POINT::default();
                let mut client = RECT::default();
                let reason = if IsWindow(target as HWND) == 0 {
                    Some("receiver closed; fixed mapping stopped")
                } else if IsIconic(GetAncestor(target as HWND, GA_ROOT)) != 0 {
                    Some("receiver minimized (possible Windows system gesture); injection stopped")
                } else if ClientToScreen(target as HWND, &mut origin) == 0 {
                    Some("receiver position unavailable; fixed mapping stopped")
                } else if origin.x != self.target_origin.x || origin.y != self.target_origin.y {
                    Some("receiver moved; fixed mapping stopped")
                } else if GetClientRect(target as HWND, &mut client) == 0
                    || (client.right, client.bottom) != self.target_size
                {
                    Some("receiver resized; fixed mapping stopped")
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
                self.accept(contacts, Instant::now(), None);
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
    /// Follow a window target. Returns false when the session must stop.
    fn follow_target(&mut self) -> bool {
        use crate::mapping::Decision;
        let observation = self.target.observe();
        let decision = self.tracker.as_mut().unwrap().observe(observation);
        match decision {
            Decision::Keep => {}
            Decision::Retarget(r) | Decision::Resume(r) => {
                let resumed = matches!(decision, Decision::Resume(_));
                // Fill-area mode re-crops the pad when the window's shape changes.
                let input = self.target.input_for(r);
                let result = self
                    .injector
                    .as_mut()
                    .map_or(Ok(()), |i| i.retarget(r, input));
                self.target.bounds = r;
                self.target.input = input;
                self.shared.lock().unwrap().mapped_input = Some(input);
                self.log.event(crate::experiment::target_record(
                    if resumed {
                        "TARGET_RESUME"
                    } else {
                        "TARGET_UPDATE"
                    },
                    &self.target,
                    r,
                ));
                {
                    let mut s = self.shared.lock().unwrap();
                    s.target_updates += 1;
                    s.mapped = r;
                    s.target_state = format!(
                        "{} {}; lift all fingers to continue",
                        if resumed { "resumed at" } else { "moved to" },
                        crate::mapping::describe(r)
                    );
                }
                self.last_occlusion = None;
                if let Err(e) = result {
                    self.error(e);
                }
            }
            Decision::Pause(reason) => {
                self.log.event(format!("TARGET_PAUSE reason={reason}"));
                {
                    let mut s = self.shared.lock().unwrap();
                    s.target_pauses += 1;
                    s.target_state = format!("paused: {reason}; contacts released");
                }
                if let Some(i) = &mut self.injector {
                    if let Err(e) = i.release(true) {
                        self.error(e);
                    }
                }
                self.assembler.reset();
                self.pending_since = None;
            }
            Decision::Stop(reason) => {
                let reason = format!("{reason}; injection stopped");
                self.log.event(format!("TARGET_LOST reason={reason}"));
                self.error(reason.clone());
                self.shared.lock().unwrap().stop_reason = reason;
                self.stop.store(true, Ordering::Relaxed);
                unsafe { PostQuitMessage(0) };
                return false;
            }
        }
        self.sample_occlusion();
        true
    }
    /// Proactive warning only: per-pointer hit tests decide actual injection.
    fn sample_occlusion(&mut self) {
        if self.tracker.as_ref().is_some_and(|t| t.paused())
            || self.last_occlusion_check.elapsed() < Duration::from_millis(250)
        {
            return;
        }
        self.last_occlusion_check = Instant::now();
        let points = crate::mapping::sample_points(self.target.bounds, 3);
        let covered = points
            .iter()
            .filter(|&&p| unsafe {
                let hit = WindowFromPoint(p);
                let root = if hit.is_null() {
                    null_mut()
                } else {
                    GetAncestor(hit, GA_ROOT)
                };
                !self.target.hit.accepts(hit as usize, root as usize)
            })
            .count();
        let sample = (covered, points.len());
        if self.last_occlusion != Some(sample) {
            self.last_occlusion = Some(sample);
            self.log.event(format!(
                "TARGET_OCCLUSION covered={covered} samples={}",
                points.len()
            ));
            let r = crate::mapping::describe(self.target.bounds);
            self.shared.lock().unwrap().target_state = if covered == 0 {
                format!("visible, {r}")
            } else {
                format!(
                    "COVERED at {covered}/{} sample points, {r}; touches there are blocked",
                    points.len()
                )
            };
        }
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
        {
            let mut s = self.shared.lock().unwrap();
            s.capture_stopped = true;
            s.live.clear();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_guard_diagnostics_follow_its_own_lifecycle_and_scope() {
        let mut s = Stats {
            background_guard: true,
            ..Default::default()
        };
        for (status, label) in [
            (
                "Preparing background guard and recovery watchdog...",
                "PREPARING (background)",
            ),
            ("Background guard ACTIVE: touchpad", "ACTIVE (background)"),
            (
                "Background guard ACTIVE: touchpad + touchscreen",
                "ACTIVE (pad + screen)",
            ),
            ("Background guard ACTIVE: touchscreen", "ACTIVE (screen)"),
            (
                "Stopping capture before restoring Windows gestures...",
                "STOPPING (background)",
            ),
            (
                "Restoring Windows gesture settings...",
                "RESTORING (background)",
            ),
            ("Windows gestures restored", "RESTORED (background)"),
        ] {
            s.guard_status = status.into();
            for foreground in [false, true] {
                s.foreground = foreground;
                let text = diagnostic_text(&s);
                assert!(text.contains(&format!("3/4-finger guard: {label}")));
                assert_eq!(text.matches(label).count(), 1);
                assert!(!text.contains("3/4-finger guard: OFF"));
                assert!(!text.contains(" / events "));
            }
        }
        s.background_guard = false;
        s.gesture_guard = true;
        s.foreground = false;
        assert_eq!(gesture_guard_label(&s), "INACTIVE (background)");
        s.foreground = true;
        assert_eq!(gesture_guard_label(&s), "ACTIVE (foreground)");
        s.capture_stopped = true;
        assert_eq!(gesture_guard_label(&s), "OFF");
    }

    #[test]
    fn frame_signal_keeps_one_message_in_flight() {
        static PENDING: AtomicBool = AtomicBool::new(false);
        const MSG: u32 = WM_APP + 50;
        // A null HWND posts to this thread's own queue.
        let signal = FrameSignal::new(null_mut(), MSG, &PENDING);
        let drain = || {
            let mut msg = MSG::default();
            let mut count = 0;
            // SAFETY: PeekMessageW only reads this thread's queue.
            while unsafe { PeekMessageW(&mut msg, null_mut(), MSG, MSG, PM_REMOVE) } != 0 {
                count += 1;
            }
            count
        };
        drain();
        for _ in 0..5 {
            signal.notify();
        }
        assert_eq!(drain(), 1);
        // Still pending: nothing was handled, so frames stay coalesced.
        signal.notify();
        assert_eq!(drain(), 0);
        // The receiver handled it; the next frame posts again.
        PENDING.store(false, Ordering::Release);
        signal.notify();
        assert_eq!(drain(), 1);
    }
}
