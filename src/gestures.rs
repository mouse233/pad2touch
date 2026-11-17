//! Optional foreground-process touchpad gesture ownership, using the published
//! UniversalApiContract v19 ABI. No registry or touchpad settings are modified.
//! SDK: windows.ui.input.h, ITouchpadGesturesController[Statics].
use crate::logging::Log;
use std::{
    ffi::c_void,
    marker::PhantomData,
    ptr::{null_mut, NonNull},
    rc::Rc,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
};
use windows_sys::core::GUID;

const CLASS: &str = "Windows.UI.Input.TouchpadGesturesController";
const IID_STATICS: GUID = GUID {
    data1: 0x207ef171,
    data2: 0x1a73,
    data3: 0x51cd,
    data4: [0xa6, 0x94, 0x88, 0x40, 0xe0, 0x9d, 0xba, 0xfa],
};
pub const THREE_FOUR_FINGER_MASK: i32 = 0x1 | 0x2 | 0x8 | 0x10;
type HString = *mut c_void;
#[link(name = "runtimeobject")]
extern "system" {
    fn RoInitialize(kind: u32) -> i32;
    fn RoUninitialize();
    fn WindowsCreateString(text: *const u16, len: u32, out: *mut HString) -> i32;
    fn WindowsDeleteString(value: HString) -> i32;
    fn RoGetActivationFactory(name: HString, iid: *const GUID, out: *mut *mut c_void) -> i32;
}
#[repr(C)]
struct Inspectable {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_iids: *const c_void,
    get_name: *const c_void,
    get_trust: *const c_void,
}
#[repr(C)]
struct Unknown {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}
#[repr(C)]
struct Statics {
    base: Inspectable,
    is_supported: unsafe extern "system" fn(*mut c_void, *mut u8) -> i32,
    create: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}
