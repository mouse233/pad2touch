//! GUI adapter around the validated bounded PowerShell transaction and watchdog.
//! Assets and journals remain on disk so recovery survives the GUI process.
use crate::{experiment::Shared, logging::Log};
use std::{
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::FILETIME,
    System::Threading::{GetCurrentProcess, GetProcessTimes},
};

pub struct Guard {
    child: Child,
    directory: PathBuf,
    monitor_stop: Arc<AtomicBool>,
    monitor: Option<JoinHandle<()>>,
    shared: Shared,
    log: Log,
}
fn signal_stop(directory: &Path) -> Result<(), String> {
    fs::write(directory.join("stop-requested.json"), "{\"stop\":true}").map_err(|e| e.to_string())
}
fn restored(directory: &Path) -> bool {
    let Ok(text) = fs::read_to_string(directory.join("restored.json")) else {
        return false;
    };
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains("\"restored\":true")
        && (!directory.join("touchscreen-required.json").exists()
            || compact.contains("\"touchscreen_exact\":true")
            || (compact.contains("\"no_mutation\":true")
                && !directory.join("mutation-started.json").exists()))
        && ((compact.contains("\"no_mutation\":true")
            && !directory.join("mutation-started.json").exists())
            || (compact.contains("\"registry_exact\":true")
                && compact.contains("\"cache_exact\":true")))
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.monitor_stop.store(true, Ordering::Relaxed);
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        if self.directory.exists() {
            let _ = signal_stop(&self.directory);
        }
        if self.child.try_wait().ok().flatten().is_none() {
            self.shared.lock().unwrap().recovery_pending = true;
        }
    }
}
fn owner_start_filetime() -> Result<u64, String> {
    let mut created = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(crate::hid::winerr("GetProcessTimes(GUI owner)"));
    }
    Ok((created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64)
}
impl Guard {
    pub fn acquire(
        out: &Path,
        shared: Shared,
        log: Log,
        stop: Arc<AtomicBool>,
        suppress_touchscreen: bool,
    ) -> Result<Option<Self>, String> {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let root = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(out)
            .join("gesture-backend");
        let scripts = root.join("experiments");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&scripts)
            .and_then(|_| fs::create_dir_all(&artifacts))
            .map_err(|e| e.to_string())?;
        let script = scripts.join("Test-BackgroundGestureSettings.ps1");
        fs::write(
            &script,
            include_str!("../experiments/Test-BackgroundGestureSettings.ps1"),
        )
        .map_err(|e| e.to_string())?;
        fs::write(
            artifacts.join("gesture-cache-probe.exe"),
            include_bytes!(concat!(env!("OUT_DIR"), "/gesture-cache-probe.exe")),
        )
        .map_err(|e| e.to_string())?;
        let directory = artifacts.join("session");
        let ps7 = PathBuf::from(
            std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()),
        )
        .join("PowerShell/7/pwsh.exe");
        let shell = if ps7.exists() {
            ps7
        } else {
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
                .join("System32/WindowsPowerShell/v1.0/powershell.exe")
        };
        let stdout = fs::File::create(root.join("stdout.txt")).map_err(|e| e.to_string())?;
        let stderr = fs::File::create(root.join("stderr.txt")).map_err(|e| e.to_string())?;
        let mut command = Command::new(shell);
        command
            .args(["-NoProfile", "-NonInteractive", "-File"])
            .arg(&script)
            .args([
                "-Mode",
                "session",
                "-ExpectedBuild",
                "26300",
                "-Seconds",
                "120",
                "-RunDirectory",
            ])
            .arg(&directory)
            .arg("-OwnerPid")
            .arg(std::process::id().to_string())
            .arg("-OwnerStartedFileTime")
            .arg(owner_start_filetime()?.to_string())
            .current_dir(&root)
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        if suppress_touchscreen {
            command.arg("-SuppressTouchscreen");
        }
        let child = command
            .spawn()
            .map_err(|e| format!("Could not start background gesture guard: {e}"))?;
        shared.lock().unwrap().guard_status =
            "Preparing background guard and recovery watchdog...".into();
        log.event("BACKGROUND_GUARD preparing bounded_seconds=120 scope=current-user");
        let mut guard = Self {
            child,
            directory,
            monitor_stop: Arc::new(AtomicBool::new(false)),
            monitor: None,
            shared,
            log,
        };
        let start = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                // A directory created before the script starts makes it refuse mutation.
                fs::create_dir_all(&guard.directory).map_err(|e| e.to_string())?;
                guard.release()?;
                return Ok(None);
            }
            if guard.directory.join("active.json").exists()
                && !guard.directory.join("restored.json").exists()
            {
                guard.shared.lock().unwrap().guard_status = if suppress_touchscreen {
                    "Background guard ACTIVE: touchpad + touchscreen (120 s maximum)".into()
                } else {
                    "Background guard ACTIVE (120 s maximum)".into()
                };
                guard.log.event(format!(
                    "BACKGROUND_GUARD active=true touchscreen={suppress_touchscreen}"
                ));
                let directory = guard.directory.clone();
                let shared = guard.shared.clone();
                let log = guard.log.clone();
                let shutdown = guard.monitor_stop.clone();
                guard.monitor = Some(thread::spawn(move || {
                    let started = Instant::now();
                    while !shutdown.load(Ordering::Relaxed) {
                        let failed = directory.join("failed.json").exists();
                        if failed
                            || directory.join("expired.json").exists()
                            || directory.join("restored.json").exists()
                            || started.elapsed() >= Duration::from_secs(120)
                        {
                            let mut s = shared.lock().unwrap();
                            s.stop_reason = if failed {
                                "Background gesture guard verification failed; see guard logs"
                                    .into()
                            } else {
                                "Background guard ended or reached its 120 s limit".into()
                            };
                            s.guard_status =
                                "Stopping capture before restoring Windows gestures...".into();
                            drop(s);
                            log.event("BACKGROUND_GUARD safety_stop=true");
                            if failed {
                                log.event(format!(
                                    "BACKGROUND_GUARD verification_failed {}",
                                    fs::read_to_string(directory.join("failed.json"))
                                        .unwrap_or_default()
                                ));
                            }
                            stop.store(true, Ordering::Relaxed);
                            break;
                        }
                        thread::sleep(Duration::from_millis(40));
                    }
                }));
                return Ok(Some(guard));
            }
            if let Some(status) = guard.child.try_wait().map_err(|e| e.to_string())? {
                let error = fs::read_to_string(root.join("stderr.txt")).unwrap_or_default();
                let pending = guard.directory.join("mutation-started.json").exists()
                    && !restored(&guard.directory);
                guard.shared.lock().unwrap().recovery_pending = pending;
                return Err(format!(
                    "Background guard did not activate ({status}). {}",
                    error.chars().take(1200).collect::<String>()
                ));
            }
            if start.elapsed() > Duration::from_secs(20) {
                fs::create_dir_all(&guard.directory).map_err(|e| e.to_string())?;
                guard.release()?;
                return Err(
                    "Background gesture guard startup timed out; capture did not start.".into(),
                );
            }
            thread::sleep(Duration::from_millis(40));
        }
    }
    pub fn release(mut self) -> Result<(), String> {
        self.monitor_stop.store(true, Ordering::Relaxed);
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        self.shared.lock().unwrap().guard_status = "Restoring Windows gesture settings...".into();
        signal_stop(&self.directory)?;
        let start = Instant::now();
        loop {
            if self.child.try_wait().map_err(|e| e.to_string())?.is_some() {
                let changed = self.directory.join("mutation-started.json").exists();
                if !changed || restored(&self.directory) {
                    self.shared.lock().unwrap().guard_status = "Windows gestures restored".into();
                    self.log.event("BACKGROUND_GUARD restored=true");
                    return Ok(());
                }
                // The independent watchdog may still be completing restoration.
            }
            if start.elapsed() > Duration::from_secs(15) {
                self.shared.lock().unwrap().recovery_pending = true;
                self.log.event("BACKGROUND_GUARD recovery_pending=true");
                return Err(format!(
                    "Gesture restoration is not verified. Recovery journal: {}",
                    self.directory.join("original.json").display()
                ));
            }
            thread::sleep(Duration::from_millis(40));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_restoration_cannot_allow_another_session() {
        let dir = std::env::temp_dir().join(format!("padbridge-guard-{}", std::process::id()));
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
            true,
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
    #[ignore = "temporarily changes gesture preferences and kills only its own backend to verify independent watchdog restoration"]
    fn native_background_guard_watchdog_restores_touchscreen() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let out = PathBuf::from(format!("artifacts/gui-guard-watchdog-test-{stamp}"));
        let (log, writer) = Log::open(&out).unwrap();
        let shared = Arc::new(std::sync::Mutex::new(crate::experiment::Stats::default()));
        let mut guard = Guard::acquire(
            &out,
            shared.clone(),
            log.clone(),
            Arc::new(AtomicBool::new(false)),
            true,
        )
        .unwrap()
        .unwrap();
        // Terminate only the transaction process we just created. The separate
        // watchdog must restore both settings paths without that process.
        guard.child.kill().unwrap();
        guard.child.wait().unwrap();
        guard.release().unwrap();
        let directory = out.join("gesture-backend/artifacts/session");
        assert!(directory.join("touchscreen-mutation-started.json").exists());
        assert!(restored(&directory));
        assert!(!shared.lock().unwrap().recovery_pending);
        drop(log);
        writer.join().unwrap().unwrap();
    }
}
