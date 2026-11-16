use std::{
    fs::File,
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
    tx: SyncSender<String>,
    pub dropped: Arc<AtomicU64>,
    start: Instant,
}
impl Log {
    pub fn open(dir: &Path) -> Result<(Self, std::thread::JoinHandle<Result<(), String>>), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let file = File::create(dir.join("events.log")).map_err(|e| e.to_string())?;
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
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
                start: Instant::now(),
            },
            handle,
        ))
    }
    pub fn event(&self, s: impl AsRef<str>) {
        if self
            .tx
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
