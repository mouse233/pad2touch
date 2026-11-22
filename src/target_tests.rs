//! Native Phase 3 integration checks against a real window owned by another
//! process. The test binary re-launches itself as that host process.
//!
//! All tests here are ignored by default. Only
//! injection tests submit native touch input only through the same hit-tested transport
//! used by capture sessions, into the dedicated host window.
use crate::{
    experiment::wide,
    injection::Injector,
    logging::Log,
    mapping::{
        self, tuple, Decision, HitPolicy, MappingTarget, NormRect, Observation, ResolvedTarget,
        Tracker,
    },
    touch::{tests::contact, FrameOutcome},
};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    ptr::{null, null_mut},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::{ClientToScreen, ScreenToClient, UpdateWindow, COLOR_WINDOW},
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, Input::Pointer::*, WindowsAndMessaging::*},
};

const HOST_ENV: &str = "PAD2TOUCH_PHASE3_HOST";
const DPI_ENV: &str = "PAD2TOUCH_PHASE3_HOST_DPI";

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP => {
            let id = (w & 0xffff) as u32;
            let mut kind = 0;
            if GetPointerType(id, &mut kind) != 0 && kind == PT_TOUCH {
                let mut p = POINT {
                    x: (l & 0xffff) as u16 as i16 as i32,
                    y: ((l >> 16) & 0xffff) as u16 as i16 as i32,
                };
                ScreenToClient(hwnd, &mut p);
                let mut rc = RECT::default();
                GetClientRect(hwnd, &mut rc);
                let action = match msg {
                    WM_POINTERDOWN => "DOWN",
                    WM_POINTERUP => "UP",
                    _ => "UPDATE",
                };
                let mut out = std::io::stdout().lock();
                let _ = writeln!(
                    out,
                    "HOST TOUCH {action} id={id} fx={:.5} fy={:.5} cx={} cy={} w={} h={}",
                    p.x as f64 / rc.right.max(1) as f64,
                    p.y as f64 / rc.bottom.max(1) as f64,
                    p.x,
                    p.y,
                    rc.right,
                    rc.bottom
                );
                let _ = out.flush();
                return 0;
            }
        }
        WM_TIMER => {
            DestroyWindow(hwnd);
            return 0;
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}

