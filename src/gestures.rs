//! Optional foreground-process touchpad gesture ownership, using the published
//! UniversalApiContract v19 ABI. No registry or touchpad settings are modified.
//! SDK: windows.ui.input.h, ITouchpadGesturesController[Statics].
use crate::logging::Log;
use std::{
    ffi::c_void,
    marker::PhantomData,
    ptr::{null_mut, NonNull},
    rc::Rc,
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
struct Statics {
    base: Inspectable,
    is_supported: unsafe extern "system" fn(*mut c_void, *mut u8) -> i32,
    create: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}
// Only the published prefix through SupportedGestures is used. The following
// event registration methods remain owned by the runtime, not fabricated here.
#[repr(C)]
struct Controller {
    base: Inspectable,
    get_enabled: unsafe extern "system" fn(*mut c_void, *mut u8) -> i32,
    set_enabled: unsafe extern "system" fn(*mut c_void, u8) -> i32,
    get_gestures: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    set_gestures: unsafe extern "system" fn(*mut c_void, i32) -> i32,
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
        unsafe { (self.vtable::<Inspectable>().release)(self.raw()) };
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
/// Must be created and dropped on the input worker, which pumps messages.
pub struct Guard {
    controller: Object,
    // COM interfaces must be released before uninitializing the apartment.
    _apartment: Apartment,
    log: Log,
    enabled: bool,
}
impl Guard {
    pub fn create(log: Log) -> Result<Self, String> {
        let apartment = Apartment::new()?;
        let factory=factory()?.ok_or("foreground touchpad gesture API is unavailable; use Windows touchpad gesture settings or --keep-system-gestures")?;
        if !supported(&factory)? {
            return Err("foreground touchpad gesture ownership is unsupported on this system; use Windows touchpad settings or --keep-system-gestures".into());
        }
        let mut raw = null_mut();
        checked("TouchpadGesturesController.CreateForProcess", unsafe {
            (factory.vtable::<Statics>().create)(factory.raw(), &mut raw)
        })?;
        let mut controller = unsafe { Object::owned(raw)? };
        enable(&mut controller)?;
        log.event(
            "GESTURE_GUARD enabled scope=foreground-process fingers=3,4 settings_changed=false",
        );
        Ok(Self {
            controller,
            _apartment: apartment,
            log,
            enabled: true,
        })
    }
    pub fn disable(&mut self) -> Result<(), String> {
        if self.enabled {
            self.controller.set_enabled(false)?;
            self.controller.verify(THREE_FOUR_FINGER_MASK, false)?;
            self.enabled = false;
            self.log
                .event("GESTURE_GUARD disabled settings_changed=false");
        }
        Ok(())
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Err(error) = self.disable() {
            self.log
                .event(format!("GESTURE_GUARD_DISABLE_ERROR {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
