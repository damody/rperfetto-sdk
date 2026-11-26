//! Explicit clocks, custom clock snapshots, tracks of other processes/threads,
//! Android logs and streaming to a file. Decoded with prost and, when
//! `$TRACE_PROCESSOR_SHELL` is available, checked with trace_processor.

use prost::Message;
use rperfetto_sdk::protos::{self, Trace, TracePacket};
use rperfetto_sdk::{
    self as pf, Arg, Backends, Category, Clock, CounterTrack, InitArgs, LogEvent, LogPriority, SessionConfig,
    Timestamp, Track, TracingSession,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());
static CAT: Category = Category::new("features");

fn init() {
    pf::init(InitArgs { backends: Backends::IN_PROCESS, process_name: Some("pf_features".into()), ..Default::default() })
        .unwrap();
}

fn decode(bytes: &[u8]) -> Vec<TracePacket> {
    Trace::decode(bytes).expect("valid Trace proto").packet
}

fn tp_shell() -> Option<PathBuf> {
    let p = std::env::var_os("TRACE_PROCESSOR_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../perfetto-arm64/out/linux_release/trace_processor_shell")));
    p.exists().then_some(p)
}

fn tp_query(trace: &Path, sql: &str) -> Vec<Vec<String>> {
    let out = std::process::Command::new(tp_shell().unwrap()).arg("-Q").arg(sql).arg(trace).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|l| l.starts_with('"') || l.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-'))
        .skip(1)
        .map(|l| l.split(',').map(|c| c.trim_matches('"').to_string()).collect())
        .collect()
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rperfetto_sdk_{name}_{}.pftrace", std::process::id()))
}

fn tp_errors(path: &Path) -> Vec<Vec<String>> {
    tp_query(path, "select name, value from stats where value > 0 and severity in ('error', 'data_loss')")
}

const BOOT: u64 = 5_000_000_000_000;
const REAL: u64 = 1_790_000_000_000_000_000;

