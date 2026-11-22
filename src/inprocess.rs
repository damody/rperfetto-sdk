//! In-process backend: the application records its own events into an
//! in-memory buffer and writes a standalone `.pftrace` file.

use crate::category::{set_sink_filter, CategoryFilter, SINK_INPROC};
use crate::platform;
use crate::protos::{self, clock_snapshot::Clock, ClockSnapshot, TracePacket};
use crate::writer;
use crate::Error;
use prost::Message;
use std::collections::VecDeque;
use std::path::Path;
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
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig { buffer_size_kb: 32 * 1024, ring_buffer: false, categories: CategoryFilter::all() }
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

fn header_packet() -> Vec<u8> {
    let clocks = platform::clock_snapshot()
        .into_iter()
        .map(|(id, ts)| Clock { clock_id: Some(id), timestamp: Some(ts) })
        .collect();
    let p = TracePacket {
        timestamp: Some(platform::now_ns()),
        clock_snapshot: Some(ClockSnapshot { clocks, primary_trace_clock: Some(platform::TRACE_CLOCK_ID) }),
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
            *g = Some(Store {
                gen,
                header: header_packet(),
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
        Ok(TracingSession { gen, stopped: false, data: Vec::new(), stats: SessionStats::default() })
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
        if let Some(st) = st {
            let mut data = Vec::with_capacity(st.header.len() + st.size);
            data.extend_from_slice(&st.header);
            for b in st.blocks {
                data.extend_from_slice(&b);
            }
            self.stats = SessionStats { bytes: data.len() as u64, dropped_bytes: st.dropped_bytes };
            self.data = data;
        }
        self.stopped = true;
    }

    /// The trace as `.pftrace` bytes. While running this flushes and returns a
    /// snapshot.
    pub fn trace_bytes(&self) -> Vec<u8> {
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

    pub fn write_to_file(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        std::fs::write(path, self.trace_bytes())
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
