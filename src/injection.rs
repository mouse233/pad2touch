use crate::{
    decode::{map_axis, Contact},
    hid::winerr,
    logging::Log,
};
use std::{
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    UI::{Input::Pointer::*, WindowsAndMessaging::*},
};

pub struct Injector {
    target: usize,
    rect: RECT,
    active: Option<(u32, POINT)>,
    armed: bool,
    pub successes: u64,
    pub failures: u64,
    pub retries: u64,
    pub durations: Vec<u128>,
    log: Log,
    last_sent: Instant,
}
impl Injector {
    pub fn new(target: usize, rect: RECT, log: Log) -> Result<Self, String> {
        if unsafe { InitializeTouchInjection(1, TOUCH_FEEDBACK_NONE) } == 0 {
            return Err(winerr("InitializeTouchInjection"));
        }
        log.event("INITIALIZE_TOUCH_INJECTION success max_contacts=1");
        Ok(Self {
            target,
            rect,
            active: None,
            armed: false,
            successes: 0,
            failures: 0,
            retries: 0,
            durations: Vec::new(),
            log,
            last_sent: Instant::now(),
        })
    }
    fn send(&mut self, point: POINT, flags: u32) -> Result<(), String> {
        let mut info = POINTER_TOUCH_INFO::default();
        // Keep the sole injected contact in slot 0. On this machine slot 1
        // with InitializeTouchInjection(1, ...) fails with error 87.
        info.pointerInfo.pointerType = PT_TOUCH;
        info.pointerInfo.pointerId = 0;
        info.pointerInfo.ptPixelLocation = point;
        info.pointerInfo.pointerFlags = flags;
        // No custom timestamps: ERROR_NOT_READY requires retrying this SAME frame.
        let start = Instant::now();
        for attempt in 0..20 {
            if unsafe { InjectTouchInput(1, &info) } != 0 {
                self.successes += 1;
                self.last_sent = Instant::now();
                if self.durations.len() < 100_000 {
                    self.durations.push(start.elapsed().as_micros());
                }
                self.log.event(format!(
                    "INJECT_OK flags=0x{flags:X} x={} y={} api_us={}",
                    point.x,
                    point.y,
                    start.elapsed().as_micros()
                ));
                return Ok(());
            }
            let e = unsafe { GetLastError() };
            if e == ERROR_NOT_READY && attempt < 19 {
                self.retries += 1;
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            self.failures += 1;
            self.log
                .event(format!("INJECT_ERROR flags=0x{flags:X} code={e}"));
            return Err(format!("InjectTouchInput: Win32 error {e}"));
        }
        unreachable!()
    }
    pub fn release(&mut self, cancel: bool) -> Result<(), String> {
        self.armed = false;
        if let Some((_, p)) = self.active {
            // Always establish a final UPDATE at the cached successful point.
            // UP alone terminates touch; INRANGE|UP would leave hover active.
            let update = self.send(
                p,
                POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE,
            );
            let up = self.send(
                p,
                POINTER_FLAG_UP | if cancel { POINTER_FLAG_CANCELED } else { 0 },
            );
            if up.is_ok() {
                self.active = None;
            }
            return update.and(up);
        }
        Ok(())
    }
    pub fn frame(&mut self, contacts: &[Contact]) -> Result<(), String> {
        let live: Vec<_> = contacts.iter().filter(|c| c.tip && c.confidence).collect();
        if live.is_empty() {
            self.release(false)?;
            self.armed = true;
            return Ok(());
        }
        if !self.armed {
            return Ok(());
        }
        let c = if let Some((id, _)) = self.active {
            if let Some(c) = live.iter().find(|c| c.id == id) {
                *c
            } else {
                self.release(false)?;
                return Ok(());
            }
        } else {
            *live.iter().min_by_key(|c| c.id).unwrap()
        };
        let p = POINT {
            x: map_axis(
                c.x,
                c.xmin,
                c.xmax,
                self.rect.left,
                self.rect.right - self.rect.left,
            )?,
            y: map_axis(
                c.y,
                c.ymin,
                c.ymax,
                self.rect.top,
                self.rect.bottom - self.rect.top,
            )?,
        };
        let hit = unsafe { WindowFromPoint(p) } as usize;
        if hit != self.target {
            self.release(true)?;
            return Err(format!("fixed rectangle hit-test mismatch: hit=0x{hit:X} target=0x{:X} x={} y={}; paused until empty frame",self.target,p.x,p.y));
        }
        let flag = if self.active.is_some() {
            POINTER_FLAG_UPDATE
        } else {
            POINTER_FLAG_DOWN
        };
        if let Err(e) = self.send(p, POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | flag) {
            let _ = self.release(true);
            return Err(e);
        }
        self.active = Some((c.id, p));
        Ok(())
    }
    pub fn keepalive(&mut self) -> Result<(), String> {
        if let Some((_, p)) = self.active {
            if self.last_sent.elapsed() > Duration::from_millis(16) {
                if unsafe { WindowFromPoint(p) } as usize != self.target {
                    self.release(true)?;
                    return Err("test rectangle occluded during hold".into());
                }
                self.send(
                    p,
                    POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE,
                )?;
            }
        }
        Ok(())
    }
    pub fn has_active(&self) -> bool {
        self.active.is_some()
    }
}
impl Drop for Injector {
    fn drop(&mut self) {
        if let Err(e) = self.release(true) {
            self.log.event(format!("RELEASE_ERROR {e}"));
        }
    }
}
