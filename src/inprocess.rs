//! In-process backend: the application records its own events into an
//! in-memory buffer and writes a standalone `.pftrace` file.

use crate::category::{set_sink_filter, CategoryFilter, SINK_INPROC};
use crate::platform;
use crate::protos::{self, clock_snapshot::Clock, ClockSnapshot, TracePacket};
use crate::writer;
use crate::Error;
use prost::Message;
use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Thread buffers are handed to the session store when they reach this size
/// (1/16 of the session buffer, clamped to 4..64 KiB).
static THREAD_BUF_FLUSH: AtomicUsize = AtomicUsize::new(64 * 1024);

#[inline]
pub(crate) fn thread_buf_flush() -> usize {
    THREAD_BUF_FLUSH.load(Ordering::Relaxed)
}

/// Sequence id used for session-level packets (clock snapshot).
const HEADER_SEQ_ID: u32 = 1;

pub(crate) static ENABLED: AtomicBool = AtomicBool::new(false);
static GEN: AtomicU64 = AtomicU64::new(0);
static EPOCH: AtomicU64 = AtomicU64::new(0);
static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

struct Store {
    gen: u64,
    header: Vec<u8>,
    blocks: VecDeque<Vec<u8>>,
    size: usize,
    limit: usize,
    ring: bool,
    full: bool,
    dropped_bytes: u64,
    /// Streaming mode: blocks go straight to this file instead of memory.
    file: Option<std::io::BufWriter<std::fs::File>>,
    file_error: Option<String>,
    compression: Compression,
    /// Streaming + compression: framed packets waiting to be compressed.
    pending: Vec<u8>,
}

/// Packet compression of the written trace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    /// Plain `TracePacket`s (default).
    #[default]
    None,
    /// Groups of about [`COMPRESS_CHUNK`] bytes of packets are zlib-deflated into one
    /// `TracePacket.compressed_packets` (as Perfetto's tracing service does); the
    /// clock-snapshot header stays plain. trace_processor and the Perfetto UI read it.
    Zlib,
}

/// Uncompressed bytes of packets per `compressed_packets` packet.
pub const COMPRESS_CHUNK: usize = 1024 * 1024;

/// `TracePacket.compressed_packets`.
const COMPRESSED_PACKETS_FIELD: u32 = 50;

/// Appends one framed `TracePacket { compressed_packets: zlib(framed) }` to `out`.
/// `framed` must hold whole framed packets (a serialized `Trace`).
fn append_compressed(out: &mut Vec<u8>, framed: &[u8]) {
    use flate2::{write::ZlibEncoder, Compression as Level};
    let mut encoder = ZlibEncoder::new(Vec::with_capacity(framed.len() / 4), Level::default());
    encoder.write_all(framed).expect("writing to a Vec cannot fail");
    let deflated = encoder.finish().expect("writing to a Vec cannot fail");
    let mut packet = Vec::with_capacity(deflated.len() + 8);
    prost::encoding::encode_key(COMPRESSED_PACKETS_FIELD, prost::encoding::WireType::LengthDelimited, &mut packet);
    prost::encoding::encode_varint(deflated.len() as u64, &mut packet);
    packet.extend_from_slice(&deflated);
    protos::append_trace_packet(out, &packet);
}

/// Decodes a `.pftrace`, inflating `compressed_packets` (zlib) groups, so readers see
/// the same packets with or without [`Compression::Zlib`].
pub fn read_packets(bytes: &[u8]) -> Result<Vec<TracePacket>, String> {
    use prost::encoding::{decode_key, decode_varint, WireType};
    use std::io::Read;
    fn walk(mut b: &[u8], out: &mut Vec<TracePacket>) -> Result<(), String> {
        while !b.is_empty() {
            let (field, wire) = decode_key(&mut b).map_err(|e| e.to_string())?;
            if wire != WireType::LengthDelimited {
                return Err(format!("Trace field {field} has wire type {wire:?}"));
            }
            let len = decode_varint(&mut b).map_err(|e| e.to_string())? as usize;
            if len > b.len() {
                return Err("truncated packet".into());
            }
            let (packet, rest) = b.split_at(len);
            b = rest;
            if field != 1 {
                continue;
            }
            match compressed_payload(packet)? {
                Some(deflated) => {
                    let mut inflated = Vec::new();
                    flate2::read::ZlibDecoder::new(deflated).read_to_end(&mut inflated).map_err(|e| e.to_string())?;
                    walk(&inflated, out)?;
                }
                None => out.push(TracePacket::decode(packet).map_err(|e| e.to_string())?),
            }
        }
        Ok(())
    }
    /// `compressed_packets` bytes when this packet carries them.
    fn compressed_payload(mut p: &[u8]) -> Result<Option<&[u8]>, String> {
        while !p.is_empty() {
            let (field, wire) = decode_key(&mut p).map_err(|e| e.to_string())?;
            match wire {
                WireType::LengthDelimited => {
                    let len = decode_varint(&mut p).map_err(|e| e.to_string())? as usize;
                    if field == COMPRESSED_PACKETS_FIELD {
                        return Ok(Some(&p[..len.min(p.len())]));
                    }
                    p = &p[len.min(p.len())..];
                }
                WireType::Varint => {
                    decode_varint(&mut p).map_err(|e| e.to_string())?;
                }
                WireType::SixtyFourBit => p = &p[8.min(p.len())..],
                WireType::ThirtyTwoBit => p = &p[4.min(p.len())..],
                _ => return Ok(None),
            }
        }
        Ok(None)
    }
    let mut out = Vec::new();
    walk(bytes, &mut out)?;
    Ok(out)
}

