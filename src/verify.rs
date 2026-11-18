//! Offline audit of format-2 logs, independent of Windows pointer IDs/order.
use crate::touch::{Action, Point};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Eq)]
struct Event {
    action: Action,
    point: Point,
}
#[derive(Default)]
struct Life {
    events: Vec<Event>,
    raw: Option<u32>,
}
#[derive(Default)]
struct Batch {
    count: usize,
    pointers: Vec<(u32, u32, Event)>,
}
#[derive(Default)]
pub struct Audit {
    pub batches: usize,
    pub injected_points: usize,
    pub injected_lifetimes: usize,
    pub received_lifetimes: usize,
    pub received_points: usize,
    pub received_peak: usize,
    /// False for external targets: their receiver is another application.
    pub receiver_audited: bool,
    pub target_updates: usize,
    pub errors: Vec<String>,
}
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|s| s.strip_prefix(key))
}
fn number(line: &str, key: &str) -> Result<u32, String> {
    field(line, key)
        .ok_or_else(|| format!("missing {key}"))?
        .parse()
        .map_err(|_| format!("invalid {key}"))
}
fn position(line: &str) -> Result<Point, String> {
    Ok(Point {
        x: field(line, "x=")
            .ok_or("missing x")?
            .parse()
            .map_err(|_| "invalid x")?,
        y: field(line, "y=")
            .ok_or("missing y")?
            .parse()
            .map_err(|_| "invalid y")?,
    })
}
/// Target bounds from FIXED_RECT / TARGET* records (exclusive right/bottom).
fn bounds(line: &str) -> Result<(i32, i32, i32, i32), String> {
    let get = |key: &str| -> Result<i32, String> {
        field(line, key)
            .ok_or_else(|| format!("missing {key}"))?
            .parse()
            .map_err(|_| format!("invalid {key}"))
    };
    Ok((get("left=")?, get("top=")?, get("right=")?, get("bottom=")?))
}
fn compatible(expected: &Life, actual: &Life) -> bool {
    if expected.events.first() != actual.events.first()
        || expected.events.last() != actual.events.last()
    {
        return false;
    }
    let mut cursor = 0;
    for event in &actual.events {
        while cursor < expected.events.len() && expected.events[cursor] != *event {
            cursor += 1;
        }
        if cursor == expected.events.len() {
            return false;
        }
        cursor += 1;
    }
    true
}
fn augment(
    actual: usize,
    candidates: &[Vec<usize>],
    owners: &mut [Option<usize>],
    seen: &mut BTreeSet<usize>,
) -> bool {
    for &expected in &candidates[actual] {
        if seen.insert(expected)
            && (owners[expected].is_none()
                || augment(owners[expected].unwrap(), candidates, owners, seen))
        {
            owners[expected] = Some(actual);
            return true;
        }
    }
    false
}
pub fn analyze(text: &str) -> Result<Audit, String> {
    if !text
        .lines()
        .any(|l| l.contains(" RUN ") && l.contains("format=2"))
    {
        return Err(
            "format-2 Pad2Touch log required; legacy Phase 0 logs need the historical analyzer"
                .into(),
        );
    }
    let header = text
        .lines()
        .find(|l| l.contains(" RUN "))
        .ok_or("missing RUN header")?;
    let capacity = number(header, "capacity=")? as usize;
    if !(1..=5).contains(&capacity) {
        return Err("invalid log capacity".into());
    }
    let mut audit = Audit {
        receiver_audited: true,
        ..Default::default()
    };
    let mut target: Option<(i32, i32, i32, i32)> = None;
    let mut batches = BTreeMap::<u32, Batch>::new();
    let mut received = Vec::<Life>::new();
    let mut receiver_active = BTreeMap::<u32, usize>::new();
    let mut ended = false;
    for (line_index, line) in text.lines().enumerate() {
        let target_record = line.contains(" TARGET ")
            || line.contains(" TARGET_UPDATE ")
            || line.contains(" TARGET_RESUME ");
        if line.contains(" FIXED_RECT ") || target_record {
            let b = bounds(line)?;
            target = (b.2 > b.0 && b.3 > b.1).then_some(b);
            if line.contains(" TARGET ") && line.contains("receiver=external") {
                audit.receiver_audited = false;
            }
            if line.contains(" TARGET_UPDATE ") || line.contains(" TARGET_RESUME ") {
                audit.target_updates += 1;
            }
        }
        if line.contains(" INJECT_BATCH ") {
            let id = number(line, "batch=")?;
            if batches
                .insert(
                    id,
                    Batch {
                        count: number(line, "count=")? as usize,
                        ..Default::default()
                    },
                )
                .is_some()
            {
                audit.errors.push("duplicate batch ID".into());
            }
        } else if line.contains(" INJECT_OK ") {
            let flags = u32::from_str_radix(field(line, "flags=0x").ok_or("missing flags")?, 16)
                .map_err(|_| "invalid flags")?;
            let action = if flags & 0x10000 != 0 {
                Action::Down
            } else if flags & 0x40000 != 0 {
                Action::Up
            } else if flags & 0x20000 != 0 {
                Action::Update
            } else {
                return Err("unsupported pointer flags".into());
            };
            let batch = number(line, "batch=")?;
            let Some(b) = batches.get_mut(&batch) else {
                return Err("pointer without batch header".into());
            };
            let point = position(line)?;
            if let Some((left, top, right, bottom)) = target {
                if point.x < left || point.x >= right || point.y < top || point.y >= bottom {
                    audit.errors.push(format!(
                        "injected point outside current target at line {}",
                        line_index + 1
                    ));
                }
            }
            b.pointers.push((
                number(line, "slot=")?,
                number(line, "raw_id=")?,
                Event { action, point },
            ));
        } else if line.contains(" RECEIVE_PT_TOUCH ") {
            let id = number(line, "id=")?;
            let action = if line.contains("RECEIVE_PT_TOUCH DOWN ") {
                Action::Down
            } else if line.contains("RECEIVE_PT_TOUCH UP ") {
                Action::Up
            } else {
                Action::Update
            };
            if action == Action::Down {
                if receiver_active.contains_key(&id) {
                    audit.errors.push("receiver duplicate DOWN".into());
                }
                receiver_active.insert(id, received.len());
                received.push(Life::default());
            }
            if let Some(&index) = receiver_active.get(&id) {
                received[index].events.push(Event {
                    action,
                    point: position(line)?,
                });
            } else {
                audit.errors.push("receiver event without DOWN".into());
            }
            if action == Action::Up {
                receiver_active.remove(&id);
            }
            if !line.contains("touch_info=true") {
                audit
                    .errors
                    .push("GetPointerTouchInfo did not succeed".into());
            }
            audit.received_peak = audit.received_peak.max(receiver_active.len());
            audit.received_points += 1;
        } else if line.contains(" INJECT_ERROR ") && !line.contains("code=21 ") {
            audit
                .errors
                .push(format!("native injection error at line {}", line_index + 1));
        } else if line.contains(" ERROR ")
            || line.contains("RELEASE_ERROR")
            || line.contains("WORKER_ERROR")
            || line.contains("GESTURE_GUARD_DISABLE_ERROR")
            || line.contains("GESTURE_HANDLER_ERROR")
            || line.contains("RENDER_ERROR")
        {
            audit
                .errors
                .push(format!("runtime error at line {}", line_index + 1));
        }
        if line.contains(" WORKER_STOP ") {
            ended = true;
            if !line.contains("active_contacts=0 cleanup_pending=false") {
                audit
                    .errors
                    .push("worker retained unresolved contacts".into());
            }
        }
        if line.contains(" SUMMARY ") {
            if field(line, "run_status=") == Some("COMPLETED_WITH_ERRORS") {
                audit.errors.push("run summary reports errors".into());
            }
            if field(line, "log_dropped=") != Some("0") {
                audit
                    .errors
                    .push("missing or nonzero log loss count".into());
            }
            for key in [
                "parse_errors=",
                "incomplete_frames=",
                "injection_errors=",
                "receiver_lifecycle_errors=",
            ] {
                if field(line, key) != Some("0") {
                    audit.errors.push(format!("missing or nonzero {key}"));
                }
            }
        }
        if audit.errors.len() > 100 {
            return Err("too many audit errors; inspect local log".into());
        }
    }
    let mut expected = Vec::<Life>::new();
    let mut active = BTreeMap::<u32, usize>::new();
    let mut updated = BTreeSet::new();
    for (id, batch) in &batches {
        audit.batches += 1;
        if batch.count != batch.pointers.len() || batch.count == 0 || batch.count > capacity {
            audit
                .errors
                .push(format!("invalid batch {id} contact count"));
        }
        let slots: BTreeSet<_> = batch.pointers.iter().map(|p| p.0).collect();
        if slots.len() != batch.pointers.len() || !active.keys().all(|s| slots.contains(s)) {
            audit
                .errors
                .push(format!("batch {id} omitted or duplicated slots"));
        }
        for (slot, raw, event) in &batch.pointers {
            if *slot >= capacity as u32 {
                audit
                    .errors
                    .push("injection slot outside configured capacity".into());
            }
            audit.injected_points += 1;
            if event.action == Action::Down {
                if active.contains_key(slot) {
                    audit.errors.push("injected duplicate DOWN".into());
                }
                active.insert(*slot, expected.len());
                expected.push(Life {
                    raw: Some(*raw),
                    ..Default::default()
                });
                updated.remove(slot);
            }
            if let Some(&index) = active.get(slot) {
                let life = &mut expected[index];
                if life.raw != Some(*raw) {
                    audit
                        .errors
                        .push("raw ID changed during a pointer lifetime".into());
                }
                if event.action == Action::Up
                    && (!updated.contains(slot)
                        || life.events.last().is_none_or(|e| e.point != event.point))
                {
                    audit
                        .errors
                        .push("UP differs from last successful UPDATE".into());
                }
                life.events.push(event.clone());
            } else {
                audit.errors.push("injected event without DOWN".into());
            }
            if event.action == Action::Update {
                updated.insert(*slot);
            }
            if event.action == Action::Up {
                active.remove(slot);
                updated.remove(slot);
            }
        }
    }
    if !ended {
        audit.errors.push("missing worker shutdown record".into());
    }
    if !text.lines().any(|l| l.contains(" SUMMARY ")) {
        audit.errors.push("missing final summary".into());
    }
    if !active.is_empty() || !receiver_active.is_empty() {
        audit.errors.push("unfinished pointer lifetimes".into());
    }
    audit.injected_lifetimes = expected.len();
    audit.received_lifetimes = received.len();
    if !audit.receiver_audited {
        // External applications receive the touches; only injection-side
        // consistency and target bounds are auditable from this log.
        if expected.is_empty() {
            audit.errors.push("no complete injected lifetimes".into());
        }
        return Ok(audit);
    }
    if expected.is_empty() || received.len() != expected.len() {
        audit
            .errors
            .push("missing or unequal complete injected/received lifetimes".into());
    }
    // OS pointer IDs differ from injection slots. Match complete lifetimes by
    // ordered coordinates, allowing UPDATE coalescing and inter-pointer reorder.
    let candidates: Vec<Vec<_>> = received
        .iter()
        .map(|r| {
            expected
                .iter()
                .enumerate()
                .filter_map(|(i, e)| compatible(e, r).then_some(i))
                .collect()
        })
        .collect();
    let mut owners = vec![None; expected.len()];
    for actual in 0..received.len() {
        if !augment(actual, &candidates, &mut owners, &mut BTreeSet::new()) {
            audit
                .errors
                .push("receiver lifetime lacks an ordered injection match".into());
            break;
        }
    }
    Ok(audit)
}
impl Audit {
    pub fn passed(&self) -> bool {
        self.errors.is_empty()
    }
    pub fn json(&self) -> String {
        let errors = self
            .errors
            .iter()
            .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\n  \"passed\": {},\n  \"injected_batches\": {},\n  \"injected_pointer_events\": {},\n  \"injected_lifetimes\": {},\n  \"received_lifetimes\": {},\n  \"received_pointer_events\": {},\n  \"receiver_peak_contacts\": {},\n  \"receiver_audited\": {},\n  \"target_updates\": {},\n  \"errors\": [{}],\n  \"validation\": \"Log consistency only; hardware acceptance remains manual.\"\n}}",self.passed(),self.batches,self.injected_points,self.injected_lifetimes,self.received_lifetimes,self.received_points,self.received_peak,self.receiver_audited,self.target_updates,errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn log() -> String {
        let mut out = "t_us=0 RUN format=2 capacity=2\n".to_string();
        for (batch, flag, x) in [
            (1, "10006", 10),
            (2, "20006", 20),
            (3, "20006", 30),
            (4, "40000", 30),
        ] {
            out += &format!("t_us=1 INJECT_BATCH batch={batch} count=2\n");
            for slot in 0..2 {
                out+=&format!("t_us=1 INJECT_OK flags=0x{flag} x={x} y={slot} api_us=1 batch={batch} slot={slot} raw_id={}\n",slot+50);
            }
        }
        for (kind, x) in [("DOWN", 10), ("UPDATE", 30), ("UP", 30)] {
            for id in [2, 1] {
                out += &format!(
                    "t_us=1 RECEIVE_PT_TOUCH {kind} id={id} frame=1 x={x} y={} touch_info=true\n",
                    id - 1
                );
            }
        }
        out+"t_us=2 WORKER_STOP active_contacts=0 cleanup_pending=false\nt_us=3 SUMMARY log_dropped=0 parse_errors=0 incomplete_frames=0 injection_errors=0 receiver_lifecycle_errors=0\n"
    }
    #[test]
    fn pointer_reordering_and_coalescing_match() {
        let a = analyze(&log()).unwrap();
        assert!(a.passed(), "{:?}", a.errors);
        assert_eq!(a.received_peak, 2);
        assert_eq!(a.batches, 4);
    }
    fn external(log: &str, rect: &str) -> String {
        log.lines()
            .filter(|l| !l.contains("RECEIVE_PT_TOUCH"))
            .map(|l| {
                if l.contains(" RUN ") {
                    format!("{l}\nt_us=0 TARGET mode=window window=0x1234 receiver=external {rect}")
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn external_target_audits_injection_without_embedded_receiver() {
        let a = analyze(&external(&log(), "left=0 top=0 right=100 bottom=10")).unwrap();
        assert!(a.passed(), "{:?}", a.errors);
        assert!(!a.receiver_audited);
        assert!(a.json().contains("\"receiver_audited\": false"));
        // Points beyond the target rectangle are reported.
        let a = analyze(&external(&log(), "left=0 top=0 right=25 bottom=10")).unwrap();
        assert!(a
            .errors
            .iter()
            .any(|e| e.starts_with("injected point outside current target")));
        // No injected lifetime is still not a successful external audit.
        let empty: String = external(&log(), "left=0 top=0 right=100 bottom=10")
            .lines()
            .filter(|l| !l.contains("INJECT_"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!analyze(&empty).unwrap().passed());
    }
    #[test]
    fn target_updates_change_the_audited_bounds() {
        let mut l = external(&log(), "left=0 top=0 right=15 bottom=10");
        // Batches 2-4 (x=20/30) follow a retarget to a wider rectangle.
        l = l.replacen(
            "t_us=1 INJECT_BATCH batch=2",
            "t_us=1 TARGET_UPDATE mode=window window=0x1234 receiver=external left=15 top=0 right=40 bottom=10\nt_us=1 INJECT_BATCH batch=2",
            1,
        );
        let a = analyze(&l).unwrap();
        assert!(a.passed(), "{:?}", a.errors);
        assert_eq!(a.target_updates, 1);
        // Embedded-receiver logs with a FIXED_RECT are bounds-checked too.
        let fixed = log().replacen(
            "capacity=2\n",
            "capacity=2\nt_us=0 FIXED_RECT left=0 top=0 right=12 bottom=10\n",
            1,
        );
        assert!(!analyze(&fixed).unwrap().passed());
    }
    #[test]
    fn absent_receiver_is_not_success() {
        let l = log()
            .lines()
            .filter(|l| !l.contains("RECEIVE_PT_TOUCH"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!analyze(&l).unwrap().passed());
    }
    #[test]
    fn wrong_coordinates_fail() {
        assert!(!analyze(&log().replace(
            "RECEIVE_PT_TOUCH UPDATE id=2 frame=1 x=30",
            "RECEIVE_PT_TOUCH UPDATE id=2 frame=1 x=999"
        ))
        .unwrap()
        .passed());
    }
    #[test]
    fn missing_up_or_summary_fail() {
        for needle in ["RECEIVE_PT_TOUCH UP", "SUMMARY"] {
            let l = log()
                .lines()
                .filter(|l| !l.contains(needle))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!analyze(&l).unwrap().passed());
        }
    }
    #[test]
    fn malformed_or_legacy_logs_rejected() {
        assert!(analyze("Phase 0 legacy").is_err());
        assert!(analyze("t_us=0 RUN format=2\nt_us=1 INJECT_OK flags=oops").is_err());
    }
    #[test]
    fn omitted_active_slots_and_bad_up_fail() {
        let l = log().replace("slot=1 raw_id=51", "slot=0 raw_id=51");
        assert!(!analyze(&l).unwrap().passed());
        let l = log().replace("flags=0x40000 x=30", "flags=0x40000 x=40");
        assert!(!analyze(&l).unwrap().passed());
    }
    #[test]
    fn logged_losses_fail() {
        assert!(!analyze(&log().replace("log_dropped=0", "log_dropped=1"))
            .unwrap()
            .passed());
    }
    #[test]
    fn gesture_handler_errors_fail_even_with_balanced_touch_delivery() {
        let l = log() + "t_us=4 GESTURE_HANDLER_ERROR pointer event Handled readback was false\n";
        assert!(!analyze(&l).unwrap().passed());
    }
    #[test]
    fn runtime_stop_in_summary_is_not_a_clean_audit() {
        let l = log().replace(
            "SUMMARY log_dropped=0",
            "SUMMARY run_status=COMPLETED_WITH_ERRORS log_dropped=0",
        );
        assert!(!analyze(&l).unwrap().passed());
    }
}
