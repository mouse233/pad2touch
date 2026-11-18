//! Capture intent is independent of worker startup/shutdown and the GUI toolkit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TakeoverMode {
    #[default]
    Manual,
    Toggle,
    Hold,
}

#[derive(Default)]
pub struct Controller {
    pub mode: TakeoverMode,
    pub enabled: bool,
    pub requested: bool,
    alt_was_down: bool,
}
impl Controller {
    pub fn enable(&mut self, alt_down: bool) {
        self.enabled = true;
        self.requested = self.mode == TakeoverMode::Manual;
        self.alt_was_down = alt_down;
    }
    pub fn hotkey(&mut self) {
        if self.enabled && self.mode == TakeoverMode::Toggle {
            self.requested = !self.requested;
        }
    }
    pub fn sample_alt(&mut self, down: bool) {
        if self.enabled && self.mode == TakeoverMode::Hold {
            if !down || !self.alt_was_down {
                self.requested = down;
            }
            self.alt_was_down = down;
        }
    }
    pub fn disarm(&mut self) {
        self.enabled = false;
        self.requested = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emergency_stop_cannot_rearm_from_a_held_key() {
        let mut c = Controller {
            mode: TakeoverMode::Hold,
            ..Default::default()
        };
        c.enable(false);
        c.sample_alt(true);
        assert!(c.requested);
        c.disarm();
        c.sample_alt(true);
        assert!(!c.requested);
        assert!(!c.enabled);
        c.enable(true);
        c.sample_alt(true);
        assert!(!c.requested);
        c.sample_alt(false);
        c.sample_alt(true);
        assert!(c.requested);
    }
    #[test]
    fn releasing_during_startup_cancels_intent() {
        let mut c = Controller {
            mode: TakeoverMode::Hold,
            ..Default::default()
        };
        c.enable(false);
        c.sample_alt(true);
        c.sample_alt(false);
        assert!(!c.requested);
        assert!(c.enabled);
    }
    #[test]
    fn toggle_intent_survives_asynchronous_worker_drain() {
        let mut c = Controller {
            mode: TakeoverMode::Toggle,
            ..Default::default()
        };
        c.enable(false);
        assert!(!c.requested);
        c.hotkey();
        assert!(c.requested);
        c.hotkey();
        assert!(!c.requested);
        c.hotkey();
        assert!(c.requested);
        c.disarm();
        c.hotkey();
        assert!(!c.requested);
    }
}
