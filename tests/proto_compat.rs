//! Cross-checks the hand-declared prost field numbers in `src/protos.rs`
//! against the Perfetto v53.1 `.proto` sources (skipped if not available).
//! Set PERFETTO_SRC to the perfetto checkout.

use std::path::PathBuf;

/// (proto file, message, field, number)
const FIELDS: &[(&str, &str, &str, u32)] = &[
    ("trace/trace.proto", "Trace", "packet", 1),
    ("trace/trace_packet.proto", "TracePacket", "timestamp", 8),
    ("trace/trace_packet.proto", "TracePacket", "timestamp_clock_id", 58),
    ("trace/trace_packet.proto", "TracePacket", "track_event", 11),
    ("trace/trace_packet.proto", "TracePacket", "track_descriptor", 60),
    ("trace/trace_packet.proto", "TracePacket", "clock_snapshot", 6),
    ("trace/trace_packet.proto", "TracePacket", "trusted_packet_sequence_id", 10),
    ("trace/trace_packet.proto", "TracePacket", "trusted_pid", 79),
    ("trace/trace_packet.proto", "TracePacket", "sequence_flags", 13),
    ("trace/trace_packet.proto", "TracePacket", "previous_packet_dropped", 42),
    ("trace/trace_packet.proto", "TracePacket", "first_packet_on_sequence", 87),
    ("trace/trace_packet.proto", "TracePacket", "android_log", 39),
    ("trace/android/android_log.proto", "AndroidLogPacket", "events", 1),
    ("trace/android/android_log.proto", "LogEvent", "log_id", 1),
    ("trace/android/android_log.proto", "LogEvent", "pid", 2),
    ("trace/android/android_log.proto", "LogEvent", "tid", 3),
    ("trace/android/android_log.proto", "LogEvent", "uid", 4),
    ("trace/android/android_log.proto", "LogEvent", "timestamp", 5),
    ("trace/android/android_log.proto", "LogEvent", "tag", 6),
    ("trace/android/android_log.proto", "LogEvent", "prio", 7),
    ("trace/android/android_log.proto", "LogEvent", "message", 8),
    ("trace/clock_snapshot.proto", "ClockSnapshot", "clocks", 1),
    ("trace/clock_snapshot.proto", "ClockSnapshot", "primary_trace_clock", 2),
    ("trace/clock_snapshot.proto", "Clock", "clock_id", 1),
    ("trace/clock_snapshot.proto", "Clock", "timestamp", 2),
    ("trace/track_event/track_event.proto", "TrackEvent", "categories", 22),
    ("trace/track_event/track_event.proto", "TrackEvent", "name", 23),
    ("trace/track_event/track_event.proto", "TrackEvent", "type", 9),
    ("trace/track_event/track_event.proto", "TrackEvent", "track_uuid", 11),
    ("trace/track_event/track_event.proto", "TrackEvent", "counter_value", 30),
    ("trace/track_event/track_event.proto", "TrackEvent", "double_counter_value", 44),
    ("trace/track_event/track_event.proto", "TrackEvent", "debug_annotations", 4),
    ("trace/track_event/track_event.proto", "TrackEvent", "flow_ids", 47),
    ("trace/track_event/track_event.proto", "TrackEvent", "terminating_flow_ids", 48),
    ("trace/track_event/debug_annotation.proto", "DebugAnnotation", "name", 10),
    ("trace/track_event/debug_annotation.proto", "DebugAnnotation", "bool_value", 2),
    ("trace/track_event/debug_annotation.proto", "DebugAnnotation", "uint_value", 3),
    ("trace/track_event/debug_annotation.proto", "DebugAnnotation", "int_value", 4),
    ("trace/track_event/debug_annotation.proto", "DebugAnnotation", "double_value", 5),
    ("trace/track_event/debug_annotation.proto", "DebugAnnotation", "string_value", 6),
    ("trace/track_event/debug_annotation.proto", "DebugAnnotation", "pointer_value", 7),
    ("trace/track_event/track_descriptor.proto", "TrackDescriptor", "uuid", 1),
    ("trace/track_event/track_descriptor.proto", "TrackDescriptor", "parent_uuid", 5),
    ("trace/track_event/track_descriptor.proto", "TrackDescriptor", "name", 2),
    ("trace/track_event/track_descriptor.proto", "TrackDescriptor", "process", 3),
    ("trace/track_event/track_descriptor.proto", "TrackDescriptor", "thread", 4),
    ("trace/track_event/track_descriptor.proto", "TrackDescriptor", "counter", 8),
    ("trace/track_event/process_descriptor.proto", "ProcessDescriptor", "pid", 1),
    ("trace/track_event/process_descriptor.proto", "ProcessDescriptor", "process_name", 6),
    ("trace/track_event/thread_descriptor.proto", "ThreadDescriptor", "pid", 1),
    ("trace/track_event/thread_descriptor.proto", "ThreadDescriptor", "tid", 2),
    ("trace/track_event/thread_descriptor.proto", "ThreadDescriptor", "thread_name", 5),
    ("trace/track_event/counter_descriptor.proto", "CounterDescriptor", "unit_name", 6),
    ("trace/track_event/counter_descriptor.proto", "CounterDescriptor", "is_incremental", 5),
    ("config/data_source_config.proto", "DataSourceConfig", "name", 1),
    ("config/data_source_config.proto", "DataSourceConfig", "target_buffer", 2),
    ("config/data_source_config.proto", "DataSourceConfig", "trace_duration_ms", 3),
    ("config/data_source_config.proto", "DataSourceConfig", "track_event_config", 113),
    ("config/track_event/track_event_config.proto", "TrackEventConfig", "disabled_categories", 1),
    ("config/track_event/track_event_config.proto", "TrackEventConfig", "enabled_categories", 2),
    ("common/data_source_descriptor.proto", "DataSourceDescriptor", "name", 1),
    ("common/data_source_descriptor.proto", "DataSourceDescriptor", "will_notify_on_stop", 2),
    ("common/data_source_descriptor.proto", "DataSourceDescriptor", "will_notify_on_start", 3),
    ("common/data_source_descriptor.proto", "DataSourceDescriptor", "handles_incremental_state_clear", 4),
    ("common/data_source_descriptor.proto", "DataSourceDescriptor", "track_event_descriptor", 6),
    ("common/track_event_descriptor.proto", "TrackEventDescriptor", "available_categories", 1),
    ("common/track_event_descriptor.proto", "TrackEventCategory", "name", 1),
    ("common/track_event_descriptor.proto", "TrackEventCategory", "description", 2),
    ("common/commit_data_request.proto", "CommitDataRequest", "chunks_to_move", 1),
    ("common/commit_data_request.proto", "CommitDataRequest", "flush_request_id", 3),
    ("common/commit_data_request.proto", "ChunksToMove", "page", 1),
    ("common/commit_data_request.proto", "ChunksToMove", "chunk", 2),
    ("common/commit_data_request.proto", "ChunksToMove", "target_buffer", 3),
    ("ipc/wire_protocol.proto", "IPCFrame", "request_id", 2),
    ("ipc/wire_protocol.proto", "IPCFrame", "msg_bind_service", 3),
    ("ipc/wire_protocol.proto", "IPCFrame", "msg_bind_service_reply", 4),
    ("ipc/wire_protocol.proto", "IPCFrame", "msg_invoke_method", 5),
    ("ipc/wire_protocol.proto", "IPCFrame", "msg_invoke_method_reply", 6),
    ("ipc/wire_protocol.proto", "IPCFrame", "msg_request_error", 7),
    ("ipc/wire_protocol.proto", "BindService", "service_name", 1),
    ("ipc/wire_protocol.proto", "BindServiceReply", "success", 1),
    ("ipc/wire_protocol.proto", "BindServiceReply", "service_id", 2),
    ("ipc/wire_protocol.proto", "BindServiceReply", "methods", 3),
    ("ipc/wire_protocol.proto", "MethodInfo", "id", 1),
    ("ipc/wire_protocol.proto", "MethodInfo", "name", 2),
    ("ipc/wire_protocol.proto", "InvokeMethod", "service_id", 1),
    ("ipc/wire_protocol.proto", "InvokeMethod", "method_id", 2),
    ("ipc/wire_protocol.proto", "InvokeMethod", "args_proto", 3),
    ("ipc/wire_protocol.proto", "InvokeMethod", "drop_reply", 4),
    ("ipc/wire_protocol.proto", "InvokeMethodReply", "success", 1),
    ("ipc/wire_protocol.proto", "InvokeMethodReply", "has_more", 2),
    ("ipc/wire_protocol.proto", "InvokeMethodReply", "reply_proto", 3),
    ("ipc/wire_protocol.proto", "RequestError", "error", 1),
    ("ipc/producer_port.proto", "InitializeConnectionRequest", "shared_memory_page_size_hint_bytes", 1),
    ("ipc/producer_port.proto", "InitializeConnectionRequest", "shared_memory_size_hint_bytes", 2),
    ("ipc/producer_port.proto", "InitializeConnectionRequest", "producer_name", 3),
    ("ipc/producer_port.proto", "InitializeConnectionRequest", "sdk_version", 8),
    ("ipc/producer_port.proto", "InitializeConnectionResponse", "using_shmem_provided_by_producer", 1),
    ("ipc/producer_port.proto", "InitializeConnectionResponse", "direct_smb_patching_supported", 2),
    ("ipc/producer_port.proto", "InitializeConnectionResponse", "use_shmem_emulation", 3),
    ("ipc/producer_port.proto", "RegisterDataSourceRequest", "data_source_descriptor", 1),
    ("ipc/producer_port.proto", "RegisterTraceWriterRequest", "trace_writer_id", 1),
    ("ipc/producer_port.proto", "RegisterTraceWriterRequest", "target_buffer", 2),
    ("ipc/producer_port.proto", "NotifyDataSourceStoppedRequest", "data_source_id", 1),
    ("ipc/producer_port.proto", "GetAsyncCommandResponse", "setup_tracing", 3),
    ("ipc/producer_port.proto", "GetAsyncCommandResponse", "setup_data_source", 6),
    ("ipc/producer_port.proto", "GetAsyncCommandResponse", "start_data_source", 1),
    ("ipc/producer_port.proto", "GetAsyncCommandResponse", "stop_data_source", 2),
    ("ipc/producer_port.proto", "GetAsyncCommandResponse", "flush", 5),
    ("ipc/producer_port.proto", "GetAsyncCommandResponse", "clear_incremental_state", 7),
    ("ipc/producer_port.proto", "SetupTracing", "shared_buffer_page_size_kb", 1),
    ("ipc/producer_port.proto", "StartDataSource", "new_instance_id", 1),
    ("ipc/producer_port.proto", "StartDataSource", "config", 2),
    ("ipc/producer_port.proto", "StopDataSource", "instance_id", 1),
    ("ipc/producer_port.proto", "Flush", "data_source_ids", 1),
    ("ipc/producer_port.proto", "Flush", "request_id", 2),
    ("ipc/producer_port.proto", "ClearIncrementalState", "data_source_ids", 1),
];

