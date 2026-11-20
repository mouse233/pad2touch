use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    time::Instant,
};
#[derive(Clone)]
pub struct Log {
    tx: Option<SyncSender<String>>,
    pub dropped: Arc<AtomicU64>,
    start: Instant,
}
impl Log {
    pub fn disabled() -> Self {
        Self {
            tx: None,
            dropped: Arc::new(AtomicU64::new(0)),
            start: Instant::now(),
        }
    }

    #[cfg(test)]
    pub fn memory() -> (Self, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::sync_channel(128);
        (
            Self {
                tx: Some(tx),
                dropped: Arc::new(AtomicU64::new(0)),
                start: Instant::now(),
            },
            rx,
        )
    }
    pub fn open(dir: &Path) -> Result<(Self, std::thread::JoinHandle<Result<(), String>>), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join("events.log"))
            .map_err(|e| {
                format!("cannot create new events.log (choose an unused --out directory): {e}")
            })?;
        let (tx, rx) = mpsc::sync_channel::<String>(16384);
        let handle = std::thread::spawn(move || {
            let mut out = BufWriter::new(file);
            for s in rx {
                writeln!(out, "{s}").map_err(|e| e.to_string())?;
            }
            out.flush().map_err(|e| e.to_string())
        });
        Ok((
            Self {
                tx: Some(tx),
                dropped: Arc::new(AtomicU64::new(0)),
                start: Instant::now(),
            },
            handle,
        ))
    }
    /// Lets hot paths skip formatting records nobody will write.
    pub fn enabled(&self) -> bool {
        self.tx.is_some()
    }

    pub fn event(&self, s: impl AsRef<str>) {
        let Some(tx) = &self.tx else { return };
        if tx
            .try_send(format!(
                "t_us={} {}",
                self.start.elapsed().as_micros(),
                s.as_ref()
            ))
            .is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