impl Store {
    /// Streaming: writes `bytes` (whole framed packets) to the file, compressing when asked;
    /// `finish` also writes the last partial chunk.
    fn write_file(&mut self, bytes: &[u8], finish: bool) {
        let Some(file) = self.file.as_mut() else { return };
        let mut out = Vec::new();
        let data: &[u8] = match self.compression {
            Compression::None => bytes,
            Compression::Zlib => {
                self.pending.extend_from_slice(bytes);
                if self.pending.len() >= COMPRESS_CHUNK || (finish && !self.pending.is_empty()) {
                    append_compressed(&mut out, &self.pending);
                    self.pending.clear();
                }
                &out
            }
        };
        if self.file_error.is_none() && !data.is_empty() {
            if let Err(e) = file.write_all(data) {
                self.file_error = Some(e.to_string());
            }
        }
    }
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// `(generation, descriptor epoch)` of the active session, if any.
#[inline]
pub(crate) fn active() -> Option<(u64, u64)> {
    let g = GEN.load(Ordering::Acquire);
    (g != 0).then(|| (g, EPOCH.load(Ordering::Relaxed)))
}

/// Appends `pkt` to `out` in `.pftrace` framing (`Trace.packet`, field 1),
/// adding `trusted_packet_sequence_id` (the in-process "service" role).
pub(crate) fn frame_packet(out: &mut Vec<u8>, pkt: &[u8], writer_id: u16) {
    let seq = writer_id as u64 + 1;
    let mut trailer = [0u8; 12];
    let mut t = &mut trailer[..];
    prost::encoding::encode_key(10, prost::encoding::WireType::Varint, &mut t);
    prost::encoding::encode_varint(seq, &mut t);
    let tlen = 12 - t.len();
    prost::encoding::encode_key(1, prost::encoding::WireType::LengthDelimited, out);
    prost::encoding::encode_varint((pkt.len() + tlen) as u64, out);
    out.extend_from_slice(pkt);
    out.extend_from_slice(&trailer[..tlen]);
}

pub(crate) fn submit(gen: u64, buf: &mut Vec<u8>) {
    let block = std::mem::replace(buf, Vec::with_capacity(thread_buf_flush() + 4096));
    let mut guard = STORE.lock().unwrap();
    let Some(st) = guard.as_mut() else { return };
    if st.gen != gen || block.is_empty() {
        return;
    }
    if st.file.is_some() {
        st.write_file(&block, false);
        if st.file_error.is_some() {
            st.dropped_bytes += block.len() as u64;
        } else {
            st.size += block.len();
        }
        return;
    }
    if st.ring {
        while st.size + block.len() > st.limit {
            let Some(old) = st.blocks.pop_front() else { break };
            st.size -= old.len();
            st.dropped_bytes += old.len() as u64;
            // Descriptors may have been in the dropped block: re-emit.
            EPOCH.fetch_add(1, Ordering::Relaxed);
        }
    } else if st.full || st.size + block.len() > st.limit {
        st.full = true;
        st.dropped_bytes += block.len() as u64;
        return;
    }
    st.size += block.len();
    st.blocks.push_back(block);
}

/// Configuration of an in-process tracing session.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Buffer size limit in KiB (default 32 MiB).
    pub buffer_size_kb: u32,
    /// `true`: ring buffer, oldest data is discarded when full.
    /// `false` (default): stop recording when full.
    pub ring_buffer: bool,
    /// Which categories this session records (default: all).
    pub categories: CategoryFilter,
    /// Streaming mode: write the trace to this file while recording instead
    /// of keeping it in memory. `buffer_size_kb`/`ring_buffer` then do not
    /// apply (only the per-thread buffers are held in memory) and
    /// [`TracingSession::trace_bytes`] returns nothing; the file is complete
    /// after [`TracingSession::stop`].
    pub output_file: Option<PathBuf>,
    /// Clock snapshot written at the start of the trace. `None` (default):
    /// this host's clocks. `Some(clocks)`: exactly these `(clock, timestamp)`
    /// pairs, read at the same instant, e.g. a phone's BOOTTIME and REALTIME,
    /// so REALTIME-stamped events are placed on the phone's BOOTTIME and line
    /// up with a system trace recorded on it. With a custom snapshot,
    /// [`Clock::Sdk`](crate::Clock::Sdk) events are only placed correctly if
    /// the snapshot includes that clock.
    pub clock_snapshot: Option<Vec<(crate::Clock, u64)>>,
    /// Packet compression of the trace (default none). Buffer limits apply to the
    /// uncompressed size.
    pub compression: Compression,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            buffer_size_kb: 32 * 1024,
            ring_buffer: false,
            categories: CategoryFilter::all(),
            output_file: None,
            clock_snapshot: None,
            compression: Compression::None,
        }
    }
}

