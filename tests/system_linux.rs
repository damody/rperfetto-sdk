//! System backend end-to-end on Linux: starts a private `traced`, records a
//! `track_event` session with the `perfetto` client while this process emits
//! events, and checks the result with trace_processor_shell.
//! Skipped when the Perfetto binaries are not available (set PERFETTO_OUT).
#![cfg(target_os = "linux")]

use rperfetto_sdk::{self as pf, Arg, Backends, Category, CounterTrack, InitArgs};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

static SYS_ON: Category = Category::new("sys_on");
static SYS_OFF: Category = Category::new("sys_off");

struct Kill(std::process::Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn query(tp: &PathBuf, trace: &PathBuf, sql: &str) -> Vec<String> {
    let out = Command::new(tp).arg("-Q").arg(sql).arg(trace).stderr(Stdio::null()).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with('"') || l.starts_with(|c: char| c.is_ascii_digit()))
        .skip(1)
        .map(str::to_string)
        .collect()
}

#[test]
fn events_reach_traced() {
    let bin = std::env::var_os("PERFETTO_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../perfetto-arm64/out/linux_release")));
    let (traced, perfetto, tp) = (bin.join("traced"), bin.join("perfetto"), bin.join("trace_processor_shell"));
    if !traced.exists() || !perfetto.exists() || !tp.exists() {
        eprintln!("perfetto binaries not found in {bin:?}; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("rpf_sys_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let prod = dir.join("prod.sock");
    let cons = dir.join("cons.sock");
    let out = dir.join("sys.pftrace");
    let _traced = Kill(
        Command::new(&traced)
            .env("PERFETTO_PRODUCER_SOCK_NAME", &prod)
            .env("PERFETTO_CONSUMER_SOCK_NAME", &cons)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let t0 = Instant::now();
    while !prod.exists() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }

    pf::init(InitArgs {
        backends: Backends::SYSTEM,
        producer_name: "rpf_test".into(),
        producer_socket: Some(prod.to_string_lossy().into_owned()),
        process_name: Some("rpf_sys_test".into()),
        retry_connect: false,
        ..Default::default()
    })
    .expect("connect to traced");
    // Registers the categories before the session starts.
    assert!(!SYS_ON.is_enabled() && !SYS_OFF.is_enabled());

    let cfg = r#"
        buffers { size_kb: 4096 }
        data_sources { config { name: "track_event"
            track_event_config { disabled_categories: "*" enabled_categories: "sys_on" } } }
        duration_ms: 1500
    "#;
    let mut client = Command::new(&perfetto)
        .env("PERFETTO_CONSUMER_SOCK_NAME", &cons)
        .args(["--txt", "-c", "-", "-o"])
        .arg(&out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    client.stdin.take().unwrap().write_all(cfg.as_bytes()).unwrap();

    let t0 = Instant::now();
    while !pf::system::is_tracing() {
        assert!(t0.elapsed() < Duration::from_secs(5), "data source never started");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(SYS_ON.is_enabled());
    assert!(!SYS_OFF.is_enabled(), "filtered by TrackEventConfig");

    let ctr = CounterTrack::new("sys_counter");
    let mut n = 0u32;
    // Several short-lived threads exercise writer-id reuse.
    while pf::system::is_tracing() && n < 400 {
        let _s = pf::scoped_slice(&SYS_ON, "sys_outer", &[Arg::new("n", n)]);
        pf::instant(&SYS_ON, "sys_inner", &[Arg::new("big", "x".repeat(6000).as_str())]);
        pf::counter(&SYS_ON, ctr, n as i64);
        if n % 50 == 0 {
            std::thread::spawn(|| pf::instant(&SYS_ON, "sys_thread", &[])).join().unwrap();
        }
        n += 1;
        std::thread::sleep(Duration::from_millis(2));
    }
    let status = client.wait().unwrap();
    assert!(status.success());
    pf::shutdown();

    let rows = query(&tp, &out, "select name, count(*) from slice group by name order by name");
    let count = |name: &str| -> u32 {
        rows.iter()
            .find(|r| r.starts_with(&format!("\"{name}\"")))
            .map(|r| r.rsplit(',').next().unwrap().parse().unwrap())
            .unwrap_or(0)
    };
    let outer = count("sys_outer");
    assert!(outer >= n.saturating_sub(2) && outer > 50, "outer={outer} n={n} rows={rows:?}");
    assert_eq!(count("sys_inner"), outer, "fragmented 6KB packets all arrive");
    assert!(count("sys_thread") >= 1);
    let errs = query(&tp, &out, "select name, value from stats where value > 0 and severity in ('error','data_loss')");
    assert!(errs.is_empty(), "errors: {errs:?}");
    let proc_ = query(&tp, &out, "select p.name from slice s join thread_track tt on s.track_id = tt.id join thread using(utid) join process p using(upid) where s.name = 'sys_outer' limit 1");
    assert_eq!(proc_, vec!["\"rpf_sys_test\"".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}
