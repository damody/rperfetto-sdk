//! Android log lines (`AndroidLogPacket`): shown in Perfetto UI's "Android
//! Logs" panel and the trace processor's `android_logs` table, like logcat
//! recorded by traced_probes.

use crate::category::{Category, SINK_PROTO};
use crate::protos::{android_log_packet, AndroidLogPacket, TracePacket, BUILTIN_CLOCK_REALTIME};
use crate::writer::emit_raw_with_sinks;
use prost::Message;

/// `AndroidLogPriority`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogPriority {
    Verbose = 2,
    Debug = 3,
    Info = 4,
    Warn = 5,
    Error = 6,
    Fatal = 7,
}

impl LogPriority {
    /// From a logcat level letter (`V`, `D`, `I`, `W`, `E`, `F`/`A`).
    pub fn from_letter(level: char) -> Option<LogPriority> {
        Some(match level.to_ascii_uppercase() {
            'V' => LogPriority::Verbose,
            'D' => LogPriority::Debug,
            'I' => LogPriority::Info,
            'W' => LogPriority::Warn,
            'E' => LogPriority::Error,
            'F' | 'A' => LogPriority::Fatal,
            _ => return None,
        })
    }
}

/// One log line. `realtime_ns` is Unix epoch nanoseconds (CLOCK_REALTIME),
/// which is how Perfetto timestamps Android logs.
#[derive(Clone, Copy, Debug)]
pub struct LogEvent<'a> {
    pub realtime_ns: u64,
    pub pid: i32,
    pub tid: i32,
    pub uid: Option<i32>,
    pub priority: LogPriority,
    pub tag: &'a str,
    pub message: &'a str,
}

/// Records `events` as one `AndroidLogPacket` (in-process and system
/// backends; ATRACE has no log format and skips them).
pub fn android_logs(cat: &'static Category, events: &[LogEvent]) {
    let sinks = cat.sinks() & SINK_PROTO;
    if sinks == 0 || events.is_empty() {
        return;
    }
    let packet = TracePacket {
        timestamp: events.iter().map(|e| e.realtime_ns).min(),
        timestamp_clock_id: Some(BUILTIN_CLOCK_REALTIME),
        android_log: Some(AndroidLogPacket {
            events: events
                .iter()
                .map(|e| android_log_packet::LogEvent {
                    log_id: Some(0),
                    pid: Some(e.pid),
                    tid: Some(e.tid),
                    uid: e.uid,
                    timestamp: Some(e.realtime_ns),
                    tag: Some(e.tag.to_string()),
                    prio: Some(e.priority as i32),
                    message: Some(e.message.to_string()),
                })
                .collect(),
        }),
        ..Default::default()
    };
    emit_raw_with_sinks(sinks, &packet.encode_to_vec());
}
