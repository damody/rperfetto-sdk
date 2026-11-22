//! Rust example.
//!
//! In-process (default): writes `rust_demo.pftrace`.
//!   cargo run --example rust_demo -- [out.pftrace]
//! System: events go to the running traced session (Linux/Android).
//!   cargo run --example rust_demo -- --system [seconds]
//! ATRACE: events go to ftrace trace_marker.
//!   cargo run --example rust_demo -- --atrace [seconds]

use rperfetto_sdk::{self as pf, Arg, Backends, Category, CounterTrack, InitArgs, SessionConfig, Track, TracingSession};
use std::time::{Duration, Instant};

static GAME: Category = Category::with_description("game", "Game loop");
static RENDER: Category = Category::new("render");
static DEBUG: Category = Category::new("debug");

fn busy(d: Duration) {
    let t = Instant::now();
    while t.elapsed() < d {
        std::hint::spin_loop();
    }
}

fn frame(i: u32, fps: CounterTrack, loading: Track) {
    let _f = pf::trace_scope!(GAME, "frame", "index" => i, "dt_ms" => 16.6, "scene" => "forest");
    {
        let _u = pf::trace_scope!(GAME, "update");
        busy(Duration::from_millis(2));
        pf::trace_instant!(GAME, "input", "key" => "W", "pressed" => true);
    }
    {
        let _r = pf::scoped_slice(&RENDER, "render", &[Arg::new("draw_calls", 120u32)]);
        busy(Duration::from_millis(3));
        pf::slice_begin(&RENDER, "submit", &[]);
        busy(Duration::from_millis(1));
        pf::slice_end(&RENDER);
    }
    if i % 2 == 0 {
        pf::slice_begin_on(&GAME, loading, "load_chunk", &[Arg::new("chunk", i as i64)]);
    } else {
        pf::slice_end_on(&GAME, loading);
    }
    pf::counter(&GAME, fps, 60.0 - i as f64);
    pf::trace_counter!(GAME, "entities", 100 + i as i64);
    // Disabled by the app: never recorded.
    pf::instant(&DEBUG, "debug_only", &[]);
}

fn run(frames: u32) {
    pf::set_thread_name("game-main");
    DEBUG.set_enabled(false);
    let fps = CounterTrack::with_unit("fps", "fps");
    let loading = Track::named("Loading");
    let worker = std::thread::Builder::new()
        .name("worker".into())
        .spawn(move || {
            for j in 0..frames {
                let _s = pf::trace_scope!(GAME, "job", "id" => j);
                busy(Duration::from_millis(5));
            }
        })
        .unwrap();
    for i in 0..frames {
        frame(i, fps, loading);
        busy(Duration::from_millis(5));
    }
    worker.join().unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--system") | Some("--atrace") => {
            let backend = if args[0] == "--system" { Backends::SYSTEM } else { Backends::ATRACE };
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
            pf::init(InitArgs { backends: backend, producer_name: "rperfetto_demo".into(), ..Default::default() })
                .expect("init");
            let end = Instant::now() + Duration::from_secs(secs);
            let mut n = 0;
            while Instant::now() < end {
                run(10);
                n += 10;
            }
            pf::shutdown();
            eprintln!("emitted {n} frames");
        }
        _ => {
            let out = args.first().cloned().unwrap_or_else(|| "rust_demo.pftrace".into());
            pf::init(InitArgs { backends: Backends::IN_PROCESS, ..Default::default() }).expect("init");
            let session = TracingSession::start(SessionConfig::default()).expect("start");
            run(6);
            session.stop_and_write(&out).expect("write");
            pf::shutdown();
            eprintln!("wrote {out}");
        }
    }
}
