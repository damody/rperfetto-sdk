//! In-process backend: decode the produced trace with prost and, when
//! available, query it with Perfetto's trace_processor_shell.

use prost::Message;
use rperfetto_sdk::protos::{self, Trace, TracePacket};
use rperfetto_sdk::{self as pf, Arg, Backends, Category, CategoryFilter, CounterTrack, InitArgs, SessionConfig, Track, TracingSession};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());
static CAT_A: Category = Category::new("test_a");
static CAT_B: Category = Category::new("test_b");
static CAT_OFF: Category = Category::new("test_off");

fn init() {
    pf::init(InitArgs { backends: Backends::IN_PROCESS, process_name: Some("pf_test".into()), ..Default::default() })
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

fn tp_query(trace: &std::path::Path, sql: &str) -> Vec<Vec<String>> {
    let out = std::process::Command::new(tp_shell().unwrap())
        .arg("-Q")
        .arg(sql)
        .arg(trace)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|l| l.starts_with('"') || l.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-'))
        .skip(1)
        .map(|l| l.split(',').map(|c| c.trim_matches('"').to_string()).collect())
        .collect()
}

#[test]
fn packets_and_trace_processor() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let s = TracingSession::start(SessionConfig::default()).unwrap();
    assert!(matches!(TracingSession::start(SessionConfig::default()), Err(pf::Error::SessionActive)));
    pf::set_thread_name("tmain");
    CAT_OFF.set_enabled(false);
    let custom = Track::named("Custom");
    let ctr = CounterTrack::with_unit("mem", "bytes");
    {
        let _outer = pf::scoped_slice(&CAT_A, "outer", &[Arg::new("i", -5i64), Arg::new("s", "str"), Arg::new("f", 2.5)]);
        pf::slice_begin(&CAT_B, "inner", &[Arg::new("u", 7u64), Arg::new("b", true)]);
        pf::instant(&CAT_A, "mark", &[Arg::new("p", 0x1234usize as *const u8)]);
        pf::slice_end(&CAT_B);
        pf::instant(&CAT_OFF, "never", &[]);
    }
    pf::slice_begin_on(&CAT_A, custom, "on_custom", &[]);
    pf::slice_end_on(&CAT_A, custom);
    pf::counter(&CAT_A, ctr, 10);
    pf::counter(&CAT_A, ctr, 20.5);
    std::thread::Builder::new()
        .name("tworker".into())
        .spawn(|| {
            let _w = pf::scoped_slice(&CAT_A, "worker_slice", &[]);
        })
        .unwrap()
        .join()
        .unwrap();
    let mut s = s;
    s.stop();
    CAT_OFF.set_enabled(true);
    let bytes = s.trace_bytes();
    let packets = decode(&bytes);

    // Clock snapshot header first.
    assert!(packets[0].clock_snapshot.is_some());
    // Every non-header packet has a sequence id; descriptors precede events.
    let mut seen_desc: HashMap<u32, Vec<u64>> = HashMap::new();
    let mut names = Vec::new();
    for p in &packets[1..] {
        let seq = p.trusted_packet_sequence_id.expect("seq id");
        if let Some(d) = &p.track_descriptor {
            seen_desc.entry(seq).or_default().push(d.uuid.unwrap());
        }
        if let Some(te) = &p.track_event {
            let uuid = te.track_uuid.unwrap();
            assert!(seen_desc.get(&seq).is_some_and(|v| v.contains(&uuid)), "descriptor before event");
            if let Some(n) = &te.name {
                names.push(n.clone());
            }
            assert!(p.timestamp.is_some());
        }
    }
    assert!(names.contains(&"outer".to_string()));
    assert!(names.contains(&"worker_slice".to_string()));
    assert!(!names.contains(&"never".to_string()));

    // Encoding check against the generated layout: inner slice args.
    let inner = packets
        .iter()
        .filter_map(|p| p.track_event.as_ref())
        .find(|te| te.name.as_deref() == Some("inner"))
        .unwrap();
    assert_eq!(inner.r#type, Some(protos::TYPE_SLICE_BEGIN));
    assert_eq!(inner.categories, vec!["test_b".to_string()]);
    assert_eq!(inner.debug_annotations[0].uint_value, Some(7));
    assert_eq!(inner.debug_annotations[1].bool_value, Some(true));

    let Some(_) = tp_shell() else {
        eprintln!("trace_processor_shell not found; skipping TP checks");
        return;
    };
    let path = std::env::temp_dir().join(format!("rperfetto_sdk_test_{}.pftrace", std::process::id()));
    s.write_to_file(&path).unwrap();

    let rows = tp_query(
        &path,
        "select s.name, s.depth, s.category, coalesce(thread.name, t.name), s.dur from slice s \
         join track t on s.track_id = t.id left join thread_track tt on tt.id = s.track_id \
         left join thread using(utid) order by s.ts",
    );
    let find = |n: &str| rows.iter().find(|r| r[0] == n).unwrap_or_else(|| panic!("{n} missing: {rows:?}")).clone();
    let outer = find("outer");
    assert_eq!(&outer[1..4], ["0", "test_a", "tmain"]);
    let inner = find("inner");
    assert_eq!(&inner[1..4], ["1", "test_b", "tmain"]);
    let mark = find("mark");
    assert_eq!(&mark[1..], ["2", "test_a", "tmain", "0"]);
    assert_eq!(&find("on_custom")[1..4], ["0", "test_a", "Custom"]);
    assert_eq!(&find("worker_slice")[3], "tworker");
    let outer_dur: i64 = outer[4].parse().unwrap();
    let inner_dur: i64 = inner[4].parse().unwrap();
    assert!(outer_dur >= inner_dur && inner_dur >= 0);

    let args = tp_query(
        &path,
        "select s.name, a.key, a.display_value from slice s join args a using(arg_set_id) \
         where a.key like 'debug.%' order by s.name, a.key",
    );
    let arg = |n: &str, k: &str| {
        args.iter().find(|r| r[0] == n && r[1] == k).map(|r| r[2].clone()).unwrap_or_else(|| panic!("{n}.{k}: {args:?}"))
    };
    assert_eq!(arg("outer", "debug.i"), "-5");
    assert_eq!(arg("outer", "debug.s"), "str");
    assert_eq!(arg("outer", "debug.f"), "2.5");
    assert_eq!(arg("inner", "debug.u"), "7");
    assert_eq!(arg("inner", "debug.b"), "true");
    assert_eq!(arg("mark", "debug.p"), "0x1234");

    let ctrs = tp_query(&path, "select t.name, c.value from counter c join counter_track t on t.id = c.track_id order by c.ts");
    assert_eq!(ctrs, vec![vec!["mem".to_string(), "10.000000".into()], vec!["mem".into(), "20.500000".into()]]);
    let procs = tp_query(&path, "select name from process where pid > 0");
    assert_eq!(procs, vec![vec!["pf_test".to_string()]]);
    let errs = tp_query(&path, "select name, value from stats where value > 0 and severity in ('error', 'data_loss')");
    assert!(errs.is_empty(), "trace_processor errors: {errs:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn session_category_filter() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let cfg = SessionConfig {
        categories: CategoryFilter { enabled: vec!["test_b".into()], disabled: vec!["*".into()] },
        ..Default::default()
    };
    let s = TracingSession::start(cfg).unwrap();
    assert!(!CAT_A.is_enabled());
    assert!(CAT_B.is_enabled());
    pf::instant(&CAT_A, "filtered_out", &[]);
    pf::instant(&CAT_B, "kept", &[]);
    let mut s = s;
    s.stop();
    assert!(!CAT_B.is_enabled(), "disabled after stop");
    let names: Vec<_> = decode(&s.trace_bytes())
        .into_iter()
        .filter_map(|p| p.track_event.and_then(|t| t.name))
        .collect();
    assert_eq!(names, vec!["kept".to_string()]);
}

#[test]
fn ring_buffer_keeps_newest() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let s = TracingSession::start(SessionConfig { buffer_size_kb: 64, ring_buffer: true, ..Default::default() }).unwrap();
    for i in 0..20_000 {
        pf::instant(&CAT_A, "spam", &[Arg::new("i", i)]);
    }
    let mut s = s;
    s.stop();
    assert!(s.stats().dropped_bytes > 0);
    let packets = decode(&s.trace_bytes());
    let last = packets.iter().rev().find_map(|p| p.track_event.as_ref()).unwrap();
    assert_eq!(last.debug_annotations[0].int_value, Some(19_999));
    // Descriptors are re-emitted after wrapping, so the oldest surviving
    // event still has its track described in the buffer.
    assert!(packets.iter().any(|p| p.track_descriptor.is_some()));
}