type AddHandler = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> i32;
type RemoveHandler = unsafe extern "system" fn(*mut c_void, i64) -> i32;
#[repr(C)]
struct Controller {
    base: Inspectable,
    get_enabled: unsafe extern "system" fn(*mut c_void, *mut u8) -> i32,
    set_enabled: unsafe extern "system" fn(*mut c_void, u8) -> i32,
    get_gestures: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    set_gestures: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    add_pressed: AddHandler,
    remove_pressed: RemoveHandler,
    add_moved: AddHandler,
    remove_moved: RemoveHandler,
    add_released: AddHandler,
    remove_released: RemoveHandler,
    add_action: AddHandler,
    remove_action: RemoveHandler,
}
const IID_POINTER_HANDLER: GUID = GUID {
    data1: 0x0b05a28c,
    data2: 0x2064,
    data3: 0x5b06,
    data4: [0x94, 0xf3, 0x32, 0x4f, 0xb0, 0x04, 0xec, 0xe0],
};
const IID_ACTION_HANDLER: GUID = GUID {
    data1: 0x3fdb14d0,
    data2: 0xc3b2,
    data3: 0x576e,
    data4: [0xb9, 0x00, 0xaa, 0xa1, 0x4e, 0x0f, 0xf7, 0x7c],
};
const IID_CORE_ARGS: GUID = GUID {
    data1: 0x272b1ef3,
    data2: 0xc633,
    data3: 0x4da5,
    data4: [0xa2, 0x6c, 0xc6, 0xd0, 0xf5, 0x6b, 0x29, 0xda],
};
const IID_UNKNOWN: GUID = GUID {
    data1: 0,
    data2: 0,
    data3: 0,
    data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_AGILE: GUID = GUID {
    data1: 0x94ea2b94,
    data2: 0xe9cc,
    data3: 0x49e0,
    data4: [0xc0, 0xff, 0xee, 0x64, 0xca, 0x8f, 0x5b, 0x90],
};
#[repr(C)]
struct CoreArgs {
    base: Inspectable,
    get_handled: unsafe extern "system" fn(*mut c_void, *mut u8) -> i32,
    set_handled: unsafe extern "system" fn(*mut c_void, u8) -> i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Pressed,
    Moved,
    Released,
    Action,
}
impl Event {
    fn methods(self, v: &Controller) -> (AddHandler, RemoveHandler) {
        match self {
            Self::Pressed => (v.add_pressed, v.remove_pressed),
            Self::Moved => (v.add_moved, v.remove_moved),
            Self::Released => (v.add_released, v.remove_released),
            Self::Action => (v.add_action, v.remove_action),
        }
    }
    fn iid(self) -> &'static GUID {
        if self == Self::Action {
            &IID_ACTION_HANDLER
        } else {
            &IID_POINTER_HANDLER
        }
    }
}
#[repr(C)]
struct SinkVtable {
    base: Unknown,
    invoke: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
}
#[repr(C)]
struct Sink {
    vtable: &'static SinkVtable,
    refs: AtomicU32,
    event: Event,
    log: Log,
    count: Arc<AtomicU64>,
}
static SINK_VTABLE: SinkVtable = SinkVtable {
    base: Unknown {
        query: sink_query,
        add_ref: sink_add_ref,
        release: sink_release,
    },
    invoke: sink_invoke,
};
fn same_guid(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
unsafe extern "system" fn sink_query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> i32 {
    if this.is_null() || iid.is_null() || out.is_null() {
        return 0x80004003u32 as i32;
    }
    *out = null_mut();
    let sink = &*(this as *const Sink);
    if same_guid(&*iid, &IID_UNKNOWN)
        || same_guid(&*iid, &IID_AGILE)
        || same_guid(&*iid, sink.event.iid())
    {
        *out = this;
        sink_add_ref(this);
        0
    } else {
        0x80004002u32 as i32
    }
}
unsafe extern "system" fn sink_add_ref(this: *mut c_void) -> u32 {
    (&*(this as *const Sink))
        .refs
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn sink_release(this: *mut c_void) -> u32 {
    let old = (&*(this as *const Sink))
        .refs
        .fetch_sub(1, Ordering::Release);
    if old == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(this as *mut Sink));
        0
    } else {
        old - 1
    }
}
unsafe fn handle_pointer(args: *mut c_void) -> Result<(), String> {
    if args.is_null() {
        return Err("pointer event arguments are null".into());
    }
    let unknown = &**(args as *const *const Unknown);
    let mut raw = null_mut();
    checked(
        "PointerEventArgs.QueryInterface(ICoreWindowEventArgs)",
        (unknown.query)(args, &IID_CORE_ARGS, &mut raw),
    )?;
    let core = Object::owned(raw)?;
    checked(
        "PointerEventArgs.Handled=true",
        (core.vtable::<CoreArgs>().set_handled)(core.raw(), 1),
    )?;
    let mut value = 0;
    checked(
        "PointerEventArgs.get_Handled",
        (core.vtable::<CoreArgs>().get_handled)(core.raw(), &mut value),
    )?;
    if value == 0 {
        return Err("pointer event Handled readback was false".into());
    }
    Ok(())
}
unsafe extern "system" fn sink_invoke(
    this: *mut c_void,
    _sender: *mut c_void,
    args: *mut c_void,
) -> i32 {
    let sink = &*(this as *const Sink);
    let count = sink.count.fetch_add(1, Ordering::Relaxed) + 1;
    let result = if sink.event == Event::Action {
        Ok(())
    } else {
        handle_pointer(args)
    };
    match result {
        Ok(()) => {
            if sink.event != Event::Moved || count.is_multiple_of(32) {
                sink.log.event(format!(
                    "GESTURE_EVENT kind={:?} handled=true count={count}",
                    sink.event
                ));
            }
            0
        }
        Err(error) => {
            sink.log.event(format!(
                "GESTURE_HANDLER_ERROR kind={:?} {error}",
                sink.event
            ));
            0x80004005u32 as i32
        }
    }
}
fn new_sink(event: Event, log: Log, count: Arc<AtomicU64>) -> Object {
    let sink = Box::new(Sink {
        vtable: &SINK_VTABLE,
        refs: AtomicU32::new(1),
        event,
        log,
        count,
    });
    unsafe { Object::owned(Box::into_raw(sink).cast()).expect("boxed delegate is non-null") }
}
fn checked(name: &str, result: i32) -> Result<(), String> {
    if result >= 0 {
        Ok(())
    } else {
        Err(format!("{name}: HRESULT 0x{:08X}", result as u32))
    }
}
fn unsupported(result: i32) -> bool {
    matches!(result as u32, 0x80040154 | 0x80004002 | 0x8000000F)
}
struct Apartment(PhantomData<Rc<()>>);
impl Apartment {
    fn new() -> Result<Self, String> {
        checked("RoInitialize", unsafe { RoInitialize(0) })?;
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() }
    }
}
struct StringHandle(HString);
impl StringHandle {
    fn new(text: &str) -> Result<Self, String> {
        let wide: Vec<_> = text.encode_utf16().collect();
        let mut value = null_mut();
        checked("WindowsCreateString", unsafe {
            WindowsCreateString(wide.as_ptr(), wide.len() as u32, &mut value)
        })?;
        Ok(Self(value))
    }
}
impl Drop for StringHandle {
    fn drop(&mut self) {
        unsafe { WindowsDeleteString(self.0) };
    }
}
struct Object(NonNull<c_void>);
impl Object {
    unsafe fn owned(raw: *mut c_void) -> Result<Self, String> {
        NonNull::new(raw)
            .map(Self)
            .ok_or_else(|| "WinRT returned a null interface".into())
    }
    fn raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }
    unsafe fn vtable<T>(&self) -> &T {
        &**(self.raw() as *const *const T)
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        unsafe { (self.vtable::<Unknown>().release)(self.raw()) };
    }
}
fn factory() -> Result<Option<Object>, String> {
    let class = StringHandle::new(CLASS)?;
    let mut raw = null_mut();
    let result = unsafe { RoGetActivationFactory(class.0, &IID_STATICS, &mut raw) };
    if unsupported(result) {
        return Ok(None);
    }
    checked("RoGetActivationFactory(TouchpadGesturesController)", result)?;
    Ok(Some(unsafe { Object::owned(raw)? }))
}
fn supported(factory: &Object) -> Result<bool, String> {
    let mut value = 0;
    checked("TouchpadGesturesController.IsSupported", unsafe {
        (factory.vtable::<Statics>().is_supported)(factory.raw(), &mut value)
    })?;
    Ok(value != 0)
}
/// Read-only capability probe: no controller is created or enabled.
pub fn probe() -> Result<bool, String> {
    let _apartment = Apartment::new()?;
    let Some(factory) = factory()? else {
        return Ok(false);
    };
    supported(&factory)
}
/// Short native activation check; no touch injection or persistent settings.
pub fn check_native_controller() -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let out = std::path::PathBuf::from(format!("artifacts/gesture-check-{stamp}"));
    let (log, writer) = Log::open(&out)?;
    let guard = Guard::create(log.clone());
    let result = match guard {
        Ok(mut guard) => {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::*;
                let start = std::time::Instant::now();
                let mut msg = MSG::default();
                while start.elapsed() < std::time::Duration::from_millis(200) {
                    while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            let disabled = guard.disable();
            drop(guard);
            disabled?;
            println!("gesture_controller_activation_readback_release=passed\nsettings_changed=false\nphysical_gesture_suppression=MANUAL_TEST_REQUIRED");
            Ok(())
        }
        Err(e) => Err(e),
    };
    drop(log);
    writer.join().map_err(|_| "gesture log writer panicked")??;
    result
}

