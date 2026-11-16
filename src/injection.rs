//! Thin Win32 transport. Lifecycle/slot allocation is tested without Windows injection.
use crate::{
    decode::Contact,
    hid::winerr,
    logging::Log,
    touch::{Action, Engine, Failure, Frame, Rectangle, Transport},
};
use std::{
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    UI::{Input::Pointer::*, WindowsAndMessaging::*},
};

pub fn flags(action: Action) -> u32 {
    match action {
        Action::Down => POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN,
        Action::Update => POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE,
        Action::Up => POINTER_FLAG_UP,
        Action::Cancel => POINTER_FLAG_UP | POINTER_FLAG_CANCELED,
    }
}
struct WindowsTransport {
    target: usize,
    log: Log,
    batch: u64,
}
impl Transport for WindowsTransport {
    fn inject(&mut self, frame: &Frame) -> Result<(), Failure> {
        if !frame.cleanup {
            for p in &frame.pointers {
                if matches!(p.action, Action::Down | Action::Update)
                    && unsafe {
                        WindowFromPoint(POINT {
                            x: p.point.x,
                            y: p.point.y,
                        })
                    } as usize
                        != self.target
                {
                    self.log
                        .event("INJECT_BLOCKED fixed target unavailable or occluded");
                    return Err(Failure::TargetUnavailable);
                }
            }
        }
        let contacts: Vec<_> = frame
            .pointers
            .iter()
            .map(|p| {
                let mut info = POINTER_TOUCH_INFO::default();
                info.pointerInfo.pointerType = PT_TOUCH;
                info.pointerInfo.pointerId = p.slot;
                info.pointerInfo.pointerFlags = flags(p.action);
                info.pointerInfo.ptPixelLocation = POINT {
                    x: p.point.x,
                    y: p.point.y,
                };
                info
            })
            .collect();
        let start = Instant::now();
        if unsafe { InjectTouchInput(contacts.len() as u32, contacts.as_ptr()) } == 0 {
            let code = unsafe { GetLastError() };
            self.log.event(format!(
                "INJECT_ERROR count={} code={code} cleanup={}",
                contacts.len(),
                frame.cleanup
            ));
            return Err(Failure::Win32(code));
        }
        self.batch += 1;
        let us = start.elapsed().as_micros();
        self.log.event(format!(
            "INJECT_BATCH batch={} count={} cleanup={} api_us={us}",
            self.batch,
            contacts.len(),
            frame.cleanup
        ));
        for p in &frame.pointers {
            self.log.event(format!(
                "INJECT_OK flags=0x{:X} x={} y={} api_us={us} batch={} slot={} raw_id={}",
                flags(p.action),
                p.point.x,
                p.point.y,
                self.batch,
                p.slot,
                p.raw_id
            ));
        }
        Ok(())
    }
    fn retry_delay(&mut self) {
        thread::sleep(Duration::from_millis(1));
    }
}
pub struct Injector {
    engine: Engine<WindowsTransport>,
    last_sent: Instant,
    log: Log,
}
impl Injector {
    pub fn new(target: usize, rect: RECT, capacity: usize, log: Log) -> Result<Self, String> {
        let transport = WindowsTransport {
            target,
            log: log.clone(),
            batch: 0,
        };
        let engine = Engine::new(
            capacity,
            Rectangle {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            },
            transport,
        )?;
        if unsafe { InitializeTouchInjection(capacity as u32, TOUCH_FEEDBACK_NONE) } == 0 {
            return Err(winerr("InitializeTouchInjection"));
        }
        log.event(format!(
            "INITIALIZE_TOUCH_INJECTION success max_contacts={capacity}"
        ));
        Ok(Self {
            engine,
            last_sent: Instant::now(),
            log,
        })
    }
    pub fn frame(&mut self, input: &[Contact]) -> Result<(), String> {
        let before = self.engine.successes;
        let result = self.engine.apply(input);
        if self.engine.successes != before {
            self.last_sent = Instant::now();
        }
        result
    }
    pub fn release(&mut self, _cancel: bool) -> Result<(), String> {
        self.engine.cancel()
    }
    pub fn keepalive(&mut self) -> Result<(), String> {
        if self.last_sent.elapsed() >= Duration::from_millis(16) {
            let before = self.engine.successes;
            let result = self.engine.keepalive();
            if self.engine.successes != before {
                self.last_sent = Instant::now();
            }
            result
        } else {
            Ok(())
        }
    }
    pub fn has_active(&self) -> bool {
        self.engine.active_count() > 0
    }
    pub fn active_count(&self) -> usize {
        self.engine.active_count()
    }
    pub fn is_armed(&self) -> bool {
        self.engine.is_armed()
    }
    pub fn cleanup_pending(&self) -> bool {
        self.engine.cleanup_pending
    }
    pub fn successes(&self) -> u64 {
        self.engine.successes
    }
    pub fn failures(&self) -> u64 {
        self.engine.failures
    }
    pub fn retries(&self) -> u64 {
        self.engine.retries
    }
    pub fn durations(&self) -> &[u128] {
        &self.engine.durations
    }
}
impl Drop for Injector {
    fn drop(&mut self) {
        if let Err(e) = self.engine.cancel() {
            self.log.event(format!("RELEASE_ERROR {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_flags_end_without_hover() {
        assert_eq!(flags(Action::Down), 0x10006);
        assert_eq!(flags(Action::Update), 0x20006);
        assert_eq!(flags(Action::Up), 0x40000);
        assert_eq!(flags(Action::Cancel), 0x48000);
    }
}
