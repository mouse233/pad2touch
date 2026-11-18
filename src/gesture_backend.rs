//! Native per-user gesture transaction and independent crash recovery.
use crate::gesture_cache::{screen_state, SessionCache};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, FileTimes},
    io::{self, BufRead, Write},
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr::{null, null_mut},
    thread,
    time::{Duration, Instant, SystemTime},
};
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::{
            CloseHandle, ERROR_FILE_NOT_FOUND, FILETIME, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
        System::{
            Com::CoCreateGuid,
            Registry::*,
            Threading::{
                CreateMutexW, GetCurrentProcess, GetProcessTimes, OpenProcess, ReleaseMutex,
                WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE,
            },
        },
    },
};

type Result<T> = std::result::Result<T, String>;
const PAD_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad";
const SCREEN_KEY: &str = r"Control Panel\Desktop";
const SCREEN_PREF: &str = "TouchGestureSetting";
const PAD_NAMES: [&str; 4] = [
    "ThreeFingerTapEnabled",
    "FourFingerTapEnabled",
    "ThreeFingerSlideEnabled",
    "FourFingerSlideEnabled",
];

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain([0]).collect()
}

fn io_error(error: io::Error) -> String {
    error.to_string()
}

fn status(operation: &str, error: u32) -> Result<()> {
    if error == 0 {
        Ok(())
    } else {
        Err(format!(
            "{operation}: {}",
            io::Error::from_raw_os_error(error as i32)
        ))
    }
}

struct Key(HKEY);
impl Key {
    fn open(root: HKEY, path: &str, write: bool) -> Result<Self> {
        let mut key = null_mut();
        let access = KEY_QUERY_VALUE | if write { KEY_SET_VALUE } else { 0 };
        status("Open registry key", unsafe {
            RegOpenKeyExW(root, wide(path).as_ptr(), 0, access, &mut key)
        })?;
        Ok(Self(key))
    }

