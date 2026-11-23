//! Hand-declared `prost` messages: the subset of the Perfetto v53.1 protos this
//! crate emits or speaks over IPC.
//!
//! Field numbers are copied from `protos/perfetto/**.proto` (v53.1) and are
//! cross-checked by `tests/proto_compat.rs` against the real .proto files.
//! Perfetto `oneof`s are flattened into plain optional fields: the wire format
//! is identical as long as only one member is set.
#![allow(clippy::derive_partial_eq_without_eq)]

use prost::Message;

// ---------------------------------------------------------------------------
// trace/trace.proto, trace/trace_packet.proto
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Message)]
pub struct Trace {
    #[prost(message, repeated, tag = "1")]
    pub packet: Vec<TracePacket>,
}

pub const SEQ_INCREMENTAL_STATE_CLEARED: u32 = 1;
pub const SEQ_NEEDS_INCREMENTAL_STATE: u32 = 2;

#[derive(Clone, PartialEq, Message)]
pub struct TracePacket {
    #[prost(uint64, optional, tag = "8")]
    pub timestamp: Option<u64>,
    #[prost(uint32, optional, tag = "58")]
    pub timestamp_clock_id: Option<u32>,
    // oneof data
    #[prost(message, optional, tag = "11")]
    pub track_event: Option<TrackEvent>,
    #[prost(message, optional, tag = "60")]
    pub track_descriptor: Option<TrackDescriptor>,
    #[prost(message, optional, tag = "6")]
    pub clock_snapshot: Option<ClockSnapshot>,
    #[prost(message, optional, tag = "39")]
    pub android_log: Option<AndroidLogPacket>,
    // oneof optional_trusted_packet_sequence_id. Only for in-process output:
    // traced rejects producer packets carrying it.
    #[prost(uint32, optional, tag = "10")]
    pub trusted_packet_sequence_id: Option<u32>,
    #[prost(int32, optional, tag = "79")]
    pub trusted_pid: Option<i32>,
    #[prost(uint32, optional, tag = "13")]
    pub sequence_flags: Option<u32>,
    #[prost(bool, optional, tag = "42")]
    pub previous_packet_dropped: Option<bool>,
    #[prost(bool, optional, tag = "87")]
    pub first_packet_on_sequence: Option<bool>,
}

// common/builtin_clock.proto
pub const BUILTIN_CLOCK_REALTIME: u32 = 1;
pub const BUILTIN_CLOCK_MONOTONIC: u32 = 3;
pub const BUILTIN_CLOCK_BOOTTIME: u32 = 6;

#[derive(Clone, PartialEq, Message)]
pub struct ClockSnapshot {
    #[prost(message, repeated, tag = "1")]
    pub clocks: Vec<clock_snapshot::Clock>,
    #[prost(uint32, optional, tag = "2")]
    pub primary_trace_clock: Option<u32>,
}

pub mod clock_snapshot {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Clock {
        #[prost(uint32, optional, tag = "1")]
        pub clock_id: Option<u32>,
        #[prost(uint64, optional, tag = "2")]
        pub timestamp: Option<u64>,
    }
}

// ---------------------------------------------------------------------------
// trace/android/android_log.proto, common/android_log_constants.proto
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Message)]
pub struct AndroidLogPacket {
    #[prost(message, repeated, tag = "1")]
    pub events: Vec<android_log_packet::LogEvent>,
}

pub mod android_log_packet {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct LogEvent {
        /// `AndroidLogId`.
        #[prost(int32, optional, tag = "1")]
        pub log_id: Option<i32>,
        #[prost(int32, optional, tag = "2")]
        pub pid: Option<i32>,
        #[prost(int32, optional, tag = "3")]
        pub tid: Option<i32>,
        #[prost(int32, optional, tag = "4")]
        pub uid: Option<i32>,
        /// CLOCK_REALTIME nanoseconds.
        #[prost(uint64, optional, tag = "5")]
        pub timestamp: Option<u64>,
        #[prost(string, optional, tag = "6")]
        pub tag: Option<String>,
        /// `AndroidLogPriority`.
        #[prost(int32, optional, tag = "7")]
        pub prio: Option<i32>,
        #[prost(string, optional, tag = "8")]
        pub message: Option<String>,
    }
}