pub trait Control {
    fn set_mask(&mut self, mask: i32) -> Result<(), String>;
    fn set_enabled(&mut self, enabled: bool) -> Result<(), String>;
    fn verify(&mut self, mask: i32, enabled: bool) -> Result<(), String>;
}
/// Keeps activation and rollback testable without claiming real gesture capture.
fn enable<C: Control>(control: &mut C) -> Result<(), String> {
    control.set_mask(THREE_FOUR_FINGER_MASK)?;
    if let Err(error) = control
        .set_enabled(true)
        .and_then(|()| control.verify(THREE_FOUR_FINGER_MASK, true))
    {
        return match control.set_enabled(false) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}; controller disable failed: {cleanup}")),
        };
    }
    Ok(())
}
impl Control for Object {
    fn set_mask(&mut self, mask: i32) -> Result<(), String> {
        checked("TouchpadGesturesController.SupportedGestures", unsafe {
            (self.vtable::<Controller>().set_gestures)(self.raw(), mask)
        })
    }
    fn set_enabled(&mut self, value: bool) -> Result<(), String> {
        checked("TouchpadGesturesController.Enabled", unsafe {
            (self.vtable::<Controller>().set_enabled)(self.raw(), u8::from(value))
        })
    }
    fn verify(&mut self, mask: i32, enabled: bool) -> Result<(), String> {
        let mut actual_mask = 0;
        let mut actual_enabled = 0;
        unsafe {
            checked(
                "get_SupportedGestures",
                (self.vtable::<Controller>().get_gestures)(self.raw(), &mut actual_mask),
            )?;
            checked(
                "get_Enabled",
                (self.vtable::<Controller>().get_enabled)(self.raw(), &mut actual_enabled),
            )?;
        }
        if actual_mask != mask || (actual_enabled != 0) != enabled {
            return Err("gesture controller readback did not match requested state".into());
        }
        Ok(())
    }
}
/// Must be created and dropped on one thread with a message pump.
pub struct Guard {
    controller: Object,
    // COM interfaces must be released before uninitializing the apartment.
    _apartment: Apartment,
    log: Log,
    enabled: bool,
    subscriptions: Vec<(Event, i64, Object)>,
    events: Arc<AtomicU64>,
}
impl Guard {
    pub fn create(log: Log) -> Result<Self, String> {
        let apartment = Apartment::new()?;
        let factory=factory()?.ok_or("foreground touchpad gesture API is unavailable; use Windows touchpad settings or omit --suppress-system-gestures")?;
        if !supported(&factory)? {
            return Err("foreground touchpad gesture ownership is unsupported on this system; use Windows touchpad settings or omit --suppress-system-gestures".into());
        }
        let mut raw = null_mut();
        checked("TouchpadGesturesController.CreateForProcess", unsafe {
            (factory.vtable::<Statics>().create)(factory.raw(), &mut raw)
        })?;
        let controller = unsafe { Object::owned(raw)? };
        let mut guard = Self {
            controller,
            _apartment: apartment,
            log,
            enabled: true,
            subscriptions: vec![],
            events: Arc::new(AtomicU64::new(0)),
        };
        guard.controller.set_enabled(false)?;
        for event in [Event::Pressed, Event::Moved, Event::Released, Event::Action] {
            let sink = new_sink(event, guard.log.clone(), guard.events.clone());
            let mut token = 0;
            checked("TouchpadGesturesController.subscribe", unsafe {
                let (add, _) = event.methods(guard.controller.vtable::<Controller>());
                add(guard.controller.raw(), sink.raw(), &mut token)
            })?;
            guard.subscriptions.push((event, token, sink));
        }
        enable(&mut guard.controller)?;
        guard.log.event("GESTURE_GUARD enabled scope=foreground-process fingers=3,4 handlers=4 settings_changed=false");
        Ok(guard)
    }
    pub fn disable(&mut self) -> Result<(), String> {
        if self.enabled {
            self.controller.set_enabled(false)?;
            let mut value = 1;
            checked("TouchpadGesturesController.get_Enabled", unsafe {
                (self.controller.vtable::<Controller>().get_enabled)(
                    self.controller.raw(),
                    &mut value,
                )
            })?;
            if value != 0 {
                return Err("gesture controller did not disable".into());
            }
            self.enabled = false;
            self.log
                .event("GESTURE_GUARD disabled settings_changed=false");
        }
        Ok(())
    }
    pub fn event_counter(&self) -> Arc<AtomicU64> {
        self.events.clone()
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Err(error) = self.disable() {
            self.log
                .event(format!("GESTURE_GUARD_DISABLE_ERROR {error}"));
        }
        for (event, token, _) in &self.subscriptions {
            let result = unsafe {
                let (_, remove) = event.methods(self.controller.vtable::<Controller>());
                remove(self.controller.raw(), *token)
            };
            if let Err(error) = checked("TouchpadGesturesController.unsubscribe", result) {
                self.log.event(format!("GESTURE_HANDLER_ERROR {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[repr(C)]
    struct FakeArgs {
        table: *const CoreArgs,
        refs: Cell<u32>,
        handled: Cell<u8>,
        ignore_handled: bool,
    }
    unsafe extern "system" fn args_query(
        this: *mut c_void,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> i32 {
        *out = null_mut();
        if same_guid(&*iid, &IID_CORE_ARGS) {
            *out = this;
            args_addref(this);
            0
        } else {
            0x80004002u32 as i32
        }
    }
    unsafe extern "system" fn args_addref(this: *mut c_void) -> u32 {
        let a = &*(this as *const FakeArgs);
        a.refs.set(a.refs.get() + 1);
        a.refs.get()
    }
    unsafe extern "system" fn args_release(this: *mut c_void) -> u32 {
        let a = &*(this as *const FakeArgs);
        a.refs.set(a.refs.get() - 1);
        a.refs.get()
    }
    unsafe extern "system" fn args_get(this: *mut c_void, out: *mut u8) -> i32 {
        *out = (&*(this as *const FakeArgs)).handled.get();
        0
    }
    unsafe extern "system" fn args_set(this: *mut c_void, value: u8) -> i32 {
        let a = &*(this as *const FakeArgs);
        if !a.ignore_handled {
            a.handled.set(value);
        }
        0
    }
    fn args_table() -> CoreArgs {
        CoreArgs {
            base: Inspectable {
                query: args_query,
                add_ref: args_addref,
                release: args_release,
                get_iids: std::ptr::null(),
                get_name: std::ptr::null(),
                get_trust: std::ptr::null(),
            },
            get_handled: args_get,
            set_handled: args_set,
        }
    }
    #[test]
    fn pointer_delegate_handles_events_and_balances_com_references() {
        let table = args_table();
        let mut args = FakeArgs {
            table: &table,
            refs: Cell::new(1),
            handled: Cell::new(0),
            ignore_handled: false,
        };
        let (log, records) = Log::memory();
        let count = Arc::new(AtomicU64::new(0));
        let sink = new_sink(Event::Pressed, log, count.clone());
        unsafe {
            let mut interface = null_mut();
            assert_eq!(
                sink_query(sink.raw(), &IID_POINTER_HANDLER, &mut interface),
                0
            );
            assert_eq!(interface, sink.raw());
            assert_eq!(sink_release(interface), 1);
            assert!(sink_query(sink.raw(), &IID_ACTION_HANDLER, &mut interface) < 0);
            assert!(interface.is_null());
            assert_eq!(
                (sink.vtable::<SinkVtable>().invoke)(
                    sink.raw(),
                    null_mut(),
                    (&mut args as *mut FakeArgs).cast()
                ),
                0
            );
        }
        assert_eq!(args.handled.get(), 1);
        assert_eq!(args.refs.get(), 1);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        drop(sink);
        assert!(records
            .try_iter()
            .any(|l| l.contains("GESTURE_EVENT kind=Pressed handled=true")));
    }
    #[test]
    fn pointer_handler_rejects_null_and_failed_handled_readback() {
        let table = args_table();
        let mut args = FakeArgs {
            table: &table,
            refs: Cell::new(1),
            handled: Cell::new(0),
            ignore_handled: true,
        };
        unsafe {
            assert!(handle_pointer(null_mut()).is_err());
            assert!(handle_pointer((&mut args as *mut FakeArgs).cast()).is_err());
        }
        assert_eq!(args.refs.get(), 1);
        let (log, records) = Log::memory();
        let sink = new_sink(Event::Moved, log, Arc::new(AtomicU64::new(0)));
        unsafe {
            assert!(sink_invoke(sink.raw(), null_mut(), null_mut()) < 0);
        }
        drop(sink);
        assert!(records
            .try_iter()
            .any(|l| l.contains("GESTURE_HANDLER_ERROR")));
    }
    #[derive(Default)]
    struct Fake {
        ops: Vec<String>,
        fail_enable: bool,
        fail_readback: bool,
    }
    impl Control for Fake {
        fn set_mask(&mut self, m: i32) -> Result<(), String> {
            self.ops.push(format!("mask={m}"));
            Ok(())
        }
        fn set_enabled(&mut self, v: bool) -> Result<(), String> {
            self.ops.push(format!("enabled={v}"));
            if v && self.fail_enable {
                Err("enable failed".into())
            } else {
                Ok(())
            }
        }
        fn verify(&mut self, _: i32, _: bool) -> Result<(), String> {
            self.ops.push("verify".into());
            if self.fail_readback {
                Err("readback failed".into())
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn mask_only_claims_three_and_four_finger_actions_and_swipes() {
        assert_eq!(THREE_FOUR_FINGER_MASK, 27);
        assert_eq!(THREE_FOUR_FINGER_MASK & (4 | 32), 0);
    }
    #[test]
    fn successful_activation_is_verified() {
        let mut f = Fake::default();
        enable(&mut f).unwrap();
        assert_eq!(f.ops, ["mask=27", "enabled=true", "verify"]);
    }
    #[test]
    fn failed_activation_or_readback_is_disabled() {
        for readback in [true, false] {
            let mut f = Fake {
                fail_enable: !readback,
                fail_readback: readback,
                ..Default::default()
            };
            assert!(enable(&mut f).is_err());
            assert_eq!(f.ops.last().unwrap(), "enabled=false");
        }
    }
    #[test]
    fn expected_missing_api_results_are_not_misreported_as_supported() {
        assert!(unsupported(0x80040154u32 as i32));
        assert!(unsupported(0x80004002u32 as i32));
        assert!(!unsupported(0x80070005u32 as i32));
    }
}