/// A phone's clocks: REALTIME events land on its BOOTTIME axis.
fn phone_session(output_file: Option<PathBuf>) -> TracingSession {
    TracingSession::start(SessionConfig {
        clock_snapshot: Some(vec![(Clock::Boottime, BOOT), (Clock::Realtime, REAL)]),
        output_file,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn clocks_tracks_and_logs() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let s = phone_session(None);
    let app = Track::process_of(4242, Some("com.example.game"));
    let render = Track::thread_of(4242, 4243, Some("RenderThread"));
    let fps = CounterTrack::with_parent("fps", app, Some("fps"));
    pf::slice_begin_on_at(&CAT, render, Timestamp::realtime(REAL + 2_000_000_000), "draw", &[Arg::new("frame", 7)]);
    pf::slice_end_on_at(&CAT, render, Timestamp::realtime(REAL + 2_016_000_000));
    pf::counter_at(&CAT, fps, Timestamp::boottime(BOOT + 3_000_000_000), 60.0);
    pf::android_logs(
        &CAT,
        &[
            LogEvent {
                realtime_ns: REAL + 1_000_000_000,
                pid: 4242,
                tid: 4243,
                uid: Some(10123),
                priority: LogPriority::Warn,
                tag: "GameTag",
                message: "low memory",
            },
            LogEvent {
                realtime_ns: REAL + 1_500_000_000,
                pid: 4242,
                tid: 4242,
                uid: None,
                priority: LogPriority::from_letter('E').unwrap(),
                tag: "GameTag",
                message: "oops",
            },
        ],
    );
    let mut s = s;
    s.stop();
    let bytes = s.trace_bytes();
    let packets = decode(&bytes);
    let snapshot = packets[0].clock_snapshot.as_ref().unwrap();
    assert_eq!(snapshot.primary_trace_clock, Some(protos::BUILTIN_CLOCK_BOOTTIME));
    let draw = packets.iter().find(|p| p.track_event.as_ref().and_then(|e| e.name.as_deref()) == Some("draw")).unwrap();
    assert_eq!(draw.timestamp_clock_id, Some(protos::BUILTIN_CLOCK_REALTIME));
    let threads: Vec<_> = packets.iter().filter_map(|p| p.track_descriptor.as_ref()?.thread.clone()).collect();
    assert!(threads.iter().any(|t| t.pid == Some(4242) && t.tid == Some(4243)));
    let logs = packets.iter().find_map(|p| p.android_log.clone()).unwrap();
    assert_eq!(logs.events.len(), 2);
    assert_eq!(logs.events[0].prio, Some(5));

    let Some(_) = tp_shell() else {
        eprintln!("trace_processor_shell not found; skipping TP checks");
        return;
    };
    let path = temp("features");
    std::fs::write(&path, &bytes).unwrap();
    assert!(tp_errors(&path).is_empty(), "{:?}", tp_errors(&path));
    let slice = tp_query(
        &path,
        "select s.ts, s.dur, t.tid, t.name, p.pid, p.name from slice s join thread_track tt on tt.id = s.track_id \
         join thread t using(utid) join process p using(upid) where s.name = 'draw'",
    );
    assert_eq!(slice, vec![vec![
        (BOOT + 2_000_000_000).to_string(),
        "16000000".into(),
        "4243".into(),
        "RenderThread".into(),
        "4242".into(),
        "com.example.game".into(),
    ]]);
    let counter = tp_query(&path, "select c.ts, c.value from counter c join counter_track t on t.id = c.track_id where t.name = 'fps'");
    assert_eq!(counter, vec![vec![(BOOT + 3_000_000_000).to_string(), "60.000000".into()]]);
    let logs = tp_query(&path, "select l.ts, l.prio, l.tag, l.msg, t.tid from android_logs l join thread t using(utid) order by l.ts");
    assert_eq!(
        logs,
        vec![
            vec![(BOOT + 1_000_000_000).to_string(), "5".into(), "GameTag".into(), "low memory".into(), "4243".into()],
            vec![(BOOT + 1_500_000_000).to_string(), "6".into(), "GameTag".into(), "oops".into(), "4242".into()],
        ]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn thread_events_carry_their_process_name() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let s = phone_session(None);
    Track::process_of(777, Some("com.other.app"));
    let t = Track::thread_of(777, 778, Some("Worker"));
    pf::counter_at(&CAT, CounterTrack::with_parent("load", t, None), Timestamp::realtime(REAL + 1), 1);
    let mut s = s;
    s.stop();
    let names: Vec<_> = decode(&s.trace_bytes())
        .into_iter()
        .filter_map(|p| p.track_descriptor?.process?.process_name)
        .collect();
    assert!(names.contains(&"com.other.app".to_string()), "{names:?}");
}

#[test]
fn host_snapshot_places_realtime_next_to_sdk_clock() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let s = TracingSession::start(SessionConfig::default()).unwrap();
    let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
    let now = pf::now_ns();
    let track = Track::named("clocks");
    pf::instant_on_at(&CAT, track, now, "sdk", &[]);
    pf::instant_on_at(&CAT, track, Timestamp::realtime(wall + 5_000_000_000), "realtime", &[]);
    let mut s = s;
    s.stop();
    let Some(_) = tp_shell() else { return };
    let path = temp("host_clocks");
    s.write_to_file(&path).unwrap();
    assert!(tp_errors(&path).is_empty(), "{:?}", tp_errors(&path));
    let rows = tp_query(&path, "select name, ts from slice where name in ('sdk', 'realtime') order by name");
    let ts = |n: &str| rows.iter().find(|r| r[0] == n).unwrap()[1].parse::<i64>().unwrap();
    // Both clocks were read within microseconds of each other.
    assert!((ts("realtime") - ts("sdk") - 5_000_000_000).abs() < 5_000_000, "{rows:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn streaming_session_writes_while_recording() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let path = temp("streaming");
    // 64 KiB would discard most of this in memory mode; streaming keeps all.
    let s = TracingSession::start(SessionConfig {
        buffer_size_kb: 64,
        output_file: Some(path.clone()),
        ..Default::default()
    })
    .unwrap();
    let track = CounterTrack::new("streamed");
    let base = pf::now_ns() + 1_000;
    for i in 0..50_000u64 {
        pf::counter_at(&CAT, track, base + i * 1_000_000, i as i64);
    }
    pf::flush_current_thread();
    assert!(std::fs::metadata(&path).unwrap().len() > 64 * 1024, "data reaches the file before stop");
    let mut s = s;
    s.stop();
    assert!(s.trace_bytes().is_empty());
    assert_eq!(s.stats().dropped_bytes, 0);
    assert!(s.file_error().is_none());
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(s.stats().bytes, bytes.len() as u64);
    let counters = decode(&bytes).iter().filter(|p| p.track_event.as_ref().and_then(|e| e.r#type) == Some(protos::TYPE_COUNTER)).count();
    assert_eq!(counters, 50_000);
    if tp_shell().is_some() {
        assert!(tp_errors(&path).is_empty());
        assert_eq!(tp_query(&path, "select count(*) from counter"), vec![vec!["50000".to_string()]]);
    }
    let copy = temp("streaming_copy");
    s.write_to_file(&copy).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&copy);
}

#[test]
fn streaming_with_phone_clocks() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let path = temp("streaming_phone");
    let s = phone_session(Some(path.clone()));
    pf::instant_on_at(&CAT, Track::thread_of(1, 2, Some("t")), Timestamp::realtime(REAL + 1), "x", &[]);
    let mut s = s;
    s.stop();
    let packets = decode(&std::fs::read(&path).unwrap());
    assert_eq!(packets[0].clock_snapshot.as_ref().unwrap().clocks.len(), 2);
    assert!(packets.iter().any(|p| p.track_event.is_some()));
    let _ = std::fs::remove_file(&path);
}