/// Statistics of a session.
#[derive(Clone, Copy, Debug, Default)]
pub struct SessionStats {
    pub bytes: u64,
    pub dropped_bytes: u64,
}

/// An in-process tracing session (at most one active at a time).
pub struct TracingSession {
    gen: u64,
    stopped: bool,
    output_file: Option<PathBuf>,
    file_error: Option<String>,
    data: Vec<u8>,
    stats: SessionStats,
}

fn flush_writers(gen: u64) {
    writer::for_each_writer(|w| {
        if w.inproc.gen == gen && !w.inproc_buf.is_empty() {
            submit(gen, &mut w.inproc_buf);
        }
    });
}

fn header_packet(custom: Option<&[(crate::Clock, u64)]>) -> Vec<u8> {
    let (pairs, primary, packet_ts, packet_clock) = match custom {
        None => (platform::clock_snapshot(), platform::TRACE_CLOCK_ID, platform::now_ns(), platform::SDK_PACKET_CLOCK),
        Some(custom) => {
            let pairs: Vec<(u32, u64)> = custom.iter().map(|(c, ts)| (c.builtin_id(), *ts)).collect();
            // Prefer BOOTTIME as the trace clock, as Android system traces do.
            let primary = pairs
                .iter()
                .map(|(id, _)| *id)
                .find(|id| *id == protos::BUILTIN_CLOCK_BOOTTIME)
                .or_else(|| pairs.first().map(|(id, _)| *id))
                .unwrap_or(platform::TRACE_CLOCK_ID);
            let ts = pairs.iter().find(|(id, _)| *id == primary).map_or(0, |(_, ts)| *ts);
            (pairs, primary, ts, Some(primary))
        }
    };
    let clocks = pairs.into_iter().map(|(id, ts)| Clock { clock_id: Some(id), timestamp: Some(ts) }).collect();
    let p = TracePacket {
        timestamp: Some(packet_ts),
        timestamp_clock_id: packet_clock,
        clock_snapshot: Some(ClockSnapshot { clocks, primary_trace_clock: Some(primary) }),
        trusted_packet_sequence_id: Some(HEADER_SEQ_ID),
        ..Default::default()
    };
    let mut out = Vec::new();
    protos::append_trace_packet(&mut out, &p.encode_to_vec());
    out
}

impl TracingSession {
    /// Starts recording. Requires [`crate::init`] with the in-process backend.
    pub fn start(cfg: SessionConfig) -> Result<TracingSession, Error> {
        if !ENABLED.load(Ordering::Acquire) {
            return Err(Error::BackendNotInitialized);
        }
        let gen = NEXT_GEN.fetch_add(1, Ordering::Relaxed);
        let limit = (cfg.buffer_size_kb.max(64) as usize) * 1024;
        {
            let mut g = STORE.lock().unwrap();
            if g.is_some() {
                return Err(Error::SessionActive);
            }
            let header = header_packet(cfg.clock_snapshot.as_deref());
            let file = match &cfg.output_file {
                None => None,
                Some(path) => {
                    let mut f = std::io::BufWriter::with_capacity(
                        256 * 1024,
                        std::fs::File::create(path).map_err(Error::Io)?,
                    );
                    f.write_all(&header).map_err(Error::Io)?;
                    Some(f)
                }
            };
            *g = Some(Store {
                gen,
                header,
                file,
                file_error: None,
                compression: cfg.compression,
                pending: Vec::new(),
                blocks: VecDeque::new(),
                size: 0,
                limit,
                ring: cfg.ring_buffer,
                full: false,
                dropped_bytes: 0,
            });
        }
        THREAD_BUF_FLUSH.store((limit / 16).clamp(4 * 1024, 64 * 1024), Ordering::Relaxed);
        GEN.store(gen, Ordering::Release);
        set_sink_filter(SINK_INPROC, Some(cfg.categories));
        Ok(TracingSession {
            gen,
            stopped: false,
            output_file: cfg.output_file,
            file_error: None,
            data: Vec::new(),
            stats: SessionStats::default(),
        })
    }

