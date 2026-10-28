//! A live stream of a work's events, one JSON object per line, written as each event is recorded.
//!
//! `work --events FILE` (or `fd:N`) lets a host show a run while it happens: the durable state is
//! locked for the run's duration, and `--json` prints only the final report. The stream carries
//! exactly the events the loop records ([`crate::service`] renders them the same way for the HTTP
//! `/events` route), so it reports reality and decides nothing: a write that fails is noted once, the
//! stream stops, and the work continues unchanged. Model prose is never an event.
//!
//! Each line: `{"schema":"chip.work-event.v1","work_id":ID,"seq":N,"at_ms":UNIX_MS,"event":{...}}`.
//! `seq` starts at 0 and has no gaps; a reader that sees one has lost a line.

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chip_core::WorkEvent;
use serde_json::{Value, json};

pub const SCHEMA: &str = "chip.work-event.v1";

pub struct EventSink {
    work_id: String,
    destination: String,
    out: Mutex<Option<Box<dyn Write + Send>>>,
    seq: AtomicU64,
    failure: Mutex<Option<String>>,
}

impl std::fmt::Debug for EventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EventSink({})", self.destination)
    }
}

impl EventSink {
    /// `spec` is a file path (created, or truncated if it exists) or `fd:N` for an inherited file
    /// descriptor (unix; 0 and 1 are refused: stdin is not a destination and stdout carries the
    /// report).
    pub fn open(spec: &str, work_id: &str) -> Result<Self, String> {
        let (destination, out): (String, Box<dyn Write + Send>) = match spec.strip_prefix("fd:") {
            Some(n) => (format!("fd:{n}"), open_fd(n)?),
            None => {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(spec)
                    .map_err(|e| format!("--events {spec}: {}", e.kind()))?;
                (spec.to_string(), Box::new(file))
            }
        };
        Ok(Self {
            work_id: work_id.to_string(),
            destination,
            out: Mutex::new(Some(out)),
            seq: AtomicU64::new(0),
            failure: Mutex::new(None),
        })
    }

    /// Writes `event` as one line, flushed. Never fails the work.
    pub fn emit(&self, event: &WorkEvent) {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let Some(writer) = out.as_mut() else { return };
        let seq = self.seq.load(Ordering::SeqCst);
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let line = json!({
            "schema": SCHEMA, "work_id": self.work_id, "seq": seq, "at_ms": at_ms,
            "event": crate::service::event_json(event),
        });
        let written = writeln!(writer, "{line}").and_then(|()| writer.flush());
        match written {
            Ok(()) => {
                self.seq.store(seq + 1, Ordering::SeqCst);
            }
            Err(e) => {
                *out = None;
                let why = format!("{} after {seq} line(s): {}", self.destination, e.kind());
                eprintln!("note: the event stream stopped ({why}); the work continues");
                *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
            }
        }
    }

    /// For the final report: what was written and whether the stream ended early.
    pub fn status(&self) -> Value {
        json!({
            "schema": SCHEMA,
            "destination": self.destination,
            "lines": self.seq.load(Ordering::SeqCst),
            "failed": *self.failure.lock().unwrap_or_else(|e| e.into_inner()),
        })
    }
}

#[cfg(unix)]
fn open_fd(n: &str) -> Result<Box<dyn Write + Send>, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd: i32 = n.parse().ok().filter(|fd| *fd >= 2).ok_or(
        "--events fd:N needs a descriptor number of 2 or more (stdout carries the report)",
    )?;
    // Only a descriptor that is open: probing with fcntl, never reading or writing it.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err(format!("--events fd:{fd} is not an open file descriptor"));
    }
    // The stream owns the descriptor from here; it is flushed after every line and closed at exit.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let _ = file.as_raw_fd();
    Ok(Box::new(file))
}

#[cfg(not(unix))]
fn open_fd(_: &str) -> Result<Box<dyn Write + Send>, String> {
    Err("--events fd:N is available on unix only; give a file path".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_core::{WorkId, WorkLimits};

    fn started() -> WorkEvent {
        WorkEvent::WorkStarted {
            work_id: WorkId::new("w"),
            goal: "g".into(),
            limits: WorkLimits {
                max_turns: 2,
                max_executions: 2,
            },
        }
    }

    #[test]
    fn every_event_is_one_flushed_line_with_a_gapless_sequence() {
        let path = std::env::temp_dir().join(format!("events-{}.jsonl", std::process::id()));
        let sink = EventSink::open(path.to_str().unwrap(), "w").unwrap();
        sink.emit(&started());
        sink.emit(&started());
        // Readable before the sink is dropped: each line was flushed as it was written.
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["schema"], SCHEMA);
        assert_eq!(
            (lines[0]["seq"].as_u64(), lines[1]["seq"].as_u64()),
            (Some(0), Some(1))
        );
        assert_eq!(lines[1]["event"]["kind"], "WorkStarted");
        assert_eq!(sink.status()["lines"], 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stdin_and_stdout_are_not_destinations_and_an_unopened_descriptor_is_refused() {
        assert!(EventSink::open("fd:0", "w").is_err());
        assert!(EventSink::open("fd:1", "w").is_err());
        assert!(EventSink::open("fd:987", "w").is_err());
        assert!(EventSink::open("/nonexistent-dir/x.jsonl", "w").is_err());
    }
}