/// Not a test of its own: the body of the external host process.
#[test]
#[ignore = "helper process for Phase 3 native tests"]
fn phase3_target_host_process() {
    let Ok(nonce) = std::env::var(HOST_ENV) else {
        return;
    };
    unsafe {
        let context = if std::env::var(DPI_ENV).as_deref() == Ok("unaware") {
            DPI_AWARENESS_CONTEXT_UNAWARE
        } else {
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
        };
        SetThreadDpiAwarenessContext(context);
        let class = wide("Pad2TouchPhase3Host");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(host_proc),
            hInstance: GetModuleHandleW(null()),
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as _,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide(&format!("Pad2Touch Phase 3 host {nonce}")).as_ptr(),
            WS_OVERLAPPEDWINDOW,
            200,
            150,
            700,
            450,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        assert!(!hwnd.is_null());
        ShowWindow(hwnd, SW_SHOWNORMAL);
        UpdateWindow(hwnd);
        SetTimer(hwnd, 1, 120_000, None);
        println!("\nHOST READY hwnd=0x{:X}", hwnd as usize);
        std::io::stdout().flush().unwrap();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Touch {
    pub action: String,
    pub id: u32,
    pub fx: f64,
    pub fy: f64,
}

pub(crate) struct Host {
    child: Child,
    pub hwnd: usize,
    pub nonce: String,
    lines: Receiver<String>,
}
impl Host {
    pub fn spawn(dpi: &str) -> Self {
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos() ^ (dpi.len() as u128)
        ) + dpi;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "target_tests::phase3_target_host_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HOST_ENV, &nonce)
            .env(DPI_ENV, dpi)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch Phase 3 host process");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // The test harness may print its own prefix on the same line.
                if let Some(at) = line.find("HOST ") {
                    if tx.send(line[at..].to_string()).is_err() {
                        break;
                    }
                }
            }
        });
        let ready = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("host window did not start");
        let hwnd = usize::from_str_radix(ready.trim_start_matches("HOST READY hwnd=0x"), 16)
            .expect("host handle");
        Self {
            child,
            hwnd,
            nonce,
            lines: rx,
        }
    }
    pub fn handle(&self) -> HWND {
        self.hwnd as HWND
    }
    /// Place the host in physical pixels and keep it above ordinary windows.
    pub unsafe fn place(&self, x: i32, y: i32, w: i32, h: i32) {
        assert_ne!(
            SetWindowPos(self.handle(), HWND_TOPMOST, x, y, w, h, SWP_SHOWWINDOW),
            0
        );
        std::thread::sleep(Duration::from_millis(150));
    }
    /// Touch notifications received until the host is quiet for `quiet`.
    pub fn touches(&self, quiet: Duration) -> Vec<Touch> {
        let mut out = Vec::new();
        while let Ok(line) = self.lines.recv_timeout(quiet) {
            let get = |key: &str| {
                line.split_whitespace()
                    .find_map(|s| s.strip_prefix(key))
                    .unwrap_or_default()
                    .to_string()
            };
            if let Some(rest) = line.strip_prefix("HOST TOUCH ") {
                out.push(Touch {
                    action: rest.split_whitespace().next().unwrap().to_string(),
                    id: get("id=").parse().unwrap(),
                    fx: get("fx=").parse().unwrap(),
                    fy: get("fy=").parse().unwrap(),
                });
            }
        }
        out
    }
    pub fn close(mut self) {
        unsafe { PostMessageW(self.handle(), WM_CLOSE, 0, 0) };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("host did not close");
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Independently computed physical client rectangle (caller must be PMv2).
unsafe fn client(hwnd: HWND) -> RECT {
    let mut rc = RECT::default();
    assert_ne!(GetClientRect(hwnd, &mut rc), 0);
    let mut a = POINT { x: 0, y: 0 };
    let mut b = POINT {
        x: rc.right,
        y: rc.bottom,
    };
    ClientToScreen(hwnd, &mut a);
    ClientToScreen(hwnd, &mut b);
    RECT {
        left: a.x,
        top: a.y,
        right: b.x,
        bottom: b.y,
    }
}

/// An ordinary opaque window. (A STATIC control would answer HTTRANSPARENT
/// and be skipped by WindowFromPoint, unlike a real covering application.)
unsafe fn cover(r: RECT) -> HWND {
    let class = wide("Pad2TouchPhase3Occluder");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(DefWindowProcW),
        hInstance: GetModuleHandleW(null()),
        lpszClassName: class.as_ptr(),
        hbrBackground: (COLOR_WINDOW + 2) as _,
        ..Default::default()
    };
    RegisterClassW(&wc);
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class.as_ptr(),
        wide("Pad2Touch occluder").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        null_mut(),
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    assert!(!hwnd.is_null());
    SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    hwnd
}

fn covered(target: &ResolvedTarget) -> usize {
    mapping::sample_points(target.bounds, 3)
        .into_iter()
        .filter(|&p| target.hit.hit_test(p).is_err())
        .count()
}

