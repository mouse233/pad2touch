//! Independent COM ABI experiment; default operation is read only.
//! Reported interface facts: https://stackoverflow.com/questions/73331905/disable-gestures-through-registry
//! No registry writes or touch injection. --reload-four explicitly notifies
//! four candidate IDs; the transaction script owns backup and restoration.
use std::{ffi::c_void, ptr::null_mut};

#[repr(C)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}
const SHELL: Guid = Guid {
    a: 0xc2f03a33,
    b: 0x21f5,
    c: 0x47fa,
    d: [0xb4, 0xbb, 0x15, 0x63, 0x62, 0xa2, 0xf2, 0x39],
};
const PROVIDER: Guid = Guid {
    a: 0x6d5140c1,
    b: 0x7436,
    c: 0x11ce,
    d: [0x80, 0x34, 0, 0xaa, 0, 0x60, 0x09, 0xfa],
};
const SERVICE: Guid = Guid {
    a: 0x53660488,
    b: 0x8855,
    c: 0x460b,
    d: [0xa9, 0xab, 0x5c, 0xfc, 0x6b, 0x50, 0x12, 0xca],
};
const CACHE: Guid = Guid {
    a: 0x4214f6fa,
    b: 0xeb36,
    c: 0x4e2f,
    d: [0x9c, 0xa2, 0x23, 0xfd, 0xc1, 0x83, 0x2d, 0xf7],
};
#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, kind: u32) -> i32;
    fn CoUninitialize();
    fn CoCreateInstance(
        class: *const Guid,
        outer: *mut c_void,
        context: u32,
        iid: *const Guid,
        result: *mut *mut c_void,
    ) -> i32;
}
#[repr(C)]
struct Unknown {
    query: *const c_void,
    addref: *const c_void,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}
#[repr(C)]
struct Provider {
    base: Unknown,
    query_service:
        unsafe extern "system" fn(*mut c_void, *const Guid, *const Guid, *mut *mut c_void) -> i32,
}
#[repr(C)]
struct Cache {
    base: Unknown,
    change: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    get_bool: unsafe extern "system" fn(*mut c_void, i32, *mut i32) -> i32,
    get_dword: unsafe extern "system" fn(*mut c_void, i32, *mut u32) -> i32,
}
struct Object(*mut c_void);
impl Object {
    unsafe fn table<T>(&self) -> &T {
        &**(self.0 as *const *const T)
    }
    fn owned(raw: *mut c_void) -> Result<Self, String> {
        if raw.is_null() {
            Err("COM returned null".into())
        } else {
            Ok(Self(raw))
        }
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            (self.table::<Unknown>().release)(self.0);
        }
    }
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
fn checked(stage: &str, hr: i32) -> Result<(), String> {
    if hr < 0 {
        Err(format!("{stage}: HRESULT 0x{:08X}", hr as u32))
    } else {
        Ok(())
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !args.is_empty() && args != ["--read-cache"] && args != ["--reload-four"] {
        return Err("usage: gesture-cache-probe.exe [--read-cache | --reload-four]".into());
    }
    checked("CoInitializeEx", unsafe { CoInitializeEx(null_mut(), 2) })?;
    let _apartment = Apartment;
    let mut raw = null_mut();
    checked(
        "CoCreateInstance(ImmersiveShell/IServiceProvider)",
        unsafe { CoCreateInstance(&SHELL, null_mut(), 4, &PROVIDER, &mut raw) },
    )?;
    let provider = Object::owned(raw)?;
    raw = null_mut();
    checked("QueryService(IImmersiveSettingsCache)", unsafe {
        (provider.table::<Provider>().query_service)(provider.0, &SERVICE, &CACHE, &mut raw)
    })?;
    let cache = Object::owned(raw)?;
    let reload = args == ["--reload-four"];
    println!("cache_interface_available=true\nundocumented_interface=true\nregistry_writes=false\ncache_reload_requested={reload}\nphysical_suppression=NOT_TESTED");
    let mut failed = false;
    if reload {
        for id in [0x0f, 0x10, 0x11, 0x12] {
            let hr = unsafe { (cache.table::<Cache>().change)(cache.0, id) };
            println!("cache_notification_id=0x{id:02X} hr=0x{:08X}", hr as u32);
            failed |= hr < 0;
        }
    }
    if !args.is_empty() {
        for (id, name) in [
            (0x0f, "ThreeFingerTapEnabled"),
            (0x10, "FourFingerTapEnabled"),
            (0x11, "ThreeFingerSlideEnabled"),
            (0x12, "FourFingerSlideEnabled"),
        ] {
            let mut boolean = 0;
            let mut value = 0;
            let (hr_bool, hr_dword) = unsafe {
                (
                    (cache.table::<Cache>().get_bool)(cache.0, id, &mut boolean),
                    (cache.table::<Cache>().get_dword)(cache.0, id, &mut value),
                )
            };
            println!("candidate_setting={name} id=0x{id:02X} bool_hr=0x{:08X} bool_value={boolean} dword_hr=0x{:08X} dword_value={value}",hr_bool as u32,hr_dword as u32);
        }
        println!("id_mapping=UNVERIFIED_REQUIRES_INDEPENDENT_COMPARISON");
    }
    if failed {
        return Err("one or more cache notifications failed".into());
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
