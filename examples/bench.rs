//! Rough per-event cost of the in-process backend.
//!   cargo run --release --example bench
use rperfetto_sdk::{self as pf, Arg, Backends, Category, InitArgs, SessionConfig, TracingSession};
use std::time::Instant;

static CAT: Category = Category::new("bench");
static OFF: Category = Category::new("bench_off");

fn main() {
    pf::init(InitArgs { backends: Backends::IN_PROCESS, ..Default::default() }).unwrap();
    OFF.set_enabled(false);
    let n = 1_000_000u32;
    let mut s = TracingSession::start(SessionConfig { buffer_size_kb: 512 * 1024, ..Default::default() }).unwrap();
    let t = Instant::now();
    for i in 0..n {
        pf::instant(&OFF, "off", &[Arg::new("i", i)]);
    }
    let off = t.elapsed().as_nanos() as f64 / n as f64;
    let t = Instant::now();
    for _ in 0..n {
        let _g = pf::scoped_slice(&CAT, "slice", &[]);
    }
    let slice = t.elapsed().as_nanos() as f64 / n as f64;
    let t = Instant::now();
    for i in 0..n {
        pf::instant(&CAT, "instant", &[Arg::new("i", i), Arg::new("s", "text")]);
    }
    let inst = t.elapsed().as_nanos() as f64 / n as f64;
    s.stop();
    println!("disabled category: {off:.1} ns/event");
    println!("scoped slice (begin+end): {slice:.1} ns");
    println!("instant + 2 args: {inst:.1} ns");
    println!("trace size: {} MB", s.stats().bytes / 1_000_000);
}
