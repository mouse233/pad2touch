//! Platform-independent interpretation of descriptor fields and HID data indices.
//! HidP handles the HID bit layout; this module has no Windows API dependency.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct Field {
    pub report: u8,
    pub index: u16,
    pub page: u16,
    pub usage: u16,
    pub link: u16,
    pub bits: u16,
    pub min: i32,
    pub max: i32,
    pub button: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Contact {
    pub id: u32,
    pub tip: bool,
    pub confidence: bool,
    pub x: i32,
    pub y: i32,
    pub xmin: i32,
    pub xmax: i32,
    pub ymin: i32,
    pub ymax: i32,
}
#[derive(Debug)]
pub struct Report {
    pub count: usize,
    pub scan: Option<u32>,
    pub contacts: Vec<Contact>,
}
fn signed(v: u32, f: &Field) -> i32 {
    if f.min < 0 && f.bits > 0 && f.bits < 32 {
        ((v << (32 - f.bits)) as i32) >> (32 - f.bits)
    } else {
        v as i32
    }
}
pub fn decode(
    fields: &[Field],
    report_id: u8,
    data: &[(u16, u32)],
) -> Result<Option<Report>, String> {
    let fields: Vec<_> = fields.iter().filter(|f| f.report == report_id).collect();
    let values: BTreeMap<_, _> = data.iter().copied().collect();
    let find = |page, usage, link: Option<u16>| {
        fields
            .iter()
            .copied()
            .find(|f| f.page == page && f.usage == usage && link.is_none_or(|l| f.link == l))
    };
    let get = |f: &Field| -> Result<u32, String> {
        values
            .get(&f.index)
            .copied()
            .or(if f.button { Some(0) } else { None })
            .ok_or_else(|| {
                format!(
                    "missing data index {} usage {:X}:{:X}",
                    f.index, f.page, f.usage
                )
            })
    };
    let Some(count_field) = find(0x0D, 0x54, None) else {
        return Ok(None);
    };
    let count = get(count_field)? as usize;
    if count > 32 {
        return Err(format!("unsupported contact count {count}"));
    }
    let scan = find(0x0D, 0x56, None).map(get).transpose()?;
    let links: BTreeSet<_> = fields
        .iter()
        .filter(|f| f.page == 0x0D && f.usage == 0x51)
        .map(|f| f.link)
        .collect();
    if links.is_empty() {
        return Err("no Contact ID fields in report".into());
    }
    let mut contacts = Vec::new();
    for link in links {
        let required = |page, usage| {
            find(page, usage, Some(link)).ok_or_else(|| {
                format!("unsupported contact layout link={link} missing {page:X}:{usage:X}")
            })
        };
        let id = get(required(0x0D, 0x51)?)?;
        let tip = get(required(0x0D, 0x42)?)? != 0;
        let confidence = find(0x0D, 0x47, Some(link))
            .map(get)
            .transpose()?
            .unwrap_or(1)
            != 0;
        let xf = required(1, 0x30)?;
        let yf = required(1, 0x31)?;
        if xf.max <= xf.min || yf.max <= yf.min {
            return Err("invalid coordinate logical range".into());
        }
        let x = signed(get(xf)?, xf);
        let y = signed(get(yf)?, yf);
        if tip && (x < xf.min || x > xf.max || y < yf.min || y > yf.max) {
            return Err("active coordinate outside descriptor range".into());
        }
        contacts.push(Contact {
            id,
            tip,
            confidence,
            x,
            y,
            xmin: xf.min,
            xmax: xf.max,
            ymin: yf.min,
            ymax: yf.max,
        });
    }
    Ok(Some(Report {
        count,
        scan,
        contacts,
    }))
}

