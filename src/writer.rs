//! Per-thread trace writers.
//!
//! Each thread owns one [`ThreadWriter`], which is one Perfetto packet
//! sequence. Events are encoded with `prost` into a reused scratch packet
//! (allocation-free after warm-up) and handed to the active proto sinks:
//! the in-process session buffer and/or the system producer's shared memory.

use crate::category::{Category, SINK_ATRACE, SINK_INPROC, SINK_SYSTEM};
use crate::event::{Arg, ArgValue, CounterValue, Phase};
use crate::inprocess;
use crate::platform;
use crate::protos::{self, DebugAnnotation, TracePacket, TrackEvent};
use crate::track;
use prost::Message;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(any(target_os = "linux", target_os = "android"))]
use crate::system;

/// Max writer id accepted by traced (`kMaxWriterID`).
const MAX_WRITER_ID: u16 = (1 << 15) - 1;

/// State of one proto sink (in-process or system) for one sequence.
#[derive(Default)]
pub(crate) struct SeqState {
    /// Session/connection generation this state belongs to.
    pub gen: u64,
    /// Bumped when descriptors must be re-emitted (ring wrap, incremental
    /// state clear).
    pub epoch: u64,
    pub emitted: HashSet<u64>,
    pub started: bool,
}

impl SeqState {
    /// Re-syncs with `(gen, epoch)`. Returns true if the sequence restarted.
    pub(crate) fn sync(&mut self, gen: u64, epoch: u64) -> bool {
        if self.gen != gen {
            self.gen = gen;
            self.epoch = epoch;
            self.emitted.clear();
            self.started = false;
            return true;
        }
        if self.epoch != epoch {
            self.epoch = epoch;
            self.emitted.clear();
            self.started = false;
        }
        false
    }
}

pub(crate) struct ThreadWriter {
    pub id: u16,
    pub tid: i32,
    pub thread_uuid: u64,
    pkt: TracePacket,
    enc: Vec<u8>,
    pub inproc: SeqState,
    pub inproc_buf: Vec<u8>,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub sys: system::SysWriter,
}

struct Registry {
    writers: Vec<Arc<Mutex<ThreadWriter>>>,
    free_ids: Vec<u16>,
    next_id: u16,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry { writers: Vec::new(), free_ids: Vec::new(), next_id: 1 }))
}

struct TlsHolder(Arc<Mutex<ThreadWriter>>);

impl Drop for TlsHolder {
    fn drop(&mut self) {
        {
            let mut w = self.0.lock().unwrap();
            w.flush();
        }
        let mut reg = registry().lock().unwrap();
        reg.writers.retain(|x| !Arc::ptr_eq(x, &self.0));
        let id = self.0.lock().unwrap().id;
        if id != 0 {
            reg.free_ids.push(id);
        }
    }
}

thread_local! {
    static TW: RefCell<Option<TlsHolder>> = const { RefCell::new(None) };
}

fn new_writer() -> Arc<Mutex<ThreadWriter>> {
    let pid = platform::pid();
    let tid = platform::tid();
    let name = platform::current_thread_name().unwrap_or_default();
    let thread_uuid = track::register_thread(pid, tid, &name);
    let mut reg = registry().lock().unwrap();
    let id = match reg.free_ids.pop() {
        Some(id) => id,
        None if reg.next_id <= MAX_WRITER_ID => {
            reg.next_id += 1;
            reg.next_id - 1
        }
        None => 0, // Out of ids: this thread can only use ATRACE.
    };
    let mut pkt = TracePacket::default();
    pkt.track_event = Some(TrackEvent::default());
    let w = Arc::new(Mutex::new(ThreadWriter {
        id,
        tid,
        thread_uuid,
        pkt,
        enc: Vec::with_capacity(256),
        inproc: SeqState::default(),
        inproc_buf: Vec::new(),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        sys: system::SysWriter::default(),
    }));
    reg.writers.push(w.clone());
    w
}

