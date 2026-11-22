//! Function entry/exit markers for per-function MIPS, without perf counters
//! in the app (no root needed).
//!
//!   cargo run --release --example fn_mips_demo -- --system 10   # into the system trace
//!   cargo run --release --example fn_mips_demo -- out.pftrace    # in-process (slices only)
//!
//! Record the system trace with `configs/function_mips.cfg` (sched + per-CPU
//! instruction counters + track_event), then compute MIPS per function with
//! `sql/function_mips.sql`. See README "Function MIPS".

use rperfetto_sdk::{self as pf, trace_fn, Backends, InitArgs, SessionConfig, TracingSession};
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Dependent integer ops: high IPC → high MIPS.
fn alu_work(n: u64) -> u64 {
    trace_fn!();
    let mut x = 0x9e3779b97f4a7c15u64;
    for i in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x = x.wrapping_add(i);
    }
    black_box(x)
}

/// Random pointer chasing through 64 MiB: cache misses → low MIPS.
fn pointer_chase(next: &[u32], steps: usize) -> u32 {
    trace_fn!();
    let mut p = 0u32;
    for _ in 0..steps {
        p = next[p as usize];
    }
    black_box(p)
}

/// Long-latency FP chain (sqrt/div).
fn fp_work(n: u32) -> f64 {
    trace_fn!();
    let mut v = 1.2345f64;
    for i in 0..n {
        v = (v * 1.000001 + i as f64).sqrt() / 1.0000003;
    }
    black_box(v)
}

/// Mostly asleep: wall-time MIPS is low, on-CPU MIPS is not.
fn sleepy() {
    trace_fn!();
    alu_work(200_000);
    std::thread::sleep(Duration::from_millis(4));
}

/// Parent of all the above (inclusive vs self numbers).
fn frame(next: &[u32]) {
    trace_fn!(pf::func::FN_CATEGORY, "frame");
    alu_work(2_000_000);
    pointer_chase(next, 300_000);
    fp_work(200_000);
    sleepy();
}

/// Sattolo's algorithm: one random cycle through all slots.
fn make_chain(len: usize) -> Vec<u32> {
    let mut v: Vec<u32> = (0..len as u32).collect();
    let mut s = 0x2545F4914F6CDD1Du64;
    for i in (1..len).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % i as u64) as usize);
    }
    v
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let next = make_chain(16 << 20);
    pf::set_thread_name("fn-main");
    if args.first().map(String::as_str) == Some("--system") {
        let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
        pf::init(InitArgs { backends: Backends::SYSTEM, producer_name: "rperfetto_fn_demo".into(), ..Default::default() })
            .unwrap();
        let end = Instant::now() + Duration::from_secs(secs);
        let mut n = 0;
        while Instant::now() < end {
            frame(&next);
            n += 1;
        }
        pf::shutdown();
        eprintln!("{n} frames");
        return;
    }
    let out = args.first().cloned().unwrap_or_else(|| "fn_mips_demo.pftrace".into());
    pf::init(InitArgs { backends: Backends::IN_PROCESS, ..Default::default() }).unwrap();
    let session = TracingSession::start(SessionConfig::default()).unwrap();
    for _ in 0..5 {
        frame(&next);
    }
    session.stop_and_write(&out).unwrap();
    pf::shutdown();
    eprintln!("wrote {out}");
}