// ---------------------------------------------------------------------------
// trace/track_event/*.proto
// ---------------------------------------------------------------------------

pub const TYPE_SLICE_BEGIN: i32 = 1;
pub const TYPE_SLICE_END: i32 = 2;
pub const TYPE_INSTANT: i32 = 3;
pub const TYPE_COUNTER: i32 = 4;

#[derive(Clone, PartialEq, Message)]
pub struct TrackEvent {
    #[prost(string, repeated, tag = "22")]
    pub categories: Vec<String>,
    #[prost(string, optional, tag = "23")]
    pub name: Option<String>,
    #[prost(int32, optional, tag = "9")]
    pub r#type: Option<i32>,
    #[prost(uint64, optional, tag = "11")]
    pub track_uuid: Option<u64>,
    #[prost(int64, optional, tag = "30")]
    pub counter_value: Option<i64>,
    #[prost(double, optional, tag = "44")]
    pub double_counter_value: Option<f64>,
    #[prost(message, repeated, tag = "4")]
    pub debug_annotations: Vec<DebugAnnotation>,
    #[prost(fixed64, repeated, packed = "false", tag = "47")]
    pub flow_ids: Vec<u64>,
    #[prost(fixed64, repeated, packed = "false", tag = "48")]
    pub terminating_flow_ids: Vec<u64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct DebugAnnotation {
    #[prost(string, optional, tag = "10")]
    pub name: Option<String>,
    #[prost(bool, optional, tag = "2")]
    pub bool_value: Option<bool>,
    #[prost(uint64, optional, tag = "3")]
    pub uint_value: Option<u64>,
    #[prost(int64, optional, tag = "4")]
    pub int_value: Option<i64>,
    #[prost(double, optional, tag = "5")]
    pub double_value: Option<f64>,
    #[prost(string, optional, tag = "6")]
    pub string_value: Option<String>,
    #[prost(uint64, optional, tag = "7")]
    pub pointer_value: Option<u64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TrackDescriptor {
    #[prost(uint64, optional, tag = "1")]
    pub uuid: Option<u64>,
    #[prost(uint64, optional, tag = "5")]
    pub parent_uuid: Option<u64>,
    #[prost(string, optional, tag = "2")]
    pub name: Option<String>,
    #[prost(message, optional, tag = "3")]
    pub process: Option<ProcessDescriptor>,
    #[prost(message, optional, tag = "4")]
    pub thread: Option<ThreadDescriptor>,
    #[prost(message, optional, tag = "8")]
    pub counter: Option<CounterDescriptor>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ProcessDescriptor {
    #[prost(int32, optional, tag = "1")]
    pub pid: Option<i32>,
    #[prost(string, optional, tag = "6")]
    pub process_name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ThreadDescriptor {
    #[prost(int32, optional, tag = "1")]
    pub pid: Option<i32>,
    #[prost(int32, optional, tag = "2")]
    pub tid: Option<i32>,
    #[prost(string, optional, tag = "5")]
    pub thread_name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct CounterDescriptor {
    #[prost(string, optional, tag = "6")]
    pub unit_name: Option<String>,
    #[prost(bool, optional, tag = "5")]
    pub is_incremental: Option<bool>,
}

// ---------------------------------------------------------------------------
// config/data_source_config.proto, config/track_event/track_event_config.proto
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Message)]
pub struct DataSourceConfig {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(uint32, optional, tag = "2")]
    pub target_buffer: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub trace_duration_ms: Option<u32>,
    #[prost(message, optional, tag = "113")]
    pub track_event_config: Option<TrackEventConfig>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TrackEventConfig {
    #[prost(string, repeated, tag = "1")]
    pub disabled_categories: Vec<String>,
    #[prost(string, repeated, tag = "2")]
    pub enabled_categories: Vec<String>,
    #[prost(string, repeated, tag = "3")]
    pub disabled_tags: Vec<String>,
    #[prost(string, repeated, tag = "4")]
    pub enabled_tags: Vec<String>,
}

// common/data_source_descriptor.proto, common/track_event_descriptor.proto
#[derive(Clone, PartialEq, Message)]
pub struct DataSourceDescriptor {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(bool, optional, tag = "2")]
    pub will_notify_on_stop: Option<bool>,
    #[prost(bool, optional, tag = "3")]
    pub will_notify_on_start: Option<bool>,
    #[prost(bool, optional, tag = "4")]
    pub handles_incremental_state_clear: Option<bool>,
    #[prost(message, optional, tag = "6")]
    pub track_event_descriptor: Option<TrackEventDescriptor>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TrackEventDescriptor {
    #[prost(message, repeated, tag = "1")]
    pub available_categories: Vec<TrackEventCategory>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TrackEventCategory {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub description: Option<String>,
    #[prost(string, repeated, tag = "3")]
    pub tags: Vec<String>,
}

// common/commit_data_request.proto
#[derive(Clone, PartialEq, Message)]
pub struct CommitDataRequest {
    #[prost(message, repeated, tag = "1")]
    pub chunks_to_move: Vec<ChunksToMove>,
    #[prost(uint64, optional, tag = "3")]
    pub flush_request_id: Option<u64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ChunksToMove {
    #[prost(uint32, optional, tag = "1")]
    pub page: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub chunk: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub target_buffer: Option<u32>,
}

// ---------------------------------------------------------------------------
// ipc/wire_protocol.proto
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Message)]
pub struct IpcFrame {
    #[prost(uint64, optional, tag = "2")]
    pub request_id: Option<u64>,
    #[prost(message, optional, tag = "3")]
    pub msg_bind_service: Option<BindService>,
    #[prost(message, optional, tag = "4")]
    pub msg_bind_service_reply: Option<BindServiceReply>,
    #[prost(message, optional, tag = "5")]
    pub msg_invoke_method: Option<InvokeMethod>,
    #[prost(message, optional, tag = "6")]
    pub msg_invoke_method_reply: Option<InvokeMethodReply>,
    #[prost(message, optional, tag = "7")]
    pub msg_request_error: Option<RequestError>,
}

#[derive(Clone, PartialEq, Message)]
pub struct BindService {
    #[prost(string, optional, tag = "1")]
    pub service_name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct BindServiceReply {
    #[prost(bool, optional, tag = "1")]
    pub success: Option<bool>,
    #[prost(uint32, optional, tag = "2")]
    pub service_id: Option<u32>,
    #[prost(message, repeated, tag = "3")]
    pub methods: Vec<MethodInfo>,
}

#[derive(Clone, PartialEq, Message)]
pub struct MethodInfo {
    #[prost(uint32, optional, tag = "1")]
    pub id: Option<u32>,
    #[prost(string, optional, tag = "2")]
    pub name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct InvokeMethod {
    #[prost(uint32, optional, tag = "1")]
    pub service_id: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub method_id: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub args_proto: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "4")]
    pub drop_reply: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct InvokeMethodReply {
    #[prost(bool, optional, tag = "1")]
    pub success: Option<bool>,
    #[prost(bool, optional, tag = "2")]
    pub has_more: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub reply_proto: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct RequestError {
    #[prost(string, optional, tag = "1")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// ipc/producer_port.proto
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Message)]
pub struct InitializeConnectionRequest {
    #[prost(uint32, optional, tag = "1")]
    pub shared_memory_page_size_hint_bytes: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub shared_memory_size_hint_bytes: Option<u32>,
    #[prost(string, optional, tag = "3")]
    pub producer_name: Option<String>,
    #[prost(string, optional, tag = "8")]
    pub sdk_version: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct InitializeConnectionResponse {
    #[prost(bool, optional, tag = "1")]
    pub using_shmem_provided_by_producer: Option<bool>,
    #[prost(bool, optional, tag = "2")]
    pub direct_smb_patching_supported: Option<bool>,
    #[prost(bool, optional, tag = "3")]
    pub use_shmem_emulation: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct RegisterDataSourceRequest {
    #[prost(message, optional, tag = "1")]
    pub data_source_descriptor: Option<DataSourceDescriptor>,
}

#[derive(Clone, PartialEq, Message)]
pub struct RegisterDataSourceResponse {
    #[prost(string, optional, tag = "1")]
    pub error: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct RegisterTraceWriterRequest {
    #[prost(uint32, optional, tag = "1")]
    pub trace_writer_id: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub target_buffer: Option<u32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct UnregisterTraceWriterRequest {
    #[prost(uint32, optional, tag = "1")]
    pub trace_writer_id: Option<u32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct NotifyDataSourceRequest {
    /// NotifyDataSourceStartedRequest / NotifyDataSourceStoppedRequest.
    #[prost(uint64, optional, tag = "1")]
    pub data_source_id: Option<u64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct GetAsyncCommandRequest {}

#[derive(Clone, PartialEq, Message)]
pub struct GetAsyncCommandResponse {
    #[prost(message, optional, tag = "3")]
    pub setup_tracing: Option<SetupTracing>,
    #[prost(message, optional, tag = "6")]
    pub setup_data_source: Option<StartDataSource>,
    #[prost(message, optional, tag = "1")]
    pub start_data_source: Option<StartDataSource>,
    #[prost(message, optional, tag = "2")]
    pub stop_data_source: Option<StopDataSource>,
    #[prost(message, optional, tag = "5")]
    pub flush: Option<Flush>,
    #[prost(message, optional, tag = "7")]
    pub clear_incremental_state: Option<ClearIncrementalState>,
}

#[derive(Clone, PartialEq, Message)]
pub struct SetupTracing {
    #[prost(uint32, optional, tag = "1")]
    pub shared_buffer_page_size_kb: Option<u32>,
}

/// SetupDataSource and StartDataSource have the same layout.
#[derive(Clone, PartialEq, Message)]
pub struct StartDataSource {
    #[prost(uint64, optional, tag = "1")]
    pub new_instance_id: Option<u64>,
    #[prost(message, optional, tag = "2")]
    pub config: Option<DataSourceConfig>,
}

#[derive(Clone, PartialEq, Message)]
pub struct StopDataSource {
    #[prost(uint64, optional, tag = "1")]
    pub instance_id: Option<u64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Flush {
    #[prost(uint64, repeated, packed = "false", tag = "1")]
    pub data_source_ids: Vec<u64>,
    #[prost(uint64, optional, tag = "2")]
    pub request_id: Option<u64>,
    #[prost(uint64, optional, tag = "3")]
    pub flags: Option<u64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ClearIncrementalState {
    #[prost(uint64, repeated, packed = "false", tag = "1")]
    pub data_source_ids: Vec<u64>,
}

/// Appends `msg` to `out` as field 1 of `perfetto.protos.Trace`, i.e. in the
/// on-disk `.pftrace` framing.
pub fn append_trace_packet(out: &mut Vec<u8>, packet_bytes: &[u8]) {
    prost::encoding::encode_key(1, prost::encoding::WireType::LengthDelimited, out);
    prost::encoding::encode_varint(packet_bytes.len() as u64, out);
    out.extend_from_slice(packet_bytes);
}

pub fn encode<M: Message>(m: &M) -> Vec<u8> {
    m.encode_to_vec()
}