    fn dword(&self, name: &str) -> Result<Option<u32>> {
        let mut value = 0u32;
        let mut bytes = 4;
        let mut kind = 0;
        let code = unsafe {
            RegQueryValueExW(
                self.0,
                wide(name).as_ptr(),
                null(),
                &mut kind,
                (&mut value as *mut u32).cast(),
                &mut bytes,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status("Read registry value", code)?;
        if bytes != 4 || kind != REG_DWORD {
            return Err(format!("Unexpected value type: {name}"));
        }
        Ok(Some(value))
    }

    fn string(&self, name: &str) -> Result<String> {
        let mut value = [0u16; 32];
        let mut bytes = std::mem::size_of_val(&value) as u32;
        status("Read Windows build", unsafe {
            RegGetValueW(
                self.0,
                null(),
                wide(name).as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                value.as_mut_ptr().cast(),
                &mut bytes,
            )
        })?;
        String::from_utf16(&value[..value.iter().position(|&c| c == 0).unwrap_or(value.len())])
            .map_err(|e| e.to_string())
    }

    fn set(&self, name: &str, value: Option<u32>) -> Result<()> {
        let name = wide(name);
        let code = match value {
            Some(v) => unsafe {
                RegSetValueExW(
                    self.0,
                    name.as_ptr(),
                    0,
                    REG_DWORD,
                    v.to_le_bytes().as_ptr(),
                    4,
                )
            },
            None => unsafe { RegDeleteValueW(self.0, name.as_ptr()) },
        };
        status("Write registry value", code)
    }

    fn flush(&self) -> Result<()> {
        status("Flush registry changes", unsafe { RegFlushKey(self.0) })
    }
}
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

struct Lock(HANDLE);
impl Lock {
    fn acquire(name: &str, timeout_ms: u32) -> Result<Self> {
        let mutex = unsafe { CreateMutexW(null(), 0, wide(name).as_ptr()) };
        if mutex.is_null() {
            return Err(io::Error::last_os_error().to_string());
        }
        if matches!(
            unsafe { WaitForSingleObject(mutex, timeout_ms) },
            WAIT_OBJECT_0 | WAIT_ABANDONED
        ) {
            Ok(Self(mutex))
        } else {
            unsafe {
                CloseHandle(mutex);
            }
            Err(format!("Could not acquire {name}"))
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

fn guid() -> Result<String> {
    let mut id = GUID::default();
    if unsafe { CoCreateGuid(&mut id) } < 0 {
        return Err("CoCreateGuid failed".into());
    }
    let mut text = format!("{:08x}{:04x}{:04x}", id.data1, id.data2, id.data3);
    for byte in id.data4 {
        text.push_str(&format!("{byte:02x}"));
    }
    Ok(text)
}

fn durable(path: &Path, data: &str) -> Result<()> {
    let mut file = File::create_new(path).map_err(io_error)?;
    file.write_all(data.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(io_error)
}

fn publish(path: &Path, data: &str) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", guid()?));
    durable(&temporary, data)?;
    if unsafe {
        MoveFileExW(
            wide(&temporary).as_ptr(),
            wide(path).as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Scope {
    pub touchpad: bool,
    pub touchscreen: bool,
}
impl Scope {
    pub fn any(self) -> bool {
        self.touchpad || self.touchscreen
    }
}

#[derive(Deserialize, Serialize)]
pub struct Request {
    pub directory: PathBuf,
    pub scope: Scope,
}

#[derive(Deserialize, Serialize)]
struct Preference {
    name: String,
    exists: bool,
    kind: String,
    bytes: String,
    #[serde(rename = "cacheValue", skip_serializing_if = "Option::is_none")]
    cached: Option<u32>,
}
impl Preference {
    fn new(name: &str, value: Option<u32>, cached: Option<u32>) -> Self {
        Self {
            name: name.into(),
            exists: value.is_some(),
            kind: if value.is_some() { "DWord" } else { "Absent" }.into(),
            bytes: value.map_or_else(String::new, |v| STANDARD.encode(v.to_le_bytes())),
            cached,
        }
    }

    fn value(&self) -> Result<Option<u32>> {
        match (self.exists, self.kind.as_str()) {
            (false, "Absent") if self.bytes.is_empty() => Ok(None),
            (true, "DWord") => STANDARD
                .decode(&self.bytes)
                .ok()
                .and_then(|v| <[u8; 4]>::try_from(v).ok())
                .map(|v| Some(u32::from_le_bytes(v)))
                .ok_or_else(|| format!("Invalid snapshot for {}", self.name)),
            _ => Err(format!("Invalid snapshot for {}", self.name)),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ScreenJournal {
    runtime: u32,
    preference: Preference,
}

#[derive(Deserialize, Serialize)]
struct Journal {
    format: u32,
    id: String,
    subkey: String,
    preferences: Vec<Preference>,
    touchscreen: Option<ScreenJournal>,
}

#[derive(Deserialize, Serialize)]
struct Lease {
    journal: PathBuf,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Completion {
    restored: bool,
    no_mutation: bool,
    registry_exact: bool,
    cache_exact: bool,
    touchscreen_exact: bool,
}
impl Completion {
    fn new(changed: bool) -> Self {
        Self {
            restored: true,
            no_mutation: !changed,
            registry_exact: changed,
            cache_exact: changed,
            touchscreen_exact: changed,
        }
    }

    fn trusted(&self, dir: &Path) -> bool {
        self.restored
            && ((self.no_mutation && !dir.join("mutation-started.json").exists())
                || (self.registry_exact
                    && self.cache_exact
                    && (self.touchscreen_exact || !dir.join("touchscreen-required.json").exists())))
    }
}

pub fn restored(directory: &Path) -> bool {
    read_json::<Completion>(&directory.join("restored.json"))
        .is_ok_and(|record| record.trusted(directory))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&data).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let data = serde_json::to_string(value).map_err(|e| e.to_string())?;
    publish(path, &data)
}

fn review_lease(lease: &Path) -> Result<()> {
    match fs::read(lease) {
        Ok(data) => {
            let previous: Lease = serde_json::from_slice(&data)
                .map_err(|e| format!("Previous gesture lease is invalid: {e}"))?;
            let dir = previous
                .journal
                .parent()
                .ok_or("Previous gesture journal has no directory")?;
            if !restored(dir) {
                return Err(format!(
                    "Previous gesture restoration is unverified: {}",
                    previous.journal.display()
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

#[derive(Clone, Copy)]
struct PadSnapshot {
    values: [Option<u32>; 4],
    cached: [u32; 4],
}
#[derive(Clone, Copy)]
struct ScreenSnapshot {
    preference: Option<u32>,
    runtime: u32,
}
#[derive(Clone, Copy)]
struct Snapshot {
    pad: Option<PadSnapshot>,
    screen: Option<ScreenSnapshot>,
}
struct Access<'a> {
    pad: Option<(Key, &'a SessionCache)>,
    screen: Option<Key>,
}
impl<'a> Access<'a> {
    fn open(scope: Scope, cache: Option<&'a SessionCache>) -> Result<Self> {
        Ok(Self {
            pad: scope
                .touchpad
                .then(|| {
                    Ok::<_, String>((
                        Key::open(HKEY_CURRENT_USER, PAD_KEY, true)?,
                        cache.ok_or("Missing touchpad cache connection")?,
                    ))
                })
                .transpose()?,
            screen: scope
                .touchscreen
                .then(|| Key::open(HKEY_CURRENT_USER, SCREEN_KEY, false))
                .transpose()?,
        })
    }

    fn snapshot(&self) -> Result<Snapshot> {
        Ok(Snapshot {
            pad: self
                .pad
                .as_ref()
                .map(|(key, cache)| -> Result<PadSnapshot> {
                    let values = pad_values(key)?;
                    let cached = cache.values(false)?;
                    if values
                        .into_iter()
                        .zip(cached)
                        .any(|(v, c)| v.is_some_and(|v| v != c))
                    {
                        return Err("Registry/cache prerequisite mismatch".into());
                    }
                    Ok(PadSnapshot { values, cached })
                })
                .transpose()?,
            screen: self
                .screen
                .as_ref()
                .map(|key| {
                    Ok::<_, String>(ScreenSnapshot {
                        preference: key.dword(SCREEN_PREF)?,
                        runtime: screen_state(None)?,
                    })
                })
                .transpose()?,
        })
    }

    fn verify_disabled(&self, snapshot: Snapshot, refresh: bool) -> Result<()> {
        if let (Some(key), Some(screen)) = (&self.screen, snapshot.screen) {
            if key.dword(SCREEN_PREF)? != screen.preference || screen_state(None)? != 0 {
                return Err("Touchscreen settings changed during capture".into());
            }
        }
        if let Some((key, cache)) = &self.pad {
            if pad_values(key)? != [Some(0); 4] {
                return Err("Touchpad preferences changed during capture".into());
            }
            if cache.values(refresh)? != [0; 4] && (refresh || cache.values(true)? != [0; 4]) {
                return Err("Touchpad cache is not disabled".into());
            }
        }
        Ok(())
    }
}

fn pad_values(key: &Key) -> Result<[Option<u32>; 4]> {
    let mut values = [None; 4];
    for (index, name) in PAD_NAMES.into_iter().enumerate() {
        values[index] = key.dword(name)?;
    }
    Ok(values)
}

impl Snapshot {
    fn scope(self) -> Scope {
        Scope {
            touchpad: self.pad.is_some(),
            touchscreen: self.screen.is_some(),
        }
    }

    fn journal(self, id: String) -> Journal {
        let preferences = self.pad.map_or_else(Vec::new, |pad| {
            PAD_NAMES
                .into_iter()
                .enumerate()
                .map(|(i, name)| Preference::new(name, pad.values[i], Some(pad.cached[i])))
                .collect()
        });
        let touchscreen = self.screen.map(|screen| ScreenJournal {
            runtime: screen.runtime,
            preference: Preference::new(SCREEN_PREF, screen.preference, None),
        });
        Journal {
            format: 1,
            id,
            subkey: PAD_KEY.into(),
            preferences,
            touchscreen,
        }
    }

    fn from_journal(journal: &Journal) -> Result<Self> {
        if journal.format != 1
            || journal.subkey != PAD_KEY
            || journal.id.len() != 32
            || !journal.id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid gesture recovery journal".into());
        }
        let pad = match journal.preferences.len() {
            0 => None,
            4 => {
                let mut values = [None; 4];
                let mut cached = [0; 4];
                for (index, name) in PAD_NAMES.into_iter().enumerate() {
                    let p = journal
                        .preferences
                        .iter()
                        .find(|p| p.name == name)
                        .ok_or("Missing touchpad preference")?;
                    values[index] = p.value()?;
                    cached[index] = p.cached.ok_or("Missing touchpad cache value")?;
                }
                Some(PadSnapshot { values, cached })
            }
            _ => return Err("Unexpected count of touchpad preferences".into()),
        };
        let screen = journal
            .touchscreen
            .as_ref()
            .map(|entry| -> Result<ScreenSnapshot> {
                if entry.runtime > 1 || entry.preference.name != SCREEN_PREF {
                    return Err("Invalid touchscreen recovery snapshot".into());
                }
                Ok(ScreenSnapshot {
                    preference: entry.preference.value()?,
                    runtime: entry.runtime,
                })
            })
            .transpose()?;
        let snapshot = Self { pad, screen };
        if !snapshot.scope().any() {
            return Err("Empty gesture recovery snapshot".into());
        }
        Ok(snapshot)
    }

    fn restore(self, access: &Access, screen_started: bool) -> Result<()> {
        // Always attempt both paths. Preserve settings that changed externally.
        let mut errors = Vec::new();
        if let (Some((key, cache)), Some(snapshot)) = (&access.pad, self.pad) {
            for (i, name) in PAD_NAMES.into_iter().enumerate() {
                let result = key.dword(name).and_then(|live| {
                    if live != snapshot.values[i] && live == Some(0) {
                        key.set(name, snapshot.values[i])?;
                    }
                    Ok(())
                });
                if let Err(e) = result {
                    errors.push(e);
                }
            }
            let result = key.flush().and_then(|()| {
                let actual = pad_values(key)?;
                let actual_cache = cache.values(true)?;
                if actual != snapshot.values || actual_cache != snapshot.cached {
                    return Err("Touchpad restoration could not be verified".into());
                }
                Ok(())
            });
            if let Err(e) = result {
                errors.push(e);
            }
        }
        if let (Some(key), Some(screen)) = (&access.screen, self.screen) {
            let result = (|| {
                if key.dword(SCREEN_PREF)? != screen.preference {
                    return Err("Touchscreen preference changed externally".into());
                }
                if screen_started && screen_state(None)? != screen.runtime {
                    screen_state(Some(screen.runtime))?;
                }
                if key.dword(SCREEN_PREF)? != screen.preference
                    || screen_state(None)? != screen.runtime
                {
                    return Err("Touchscreen restoration could not be verified".into());
                }
                Ok(())
            })();
            if let Err(e) = result {
                errors.push(e);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

struct Process(HANDLE);
impl Process {
    fn open(pid: &OsStr, created: &OsStr) -> Result<Self> {
        let pid = pid
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid process ID")?;
        let created: u64 = created
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid process creation time")?;
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return Err(io_error(io::Error::last_os_error()));
        }
        let process = Self(handle);
        if process_started(handle)? != created {
            return Err("Process identity changed".into());
        }
        Ok(process)
    }

    fn running(&self) -> bool {
        (unsafe { WaitForSingleObject(self.0, 0) }) == WAIT_TIMEOUT
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn process_started(handle: HANDLE) -> Result<u64> {
    let (mut created, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    if unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    Ok(u64::from(created.dwLowDateTime) | (u64::from(created.dwHighDateTime) << 32))
}

pub fn self_started() -> Result<u64> {
    process_started(unsafe { GetCurrentProcess() })
}

pub fn command(mode: &str) -> Result<Command> {
    let mut cmd = Command::new(std::env::current_exe().map_err(io_error)?);
    #[cfg(test)]
    cmd.args([
        "--exact",
        "gesture_backend::tests::child_process",
        "--ignored",
        "--nocapture",
    ])
    .env("PAD2TOUCH_TEST_GESTURE_CHILD", mode);
    #[cfg(not(test))]
    cmd.arg(mode);
    cmd.creation_flags(CREATE_NO_WINDOW);
    Ok(cmd)
}

// Access only per-session directories inside this instance's artifacts root.
fn validate_directory(directory: &Path) -> Result<()> {
    let allowed = std::env::current_dir().map_err(io_error)?.join("artifacts");
    let allowed = fs::canonicalize(allowed).map_err(io_error)?;
    let actual = fs::canonicalize(directory).map_err(io_error)?;
    if actual == allowed || !actual.starts_with(allowed) {
        return Err("Gesture journal must be under Pad2Touch's artifacts directory".into());
    }
    Ok(())
}

fn validate_build() -> Result<()> {
    let version = Key::open(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        false,
    )?;
    if version.string("CurrentBuildNumber")? != "26300" || version.dword("UBR")? != Some(9974) {
        return Err(
            "Background gesture suppression is supported on Windows build 26300.9974".into(),
        );
    }
    Ok(())
}

fn recover(directory: &Path) -> Result<()> {
    let journal: Journal = read_json(&directory.join("original.json"))?;
    let snapshot = Snapshot::from_journal(&journal)?;
    let done = directory.join("restored.json");
    let _lock = Lock::acquire(
        &format!(r"Local\PadBridgeGestureRestore-{}", journal.id),
        5000,
    )?;
    if restored(directory) {
        return Ok(());
    }
    let changed = directory.join("mutation-started.json").exists();
    if changed {
        validate_build()?;
        let pad = Snapshot {
            screen: None,
            ..snapshot
        };
        let screen = Snapshot {
            pad: None,
            ..snapshot
        };
        let pad_result = (|| {
            let cache = pad.scope().touchpad.then(SessionCache::open).transpose()?;
            pad.restore(&Access::open(pad.scope(), cache.as_ref())?, false)
        })();
        let screen_result = (|| {
            screen.restore(
                &Access::open(screen.scope(), None)?,
                directory.join("touchscreen-mutation-started.json").exists(),
            )
        })();
        pad_result.and(screen_result)?;
    }
    write_json(&done, &Completion::new(changed))
}

fn watchdog(args: &[OsString]) -> Result<()> {
    let [directory, backend_pid, backend_started, owner_pid, owner_started] = args else {
        return Err("Invalid gesture watchdog arguments".into());
    };
    let directory = Path::new(directory);
    validate_directory(directory)?;
    let backend = Process::open(backend_pid, backend_started)?;
    let owner = Process::open(owner_pid, owner_started)?;
    let journal: Journal = read_json(&directory.join("original.json"))?;
    Snapshot::from_journal(&journal)?;
    publish(&directory.join("watchdog-ready.json"), "{}")?;
    while !restored(directory) {
        let stale = fs::metadata(directory.join("heartbeat.json"))
            .and_then(|m| m.modified())
            .map_or(true, |time| {
                time.elapsed()
                    .is_ok_and(|age| age > Duration::from_secs(30))
            });
        if !backend.running() || !owner.running() || stale {
            // Recovery and mutation use the same lock, and the journal is durable.
            return recover(directory);
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

struct Worker {
    owner_pid: OsString,
    owner_started: OsString,
    owner: Process,
    cache: Option<SessionCache>,
}
impl Worker {
    fn start_watchdog(&self, directory: &Path) -> Result<Child> {
        let output = File::create(directory.join("watchdog.stdout.txt")).map_err(io_error)?;
        let errors = File::create(directory.join("watchdog.stderr.txt")).map_err(io_error)?;
        let mut cmd = command("--gesture-watchdog")?;
        #[cfg(test)]
        cmd.env(
            "PAD2TOUCH_TEST_GESTURE_ARGS",
            serde_json::to_string(&[
                directory
                    .to_str()
                    .ok_or("Unsupported session path")?
                    .to_owned(),
                std::process::id().to_string(),
                self_started()?.to_string(),
                self.owner_pid.to_string_lossy().into_owned(),
                self.owner_started.to_string_lossy().into_owned(),
            ])
            .map_err(|e| e.to_string())?,
        );
        #[cfg(not(test))]
        cmd.arg(directory)
            .arg(std::process::id().to_string())
            .arg(self_started()?.to_string())
            .arg(&self.owner_pid)
            .arg(&self.owner_started);
        cmd.stdin(Stdio::null())
            .stdout(output)
            .stderr(errors)
            .spawn()
            .map_err(io_error)
    }

    fn session(&mut self, request: &Request) -> Result<()> {
        let directory = &request.directory;
        let stop = directory.join("stop-requested.json");
        if stop.exists() || !self.owner.running() {
            return Ok(());
        }
        if !request.scope.any() {
            return Err("Select a gesture override".into());
        }
        validate_build()?;
        if request.scope.touchpad && self.cache.is_none() {
            self.cache = Some(SessionCache::open()?);
        }
        let lease = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or("Local AppData is unavailable")?
            .join("PadBridge/ExperimentalGestureCache/active.json");
        let _global = Lock::acquire(r"Local\PadBridgeGestureCacheExperiment", 0)?;
        review_lease(&lease)?;

        let access = Access::open(request.scope, self.cache.as_ref())?;
        let snapshot = access.snapshot()?;
        let id = guid()?;
        let journal = directory.join("original.json");
        durable(
            &journal,
            &serde_json::to_string(&snapshot.journal(id.clone())).map_err(|e| e.to_string())?,
        )?;
        let heartbeat = directory.join("heartbeat.json");
        durable(&heartbeat, "{}")?;
        if request.scope.touchscreen {
            durable(&directory.join("touchscreen-required.json"), "{}")?;
        }
        let mut watchdog = self.start_watchdog(directory)?;
        let done = directory.join("restored.json");
        let started = directory.join("mutation-started.json");
        let screen_started = directory.join("touchscreen-mutation-started.json");
        let restore_lock = format!(r"Local\PadBridgeGestureRestore-{id}");
        let result = (|| {
            let wait = Instant::now();
            while !directory.join("watchdog-ready.json").exists() {
                if stop.exists() || !self.owner.running() {
                    return Ok(());
                }
                if watchdog.try_wait().map_err(io_error)?.is_some()
                    || wait.elapsed() > Duration::from_secs(5)
                {
                    return Err("Recovery watchdog did not become ready".into());
                }
                thread::sleep(Duration::from_millis(4));
            }
            {
                let _mutation = Lock::acquire(&restore_lock, 3000)?;
                if done.exists() || stop.exists() || !self.owner.running() {
                    return Ok(());
                }
                if watchdog.try_wait().map_err(io_error)?.is_some() {
                    return Err("Recovery watchdog exited before settings changed".into());
                }
                if let (Some((key, _)), Some(pad)) = (&access.pad, snapshot.pad) {
                    if pad_values(key)? != pad.values {
                        return Err("Touchpad settings changed since snapshot".into());
                    }
                }
                if let (Some(key), Some(screen)) = (&access.screen, snapshot.screen) {
                    if key.dword(SCREEN_PREF)? != screen.preference
                        || screen_state(None)? != screen.runtime
                    {
                        return Err("Touchscreen settings changed since snapshot".into());
                    }
                }
                fs::create_dir_all(lease.parent().ok_or("Missing gesture lease directory")?)
                    .map_err(io_error)?;
                write_json(
                    &lease,
                    &Lease {
                        journal: journal.clone(),
                    },
                )?;
                durable(&started, "{}")?;
                if let Some((key, _)) = &access.pad {
                    for name in PAD_NAMES {
                        key.set(name, Some(0))?;
                    }
                    key.flush()?;
                }
                if access.screen.is_some() {
                    durable(&screen_started, "{}")?;
                    screen_state(Some(0))?;
                }
                access.verify_disabled(snapshot, true)?;
                if !stop.exists()
                    && self.owner.running()
                    && watchdog.try_wait().map_err(io_error)?.is_none()
                {
                    publish(&directory.join("active.json"), r#"{"active":true}"#)?;
                } else {
                    return Ok(());
                }
            }
            let beat = File::options()
                .write(true)
                .open(&heartbeat)
                .map_err(io_error)?;
            let mut health = Instant::now();
            while !stop.exists() && self.owner.running() && !done.exists() {
                beat.set_times(FileTimes::new().set_modified(SystemTime::now()))
                    .map_err(io_error)?;
                if health.elapsed() >= Duration::from_secs(1) {
                    if watchdog.try_wait().map_err(io_error)?.is_some() {
                        return Err("Recovery watchdog exited".into());
                    }
                    access.verify_disabled(snapshot, false)?;
                    health = Instant::now();
                }
                thread::sleep(Duration::from_millis(25));
            }
            Ok(())
        })();

        if let Err(error) = &result {
            if directory.join("active.json").exists() {
                let _ = write_json(
                    &directory.join("failed.json"),
                    &serde_json::json!({"reason":error}),
                );
                let drain = Instant::now();
                while !stop.exists()
                    && self.owner.running()
                    && drain.elapsed() < Duration::from_secs(3)
                {
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
        let restore = (|| {
            let _lock = Lock::acquire(&restore_lock, 5000)?;
            if restored(directory) {
                return Ok::<_, String>(());
            }
            let changed = started.exists();
            if changed {
                snapshot.restore(&access, screen_started.exists())?;
            }
            write_json(&done, &Completion::new(changed))
        })();
        // If local recovery fails, ending the worker lets the watchdog retry.
        if let Err(error) = restore {
            let _ = write_json(
                &directory.join("finished.json"),
                &serde_json::json!({"error":error}),
            );
            return Err(error);
        }
        result
    }
}

pub fn run(mode: &str, args: &[OsString]) -> Result<()> {
    if mode == "--gesture-watchdog" {
        return watchdog(args);
    }
    let [owner_pid, owner_started, warm_pad] = args else {
        return Err("Invalid gesture worker arguments".into());
    };
    let owner = Process::open(owner_pid, owner_started)?;
    let mut worker = Worker {
        owner_pid: owner_pid.clone(),
        owner_started: owner_started.clone(),
        owner,
        cache: None,
    };
    if warm_pad == "true" {
        // Prewarm may fail on an unsupported runtime. The capture request
        // performs its normal checks and can report the error to the GUI.
        worker.cache = validate_build().and_then(|()| SessionCache::open()).ok();
    }
    for line in io::stdin().lock().lines() {
        let request: Request =
            serde_json::from_str(&line.map_err(io_error)?).map_err(|e| e.to_string())?;
        validate_directory(&request.directory)?;
        let result = worker.session(&request);
        let finished = request.directory.join("finished.json");
        if finished.exists() {
            // A recovery error was published. Exit so the watchdog can retry it.
            return result;
        }
        write_json(&finished, &serde_json::json!({"error":result.err()}))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "internal child process entry point for native gesture integration tests"]
    fn child_process() {
        let mode = std::env::var("PAD2TOUCH_TEST_GESTURE_CHILD").unwrap();
        let args: Vec<String> =
            serde_json::from_str(&std::env::var("PAD2TOUCH_TEST_GESTURE_ARGS").unwrap()).unwrap();
        super::run(&mode, &args.into_iter().map(Into::into).collect::<Vec<_>>()).unwrap();
    }
}