fn wait_for(mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
#[ignore = "creates windows in a helper process; run on an interactive Windows desktop"]
fn native_external_window_targets_follow_move_resize_minimize_and_close() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let host = Host::spawn("pmv2");
        host.place(160, 140, 820, 520);
        let listed = mapping::list_windows();
        let entry = listed
            .iter()
            .find(|w| w.handle == host.hwnd)
            .expect("host window is listed as a target");
        assert!(entry.title.contains(&host.nonce));
        assert!(
            !listed
                .iter()
                .any(|w| GetAncestor(w.handle as HWND, GA_ROOT) as usize != w.handle),
            "only top-level windows are offered"
        );
        assert!(mapping::integrity_warning(host.hwnd).is_none());

        // Window mode resolves the physical client area, not the outer frame.
        let spec = MappingTarget::Window { handle: host.hwnd };
        let target = spec.resolve(null_mut()).unwrap();
        let expected = client(host.handle());
        assert_eq!(tuple(target.bounds), tuple(expected));
        let mut frame = RECT::default();
        GetWindowRect(host.handle(), &mut frame);
        assert!(target.bounds.top > frame.top, "title bar excluded");
        assert_eq!(target.hit, HitPolicy::SameRoot(host.hwnd));

        // Physical-aspect fitting stays inside the client area.
        let fitted = spec.resolve_with_aspect(null_mut(), 1.6).unwrap();
        assert!(mapping::intersect(fitted.bounds, expected).is_some());
        assert!(fitted.bounds.left >= expected.left && fitted.bounds.right <= expected.right);

        // Window rectangle mode follows the client area proportionally.
        let area = NormRect {
            left: 0.25,
            top: 0.2,
            right: 0.75,
            bottom: 0.8,
        };
        let rect_spec = MappingTarget::WindowRectangle {
            handle: host.hwnd,
            area,
        };
        let rect_target = rect_spec.resolve(null_mut()).unwrap();
        assert_eq!(tuple(rect_target.bounds), tuple(area.apply(expected)));

        let mut tracker = Tracker::new(target.bounds);
        assert_eq!(tracker.observe(target.observe()), Decision::Keep);

        // Move + resize: Retarget to the new physical client area.
        host.place(260, 200, 900, 600);
        let moved = client(host.handle());
        assert_ne!(tuple(moved), tuple(expected));
        assert_eq!(tracker.observe(target.observe()), Decision::Retarget(moved));
        assert_eq!(
            rect_spec.observe_bounds(),
            Observation::Ready(area.apply(moved))
        );

        // Occlusion sampling distinguishes children/self from other windows.
        let target = spec.resolve(null_mut()).unwrap();
        assert_eq!(covered(&target), 0, "host must start uncovered");
        let c = moved;
        let cx = (c.left + c.right) / 2;
        let cy = (c.top + c.bottom) / 2;
        let occluder = cover(RECT {
            left: cx - 40,
            top: cy - 40,
            right: cx + 40,
            bottom: cy + 40,
        });
        assert!(wait_for(|| covered(&target) == 1), "center sample covered");
        DestroyWindow(occluder);
        assert!(wait_for(|| covered(&target) == 0));

        // Minimize pauses; restore resumes (requiring a new clear frame).
        ShowWindow(host.handle(), SW_MINIMIZE);
        assert!(wait_for(|| IsIconic(host.handle()) != 0));
        assert_eq!(
            tracker.observe(target.observe()),
            Decision::Pause("target window minimized".into())
        );
        assert_eq!(tracker.observe(target.observe()), Decision::Keep);
        assert!(spec.resolve(null_mut()).is_err(), "cannot start minimized");
        ShowWindow(host.handle(), SW_RESTORE);
        assert!(wait_for(|| IsIconic(host.handle()) == 0));
        assert!(matches!(
            tracker.observe(target.observe()),
            Decision::Resume(_)
        ));

        // Hidden windows pause as well.
        ShowWindow(host.handle(), SW_HIDE);
        assert!(wait_for(|| IsWindowVisible(host.handle()) == 0));
        assert_eq!(
            tracker.observe(target.observe()),
            Decision::Pause("target window hidden".into())
        );
        ShowWindow(host.handle(), SW_SHOWNOACTIVATE);
        assert!(wait_for(|| IsWindowVisible(host.handle()) != 0));
        assert!(matches!(
            tracker.observe(target.observe()),
            Decision::Resume(_)
        ));

        // Pad2Touch's own windows are not application targets.
        let own = cover(RECT {
            left: 0,
            top: 0,
            right: 40,
            bottom: 40,
        });
        let error = MappingTarget::Window {
            handle: own as usize,
        }
        .resolve(null_mut())
        .err()
        .unwrap();
        assert!(error.contains("own window"), "{error}");
        DestroyWindow(own);

        // Destruction stops the session.
        let handle = host.hwnd;
        host.close();
        assert_eq!(IsWindow(handle as HWND), 0);
        assert!(matches!(
            tracker.observe(target.observe()),
            Decision::Stop(_)
        ));
    }
}

fn near(actual: f64, expected: f64, tolerance: f64) -> bool {
    (actual - expected).abs() <= tolerance
}

/// Fraction of the client area at which a mapped raw coordinate should arrive.
fn expected_fraction(raw: i32, start: i32, len: i32) -> f64 {
    let p = crate::decode::map_axis(raw, 0, 1000, start, len).unwrap();
    (p - start) as f64 / len as f64
}

/// Audit one injector's log segment with the offline verifier.
fn audit(lines: Vec<String>) -> crate::verify::Audit {
    let mut text = vec!["t_us=0 RUN format=2 mode=capture inject=true capacity=2".to_string()];
    text.extend(lines);
    text.push("t_us=9 WORKER_STOP active_contacts=0 cleanup_pending=false".into());
    text.push("t_us=9 SUMMARY run_status=COMPLETED log_dropped=0 parse_errors=0 incomplete_frames=0 injection_errors=0 receiver_lifecycle_errors=0".into());
    crate::verify::analyze(&text.join("\n")).unwrap()
}

