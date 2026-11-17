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
}
impl Controller {
    pub fn enable(&mut self) {
        self.enabled = true;
        self.requested = self.mode == TakeoverMode::Manual;
    }
    pub fn hotkey(&mut self, held: bool) {
        if self.enabled {
            match self.mode {
                TakeoverMode::Toggle => self.requested = !self.requested,
                TakeoverMode::Hold => self.requested = held,
                TakeoverMode::Manual => {}
            }
        }
    }
    pub fn release(&mut self) {
        if self.mode == TakeoverMode::Hold {
            self.requested = false;
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
        c.enable();
        c.hotkey(true);
        assert!(c.requested);
        c.disarm();
        c.hotkey(true);
        assert!(!c.requested);
        assert!(!c.enabled);
    }
    #[test]
    fn releasing_during_startup_cancels_intent() {
        let mut c = Controller {
            mode: TakeoverMode::Hold,
            ..Default::default()
        };
        c.enable();
        c.hotkey(true);
        c.release();
        assert!(!c.requested);
        assert!(c.enabled);
    }
    #[test]
    fn toggle_intent_survives_asynchronous_worker_drain() {
        let mut c = Controller {
            mode: TakeoverMode::Toggle,
            ..Default::default()
        };
        c.enable();
        assert!(!c.requested);
        c.hotkey(false);
        assert!(c.requested);
        c.hotkey(false);
        assert!(!c.requested);
        c.hotkey(false);
        assert!(c.requested);
        c.disarm();
        c.hotkey(false);
        assert!(!c.requested);
    }
}