#[test]
fn discard_mode_stops_when_full() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let s = TracingSession::start(SessionConfig { buffer_size_kb: 64, ring_buffer: false, ..Default::default() }).unwrap();
    for i in 0..20_000 {
        pf::instant(&CAT_A, "spam", &[Arg::new("i", i)]);
    }
    let mut s = s;
    s.stop();
    assert!(s.stats().dropped_bytes > 0);
    assert!(s.stats().bytes <= 64 * 1024 + 256);
    let first = decode(&s.trace_bytes()).into_iter().find_map(|p| p.track_event).unwrap();
    assert_eq!(first.debug_annotations[0].int_value, Some(0));
}

#[test]
fn explicit_timestamps_are_kept() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let s = TracingSession::start(SessionConfig::default()).unwrap();
    let base = pf::now_ns() + 1_000;
    let frames = Track::named("Frames");
    let fps = CounterTrack::with_unit("fps", "fps");
    pf::slice_begin_on_at(&CAT_A, frames, base, "frame", &[Arg::new("ms", 16.6)]);
    pf::slice_end_on_at(&CAT_A, frames, base + 16_600_000);
    pf::instant_on_at(&CAT_A, frames, base + 20_000_000, "mark", &[]);
    pf::counter_at(&CAT_A, fps, base + 100_000_000, 59.5);
    pf::counter_at(&CAT_A, fps, base + 200_000_000, 60);
    let mut s = s;
    s.stop();
    let events: Vec<(u64, i32)> = decode(&s.trace_bytes())
        .into_iter()
        .filter_map(|p| Some((p.timestamp?, p.track_event?.r#type?)))
        .collect();
    assert_eq!(
        events,
        vec![
            (base, protos::TYPE_SLICE_BEGIN),
            (base + 16_600_000, protos::TYPE_SLICE_END),
            (base + 20_000_000, protos::TYPE_INSTANT),
            (base + 100_000_000, protos::TYPE_COUNTER),
            (base + 200_000_000, protos::TYPE_COUNTER),
        ]
    );
}

#[test]
fn process_name_can_change_between_sessions() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let name_in = |bytes: &[u8]| {
        decode(bytes).into_iter().find_map(|p| p.track_descriptor.and_then(|d| d.process).and_then(|p| p.process_name))
    };
    for name in ["first_proc", "second_proc"] {
        pf::init(InitArgs { backends: Backends::IN_PROCESS, process_name: Some(name.into()), ..Default::default() }).unwrap();
        let mut s = TracingSession::start(SessionConfig::default()).unwrap();
        pf::instant_on(&CAT_A, Track::process(), "x", &[]);
        s.stop();
        assert_eq!(name_in(&s.trace_bytes()).as_deref(), Some(name));
    }
    init();
}

/// Packets inside `compressed_packets` (field 50), plus the plain ones. Walks the raw
/// framing because prost drops fields it does not know.
fn inflate_all(bytes: &[u8]) -> (usize, Vec<TracePacket>) {
    use prost::encoding::{decode_key, decode_varint, WireType};
    use std::io::Read;
    fn split(mut b: &[u8]) -> Vec<(u32, &[u8])> {
        let mut out = Vec::new();
        while !b.is_empty() {
            let (field, wire) = decode_key(&mut b).unwrap();
            match wire {
                WireType::LengthDelimited => {
                    let len = decode_varint(&mut b).unwrap() as usize;
                    out.push((field, &b[..len]));
                    b = &b[len..];
                }
                WireType::Varint => {
                    decode_varint(&mut b).unwrap();
                }
                other => panic!("unexpected wire type {other:?}"),
            }
        }
        out
    }
    let mut compressed = 0;
    let mut out = Vec::new();
    for (_, packet) in split(bytes) {
        match split(packet).into_iter().find(|(field, _)| *field == 50) {
            Some((_, deflated)) => {
                compressed += 1;
                let mut inflated = Vec::new();
                flate2::read::ZlibDecoder::new(deflated).read_to_end(&mut inflated).unwrap();
                out.extend(decode(&inflated));
            }
            None => out.push(TracePacket::decode(packet).unwrap()),
        }
    }
    (compressed, out)
}

#[test]
fn zlib_compression_round_trips_in_memory_and_streaming() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let emit = || {
        let track = Track::named("Logs");
        for i in 0..60_000 {
            pf::instant_on(&CAT_A, track, "repeated log line text", &[Arg::new("i", i as i64)]);
        }
    };
    let plain = {
        let mut s = TracingSession::start(SessionConfig::default()).unwrap();
        emit();
        s.stop();
        s.trace_bytes()
    };
    let memory = {
        let mut s = TracingSession::start(SessionConfig { compression: pf::Compression::Zlib, ..Default::default() }).unwrap();
        emit();
        s.stop();
        s.trace_bytes()
    };
    let path = std::env::temp_dir().join(format!("rperfetto_sdk_zlib_{}.pftrace", std::process::id()));
    {
        let mut s = TracingSession::start(SessionConfig {
            compression: pf::Compression::Zlib,
            output_file: Some(path.clone()),
            ..Default::default()
        })
        .unwrap();
        emit();
        s.stop();
        assert!(s.file_error().is_none());
    }
    let streamed = std::fs::read(&path).unwrap();
    let events = |packets: &[TracePacket]| packets.iter().filter(|p| p.track_event.is_some()).count();
    let plain_events = events(&decode(&plain));
    assert_eq!(plain_events, 60_000);
    for bytes in [&memory, &streamed] {
        assert!(bytes.len() * 5 < plain.len(), "{} vs {}", bytes.len(), plain.len());
        let (chunks, packets) = inflate_all(bytes);
        assert!(chunks >= 2, "expected several compressed chunks, got {chunks}");
        assert_eq!(events(&packets), plain_events);
        assert!(packets[0].clock_snapshot.is_some(), "header stays plain");
    }
    if tp_shell().is_some() {
        assert_eq!(tp_query(&path, "select count(*) from slice"), vec![vec!["60000".to_string()]]);
    }
    let _ = std::fs::remove_file(&path);
}