/// Tap at each raw coordinate: DOWN, UPDATE, then a clear frame for UP.
fn taps(injector: &mut Injector, id: u32, raws: &[(i32, i32)]) {
    for &(x, y) in raws {
        injector.frame(&[contact(id, x, y)]).unwrap();
        injector.frame(&[contact(id, x, y)]).unwrap();
        injector.frame(&[]).unwrap();
    }
}

#[test]
#[ignore = "temporarily suppresses touchpad/touchscreen system gestures and injects multi-finger swipes into a dedicated helper window"]
fn native_background_guard_preserves_injected_multitouch() {
    use std::sync::{atomic::AtomicBool, Arc, Mutex};
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let out = std::path::PathBuf::from(format!("artifacts/runs/touchscreen-guard-test-{stamp}"));
    let (log, writer) = Log::open(&out).unwrap();
    let shared = Arc::new(Mutex::new(crate::experiment::Stats::default()));
    let guard = crate::background_guard::Guard::acquire(
        &out,
        shared,
        log.clone(),
        Arc::new(AtomicBool::new(false)),
        crate::gesture_backend::Scope {
            touchpad: true,
            touchscreen: true,
        },
        None,
    )
    .unwrap()
    .unwrap();
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let host = Host::spawn("pmv2");
        host.place(180, 160, 840, 540);
        let target = MappingTarget::Window { handle: host.hwnd }
            .resolve(null_mut())
            .unwrap();
        assert_eq!(covered(&target), 0);
        let mut injector = Injector::new(
            target.hit,
            target.bounds,
            NormRect::FULL,
            5,
            false,
            log.clone(),
        )
        .unwrap();
        for fingers in [3, 4, 5] {
            // Swipe down far enough to exercise shell recognition while keeping
            // every point safely inside the dedicated host's client rectangle.
            for step in 0..25 {
                let contacts: Vec<_> = (0..fingers)
                    .map(|id| contact(id + 1, 250 + id as i32 * 100, 250 + step * 20))
                    .collect();
                injector.frame(&contacts).unwrap();
                std::thread::sleep(Duration::from_millis(10));
            }
            injector.frame(&[]).unwrap();
            let touches = host.touches(Duration::from_millis(300));
            let downs = touches.iter().filter(|t| t.action == "DOWN").count();
            let ups = touches.iter().filter(|t| t.action == "UP").count();
            assert_eq!(
                (downs, ups),
                (fingers as usize, fingers as usize),
                "{fingers} fingers: {touches:?}"
            );
            assert_eq!(
                IsIconic(host.handle()),
                0,
                "host minimized by system gesture"
            );
            assert_eq!(
                GetAncestor(GetForegroundWindow(), GA_ROOT),
                host.handle(),
                "system gesture took foreground"
            );
        }
        assert_eq!(injector.active_count(), 0);
        drop(injector);
    }
    guard.release().unwrap();
    drop(log);
    writer.join().unwrap().unwrap();
}

