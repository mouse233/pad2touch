//! Asynchronous GUI adapter for the native gesture worker and its watchdog.
use crate::{
    experiment::Shared,
    gesture_backend::{self, restored, Request, Scope},
    logging::Log,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The GUI may keep an idle worker while Hold is enabled. Its input pipe
/// closes when the last handle is dropped. An active session keeps a handle
/// until capture stops and settings have been restored.
#[derive(Clone)]
pub struct Backend(Arc<Mutex<Child>>);
impl Backend {
    pub fn spawn(prewarm_touchpad: bool) -> Result<Self, String> {
        let mut command = gesture_backend::command("--gesture-worker")?;
        let args = [
            std::process::id().to_string(),
            gesture_backend::self_started()?.to_string(),
            prewarm_touchpad.to_string(),
        ];
        #[cfg(test)]
        command.env(
            "PAD2TOUCH_TEST_GESTURE_ARGS",
            serde_json::to_string(&args).map_err(|e| e.to_string())?,
        );
        #[cfg(not(test))]
        command.args(args);
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Could not start the native gesture worker: {e}"))?;
        Ok(Self(Arc::new(Mutex::new(child))))
    }

    fn running(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .try_wait()
            .is_ok_and(|status| status.is_none())
    }

    fn start(&self, request: &Request) -> Result<(), String> {
        let mut message = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        message.push(b'\n');
        let mut child = self.0.lock().unwrap();
        let input = child
            .stdin
            .as_mut()
            .ok_or("Gesture worker input is unavailable")?;
        input.write_all(&message).map_err(|e| e.to_string())
    }

    #[cfg(test)]
    fn kill_for_test(&self) {
        let mut child = self.0.lock().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

pub struct Guard {
    backend: Backend,
    directory: PathBuf,
    monitor_stop: Arc<AtomicBool>,
    monitor: Option<JoinHandle<()>>,
    shared: Shared,
    log: Log,
}

fn signal_stop(directory: &Path) -> Result<(), String> {
    fs::write(directory.join("stop-requested.json"), "{}").map_err(|e| e.to_string())
}

fn finish_error(directory: &Path) -> Option<String> {
    let finished = fs::read(directory.join("finished.json")).ok()?;
    let record: serde_json::Value = serde_json::from_slice(&finished).ok()?;
    record["error"].as_str().map(str::to_owned)
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop_monitor();
        let _ = signal_stop(&self.directory);
        if self.directory.join("mutation-started.json").exists() && !restored(&self.directory) {
            self.shared.lock().unwrap().recovery_pending = true;
        }
    }
}

impl Guard {
    pub fn acquire(
        out: &Path,
        shared: Shared,
        log: Log,
        stop: Arc<AtomicBool>,
        scope: Scope,
        prewarmed: Option<Backend>,
    ) -> Result<Option<Self>, String> {
        if stop.load(Ordering::Relaxed) || !scope.any() {
            return Ok(None);
        }
        let directory = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(out)
            .join("gesture-backend/artifacts/session");
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let (backend, warm) = match prewarmed.filter(Backend::running) {
            Some(backend) => (backend, true),
            None => (Backend::spawn(false)?, false),
        };
        let mut guard = Self {
            backend,
            directory,
            monitor_stop: Arc::new(AtomicBool::new(false)),
            monitor: None,
            shared,
            log,
        };
        guard.log.event(format!(
            "BACKGROUND_GUARD preparing native=true prewarmed={warm} touchpad={} touchscreen={}",
            scope.touchpad, scope.touchscreen
        ));
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        guard.backend.start(&Request {
            directory: guard.directory.clone(),
            scope,
        })?;
        let start = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                guard.release()?;
                return Ok(None);
            }
            if guard.directory.join("finished.json").exists() || !guard.backend.running() {
                let reason = finish_error(&guard.directory)
                    .unwrap_or_else(|| "Gesture worker ended before capture started".into());
                let _ = guard.release();
                return Err(reason);
            }
            if guard.directory.join("active.json").exists() && !restored(&guard.directory) {
                let label = match scope {
                    Scope {
                        touchpad: true,
                        touchscreen: true,
                    } => "touchpad + touchscreen",
                    Scope {
                        touchpad: true,
                        touchscreen: false,
                    } => "touchpad",
                    _ => "touchscreen",
                };
                guard.shared.lock().unwrap().guard_status =
                    format!("Background guard ACTIVE: {label}");
                guard.log.event(format!(
                    "BACKGROUND_GUARD active=true activation_ms={}",
                    start.elapsed().as_millis()
                ));
                guard.start_monitor(stop);
                return Ok(Some(guard));
            }
            if start.elapsed() > Duration::from_secs(12) {
                guard.release()?;
                return Err(
                    "Native gesture worker startup timed out; capture did not start.".into(),
                );
            }
            thread::sleep(Duration::from_millis(8));
        }
    }

    fn start_monitor(&mut self, stop: Arc<AtomicBool>) {
        let directory = self.directory.clone();
        let shared = self.shared.clone();
        let log = self.log.clone();
        let backend = self.backend.clone();
        let shutdown = self.monitor_stop.clone();
        self.monitor = Some(thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                let stale = fs::metadata(directory.join("heartbeat.json"))
                    .and_then(|m| m.modified())
                    .ok()
                    .is_none_or(|t| t.elapsed().is_ok_and(|age| age > Duration::from_secs(20)));
                let failed = directory.join("failed.json");
                let reason = if failed.exists() {
                    Some("Background gesture settings changed or verification failed")
                } else if stale {
                    Some("Background gesture worker stopped responding")
                } else if directory.join("restored.json").exists() || !backend.running() {
                    Some("Background gesture worker ended unexpectedly")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    let mut s = shared.lock().unwrap();
                    s.stop_reason = reason.into();
                    s.guard_status = "Stopping capture before restoring Windows gestures...".into();
                    drop(s);
                    log.event(format!(
                        "BACKGROUND_GUARD safety_stop=true reason={reason} {}",
                        fs::read_to_string(failed).unwrap_or_default()
                    ));
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }));
    }

    fn stop_monitor(&mut self) {
        self.monitor_stop.store(true, Ordering::Relaxed);
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
    }

    pub fn release(mut self) -> Result<(), String> {
        self.stop_monitor();
        self.shared.lock().unwrap().guard_status = "Restoring Windows gesture settings...".into();
        signal_stop(&self.directory)?;
        let start = Instant::now();
        loop {
            if self.directory.join("finished.json").exists() || !self.backend.running() {
                let changed = self.directory.join("mutation-started.json").exists();
                if !changed || restored(&self.directory) {
                    self.shared.lock().unwrap().guard_status = "Windows gestures restored".into();
                    self.log.event("BACKGROUND_GUARD restored=true");
                    return finish_error(&self.directory).map_or(Ok(()), Err);
                }
            }
            if start.elapsed() > Duration::from_secs(15) {
                self.shared.lock().unwrap().recovery_pending = true;
                self.log.event("BACKGROUND_GUARD recovery_pending=true");
                return Err(format!(
                    "Gesture restoration is not verified. Recovery journal: {}",
                    self.directory.join("original.json").display()
                ));
            }
            thread::sleep(Duration::from_millis(16));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_restoration_cannot_allow_another_session() {
        let dir = std::env::temp_dir().join(format!("pad2touch-guard-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for value in [
            "{}",
            "{\"restored\":false,\"registry_exact\":true,\"cache_exact\":true}",
            "{\"restored\":true,\"registry_exact\":true}",
        ] {
            fs::write(dir.join("restored.json"), value).unwrap();
            assert!(!restored(&dir));
        }
        fs::write(
            dir.join("restored.json"),
            "{\"restored\":true,\"registry_exact\":true,\"cache_exact\":true}",
        )
        .unwrap();
        assert!(restored(&dir));
        fs::write(dir.join("touchscreen-required.json"), "{}").unwrap();
        assert!(!restored(&dir));
        fs::write(
            dir.join("restored.json"),
            "{\"restored\":true,\"registry_exact\":true,\"cache_exact\":true,\"touchscreen_exact\":true}",
        ).unwrap();
        assert!(restored(&dir));
        fs::remove_file(dir.join("touchscreen-required.json")).unwrap();
        fs::remove_file(dir.join("restored.json")).unwrap();
        fs::remove_dir(dir).unwrap();
    }
    #[test]
    #[ignore = "temporarily changes four Windows gesture preferences and verifies exact restoration; run alone on the validated desktop"]
    fn native_background_guard_applies_and_restores_without_injection() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let out = PathBuf::from(format!("artifacts/gui-guard-test-{stamp}"));
        let (log, writer) = Log::open(&out).unwrap();
        let shared = Arc::new(std::sync::Mutex::new(crate::experiment::Stats::default()));
        let guard = Guard::acquire(
            &out,
            shared.clone(),
            log.clone(),
            Arc::new(AtomicBool::new(false)),
            Scope {
                touchpad: true,
                touchscreen: true,
            },
            None,
        )
        .unwrap()
        .unwrap();
        assert!(shared.lock().unwrap().guard_status.contains("ACTIVE"));
        assert!(shared
            .lock()
            .unwrap()
            .guard_status
            .contains("touchpad + touchscreen"));
        // Include a periodic live-state verification before normal restoration.
        thread::sleep(Duration::from_millis(1500));
        guard.release().unwrap();
        let directory = out.join("gesture-backend/artifacts/session");
        assert!(directory.join("touchscreen-required.json").exists());
        assert!(restored(&directory));
        assert!(!shared.lock().unwrap().recovery_pending);
        drop(log);
        writer.join().unwrap().unwrap();
    }

    #[test]
    #[ignore = "changes gesture preferences temporarily, tests both independent scopes and runs beyond the old timeout"]
    fn native_background_guard_unlimited_and_independent_scopes() {
        let prewarmed = Backend::spawn(true).unwrap();
        for (touchpad, touchscreen) in [(true, false), (false, true), (true, true)] {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let out = PathBuf::from(format!("artifacts/gui-guard-unlimited-{stamp}"));
            let (log, writer) = Log::open(&out).unwrap();
            let shared = Arc::new(Mutex::new(crate::experiment::Stats::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let guard = Guard::acquire(
                &out,
                shared.clone(),
                log.clone(),
                stop.clone(),
                Scope {
                    touchpad,
                    touchscreen,
                },
                Some(prewarmed.clone()),
            )
            .unwrap()
            .unwrap();
            assert!(Arc::ptr_eq(&guard.backend.0, &prewarmed.0));
            let directory = guard.directory.clone();
            let journal: serde_json::Value =
                serde_json::from_slice(&fs::read(directory.join("original.json")).unwrap())
                    .unwrap();
            assert_eq!(
                journal["preferences"].as_array().unwrap().len(),
                if touchpad { 4 } else { 0 }
            );
            assert_eq!(!journal["touchscreen"].is_null(), touchscreen);
            assert_eq!(
                directory.join("touchscreen-mutation-started.json").exists(),
                touchscreen
            );
            thread::sleep(if touchpad && touchscreen {
                Duration::from_secs(125)
            } else {
                Duration::from_millis(1500)
            });
            assert!(
                !stop.load(Ordering::Relaxed),
                "{}",
                shared.lock().unwrap().stop_reason
            );
            assert!(!directory.join("restored.json").exists());
            guard.release().unwrap();
            assert!(restored(&directory));
            assert!(!shared.lock().unwrap().recovery_pending);
            drop(log);
            writer.join().unwrap().unwrap();
        }
        assert!(prewarmed.running());
    }

    #[test]
    #[ignore = "temporarily changes gesture preferences and kills only its own backend to verify independent watchdog restoration"]
    fn native_background_guard_watchdog_restores_touchscreen() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let out = PathBuf::from(format!("artifacts/gui-guard-watchdog-test-{stamp}"));
        let (log, writer) = Log::open(&out).unwrap();
        let shared = Arc::new(std::sync::Mutex::new(crate::experiment::Stats::default()));
        let guard = Guard::acquire(
            &out,
            shared.clone(),
            log.clone(),
            Arc::new(AtomicBool::new(false)),
            Scope {
                touchpad: true,
                touchscreen: true,
            },
            Some(Backend::spawn(true).unwrap()),
        )
        .unwrap()
        .unwrap();
        // Terminate only the transaction process we just created. The separate
        // watchdog must restore both settings paths without that process.
        guard.backend.kill_for_test();
        guard.release().unwrap();
        let directory = out.join("gesture-backend/artifacts/session");
        assert!(directory.join("touchscreen-mutation-started.json").exists());
        assert!(restored(&directory));
        assert!(!shared.lock().unwrap().recovery_pending);
        drop(log);
        writer.join().unwrap().unwrap();
    }
}
