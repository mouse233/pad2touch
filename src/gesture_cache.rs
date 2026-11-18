//! Shared access to the build-bound Windows gesture settings interfaces.
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

#[link(name = "user32")]
extern "system" {
    fn SystemParametersInfoW(action: u32, param: u32, value: *mut c_void, flags: u32) -> i32;
}

pub(super) struct SessionCache {
    cache: Object,
    _provider: Object,
    _apartment: Apartment,
}

impl SessionCache {
    pub(super) fn open() -> Result<Self, String> {
        checked("CoInitializeEx", unsafe { CoInitializeEx(null_mut(), 2) })?;
        let apartment = Apartment;
        let mut raw = null_mut();
        checked("CoCreateInstance(ImmersiveShell)", unsafe {
            CoCreateInstance(&SHELL, null_mut(), 4, &PROVIDER, &mut raw)
        })?;
        let provider = Object::owned(raw)?;
        raw = null_mut();
        checked("QueryService(IImmersiveSettingsCache)", unsafe {
            (provider.table::<Provider>().query_service)(provider.0, &SERVICE, &CACHE, &mut raw)
        })?;
        Ok(Self {
            cache: Object::owned(raw)?,
            _provider: provider,
            _apartment: apartment,
        })
    }

    pub(super) fn values(&self, reload: bool) -> Result<[u32; 4], String> {
        let mut values = [0; 4];
        let table = unsafe { self.cache.table::<Cache>() };
        if reload {
            for id in 0x0f..=0x12 {
                checked("Refresh settings cache", unsafe {
                    (table.change)(self.cache.0, id)
                })?;
            }
        }
        for (index, value) in values.iter_mut().enumerate() {
            checked("Read settings cache", unsafe {
                (table.get_dword)(self.cache.0, 0x0f + index as i32, value)
            })?;
        }
        Ok(values)
    }
}

pub(super) fn screen_state(set: Option<u32>) -> Result<u32, String> {
    if let Some(value) = set {
        if unsafe { SystemParametersInfoW(0x2031, 0, value as usize as *mut c_void, 0) } == 0 {
            return Err(format!(
                "Set touchscreen state: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    let mut live = 0u32;
    if unsafe { SystemParametersInfoW(0x2030, 0, (&mut live as *mut u32).cast(), 0) } == 0 {
        return Err(format!(
            "Get touchscreen state: {}",
            std::io::Error::last_os_error()
        ));
    }
    if live > 1 || set.is_some_and(|expected| expected != live) {
        return Err("Unexpected touchscreen state".into());
    }
    Ok(live)
}