    pub fn is_running(&self) -> bool {
        !self.stopped
    }

    /// Moves all thread-local buffered events into the session buffer.
    pub fn flush(&self) {
        if !self.stopped {
            flush_writers(self.gen);
        }
    }

    /// Stops recording. The trace stays available via [`Self::trace_bytes`] /
    /// [`Self::write_to_file`].
    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        set_sink_filter(SINK_INPROC, None);
        flush_writers(self.gen);
        let _ = GEN.compare_exchange(self.gen, 0, Ordering::AcqRel, Ordering::Relaxed);
        let st = STORE.lock().unwrap().take();
        if let Some(mut st) = st {
            if st.file.is_some() {
                st.write_file(&[], true);
                let mut file = st.file.take().unwrap();
                if let Err(e) = file.flush() {
                    st.file_error.get_or_insert(e.to_string());
                }
                self.stats = SessionStats { bytes: (st.header.len() + st.size) as u64, dropped_bytes: st.dropped_bytes };
                self.file_error = st.file_error;
                self.stopped = true;
                return;
            }
            let mut data = Vec::with_capacity(st.header.len() + st.size);
            data.extend_from_slice(&st.header);
            match st.compression {
                Compression::None => {
                    for b in st.blocks {
                        data.extend_from_slice(&b);
                    }
                }
                Compression::Zlib => {
                    let mut chunk = Vec::new();
                    for b in st.blocks {
                        chunk.extend_from_slice(&b);
                        if chunk.len() >= COMPRESS_CHUNK {
                            append_compressed(&mut data, &chunk);
                            chunk.clear();
                        }
                    }
                    if !chunk.is_empty() {
                        append_compressed(&mut data, &chunk);
                    }
                }
            }
            self.stats = SessionStats { bytes: data.len() as u64, dropped_bytes: st.dropped_bytes };
            self.data = data;
        }
        self.stopped = true;
    }

    /// The trace as `.pftrace` bytes. While running this flushes and returns a
    /// snapshot.
    pub fn trace_bytes(&self) -> Vec<u8> {
        if self.output_file.is_some() {
            return Vec::new();
        }
        if self.stopped {
            return self.data.clone();
        }
        flush_writers(self.gen);
        let g = STORE.lock().unwrap();
        let mut out = Vec::new();
        if let Some(st) = g.as_ref() {
            out.extend_from_slice(&st.header);
            for b in &st.blocks {
                out.extend_from_slice(b);
            }
        }
        out
    }

    /// Writes the trace to `path`. In streaming mode this copies the output
    /// file (a no-op when `path` is the output file); stop first.
    pub fn write_to_file(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        if let Some(out) = &self.output_file {
            if out.as_path() != path.as_ref() {
                std::fs::copy(out, path)?;
            }
            return match &self.file_error {
                Some(e) => Err(std::io::Error::other(e.clone())),
                None => Ok(()),
            };
        }
        std::fs::write(path, self.trace_bytes())
    }

    /// Streaming mode: the error that stopped writing to the output file, if any.
    pub fn file_error(&self) -> Option<&str> {
        self.file_error.as_deref()
    }

    /// Stops the session and writes the trace to `path`.
    pub fn stop_and_write(mut self, path: impl AsRef<Path>) -> std::io::Result<()> {
        self.stop();
        self.write_to_file(path)
    }

    pub fn stats(&self) -> SessionStats {
        if self.stopped {
            return self.stats;
        }
        let g = STORE.lock().unwrap();
        g.as_ref()
            .map(|s| SessionStats { bytes: (s.header.len() + s.size) as u64, dropped_bytes: s.dropped_bytes })
            .unwrap_or_default()
    }
}

impl Drop for TracingSession {
    fn drop(&mut self) {
        self.stop();
    }
}