/// Runs `f` with the calling thread's writer. Returns `None` during thread
/// teardown (TLS already destroyed).
pub(crate) fn with_writer<R>(f: impl FnOnce(&mut ThreadWriter) -> R) -> Option<R> {
    TW.try_with(|cell| {
        let arc = {
            let mut slot = cell.borrow_mut();
            slot.get_or_insert_with(|| TlsHolder(new_writer())).0.clone()
        };
        let mut w = arc.lock().unwrap();
        f(&mut w)
    })
    .ok()
}

/// Runs `f` on every live writer (used for flushes driven by other threads).
pub(crate) fn for_each_writer(mut f: impl FnMut(&mut ThreadWriter)) {
    let writers = registry().lock().unwrap().writers.clone();
    for w in writers {
        f(&mut w.lock().unwrap());
    }
}

/// Sets the calling thread's name and re-emits its descriptor.
pub(crate) fn set_thread_name(name: &str) {
    with_writer(|w| {
        track::register_thread(platform::pid(), w.tid, name);
        let u = w.thread_uuid;
        w.inproc.emitted.remove(&u);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        w.sys.seq.emitted.remove(&u);
    });
}

fn set_str(slot: &mut Option<String>, v: &str) {
    let s = slot.get_or_insert_with(String::new);
    s.clear();
    s.push_str(v);
}

fn fill_args(dst: &mut Vec<DebugAnnotation>, args: &[Arg]) {
    dst.truncate(args.len());
    while dst.len() < args.len() {
        dst.push(DebugAnnotation::default());
    }
    for (d, a) in dst.iter_mut().zip(args) {
        set_str(&mut d.name, a.name);
        d.bool_value = None;
        d.uint_value = None;
        d.int_value = None;
        d.double_value = None;
        d.pointer_value = None;
        match a.value {
            ArgValue::Bool(v) => d.bool_value = Some(v),
            ArgValue::Int(v) => d.int_value = Some(v),
            ArgValue::UInt(v) => d.uint_value = Some(v),
            ArgValue::Double(v) => d.double_value = Some(v),
            ArgValue::Pointer(v) => d.pointer_value = Some(v),
            ArgValue::Str(v) => {
                set_str(&mut d.string_value, v);
                continue;
            }
        }
        d.string_value = None;
    }
}

pub(crate) struct EventSpec<'a> {
    pub cat: &'static Category,
    pub phase: Phase,
    pub track_uuid: Option<u64>,
    pub name: Option<&'a str>,
    pub args: &'a [Arg<'a>],
    pub counter: Option<CounterValue>,
    pub flows: &'a [u64],
    pub terminating_flows: &'a [u64],
}

