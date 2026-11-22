//! Function entry/exit markers (func_begin/func_end, trace_fn!) and
//! sql/function_mips.sql on a trace without CPU counters.

use prost::Message;
use rperfetto_sdk::protos::Trace;
use rperfetto_sdk::{self as pf, func, trace_fn, Backends, Category, InitArgs, SessionConfig, TracingSession};
use std::path::PathBuf;

static OFF: Category = Category::new("func_test_off");

fn leaf() -> u32 {
    trace_fn!();
    std::hint::black_box(7)
}

fn parent() -> u32 {
    trace_fn!();
    leaf() + leaf()
}

fn named() {
    trace_fn!(func::FN_CATEGORY, "custom_name");
}

fn tp_shell() -> Option<PathBuf> {
    let p = std::env::var_os("TRACE_PROCESSOR_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/damody/work/perfetto-arm64/out/linux_release/trace_processor_shell"));
    p.exists().then_some(p)
}

#[test]
fn func_markers() {
    pf::init(InitArgs { backends: Backends::IN_PROCESS, ..Default::default() }).unwrap();
    let mut s = TracingSession::start(SessionConfig::default()).unwrap();
    parent();
    named();
    // Disabled category: begin/end still pair, nothing is recorded.
    OFF.set_enabled(false);
    func::func_begin(&OFF, "hidden");
    assert_eq!(func::func_depth(), 1);
    // Enabled again in between: the end must not close a recorded slice.
    OFF.set_enabled(true);
    func::func_begin(&func::FN_CATEGORY, "inner_visible");
    func::func_end();
    func::func_end();
    assert_eq!(func::func_depth(), 0);
    func::func_end(); // unmatched: ignored
    s.stop();

    let trace = Trace::decode(&s.trace_bytes()[..]).unwrap();
    let names: Vec<String> = trace.packet.iter().filter_map(|p| p.track_event.as_ref()?.name.clone()).collect();
    assert_eq!(names, ["func::parent", "func::leaf", "func::leaf", "custom_name", "inner_visible"]);
    let ends = trace
        .packet
        .iter()
        .filter(|p| p.track_event.as_ref().is_some_and(|t| t.r#type == Some(rperfetto_sdk::protos::TYPE_SLICE_END)))
        .count();
    assert_eq!(ends, 5);

    let Some(tp) = tp_shell() else { return };
    let path = std::env::temp_dir().join(format!("rpf_func_{}.pftrace", std::process::id()));
    s.write_to_file(&path).unwrap();
    let sql = concat!(env!("CARGO_MANIFEST_DIR"), "/sql/function_mips.sql");
    let out = std::process::Command::new(tp).arg("-q").arg(sql).arg(&path).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with('"')).collect();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(rows[0].starts_with("\"name\",\"calls\""), "{rows:?}");
    let leaf = rows.iter().find(|r| r.starts_with("\"func::leaf\"")).expect("leaf row");
    assert!(leaf.starts_with("\"func::leaf\",2,"), "{leaf}");
    let _ = std::fs::remove_file(&path);
}