/// Assemble both parallel and hybrid PTP frames. Count is the number of
/// contact records (including explicit lifts), not the number of tip=true bits.
#[derive(Default)]
pub struct Assembler {
    pending: Vec<Contact>,
    expected: usize,
    scan: Option<u32>,
}
impl Assembler {
    pub fn push(&mut self, r: Report) -> Result<Option<Vec<Contact>>, String> {
        if r.count > 0 {
            if self.expected != 0 {
                self.reset();
                return Err("new frame before hybrid frame completed".into());
            }
            self.expected = r.count;
            self.scan = r.scan;
        } else if self.expected == 0 {
            if r.contacts.iter().any(|c| c.tip) {
                return Err("orphan hybrid continuation with active tip".into());
            }
            // Idle count=0 may retain stale coordinates, never an active tip.
            return Ok(Some(Vec::new()));
        }
        if r.scan.is_none() && self.expected > r.contacts.len() {
            self.reset();
            return Err("fragmented frame requires Scan Time".into());
        }
        if self.scan != r.scan {
            self.reset();
            return Err("hybrid Scan Time changed before frame completed".into());
        }
        let remaining = self.expected - self.pending.len();
        self.pending.extend(r.contacts.into_iter().take(remaining));
        if self.pending.len() != self.expected {
            return Ok(None);
        }
        let mut ids = BTreeSet::new();
        if self.pending.iter().any(|c| !ids.insert(c.id)) {
            self.reset();
            return Err("duplicate Contact ID in frame".into());
        }
        self.expected = 0;
        Ok(Some(std::mem::take(&mut self.pending)))
    }
    pub fn reset(&mut self) {
        self.pending.clear();
        self.expected = 0;
        self.scan = None;
    }
    pub fn is_pending(&self) -> bool {
        self.expected != 0
    }
}
#[cfg(test)]
pub fn map_axis(raw: i32, min: i32, max: i32, start: i32, length: i32) -> Result<i32, String> {
    if max <= min || length <= 0 {
        return Err("invalid mapping range".into());
    }
    let t = ((raw as f64 - min as f64) / (max as f64 - min as f64)).clamp(0.0, 1.0);
    Ok((start as f64 + t * (length - 1) as f64).round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(id: u32) -> Contact {
        Contact {
            id,
            tip: true,
            confidence: true,
            x: 50,
            y: 100,
            xmin: 0,
            xmax: 100,
            ymin: 0,
            ymax: 200,
        }
    }
    fn r(count: usize, ids: &[u32]) -> Report {
        Report {
            count,
            scan: Some(42),
            contacts: ids.iter().map(|&id| c(id)).collect(),
        }
    }
    #[test]
    fn hybrid_waits_for_all_records() {
        let mut a = Assembler::default();
        assert!(a.push(r(3, &[2])).unwrap().is_none());
        assert!(a.push(r(0, &[9])).unwrap().is_none());
        assert_eq!(
            a.push(r(0, &[7]))
                .unwrap()
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![2, 9, 7]
        );
    }
    #[test]
    fn parallel_ignores_padding_and_preserves_lift() {
        let mut a = Assembler::default();
        let mut f = r(2, &[8, 3, 0, 0, 0]);
        f.contacts[1].tip = false;
        let got = a.push(f).unwrap().unwrap();
        assert_eq!(got.len(), 2);
        assert!(!got[1].tip);
    }
    #[test]
    fn incomplete_and_duplicate_frames_fail_closed() {
        let mut a = Assembler::default();
        a.push(r(2, &[1])).unwrap();
        assert!(a.push(r(1, &[2])).is_err());
        assert!(a.push(r(2, &[3, 3])).is_err());
        assert!(!a.is_pending());
    }
    #[test]
    fn hybrid_mismatched_scan_rejected() {
        let mut a = Assembler::default();
        a.push(r(2, &[1])).unwrap();
        let mut f = r(0, &[2]);
        f.scan = Some(43);
        assert!(a.push(f).is_err());
    }
    #[test]
    fn idle_ignores_stale_record() {
        let mut idle = r(0, &[1]);
        idle.contacts[0].tip = false;
        assert!(Assembler::default().push(idle).unwrap().unwrap().is_empty());
    }
    #[test]
    fn orphan_active_continuation_is_not_an_empty_frame() {
        assert!(Assembler::default().push(r(0, &[1])).is_err());
    }
    #[test]
    fn fragmented_reports_without_scan_time_fail() {
        let mut a = Assembler::default();
        let mut frame = r(2, &[1]);
        frame.scan = None;
        assert!(a.push(frame).is_err());
        assert!(!a.is_pending());
    }
    #[test]
    fn mapping_corners_clamp_and_negative_origin() {
        assert_eq!(map_axis(0, 0, 100, -500, 401).unwrap(), -500);
        assert_eq!(map_axis(100, 0, 100, -500, 401).unwrap(), -100);
        assert_eq!(map_axis(50, 0, 100, -500, 401).unwrap(), -300);
        assert_eq!(map_axis(999, 0, 100, 0, 10).unwrap(), 9);
        assert!(map_axis(0, 1, 1, 0, 10).is_err());
    }
    #[test]
    fn signed_values() {
        let f = Field {
            report: 1,
            index: 0,
            page: 1,
            usage: 0x30,
            link: 1,
            bits: 16,
            min: -100,
            max: 100,
            button: false,
        };
        assert_eq!(signed(65526, &f), -10);
    }
    fn fields() -> Vec<Field> {
        [
            (0x0D, 0x54, 0, false),
            (0x0D, 0x51, 1, false),
            (0x0D, 0x42, 1, true),
            (0x0D, 0x47, 1, true),
            (1, 0x30, 1, false),
            (1, 0x31, 1, false),
        ]
        .iter()
        .enumerate()
        .map(|(i, &(page, usage, link, button))| Field {
            report: 7,
            index: i as u16,
            page,
            usage,
            link,
            bits: 16,
            min: 0,
            max: 1000,
            button,
        })
        .collect()
    }
    #[test]
    fn descriptor_indices_buttons_and_report_id() {
        let f = fields();
        let data = [(0, 1), (1, 23), (2, 1), (4, 500), (5, 800)];
        let p = decode(&f, 7, &data).unwrap().unwrap();
        assert_eq!(p.contacts[0].id, 23);
        assert!(p.contacts[0].tip);
        assert!(!p.contacts[0].confidence);
        assert_eq!(p.contacts[0].x, 500);
        assert!(decode(&f, 8, &data).unwrap().is_none());
        assert!(decode(&f, 7, &[(0, 1)]).is_err());
    }
}