impl ThreadWriter {
    fn encode_event(&mut self, ts: u64, track_uuid: u64, ev: &EventSpec) {
        let pkt = &mut self.pkt;
        pkt.timestamp = Some(ts);
        let te = pkt.track_event.as_mut().unwrap();
        te.track_uuid = Some(track_uuid);
        te.r#type = Some(match ev.phase {
            Phase::Begin => protos::TYPE_SLICE_BEGIN,
            Phase::End => protos::TYPE_SLICE_END,
            Phase::Instant => protos::TYPE_INSTANT,
            Phase::Counter => protos::TYPE_COUNTER,
        });
        if matches!(ev.phase, Phase::Begin | Phase::Instant) {
            te.categories.truncate(1);
            if te.categories.is_empty() {
                te.categories.push(String::new());
            }
            te.categories[0].clear();
            te.categories[0].push_str(ev.cat.name());
        } else {
            te.categories.clear();
        }
        match ev.name {
            Some(n) if ev.phase != Phase::Counter => set_str(&mut te.name, n),
            _ => te.name = None,
        }
        te.counter_value = None;
        te.double_counter_value = None;
        match ev.counter {
            Some(CounterValue::Int(v)) => te.counter_value = Some(v),
            Some(CounterValue::Double(v)) => te.double_counter_value = Some(v),
            None => {}
        }
        fill_args(&mut te.debug_annotations, ev.args);
        te.flow_ids.clear();
        te.flow_ids.extend_from_slice(ev.flows);
        te.terminating_flow_ids.clear();
        te.terminating_flow_ids.extend_from_slice(ev.terminating_flows);
        self.enc.clear();
        pkt.encode(&mut self.enc).expect("Vec has unlimited capacity");
    }

    /// Packets that must precede an event on `track_uuid` in a sequence whose
    /// state is `seq`: the sequence-start marker and missing descriptors.
    fn preamble(seq: &mut SeqState, track_uuid: u64, ts: u64, out: &mut Vec<Vec<u8>>) {
        if !seq.started {
            seq.started = true;
            let p = TracePacket {
                sequence_flags: Some(protos::SEQ_INCREMENTAL_STATE_CLEARED),
                first_packet_on_sequence: Some(true),
                ..Default::default()
            };
            out.push(p.encode_to_vec());
        }
        if seq.emitted.contains(&track_uuid) {
            return;
        }
        for uuid in track::ancestry(track_uuid).into_iter().rev() {
            if seq.emitted.insert(uuid) {
                if let Some(d) = track::descriptor(uuid) {
                    let p = TracePacket { timestamp: Some(ts), track_descriptor: Some(d), ..Default::default() };
                    out.push(p.encode_to_vec());
                }
            }
        }
    }

    pub(crate) fn emit(&mut self, sinks: u8, ts: u64, ev: &EventSpec) {
        if self.id == 0 {
            return;
        }
        let track_uuid = ev.track_uuid.unwrap_or(self.thread_uuid);
        self.encode_event(ts, track_uuid, ev);
        let mut pre: Vec<Vec<u8>> = Vec::new();

        if sinks & SINK_INPROC != 0 {
            if let Some((gen, epoch)) = inprocess::active() {
                if self.inproc.sync(gen, epoch) {
                    self.inproc_buf.clear();
                }
                Self::preamble(&mut self.inproc, track_uuid, ts, &mut pre);
                for p in pre.drain(..) {
                    inprocess::frame_packet(&mut self.inproc_buf, &p, self.id);
                }
                inprocess::frame_packet(&mut self.inproc_buf, &self.enc, self.id);
                if self.inproc_buf.len() >= inprocess::thread_buf_flush() {
                    inprocess::submit(gen, &mut self.inproc_buf);
                }
            }
        }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        if sinks & SINK_SYSTEM != 0 {
            if let Some(st) = system::active() {
                self.sys.sync(&st);
                Self::preamble(&mut self.sys.seq, track_uuid, ts, &mut pre);
                for p in pre.drain(..) {
                    self.sys.write_packet(self.id, &p, &st);
                }
                self.sys.write_packet(self.id, &self.enc, &st);
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let _ = SINK_SYSTEM;
    }

    /// Hands buffered data to the sinks (in-process store; system chunks are
    /// completed and queued for commit).
    pub(crate) fn flush(&mut self) {
        if !self.inproc_buf.is_empty() {
            match inprocess::active() {
                Some((gen, _)) if gen == self.inproc.gen => inprocess::submit(gen, &mut self.inproc_buf),
                _ => self.inproc_buf.clear(),
            }
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        self.sys.flush(self.id);
    }
}

/// Emits one event to every sink the category is enabled for.
#[inline]
pub(crate) fn emit_with_sinks(sinks: u8, ev: &EventSpec) {
    if sinks == 0 {
        return;
    }
    let ts = platform::now_ns();
    if sinks & (SINK_INPROC | SINK_SYSTEM) != 0 {
        with_writer(|w| w.emit(sinks, ts, ev));
    }
    if sinks & SINK_ATRACE != 0 {
        crate::atrace::emit(ev);
    }
}
