//! Conservative evidence for background Raw Input, independent of Windows APIs.
use crate::decode::Contact;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    Foreground,
    Background,
    Mixed,
    #[default]
    Unknown,
}
impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
    fn merge(self, other: Self) -> Self {
        if self == other {
            self
        } else if self == Self::Unknown || other == Self::Unknown {
            Self::Unknown
        } else {
            Self::Mixed
        }
    }
}
#[derive(Clone, Default)]
pub struct Counters {
    pub foreground_raw: u64,
    pub background_raw: u64,
    pub unknown_raw: u64,
    pub background_sink_raw: u64,
    pub foreground_frames: u64,
    pub background_frames: u64,
    pub mixed_frames: u64,
    pub unknown_frames: u64,
    pub background_peak: usize,
    pub background_starts: u64,
    pub background_ends: u64,
    pub background_moves: u64,
    pub background_interrupted: u64,
    pub background_active: usize,
    pub background_clear_frames: u64,
}
impl Counters {
    pub fn summary(&self) -> String {
        format!("foreground_raw_reports={}\nbackground_raw_reports={}\nunknown_focus_raw_reports={}\nbackground_RIM_INPUTSINK_reports={}\nforeground_complete_frames={}\nbackground_complete_frames={}\nmixed_focus_frames={}\nunknown_focus_frames={}\nbackground_peak_contacts={}\nbackground_contact_starts={}\nbackground_contact_ends={}\nbackground_coordinate_changes={}\nbackground_contacts_interrupted_by_focus_or_error={}\nbackground_contacts_remaining={}\nbackground_clear_frames={}\nbackground_validation=OBSERVATIONS_ONLY_MANUAL_SCENARIOS_REQUIRED\n", self.foreground_raw,self.background_raw,self.unknown_raw,self.background_sink_raw,self.foreground_frames,self.background_frames,self.mixed_frames,self.unknown_frames,self.background_peak,self.background_starts,self.background_ends,self.background_moves,self.background_interrupted,self.background_active,self.background_clear_frames)
    }
}
#[derive(Default)]
pub struct Evidence {
    pub counts: Counters,
    focus: Scope,
    fragment_scope: Option<Scope>,
    clear_seen: bool,
    active: BTreeMap<u32, (i32, i32)>,
}
impl Evidence {
    pub fn focus(&mut self, scope: Scope) {
        if scope != self.focus {
            self.invalidate();
            self.focus = scope;
        }
    }
    pub fn raw(&mut self, scope: Scope, sink: bool) {
        match scope {
            Scope::Foreground => self.counts.foreground_raw += 1,
            Scope::Background => {
                self.counts.background_raw += 1;
                if sink {
                    self.counts.background_sink_raw += 1;
                }
            }
            _ => self.counts.unknown_raw += 1,
        }
    }
    pub fn fragment(&mut self, scope: Scope) {
        self.fragment_scope = Some(self.fragment_scope.map_or(scope, |old| old.merge(scope)));
    }
    pub fn invalidate(&mut self) {
        self.counts.background_interrupted += self.active.len() as u64;
        self.active.clear();
        self.counts.background_active = 0;
        self.clear_seen = false;
    }
    pub fn reset_fragments(&mut self) {
        self.fragment_scope = None;
        self.invalidate();
    }
    pub fn frame(&mut self, contacts: &[Contact]) -> Scope {
        let scope = self.fragment_scope.take().unwrap_or(Scope::Unknown);
        match scope {
            Scope::Foreground => self.counts.foreground_frames += 1,
            Scope::Background => self.counts.background_frames += 1,
            Scope::Mixed => self.counts.mixed_frames += 1,
            Scope::Unknown => self.counts.unknown_frames += 1,
        }
        if scope != Scope::Background {
            self.invalidate();
            return scope;
        }
        let current: BTreeMap<_, _> = contacts
            .iter()
            .filter(|c| c.tip && c.confidence)
            .map(|c| (c.id, (c.x, c.y)))
            .collect();
        self.counts.background_peak = self.counts.background_peak.max(current.len());
        if current.is_empty() {
            self.counts.background_clear_frames += 1;
            self.clear_seen = true;
        }
        // A clear frame in this background interval is required before counting
        // complete lifetimes. A contact spanning a focus boundary is unverified.
        if self.clear_seen {
            self.counts.background_ends += self
                .active
                .keys()
                .filter(|id| !current.contains_key(id))
                .count() as u64;
            for (id, point) in &current {
                match self.active.get(id) {
                    None => self.counts.background_starts += 1,
                    Some(old) if old != point => self.counts.background_moves += 1,
                    _ => {}
                }
            }
            self.active = current;
            self.counts.background_active = self.active.len();
        }
        scope
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contact(id: u32, x: i32) -> Contact {
        Contact {
            id,
            x,
            y: 0,
            tip: true,
            confidence: true,
            xmin: 0,
            xmax: 100,
            ymin: 0,
            ymax: 100,
        }
    }
    fn frame(e: &mut Evidence, scope: Scope, contacts: &[Contact]) -> Scope {
        e.focus(scope);
        e.fragment(scope);
        e.frame(contacts)
    }
    #[test]
    fn background_lifetimes_need_clear_and_preserve_partial_lifts() {
        let mut e = Evidence::default();
        frame(&mut e, Scope::Background, &[contact(7, 1)]);
        assert_eq!(e.counts.background_starts, 0);
        frame(&mut e, Scope::Background, &[]);
        frame(&mut e, Scope::Background, &[contact(7, 1), contact(9, 2)]);
        frame(&mut e, Scope::Background, &[contact(9, 3)]);
        frame(&mut e, Scope::Background, &[]);
        assert_eq!(
            (
                e.counts.background_starts,
                e.counts.background_ends,
                e.counts.background_moves,
                e.counts.background_active
            ),
            (2, 2, 1, 0)
        );
    }
    #[test]
    fn focus_boundary_does_not_manufacture_background_release() {
        let mut e = Evidence::default();
        frame(&mut e, Scope::Background, &[]);
        frame(&mut e, Scope::Background, &[contact(7, 1)]);
        frame(&mut e, Scope::Foreground, &[]);
        assert_eq!(
            (e.counts.background_ends, e.counts.background_interrupted),
            (0, 1)
        );
        frame(&mut e, Scope::Background, &[contact(9, 2)]);
        assert_eq!(e.counts.background_starts, 1);
    }
    #[test]
    fn hybrid_frame_straddling_focus_is_not_background_evidence() {
        let mut e = Evidence::default();
        e.fragment(Scope::Background);
        e.fragment(Scope::Foreground);
        assert_eq!(e.frame(&[contact(7, 1)]), Scope::Mixed);
        assert_eq!(e.counts.background_frames, 0);
        e.fragment(Scope::Unknown);
        e.fragment(Scope::Background);
        assert_eq!(e.frame(&[]), Scope::Unknown);
    }
    #[test]
    fn delivery_code_and_actual_foreground_are_independent_evidence() {
        let mut e = Evidence::default();
        e.raw(Scope::Foreground, true);
        e.raw(Scope::Background, true);
        e.raw(Scope::Background, false);
        e.raw(Scope::Unknown, true);
        assert_eq!(
            (
                e.counts.foreground_raw,
                e.counts.background_raw,
                e.counts.background_sink_raw,
                e.counts.unknown_raw
            ),
            (1, 2, 1, 1)
        );
    }
}
