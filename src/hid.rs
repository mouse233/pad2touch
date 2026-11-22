use crate::decode::{Field, Report};
use std::{mem::size_of, ptr::null_mut};
use windows_sys::Win32::{
    Devices::{
        DeviceAndDriverInstallation::{
            CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_Locate_DevNodeW,
            CM_LOCATE_DEVNODE_NORMAL, CONFIGRET, CR_BUFFER_SMALL, CR_SUCCESS,
        },
        HumanInterfaceDevice::*,
        Properties::{DEVPKEY_Device_InstanceId, DEVPKEY_NAME, DEVPROP_TYPE_STRING},
    },
    Foundation::*,
    UI::Input::*,
};

pub fn winerr(api: &str) -> String {
    format!("{api}: Win32 error {}", unsafe { GetLastError() })
}
pub fn check_hid(api: &str, status: i32) -> Result<(), String> {
    if status == HIDP_STATUS_SUCCESS {
        Ok(())
    } else {
        Err(format!("{api}: HID status 0x{:08X}", status as u32))
    }
}
pub struct Device {
    pub handle: usize,
    pub path: String,
    pub vid: u32,
    pub pid: u32,
    pub pp: Vec<u64>,
    pub caps: HIDP_CAPS,
}
impl Device {
    pub fn geometry(&self) -> Result<crate::device_info::Geometry, String> {
        use crate::device_info::{Axis, Geometry};
        let mut count = self.caps.NumberInputValueCaps;
        let mut caps = vec![HIDP_VALUE_CAPS::default(); count as usize];
        if count == 0 {
            return Err("no input value capabilities".into());
        }
        unsafe {
            check_hid(
                "HidP_GetValueCaps(geometry)",
                HidP_GetValueCaps(HidP_Input, caps.as_mut_ptr(), &mut count, self.pp()),
            )?;
        }
        let mut links = std::collections::BTreeSet::new();
        for v in caps
            .iter()
            .take(count as usize)
            .filter(|v| !v.IsAlias && v.UsagePage == 0x0D)
        {
            let (lo, hi) = unsafe {
                if v.IsRange {
                    (v.Anonymous.Range.UsageMin, v.Anonymous.Range.UsageMax)
                } else {
                    (v.Anonymous.NotRange.Usage, v.Anonymous.NotRange.Usage)
                }
            };
            if (lo..=hi).contains(&0x51) {
                links.insert((v.ReportID, v.LinkCollection));
            }
        }
        let mut axes = std::collections::BTreeMap::<(u8, u16), (Option<Axis>, Option<Axis>)>::new();
        for v in caps.iter().take(count as usize).filter(|v| {
            !v.IsAlias && v.UsagePage == 1 && links.contains(&(v.ReportID, v.LinkCollection))
        }) {
            let (lo, hi) = unsafe {
                if v.IsRange {
                    (v.Anonymous.Range.UsageMin, v.Anonymous.Range.UsageMax)
                } else {
                    (v.Anonymous.NotRange.Usage, v.Anonymous.NotRange.Usage)
                }
            };
            if !(lo..=hi).contains(&0x30) && !(lo..=hi).contains(&0x31) {
                continue;
            }
            if !v.IsAbsolute {
                return Err("contact coordinates are not absolute".into());
            }
            let axis = Axis {
                logical_min: v.LogicalMin,
                logical_max: v.LogicalMax,
                physical_min: v.PhysicalMin,
                physical_max: v.PhysicalMax,
                units: v.Units,
                exponent: v.UnitsExp,
                bits: v.BitSize,
            };
            let pair = axes.entry((v.ReportID, v.LinkCollection)).or_default();
            if (lo..=hi).contains(&0x30) {
                pair.0 = Some(axis);
            }
            if (lo..=hi).contains(&0x31) {
                pair.1 = Some(axis);
            }
        }
        let mut result = None;
        for (x, y) in axes.into_values() {
            let geometry = Geometry {
                x: x.ok_or("missing contact X capability")?,
                y: y.ok_or("missing contact Y capability")?,
            };
            geometry.aspect()?;
            if result.is_some_and(|old: Geometry| old != geometry) {
                return Err("inconsistent physical/logical geometry across contacts".into());
            }
            result = Some(geometry);
        }
        result.ok_or("no physical contact X/Y geometry".into())
    }
    pub fn pp(&self) -> PHIDP_PREPARSED_DATA {
        self.pp.as_ptr() as isize
    }
    pub fn name(&self) -> Option<String> {
        let path: Vec<u16> = self.path.encode_utf16().chain([0]).collect();
        let interface_property = |key| {
            device_property_string(|kind, buffer, size| unsafe {
                CM_Get_Device_Interface_PropertyW(path.as_ptr(), key, kind, buffer, size, 0)
            })
        };
        if let Some(name) = interface_property(&DEVPKEY_NAME) {
            return Some(name);
        }

        // A Raw Input path identifies the HID interface. Its owning device
        // can have a display name even when the interface itself has none.
        let instance = interface_property(&DEVPKEY_Device_InstanceId)?;
        let instance: Vec<u16> = instance.encode_utf16().chain([0]).collect();
        let mut node = 0;
        if unsafe { CM_Locate_DevNodeW(&mut node, instance.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) }
            != CR_SUCCESS
        {
            return None;
        }
        device_property_string(|kind, buffer, size| unsafe {
            CM_Get_DevNode_PropertyW(node, &DEVPKEY_NAME, kind, buffer, size, 0)
        })
    }
    pub fn summary(&self) -> String {
        format!("handle=0x{:X} VID={:04X} PID={:04X} usage={:02X}:{:02X} input_bytes={} value_caps={} button_caps={} path={}", self.handle, self.vid, self.pid, self.caps.UsagePage, self.caps.Usage, self.caps.InputReportByteLength, self.caps.NumberInputValueCaps, self.caps.NumberInputButtonCaps, self.path)
    }
    pub fn fields(&self) -> Result<Vec<Field>, String> {
        unsafe {
            let mut vn = self.caps.NumberInputValueCaps;
            let mut values = vec![HIDP_VALUE_CAPS::default(); vn as usize];
            if vn > 0 {
                check_hid(
                    "HidP_GetValueCaps",
                    HidP_GetValueCaps(HidP_Input, values.as_mut_ptr(), &mut vn, self.pp()),
                )?;
            }
            let mut fields = Vec::new();
            for v in values.iter().take(vn as usize) {
                if v.IsAlias {
                    continue;
                }
                let (umin, umax, dmin, dmax) = if v.IsRange {
                    let r = v.Anonymous.Range;
                    (r.UsageMin, r.UsageMax, r.DataIndexMin, r.DataIndexMax)
                } else {
                    let r = v.Anonymous.NotRange;
                    (r.Usage, r.Usage, r.DataIndex, r.DataIndex)
                };
                if !v.IsRange && v.ReportCount != 1 {
                    return Err(format!(
                        "unsupported HID value array report={} usage={:X}:{:X} count={}",
                        v.ReportID, v.UsagePage, umin, v.ReportCount
                    ));
                }
                if umax < umin || dmax < dmin || umax - umin != dmax - dmin {
                    return Err("unsupported HID value range".into());
                }
                for i in 0..=umax - umin {
                    fields.push(Field {
                        report: v.ReportID,
                        index: dmin + i,
                        page: v.UsagePage,
                        usage: umin + i,
                        link: v.LinkCollection,
                        bits: v.BitSize,
                        min: v.LogicalMin,
                        max: v.LogicalMax,
                        button: false,
                    });
                }
            }
            let mut bn = self.caps.NumberInputButtonCaps;
            let mut buttons = vec![HIDP_BUTTON_CAPS::default(); bn as usize];
            if bn > 0 {
                check_hid(
                    "HidP_GetButtonCaps",
                    HidP_GetButtonCaps(HidP_Input, buttons.as_mut_ptr(), &mut bn, self.pp()),
                )?;
            }
            for b in buttons.iter().take(bn as usize) {
                if b.IsAlias {
                    continue;
                }
                let (umin, umax, dmin, dmax) = if b.IsRange {
                    let r = b.Anonymous.Range;
                    (r.UsageMin, r.UsageMax, r.DataIndexMin, r.DataIndexMax)
                } else {
                    let r = b.Anonymous.NotRange;
                    (r.Usage, r.Usage, r.DataIndex, r.DataIndex)
                };
                if umax < umin || dmax < dmin || umax - umin != dmax - dmin {
                    return Err("unsupported HID button range/array".into());
                }
                for i in 0..=umax - umin {
                    fields.push(Field {
                        report: b.ReportID,
                        index: dmin + i,
                        page: b.UsagePage,
                        usage: umin + i,
                        link: b.LinkCollection,
                        bits: 1,
                        min: 0,
                        max: 1,
                        button: true,
                    });
                }
            }
            Ok(fields)
        }
    }
    pub fn parse(&self, fields: &[Field], report: &[u8]) -> Result<Option<Report>, String> {
        if report.len() != self.caps.InputReportByteLength as usize {
            return Err(format!(
                "unexpected report length {} expected {}",
                report.len(),
                self.caps.InputReportByteLength
            ));
        }
        let mut length = unsafe { HidP_MaxDataListLength(HidP_Input, self.pp()) };
        let mut data = vec![HIDP_DATA::default(); length as usize];
        let mut copy = report.to_vec();
        unsafe {
            check_hid(
                "HidP_GetData",
                HidP_GetData(
                    HidP_Input,
                    data.as_mut_ptr(),
                    &mut length,
                    self.pp(),
                    copy.as_mut_ptr(),
                    copy.len() as u32,
                ),
            )?;
        }
        let entries: Vec<_> = data
            .iter()
            .take(length as usize)
            .map(|d| {
                let button = fields
                    .iter()
                    .any(|f| f.report == report[0] && f.index == d.DataIndex && f.button);
                (d.DataIndex, unsafe {
                    if button {
                        u32::from(d.Anonymous.On)
                    } else {
                        d.Anonymous.RawValue
                    }
                })
            })
            .collect();
        crate::decode::decode(fields, report[0], &entries)
    }
}
fn device_property_string(
    mut query: impl FnMut(*mut u32, *mut u8, *mut u32) -> CONFIGRET,
) -> Option<String> {
    let mut kind = 0;
    let mut bytes = 0;
    if query(&mut kind, null_mut(), &mut bytes) != CR_BUFFER_SMALL {
        return None;
    }
    let mut value = vec![0u16; (bytes as usize).div_ceil(2)];
    if query(&mut kind, value.as_mut_ptr().cast(), &mut bytes) != CR_SUCCESS
        || kind != DEVPROP_TYPE_STRING
    {
        return None;
    }
    let end = value.iter().position(|&unit| unit == 0)?;
    let value = String::from_utf16_lossy(&value[..end]);
    let name = value.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

pub fn discover() -> Result<Vec<Device>, String> {
    unsafe {
        // The device count can grow between the size query and the copy, for
        // example while a Bluetooth device connects. Query again in that case.
        let mut list = Vec::new();
        let mut got = u32::MAX;
        for _ in 0..4 {
            let mut count = 0;
            if GetRawInputDeviceList(
                null_mut(),
                &mut count,
                size_of::<RAWINPUTDEVICELIST>() as u32,
            ) == u32::MAX
            {
                return Err(winerr("GetRawInputDeviceList(size)"));
            }
            list = vec![RAWINPUTDEVICELIST::default(); count as usize];
            got = GetRawInputDeviceList(
                list.as_mut_ptr(),
                &mut count,
                size_of::<RAWINPUTDEVICELIST>() as u32,
            );
            if got != u32::MAX || GetLastError() != ERROR_INSUFFICIENT_BUFFER {
                break;
            }
        }
        if got == u32::MAX {
            return Err(winerr("GetRawInputDeviceList"));
        }
        let mut result = Vec::new();
        for item in list.into_iter().take(got as usize) {
            if item.dwType != RIM_TYPEHID {
                continue;
            }
            let mut info = RID_DEVICE_INFO {
                cbSize: size_of::<RID_DEVICE_INFO>() as u32,
                ..Default::default()
            };
            let mut n = info.cbSize;
            if GetRawInputDeviceInfoW(
                item.hDevice,
                RIDI_DEVICEINFO,
                (&mut info as *mut RID_DEVICE_INFO).cast(),
                &mut n,
            ) == u32::MAX
            {
                // Any HID device can disappear after listing. An unrelated
                // device must not hide every touchpad, so skip this entry.
                continue;
            }
            let hi = info.Anonymous.hid;
            if hi.usUsagePage != 0x0D || hi.usUsage != 0x05 {
                continue;
            }
            let mut chars = 0;
            if GetRawInputDeviceInfoW(item.hDevice, RIDI_DEVICENAME, null_mut(), &mut chars)
                == u32::MAX
            {
                return Err(winerr("RIDI_DEVICENAME(size)"));
            }
            // u32 storage satisfies the API's DWORD alignment requirement.
            let mut name = vec![0u32; (chars as usize * 2).div_ceil(4)];
            if GetRawInputDeviceInfoW(
                item.hDevice,
                RIDI_DEVICENAME,
                name.as_mut_ptr().cast(),
                &mut chars,
            ) == u32::MAX
            {
                return Err(winerr("RIDI_DEVICENAME"));
            }
            let wide = std::slice::from_raw_parts(name.as_ptr().cast::<u16>(), chars as usize);
            let path = String::from_utf16_lossy(
                &wide[..wide.iter().position(|&x| x == 0).unwrap_or(wide.len())],
            );
            let mut bytes = 0;
            if GetRawInputDeviceInfoW(item.hDevice, RIDI_PREPARSEDDATA, null_mut(), &mut bytes)
                == u32::MAX
            {
                return Err(winerr("RIDI_PREPARSEDDATA(size)"));
            }
            let mut pp = vec![0u64; (bytes as usize).div_ceil(8)];
            if GetRawInputDeviceInfoW(
                item.hDevice,
                RIDI_PREPARSEDDATA,
                pp.as_mut_ptr().cast(),
                &mut bytes,
            ) == u32::MAX
            {
                return Err(winerr("RIDI_PREPARSEDDATA"));
            }
            let mut caps = HIDP_CAPS::default();
            check_hid(
                "HidP_GetCaps",
                HidP_GetCaps(pp.as_ptr() as isize, &mut caps),
            )?;
            result.push(Device {
                handle: item.hDevice as usize,
                path,
                vid: hi.dwVendorId,
                pid: hi.dwProductId,
                pp,
                caps,
            });
        }
        Ok(result)
    }
}
pub fn probe() -> Result<(), String> {
    let devices = discover()?;
    println!(
        "Pad2Touch Phase 0: Raw Input touchpad collections={}",
        devices.len()
    );
    for (i, d) in devices.iter().enumerate() {
        println!("device[{i}]: {}", d.summary());
        if let Some(name) = d.name() {
            println!("  Name: {name}");
        }
        match d.geometry().and_then(|g| g.description()) {
            Ok(info) => println!("  {info}"),
            Err(error) => println!("  Physical geometry unavailable: {error}"),
        }
        for f in d.fields()? {
            println!("  {f:?}");
        }
    }
    if devices.is_empty() {
        println!("NO_PTP_COLLECTION: no Raw Input collection with usage 0D:05; hardware feasibility is unverified.");
    }
    Ok(())
}

/// Read into 8-byte-aligned storage and bounds-check the variable RAWHID tail.
pub fn raw_reports(raw: HRAWINPUT) -> Result<(usize, Vec<Vec<u8>>), String> {
    unsafe {
        let hs = size_of::<RAWINPUTHEADER>() as u32;
        let mut bytes = 0;
        if GetRawInputData(raw, RID_INPUT, null_mut(), &mut bytes, hs) == u32::MAX {
            return Err(winerr("GetRawInputData(size)"));
        }
        let mut storage = vec![0u64; (bytes as usize).div_ceil(8)];
        let got = GetRawInputData(raw, RID_INPUT, storage.as_mut_ptr().cast(), &mut bytes, hs);
        if got == u32::MAX {
            return Err(winerr("GetRawInputData"));
        }
        if got < (hs + 8) {
            return Err("short RAWINPUT".into());
        }
        let header = std::ptr::read_unaligned(storage.as_ptr().cast::<RAWINPUTHEADER>());
        if header.dwType != RIM_TYPEHID {
            return Ok((header.hDevice as usize, Vec::new()));
        }
        let b = std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), got as usize);
        let offset = hs as usize;
        let size = u32::from_le_bytes(b[offset..offset + 4].try_into().unwrap()) as usize;
        let count = u32::from_le_bytes(b[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let needed = size
            .checked_mul(count)
            .and_then(|x| x.checked_add(offset + 8))
            .ok_or("RAWHID size overflow")?;
        if size == 0 || count == 0 || needed > b.len() || header.dwSize as usize != got as usize {
            return Err("invalid RAWHID bounds".into());
        }
        Ok((
            header.hDevice as usize,
            b[offset + 8..needed]
                .chunks_exact(size)
                .map(|r| r.to_vec())
                .collect(),
        ))
    }
}

#[cfg(test)]
mod recorded_tests {
    use super::*;
    fn recorded_device() -> Device {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/asue1a03-preparsed.bin"),
        )
        .expect("local HID recording required; see tests/fixtures/README.md");
        let mut pp = vec![0u64; bytes.len().div_ceil(8)];
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pp.as_mut_ptr().cast(), bytes.len());
        }
        let mut caps = HIDP_CAPS::default();
        check_hid("recorded HidP_GetCaps", unsafe {
            HidP_GetCaps(pp.as_ptr() as isize, &mut caps)
        })
        .unwrap();
        Device {
            handle: 0,
            path: "LOCAL_RECORDED_FIXTURE_NOT_LIVE_HARDWARE".into(),
            vid: 0x04F3,
            pid: 0x32A0,
            pp,
            caps,
        }
    }
    #[test]
    #[ignore = "requires private local HID recordings; see tests/fixtures/README.md"]
    fn recorded_hybrid_five_finger_reports_use_real_hid_parser() {
        let d = recorded_device();
        let fields = d.fields().unwrap();
        let mut a = crate::decode::Assembler::default();
        let mut result = None;
        let reports = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/five-contacts.hex"),
        )
        .expect("local HID recording required; see tests/fixtures/README.md");
        for line in reports.lines() {
            let bytes: Vec<_> = line
                .trim()
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|s| u8::from_str_radix(std::str::from_utf8(s).unwrap(), 16).unwrap())
                .collect();
            result = a.push(d.parse(&fields, &bytes).unwrap().unwrap()).unwrap();
        }
        let f = result.unwrap();
        assert_eq!(f.len(), 5);
        assert!(f.iter().all(|c| c.tip && c.confidence));
        assert_eq!(
            f.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert!(f
            .iter()
            .all(|c| c.x >= c.xmin && c.x <= c.xmax && c.y >= c.ymin && c.y <= c.ymax));
        assert_eq!((f[0].xmax, f[0].ymax), (3987, 2493));
        assert!(!a.is_pending());
    }
    #[test]
    #[ignore = "requires private local HID recordings; see tests/fixtures/README.md"]
    fn truncated_recorded_report_rejected() {
        let d = recorded_device();
        assert!(d.parse(&d.fields().unwrap(), &[4, 3, 0]).is_err());
    }
}
