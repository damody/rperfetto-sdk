//! ATRACE backend: writes `android.os.Trace`-style lines to the ftrace
//! `trace_marker` file. trace_processor parses them from `ftrace/print` events
//! (systrace format, see `systrace_parser.h`):
//!
//! | event                    | line                               |
//! |--------------------------|------------------------------------|
//! | slice begin (thread)     | `B|pid|name`                       |
//! | slice end (thread)       | `E|pid`                            |
//! | instant (thread)         | `I|pid|name`                       |
//! | slice begin (named track)| `G|pid|track|name|cookie`          |
//! | slice end (named track)  | `H|pid|track|cookie`               |
//! | instant (named track)    | `N|pid|track|name`                 |
//! | counter                  | `C|pid|name|value` (integer only)  |
//!
//! Debug annotations are not representable and are dropped. Process-scoped
//! tracks are written as named tracks with the process name.

use crate::writer::EventSpec;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::EventSpec;
    use crate::event::{CounterValue, Phase};
    use crate::platform;
    use crate::track::{self, TrackKind};
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::{Mutex, OnceLock};

    pub(crate) const MARKER_PATHS: [&str; 2] =
        ["/sys/kernel/tracing/trace_marker", "/sys/kernel/debug/tracing/trace_marker"];

    static FD: OnceLock<std::fs::File> = OnceLock::new();
    /// Open async-slice cookies per named track (stack → proper nesting).
    static STACKS: Mutex<Option<HashMap<u64, Vec<i64>>>> = Mutex::new(None);
    static NEXT_COOKIE: AtomicI64 = AtomicI64::new(1);

    pub fn open(path: Option<&str>) -> std::io::Result<()> {
        if FD.get().is_some() {
            return Ok(());
        }
        let mut last = std::io::Error::from(std::io::ErrorKind::NotFound);
        let candidates: Vec<&str> = match path {
            Some(p) => vec![p],
            None => MARKER_PATHS.to_vec(),
        };
        for p in candidates {
            match std::fs::OpenOptions::new().write(true).open(p) {
                Ok(f) => {
                    let _ = FD.set(f);
                    return Ok(());
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn sanitize(s: &str, out: &mut String) {
        for c in s.chars() {
            out.push(if c == '|' || c == '\n' { ' ' } else { c });
        }
    }

    pub fn emit(ev: &EventSpec) {
        let Some(mut fd) = FD.get() else { return };
        let pid = platform::pid();
        let name = ev.name.unwrap_or("");
        let info = ev.track_uuid.and_then(track::track_info);
        // None → the calling thread's track.
        let track_name: Option<String> = match &info {
            None => None,
            Some(i) => match i.kind {
                TrackKind::Thread { tid } if tid == platform::tid() => None,
                _ => Some(i.name.clone()),
            },
        };
        let mut line = String::with_capacity(64);
        use std::fmt::Write as _;
        match (ev.phase, track_name) {
            (Phase::Begin, None) => {
                let _ = write!(line, "B|{pid}|");
                sanitize(name, &mut line);
            }
            (Phase::End, None) => {
                let _ = write!(line, "E|{pid}");
            }
            (Phase::Instant, None) => {
                let _ = write!(line, "I|{pid}|");
                sanitize(name, &mut line);
            }
            (Phase::Begin, Some(t)) => {
                let cookie = NEXT_COOKIE.fetch_add(1, Ordering::Relaxed);
                STACKS
                    .lock()
                    .unwrap()
                    .get_or_insert_with(HashMap::new)
                    .entry(ev.track_uuid.unwrap())
                    .or_default()
                    .push(cookie);
                let _ = write!(line, "G|{pid}|");
                sanitize(&t, &mut line);
                line.push('|');
                sanitize(name, &mut line);
                let _ = write!(line, "|{cookie}");
            }
            (Phase::End, Some(t)) => {
                let cookie = STACKS
                    .lock()
                    .unwrap()
                    .get_or_insert_with(HashMap::new)
                    .get_mut(&ev.track_uuid.unwrap())
                    .and_then(|v| v.pop());
                let Some(cookie) = cookie else { return };
                let _ = write!(line, "H|{pid}|");
                sanitize(&t, &mut line);
                let _ = write!(line, "|{cookie}");
            }
            (Phase::Instant, Some(t)) => {
                let _ = write!(line, "N|{pid}|");
                sanitize(&t, &mut line);
                line.push('|');
                sanitize(name, &mut line);
            }
            (Phase::Counter, t) => {
                let v = match ev.counter {
                    Some(CounterValue::Int(v)) => v,
                    Some(CounterValue::Double(v)) => v.round() as i64,
                    None => return,
                };
                let _ = write!(line, "C|{pid}|");
                sanitize(t.as_deref().unwrap_or(name), &mut line);
                let _ = write!(line, "|{v}");
            }
        }
        let _ = fd.write_all(line.as_bytes());
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) use imp::{emit, open};

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub(crate) fn emit(_ev: &EventSpec) {}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub(crate) fn open(_path: Option<&str>) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}