#[test]
#[ignore = "SUBMITS NATIVE TOUCH INPUT into a helper window; run alone on an interactive desktop"]
fn native_touch_injection_lands_in_external_window_at_mapped_positions() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        for dpi in ["pmv2", "unaware"] {
            let host = Host::spawn(dpi);
            host.place(180, 160, 840, 540);
            let monitor_dpi = {
                let probe = cover(RECT {
                    left: 200,
                    top: 200,
                    right: 210,
                    bottom: 210,
                });
                let d = GetDpiForWindow(probe);
                DestroyWindow(probe);
                d.max(96)
            };
            println!(
                "host={dpi} monitor_dpi={monitor_dpi} host_window_dpi={}",
                GetDpiForWindow(host.handle())
            );

            // 1. Window mode: corners and center of the whole client area.
            let (log, rx) = Log::memory();
            let spec = MappingTarget::Window { handle: host.hwnd };
            let target = spec.resolve(null_mut()).unwrap();
            assert_eq!(covered(&target), 0, "{dpi}: host must be unobstructed");
            log.event(crate::experiment::target_record(
                "TARGET",
                &target,
                target.bounds,
            ));
            let mut injector = Injector::new(
                target.hit,
                target.bounds,
                NormRect::FULL,
                2,
                false,
                log.clone(),
            )
            .unwrap();
            injector.frame(&[]).unwrap();
            let c = target.bounds;
            let (w, h) = (c.right - c.left, c.bottom - c.top);
            // One physical pixel plus DPI-virtualization rounding of the host.
            let tol = (2.0 + monitor_dpi as f64 / 96.0) / w.min(h) as f64;
            taps(&mut injector, 1, &[(0, 0), (500, 500), (1000, 1000)]);
            let touches = host.touches(Duration::from_millis(400));
            let downs: Vec<_> = touches.iter().filter(|t| t.action == "DOWN").collect();
            let ups = touches.iter().filter(|t| t.action == "UP").count();
            assert_eq!((downs.len(), ups), (3, 3), "{dpi}: {touches:?}");
            for (t, raw) in downs.iter().zip([0, 500, 1000]) {
                let (ex, ey) = (
                    expected_fraction(raw, c.left, w),
                    expected_fraction(raw, c.top, h),
                );
                assert!(
                    near(t.fx, ex, tol) && near(t.fy, ey, tol),
                    "{dpi}: raw {raw} arrived at ({}, {}), expected ({ex}, {ey})",
                    t.fx,
                    t.fy
                );
            }

            // 2. Moving/resizing retargets; the same raw point keeps its
            //    relative position in the new client area.
            host.place(320, 220, 760, 500);
            let mut tracker = Tracker::new(target.bounds);
            let Decision::Retarget(moved) = tracker.observe(target.observe()) else {
                panic!("{dpi}: move not detected");
            };
            injector.retarget(moved, NormRect::FULL).unwrap();
            log.event(crate::experiment::target_record(
                "TARGET_UPDATE",
                &target,
                moved,
            ));
            injector.frame(&[]).unwrap();
            taps(&mut injector, 4, &[(250, 750)]);
            drop(injector);
            let touches = host.touches(Duration::from_millis(400));
            let down = touches
                .iter()
                .find(|t| t.action == "DOWN")
                .expect("DOWN after move");
            let (mw, mh) = (moved.right - moved.left, moved.bottom - moved.top);
            let (ex, ey) = (
                expected_fraction(250, moved.left, mw),
                expected_fraction(750, moved.top, mh),
            );
            assert!(
                near(down.fx, ex, tol) && near(down.fy, ey, tol),
                "{dpi}: {down:?} expected ({ex}, {ey})"
            );
            assert!(touches.iter().any(|t| t.action == "UP" && t.id == down.id));
            let a = audit(rx.try_iter().collect());
            assert!(a.passed(), "{dpi}: {:?}", a.errors);
            assert!(!a.receiver_audited);
            assert_eq!((a.injected_lifetimes, a.target_updates), (4, 1));

            // 3. Window rectangle: only the selected sub-area is used.
            let (log, rx) = Log::memory();
            let area = NormRect {
                left: 0.25,
                top: 0.25,
                right: 0.75,
                bottom: 0.75,
            };
            let rect_target = MappingTarget::WindowRectangle {
                handle: host.hwnd,
                area,
            }
            .resolve(null_mut())
            .unwrap();
            log.event(crate::experiment::target_record(
                "TARGET",
                &rect_target,
                rect_target.bounds,
            ));
            let mut injector = Injector::new(
                rect_target.hit,
                rect_target.bounds,
                NormRect::FULL,
                2,
                false,
                log.clone(),
            )
            .unwrap();
            injector.frame(&[]).unwrap();
            taps(&mut injector, 5, &[(0, 0), (1000, 1000)]);
            let touches = host.touches(Duration::from_millis(400));
            let downs: Vec<_> = touches.iter().filter(|t| t.action == "DOWN").collect();
            assert_eq!(downs.len(), 2, "{dpi}: {touches:?}");
            assert!(
                near(downs[0].fx, 0.25, tol) && near(downs[0].fy, 0.25, tol),
                "{dpi}: {:?}",
                downs[0]
            );
            assert!(
                near(downs[1].fx, 0.75, tol) && near(downs[1].fy, 0.75, tol),
                "{dpi}: {:?}",
                downs[1]
            );

            // 4. Occlusion: a covering window blocks the DOWN before
            //    InjectTouchInput, so nothing reaches the host.
            let r = rect_target.bounds;
            let center = POINT {
                x: (r.left + r.right) / 2,
                y: (r.top + r.bottom) / 2,
            };
            let occluder = cover(RECT {
                left: center.x - 30,
                top: center.y - 30,
                right: center.x + 30,
                bottom: center.y + 30,
            });
            assert!(wait_for(|| covered(&rect_target) > 0));
            assert_eq!(
                injector.frame(&[contact(6, 500, 500)]),
                Ok(FrameOutcome::Occluded)
            );
            assert_eq!(injector.failures(), 0);
            assert_eq!((injector.active_count(), injector.blocked()), (0, 1));
            assert!(host.touches(Duration::from_millis(250)).is_empty());
            DestroyWindow(occluder);
            assert!(wait_for(|| covered(&rect_target) == 0));
            drop(injector);
            let lines: Vec<String> = rx.try_iter().collect();
            assert!(lines.iter().any(|l| l.contains("INJECT_BLOCKED")));
            let a = audit(lines);
            assert!(a.passed(), "{dpi}: {:?}", a.errors);
            assert_eq!(a.injected_lifetimes, 2);

            // 5. Screen rectangle: no window ownership; Windows hit testing
            //    delivers to whatever is on screen there (the host).
            let (log, rx) = Log::memory();
            let screen = MappingTarget::ScreenRectangle {
                bounds: client(host.handle()),
            }
            .resolve(null_mut())
            .unwrap();
            assert_eq!(screen.hit, HitPolicy::Any);
            log.event(crate::experiment::target_record(
                "TARGET",
                &screen,
                screen.bounds,
            ));
            let mut injector = Injector::new(
                screen.hit,
                screen.bounds,
                NormRect::FULL,
                1,
                false,
                log.clone(),
            )
            .unwrap();
            injector.frame(&[]).unwrap();
            taps(&mut injector, 7, &[(500, 500)]);
            drop(injector);
            let touches = host.touches(Duration::from_millis(400));
            let down = touches
                .iter()
                .find(|t| t.action == "DOWN")
                .expect("screen DOWN");
            assert!(
                near(down.fx, 0.5, tol) && near(down.fy, 0.5, tol),
                "{dpi}: {down:?}"
            );
            let a = audit(rx.try_iter().collect());
            assert!(a.passed(), "{dpi}: {:?}", a.errors);
            assert_eq!(log.dropped.load(std::sync::atomic::Ordering::Relaxed), 0);

            // 6. Touchpad area "part of pad · fill area": a tall, narrow
            //    window strip uses only a centered part of a 1.6:1 touchpad.
            let (log, rx) = Log::memory();
            let strip = MappingTarget::WindowRectangle {
                handle: host.hwnd,
                area: NormRect {
                    left: 0.4,
                    top: 0.0,
                    right: 0.6,
                    bottom: 1.0,
                },
            }
            .resolve_fitted(null_mut(), Some(1.6), crate::mapping::PadFit::FillArea)
            .unwrap();
            let input = strip.input;
            assert!(input.left > 0.2 && input.right < 0.8, "{input:?}");
            assert_eq!((input.top, input.bottom), (0.0, 1.0));
            log.event(crate::experiment::target_record(
                "TARGET",
                &strip,
                strip.bounds,
            ));
            let mut injector =
                Injector::new(strip.hit, strip.bounds, input, 2, false, log.clone()).unwrap();
            injector.frame(&[]).unwrap();
            // Outside the used part of the pad: ignored, nothing delivered.
            taps(&mut injector, 8, &[(0, 500), (1000, 500)]);
            assert!(host.touches(Duration::from_millis(250)).is_empty());
            // The used part's left edge and center reach the strip's left
            // edge and center: the whole area is used, undistorted.
            let edge = (input.left * 1000.0).ceil() as i32;
            taps(&mut injector, 9, &[(edge, 0), (500, 500)]);
            drop(injector);
            let touches = host.touches(Duration::from_millis(400));
            let downs: Vec<_> = touches.iter().filter(|t| t.action == "DOWN").collect();
            assert_eq!(downs.len(), 2, "{dpi}: {touches:?}");
            assert!(
                near(downs[0].fx, 0.4, tol) && near(downs[0].fy, 0.0, tol),
                "{dpi}: {:?}",
                downs[0]
            );
            assert!(
                near(downs[1].fx, 0.5, tol) && near(downs[1].fy, 0.5, tol),
                "{dpi}: {:?}",
                downs[1]
            );
            let a = audit(rx.try_iter().collect());
            assert!(a.passed(), "{dpi}: {:?}", a.errors);
            assert_eq!(a.injected_lifetimes, 2);
            host.close();
        }
    }
}