/// Returns the body of `message <name> {...}` (brace matched).
fn message_body<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("message {name} {{");
    let start = src.find(&pat)? + pat.len();
    let mut depth = 1;
    for (i, c) in src[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[start..start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Field number of `field` declared directly in `body` (nested messages and
/// enums skipped; `oneof` members count as direct fields).
fn field_number(body: &str, field: &str) -> Option<u32> {
    // Stack of open blocks: true = transparent (oneof).
    let mut stack: Vec<bool> = Vec::new();
    for line in body.lines() {
        let l = line.split("//").next().unwrap().trim();
        if stack.iter().all(|&t| t) {
            let toks: Vec<&str> = l
                .split(|c: char| c.is_whitespace() || c == ';' || c == '[')
                .filter(|t| !t.is_empty())
                .collect();
            if let Some(eq) = toks.iter().position(|t| *t == "=") {
                if eq >= 1 && toks[eq - 1] == field {
                    return toks.get(eq + 1)?.parse().ok();
                }
            }
        }
        for c in l.chars() {
            match c {
                '{' => stack.push(l.starts_with("oneof ")),
                '}' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }
    None
}

#[test]
fn field_numbers_match_perfetto_v53() {
    let root = std::env::var_os("PERFETTO_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/damody/work/perfetto-arm64"))
        .join("protos/perfetto");
    if !root.exists() {
        eprintln!("perfetto sources not found; skipping");
        return;
    }
    let mut bad = Vec::new();
    for &(file, msg, field, num) in FIELDS {
        let src = std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        let got = message_body(&src, msg).and_then(|b| field_number(b, field));
        if got != Some(num) {
            bad.push(format!("{file} {msg}.{field}: expected {num}, found {got:?}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
