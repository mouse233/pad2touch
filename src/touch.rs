//! Transactional contact planning and injection orchestration. No Windows APIs.
use crate::decode::{map_axis, Contact};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}
#[derive(Clone, Copy, Debug)]
pub struct Rectangle {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
impl Rectangle {
    pub fn validate(self) -> Result<Self, String> {
        if self.right.checked_sub(self.left).is_none_or(|n| n <= 0)
            || self.bottom.checked_sub(self.top).is_none_or(|n| n <= 0)
        {
            return Err("invalid fixed rectangle".into());
        }
        Ok(self)
    }
    fn map(self, c: &Contact) -> Result<Point, String> {
        Ok(Point {
            x: map_axis(c.x, c.xmin, c.xmax, self.left, self.right - self.left)?,
            y: map_axis(c.y, c.ymin, c.ymax, self.top, self.bottom - self.top)?,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Down,
    Update,
    Up,
    Cancel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    pub raw_id: u32,
    pub slot: u32,
    pub point: Point,
    pub action: Action,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub pointers: Vec<Pointer>,
    pub cleanup: bool,
}
#[derive(Clone, Debug)]
struct Active {
    slot: u32,
    point: Point,
    updated: bool,
}
type Contacts = BTreeMap<u32, Active>;
struct Submission {
    frame: Frame,
    after: Contacts,
}

pub struct Planner {
    limit: usize,
    rect: Rectangle,
    active: Contacts,
    armed: bool,
}
impl Planner {
    pub fn new(limit: usize, rect: Rectangle) -> Result<Self, String> {
        if !(1..=5).contains(&limit) {
            return Err("contact capacity must be 1..=5".into());
        }
        Ok(Self {
            limit,
            rect: rect.validate()?,
            active: Contacts::new(),
            armed: false,
        })
    }
    pub fn active_count(&self) -> usize {
        self.active.len()
    }
    pub fn is_armed(&self) -> bool {
        self.armed
    }
    fn update_frame(active: &Contacts, cleanup: bool) -> Submission {
        let mut after = active.clone();
        for a in after.values_mut() {
            a.updated = true;
        }
        let mut pointers: Vec<_> = active
            .iter()
            .map(|(&id, a)| Pointer {
                raw_id: id,
                slot: a.slot,
                point: a.point,
                action: Action::Update,
            })
            .collect();
        pointers.sort_by_key(|p| p.slot);
        Submission {
            frame: Frame { pointers, cleanup },
            after,
        }
    }
    fn transition(
        &self,
        before: &Contacts,
        desired: &BTreeMap<u32, Point>,
        add_new: bool,
    ) -> Submission {
        let mut after = Contacts::new();
        let mut pointers = Vec::new();
        // A slot remains occupied for the entire frame carrying its UP.
        let mut used: BTreeSet<_> = before.values().map(|a| a.slot).collect();
        for (&id, a) in before {
            let point = desired.get(&id).copied().unwrap_or(a.point);
            let action = if desired.contains_key(&id) {
                Action::Update
            } else {
                Action::Up
            };
            pointers.push(Pointer {
                raw_id: id,
                slot: a.slot,
                point,
                action,
            });
            if action == Action::Update {
                after.insert(
                    id,
                    Active {
                        slot: a.slot,
                        point,
                        updated: true,
                    },
                );
            }
        }
        if add_new {
            for (&id, &point) in desired {
                if !before.contains_key(&id) {
                    let slot = (0..self.limit as u32)
                        .find(|s| !used.contains(s))
                        .expect("planner reserved enough slots");
                    used.insert(slot);
                    pointers.push(Pointer {
                        raw_id: id,
                        slot,
                        point,
                        action: Action::Down,
                    });
                    after.insert(
                        id,
                        Active {
                            slot,
                            point,
                            updated: false,
                        },
                    );
                }
            }
        }
        pointers.sort_by_key(|p| p.slot);
        Submission {
            frame: Frame {
                pointers,
                cleanup: false,
            },
            after,
        }
    }
    fn plan(&self, input: &[Contact]) -> Result<(Vec<Submission>, bool), String> {
        let mut ids = BTreeSet::new();
        if input.iter().any(|c| !ids.insert(c.id)) {
            return Err("duplicate raw Contact ID".into());
        }
        let physically_empty = !input.iter().any(|c| c.tip);
        if !self.armed && self.active.is_empty() {
            return Ok((vec![], physically_empty));
        }
        let live: Vec<_> = input.iter().filter(|c| c.tip && c.confidence).collect();
        if self.limit > 1 && live.len() > self.limit {
            return Err(format!(
                "{} confident contacts exceed capacity {}",
                live.len(),
                self.limit
            ));
        }
        let selected = if self.limit == 1 {
            let chosen = if let Some((&id, _)) = self.active.first_key_value() {
                live.iter().copied().find(|c| c.id == id)
            } else {
                live.iter().copied().min_by_key(|c| c.id)
            };
            chosen.into_iter().collect::<Vec<_>>()
        } else {
            live
        };
        let mut desired = BTreeMap::new();
        for c in selected {
            desired.insert(c.id, self.rect.map(c)?);
        }
        let mut state = self.active.clone();
        let mut plan = Vec::new();
        // Even an immediate tap has an UPDATE before its UP, at cached position.
        if state
            .iter()
            .any(|(id, a)| !desired.contains_key(id) && !a.updated)
        {
            let prelude = Self::update_frame(&state, false);
            state = prelude.after.clone();
            plan.push(prelude);
        }
        let incoming = desired.keys().filter(|id| !state.contains_key(id)).count();
        if incoming > self.limit - state.len() {
            // Capacity-constrained replacement: release old slots successfully
            // before reusing them. Every frame still includes all existing slots.
            let release = self.transition(&state, &desired, false);
            state = release.after.clone();
            plan.push(release);
        }
        if !state.is_empty() || !desired.is_empty() {
            plan.push(self.transition(&state, &desired, true));
        }
        let single_lost =
            self.limit == 1 && !self.active.is_empty() && desired.is_empty() && !physically_empty;
        Ok((plan, physically_empty || (self.armed && !single_lost)))
    }
    fn cancellation(&self) -> Vec<Submission> {
        if self.active.is_empty() {
            return vec![];
        }
        let update = Self::update_frame(&self.active, true);
        let pointers = update
            .frame
            .pointers
            .iter()
            .copied()
            .map(|p| Pointer {
                action: Action::Cancel,
                ..p
            })
            .collect();
        vec![
            update,
            Submission {
                frame: Frame {
                    pointers,
                    cleanup: true,
                },
                after: Contacts::new(),
            },
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Win32(u32),
    TargetUnavailable,
}
impl Failure {
    fn not_ready(&self) -> bool {
        *self == Self::Win32(21)
    }
    fn cancels_all(&self) -> bool {
        *self == Self::Win32(87)
    }
}
pub trait Transport {
    fn inject(&mut self, frame: &Frame) -> Result<(), Failure>;
    fn retry_delay(&mut self) {}
}
pub struct Engine<T: Transport> {
    planner: Planner,
    pub transport: T,
    pub successes: u64,
    pub failures: u64,
    pub retries: u64,
    pub durations: Vec<u128>,
    pub cleanup_pending: bool,
}
impl<T: Transport> Engine<T> {
    pub fn new(limit: usize, rect: Rectangle, transport: T) -> Result<Self, String> {
        Ok(Self {
            planner: Planner::new(limit, rect)?,
            transport,
            successes: 0,
            failures: 0,
            retries: 0,
            durations: Vec::new(),
            cleanup_pending: false,
        })
    }
    pub fn active_count(&self) -> usize {
        self.planner.active_count()
    }
    pub fn is_armed(&self) -> bool {
        self.planner.is_armed()
    }
    fn send(&mut self, submission: Submission) -> Result<(), Failure> {
        let start = Instant::now();
        for attempt in 0..20 {
            match self.transport.inject(&submission.frame) {
                Ok(()) => {
                    self.planner.active = submission.after;
                    self.successes += 1;
                    if self.durations.len() < 100_000 {
                        self.durations.push(start.elapsed().as_micros());
                    }
                    return Ok(());
                }
                Err(e) if e.not_ready() && attempt < 19 => {
                    self.retries += 1;
                    self.transport.retry_delay();
                }
                Err(e) => {
                    self.failures += 1;
                    self.planner.armed = false;
                    if e.cancels_all() {
                        self.planner.active.clear();
                        self.cleanup_pending = false;
                    }
                    return Err(e);
                }
            }
        }
        unreachable!()
    }
    pub fn cancel(&mut self) -> Result<(), String> {
        self.planner.armed = false;
        for frame in self.planner.cancellation() {
            if let Err(e) = self.send(frame) {
                self.cleanup_pending = self.active_count() > 0;
                return Err(format!(
                    "touch cancellation failed: {e:?}; unresolved_contacts={}",
                    self.active_count()
                ));
            }
        }
        self.cleanup_pending = false;
        Ok(())
    }
    /// Move the mapping rectangle. Active contacts are cancelled at their cached
    /// positions first, and a new all-fingers-up frame is required afterward,
    /// so no lifetime spans two different mappings.
    pub fn retarget(&mut self, rect: Rectangle) -> Result<(), String> {
        let rect = rect.validate()?;
        let released = self.cancel();
        self.planner.rect = rect;
        released
    }
    fn fail(&mut self, cause: String) -> String {
        match self.cancel() {
            Ok(()) => cause,
            Err(cleanup) => format!("{cause}; {cleanup}"),
        }
    }
    pub fn apply(&mut self, input: &[Contact]) -> Result<(), String> {
        if self.cleanup_pending {
            self.cancel()?;
            return Err("cleanup recovered; waiting for a new empty physical frame".into());
        }
        let (plan, armed) = self.planner.plan(input).map_err(|e| self.fail(e))?;
        for frame in plan {
            if let Err(e) = self.send(frame) {
                let msg = self.fail(format!("touch frame failed: {e:?}"));
                return Err(msg);
            }
        }
        self.planner.armed = armed;
        Ok(())
    }
    pub fn keepalive(&mut self) -> Result<(), String> {
        if self.cleanup_pending {
            return self.cancel();
        }
        if self.active_count() > 0 {
            let frame = Planner::update_frame(&self.planner.active, false);
            if let Err(e) = self.send(frame) {
                let msg = self.fail(format!("touch keepalive failed: {e:?}"));
                return Err(msg);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[derive(Default)]
    pub struct Fake {
        pub attempts: Vec<Frame>,
        pub accepted: Vec<Frame>,
        pub errors: VecDeque<Option<Failure>>,
    }
    impl Transport for Fake {
        fn inject(&mut self, f: &Frame) -> Result<(), Failure> {
            self.attempts.push(f.clone());
            if let Some(Some(e)) = self.errors.pop_front() {
                Err(e)
            } else {
                self.accepted.push(f.clone());
                Ok(())
            }
        }
    }
    pub fn contact(id: u32, x: i32, y: i32) -> Contact {
        Contact {
            id,
            tip: true,
            confidence: true,
            x,
            y,
            xmin: 0,
            xmax: 1000,
            ymin: 0,
            ymax: 1000,
        }
    }
    pub fn engine(limit: usize) -> Engine<Fake> {
        Engine::new(
            limit,
            Rectangle {
                left: 100,
                top: 200,
                right: 1101,
                bottom: 1201,
            },
            Fake::default(),
        )
        .unwrap()
    }
    fn armed(limit: usize) -> Engine<Fake> {
        let mut e = engine(limit);
        e.apply(&[]).unwrap();
        e
    }
    fn ids(f: &Frame) -> Vec<(u32, u32, Action)> {
        f.pointers
            .iter()
            .map(|p| (p.raw_id, p.slot, p.action))
            .collect()
    }
    #[test]
    fn startup_waits_for_all_physical_tips_even_low_confidence() {
        let mut e = engine(5);
        let mut palm = contact(8, 0, 0);
        palm.confidence = false;
        e.apply(&[palm]).unwrap();
        assert!(!e.is_armed());
        e.apply(&[]).unwrap();
        assert!(e.is_armed());
        e.apply(&[contact(9, 0, 0)]).unwrap();
        assert_eq!(e.active_count(), 1);
    }
    #[test]
    fn five_downs_are_one_atomic_frame() {
        let mut e = armed(5);
        e.apply(
            &(0..5)
                .map(|id| contact(id * 100, 100, 200))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(e.transport.accepted.len(), 1);
        assert_eq!(e.transport.accepted[0].pointers.len(), 5);
        assert!(e.transport.accepted[0]
            .pointers
            .iter()
            .all(|p| p.action == Action::Down && p.slot < 5));
    }
    #[test]
    fn reorder_and_raw_ids_do_not_change_slots() {
        let mut e = armed(5);
        e.apply(&[contact(900, 0, 0), contact(7, 500, 500)])
            .unwrap();
        e.apply(&[contact(7, 600, 600), contact(900, 100, 100)])
            .unwrap();
        assert_eq!(
            ids(&e.transport.accepted[1]),
            vec![(7, 0, Action::Update), (900, 1, Action::Update)]
        );
    }
    #[test]
    fn partial_release_keeps_other_fingers_in_same_frame() {
        let mut e = armed(5);
        e.apply(&[contact(1, 0, 0), contact(2, 500, 500)]).unwrap();
        e.keepalive().unwrap();
        e.apply(&[contact(2, 600, 600)]).unwrap();
        let f = e.transport.accepted.last().unwrap();
        assert_eq!(ids(f), vec![(1, 0, Action::Up), (2, 1, Action::Update)]);
        assert_eq!(f.pointers[0].point, Point { x: 100, y: 200 });
        assert_eq!(e.active_count(), 1);
    }
    #[test]
    fn immediate_up_uses_cached_down_coordinate_after_update() {
        let mut e = armed(1);
        e.apply(&[contact(3, 100, 200)]).unwrap();
        let mut lift = contact(3, 999, 999);
        lift.tip = false;
        e.apply(&[lift]).unwrap();
        assert_eq!(e.transport.accepted.len(), 3);
        let frames = &e.transport.accepted;
        assert_eq!(frames[1].pointers[0].action, Action::Update);
        assert_eq!(frames[2].pointers[0].action, Action::Up);
        assert_eq!(frames[0].pointers[0].point, frames[2].pointers[0].point);
        assert_eq!(e.active_count(), 0);
    }
    #[test]
    fn all_five_release_together() {
        let mut e = armed(5);
        e.apply(&(0..5).map(|id| contact(id, 0, 0)).collect::<Vec<_>>())
            .unwrap();
        e.apply(&[]).unwrap();
        assert_eq!(e.transport.accepted.last().unwrap().pointers.len(), 5);
        assert!(e
            .transport
            .accepted
            .last()
            .unwrap()
            .pointers
            .iter()
            .all(|p| p.action == Action::Up));
        assert!(e.is_armed());
    }
    #[test]
    fn mixed_change_uses_one_frame_when_slots_are_available() {
        let mut e = armed(5);
        e.apply(&[contact(1, 0, 0), contact(2, 100, 100)]).unwrap();
        e.keepalive().unwrap();
        let n = e.successes;
        e.apply(&[contact(2, 200, 200), contact(3, 300, 300)])
            .unwrap();
        assert_eq!(e.successes, n + 1);
        assert_eq!(
            ids(e.transport.accepted.last().unwrap()),
            vec![
                (1, 0, Action::Up),
                (2, 1, Action::Update),
                (3, 2, Action::Down)
            ]
        );
    }
    #[test]
    fn full_capacity_replacement_releases_before_slot_reuse() {
        let mut e = armed(2);
        e.apply(&[contact(1, 0, 0), contact(2, 100, 100)]).unwrap();
        e.keepalive().unwrap();
        let n = e.successes;
        e.apply(&[contact(2, 200, 200), contact(3, 300, 300)])
            .unwrap();
        assert_eq!(e.successes, n + 2);
        let f = &e.transport.accepted;
        assert_eq!(
            ids(&f[f.len() - 2]),
            vec![(1, 0, Action::Up), (2, 1, Action::Update)]
        );
        assert_eq!(
            ids(f.last().unwrap()),
            vec![(3, 0, Action::Down), (2, 1, Action::Update)]
        );
    }
    #[test]
    fn single_mode_never_jumps_to_another_finger() {
        let mut e = armed(1);
        e.apply(&[contact(10, 0, 0), contact(20, 100, 100)])
            .unwrap();
        e.apply(&[contact(20, 200, 200)]).unwrap();
        assert_eq!(e.active_count(), 0);
        assert!(!e.is_armed());
        let n = e.successes;
        e.apply(&[contact(20, 300, 300)]).unwrap();
        assert_eq!(e.successes, n);
        e.apply(&[]).unwrap();
        e.apply(&[contact(20, 300, 300)]).unwrap();
        assert_eq!(e.active_count(), 1);
    }
    #[test]
    fn low_confidence_releases_only_that_contact() {
        let mut e = armed(5);
        let a = contact(1, 0, 0);
        let mut b = contact(2, 10, 10);
        e.apply(&[a.clone(), b.clone()]).unwrap();
        b.confidence = false;
        e.apply(&[a, b]).unwrap();
        assert_eq!(e.active_count(), 1);
    }
    #[test]
    fn overflow_and_duplicate_input_cancel_and_require_clear() {
        for input in [
            vec![contact(7, 0, 0); 2],
            (0..6).map(|id| contact(id, 0, 0)).collect(),
        ] {
            let mut e = armed(5);
            e.apply(&[contact(99, 0, 0)]).unwrap();
            assert!(e.apply(&input).is_err());
            assert_eq!(e.active_count(), 0);
            assert!(!e.is_armed());
        }
    }
    #[test]
    fn not_ready_retries_identical_frame_without_state_loss() {
        let mut e = armed(5);
        e.transport
            .errors
            .extend([Some(Failure::Win32(21)), Some(Failure::Win32(21)), None]);
        e.apply(&[contact(80, 0, 0), contact(90, 100, 100)])
            .unwrap();
        assert_eq!(e.retries, 2);
        assert_eq!(e.successes, 1);
        assert_eq!(e.active_count(), 2);
        assert_eq!(e.transport.attempts[0], e.transport.attempts[1]);
        assert_eq!(e.transport.attempts[1], e.transport.attempts[2]);
    }
    #[test]
    fn invalid_parameter_resets_system_cancelled_contacts() {
        let mut e = armed(5);
        e.apply(&[contact(1, 0, 0)]).unwrap();
        e.transport.errors.push_back(Some(Failure::Win32(87)));
        assert!(e.apply(&[contact(1, 100, 100)]).is_err());
        assert_eq!(e.active_count(), 0);
        assert!(!e.is_armed());
        e.apply(&[]).unwrap();
        e.apply(&[contact(2, 0, 0)]).unwrap();
        assert_eq!(e.active_count(), 1);
    }
    #[test]
    fn failed_cancel_retains_contacts_and_blocks_new_downs() {
        let mut e = armed(5);
        e.apply(&[contact(1, 0, 0)]).unwrap();
        e.transport
            .errors
            .extend([Some(Failure::Win32(5)), Some(Failure::Win32(5))]);
        assert!(e.apply(&[contact(2, 0, 0)]).is_err());
        assert!(e.cleanup_pending);
        assert_eq!(e.active_count(), 1);
        assert!(e.apply(&[contact(2, 0, 0)]).is_err());
        assert_eq!(e.active_count(), 0);
        assert!(!e.is_armed());
        assert!(!e
            .transport
            .accepted
            .iter()
            .any(|f| f.pointers.iter().any(|p| p.raw_id == 2)));
    }
    #[test]
    fn exhausted_not_ready_cannot_drop_a_down_or_advance_slots() {
        let mut e = armed(5);
        e.transport
            .errors
            .extend((0..20).map(|_| Some(Failure::Win32(21))));
        assert!(e.apply(&[contact(1, 0, 0)]).is_err());
        assert_eq!(e.retries, 19);
        assert_eq!(e.active_count(), 0);
        assert!(!e.is_armed());
    }
    #[test]
    fn failed_second_replacement_frame_cleans_committed_survivor() {
        let mut e = armed(2);
        e.apply(&[contact(1, 0, 0), contact(2, 100, 100)]).unwrap();
        e.keepalive().unwrap();
        e.transport.errors.extend([None, Some(Failure::Win32(5))]);
        assert!(e
            .apply(&[contact(2, 300, 300), contact(3, 400, 400)])
            .is_err());
        assert_eq!(e.active_count(), 0);
        assert!(!e
            .transport
            .accepted
            .iter()
            .any(|f| f.pointers.iter().any(|p| p.raw_id == 3)));
    }
    #[test]
    fn cancel_marks_all_contacts_and_keepalive_preserves_ids() {
        let mut e = armed(5);
        e.apply(&[contact(2, 0, 0), contact(3, 500, 500)]).unwrap();
        e.keepalive().unwrap();
        e.cancel().unwrap();
        let last = e.transport.accepted.last().unwrap();
        assert!(last.cleanup);
        assert!(last.pointers.iter().all(|p| p.action == Action::Cancel));
        assert_eq!(e.active_count(), 0);
        assert!(!e.is_armed());
    }
    #[test]
    fn stationary_long_hold_keeps_full_frames() {
        let mut e = armed(5);
        e.apply(&(0..5).map(|id| contact(id, 100, 100)).collect::<Vec<_>>())
            .unwrap();
        for _ in 0..1000 {
            e.keepalive().unwrap();
        }
        assert_eq!(e.successes, 1001);
        assert!(e.transport.accepted.iter().all(|f| f.pointers.len() == 5));
        e.apply(&[]).unwrap();
        assert_eq!(e.active_count(), 0);
    }
    #[test]
    fn thousands_of_taps_leave_no_contacts() {
        let mut e = armed(1);
        for _ in 0..2000 {
            e.apply(&[contact(30, 500, 500)]).unwrap();
            e.apply(&[]).unwrap();
        }
        assert_eq!(e.active_count(), 0);
        assert_eq!(e.successes, 6000);
    }
    #[test]
    fn invalid_rect_capacity_and_range_rejected() {
        assert!(Engine::new(
            0,
            Rectangle {
                left: 0,
                top: 0,
                right: 100,
                bottom: 100
            },
            Fake::default()
        )
        .is_err());
        assert!(Engine::new(
            5,
            Rectangle {
                left: i32::MIN,
                top: 0,
                right: i32::MAX,
                bottom: 1
            },
            Fake::default()
        )
        .is_err());
        let mut e = armed(5);
        let mut c = contact(1, 0, 0);
        c.xmax = 0;
        assert!(e.apply(&[c]).is_err());
    }
    #[test]
    fn generated_frames_obey_independent_protocol_model() {
        struct Model {
            limit: usize,
            state: BTreeMap<u32, (u32, Point, bool)>,
        }
        impl Transport for Model {
            fn inject(&mut self, f: &Frame) -> Result<(), Failure> {
                assert!(!f.pointers.is_empty());
                assert!(f.pointers.len() <= self.limit);
                let slots: BTreeSet<_> = f.pointers.iter().map(|p| p.slot).collect();
                assert_eq!(slots.len(), f.pointers.len());
                assert!(
                    self.state.keys().all(|slot| slots.contains(slot)),
                    "frame omitted an active pointer"
                );
                let mut next = self.state.clone();
                for p in &f.pointers {
                    assert!(p.slot < self.limit as u32);
                    match p.action {
                        Action::Down => {
                            assert!(!self.state.contains_key(&p.slot));
                            next.insert(p.slot, (p.raw_id, p.point, false));
                        }
                        Action::Update => {
                            let previous = self.state.get(&p.slot).expect("UPDATE without DOWN");
                            assert_eq!(previous.0, p.raw_id);
                            next.insert(p.slot, (p.raw_id, p.point, true));
                        }
                        Action::Up | Action::Cancel => {
                            let previous = self.state.get(&p.slot).expect("UP without DOWN");
                            assert_eq!(previous.0, p.raw_id);
                            assert!(previous.2, "UP without previous UPDATE");
                            assert_eq!(previous.1, p.point);
                            next.remove(&p.slot);
                        }
                    }
                }
                self.state = next;
                Ok(())
            }
        }
        for limit in 1..=5 {
            let mut e = Engine::new(
                limit,
                Rectangle {
                    left: -500,
                    top: 100,
                    right: 501,
                    bottom: 1101,
                },
                Model {
                    limit,
                    state: BTreeMap::new(),
                },
            )
            .unwrap();
            e.apply(&[]).unwrap();
            let mut random = 17u64;
            for frame in 0..2000 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let count = (random as usize) % (limit + 1);
                let mut contacts = Vec::new();
                for i in 0..count {
                    let id = ((frame + i * 3) % 13) as u32;
                    contacts.push(contact(
                        id,
                        ((random >> 16) % 1001) as i32,
                        ((random >> 32) % 1001) as i32,
                    ));
                }
                if frame % 2 == 1 {
                    contacts.reverse();
                }
                e.apply(&contacts).unwrap();
                assert_eq!(e.active_count(), e.transport.state.len());
                if frame % 7 == 0 {
                    e.keepalive().unwrap();
                }
            }
            e.cancel().unwrap();
            assert!(e.transport.state.is_empty());
        }
    }
    #[test]
    fn retarget_cancels_at_old_position_and_maps_into_new_rectangle() {
        let mut e = armed(2);
        e.apply(&[contact(7, 1000, 1000)]).unwrap();
        e.apply(&[contact(7, 1000, 1000)]).unwrap();
        let last = e.transport.accepted.last().unwrap().pointers[0].point;
        assert_eq!(last, Point { x: 1100, y: 1200 });
        e.retarget(Rectangle {
            left: -800,
            top: -600,
            right: -399,
            bottom: -399,
        })
        .unwrap();
        let tail = &e.transport.accepted[e.transport.accepted.len() - 2..];
        assert!(tail
            .iter()
            .all(|f| f.cleanup && f.pointers[0].point == last));
        assert_eq!(tail[1].pointers[0].action, Action::Cancel);
        assert_eq!(e.active_count(), 0);
        assert!(!e.is_armed());
        // A finger still down after retargeting must not start a new lifetime.
        let sent = e.transport.accepted.len();
        e.apply(&[contact(7, 0, 0)]).unwrap();
        assert_eq!(e.transport.accepted.len(), sent);
        e.apply(&[]).unwrap();
        e.apply(&[contact(9, 0, 0)]).unwrap();
        e.apply(&[contact(9, 1000, 1000)]).unwrap();
        let points: Vec<_> = e.transport.accepted[sent..]
            .iter()
            .map(|f| f.pointers[0].point)
            .collect();
        assert_eq!(
            points,
            [Point { x: -800, y: -600 }, Point { x: -400, y: -400 }]
        );
        assert!(e
            .retarget(Rectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 5
            })
            .is_err());
    }
    #[test]
    fn retarget_failure_keeps_cleanup_pending_and_new_rectangle() {
        let mut e = armed(1);
        e.apply(&[contact(1, 0, 0)]).unwrap();
        e.transport.errors.push_back(Some(Failure::Win32(5)));
        let r = Rectangle {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        };
        assert!(e.retarget(r).is_err());
        assert!(e.cleanup_pending);
        assert_eq!(e.active_count(), 1);
        // Recovery still ends the old contact before any new DOWN.
        assert!(e.apply(&[]).is_err());
        assert!(!e.cleanup_pending);
        assert_eq!(e.active_count(), 0);
    }
    #[test]
    fn keepalive_and_target_failures_cancel_all() {
        for failure in [Failure::Win32(5), Failure::TargetUnavailable] {
            let mut e = armed(5);
            e.apply(&[contact(7, 0, 0), contact(8, 500, 500)]).unwrap();
            e.transport.errors.push_back(Some(failure));
            assert!(e.keepalive().is_err());
            assert_eq!(e.active_count(), 0);
            assert!(!e.is_armed());
        }
    }
}
