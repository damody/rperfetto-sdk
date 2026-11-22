//! System backend: a Perfetto producer that connects to `traced` over the
//! producer socket, registers the `track_event` data source and writes
//! packets into the shared memory buffer (SMB) handed out by the service.
//!
//! Protocol (ProducerPort, `protos/perfetto/ipc/producer_port.proto`):
//! 1. `BindService("ProducerPort")` → method ids.
//! 2. `InitializeConnection{producer_name, smb size hints}`.
//! 3. `RegisterDataSource{name: "track_event", track_event_descriptor}`.
//! 4. `GetAsyncCommand` (streaming): `SetupTracing` (carries the SMB fd),
//!    `SetupDataSource`, `StartDataSource`, `Flush`, `StopDataSource`,
//!    `ClearIncrementalState`.
//! 5. Completed chunks are announced with `CommitData{chunks_to_move}`; flush
//!    requests are acknowledged by `CommitData{flush_request_id}`.

pub mod ipc;
pub mod smb;

use crate::category::{set_sink_filter, CategoryFilter, SINK_SYSTEM};
use crate::protos::*;
use crate::writer::{self, SeqState};
use prost::Message;
use smb::{ChunkCursor, Smb};
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

pub const DATA_SOURCE_NAME: &str = "track_event";

#[derive(Clone, Debug)]
pub struct SystemArgs {
    pub producer_name: String,
    /// Socket path; `None` = `$PERFETTO_PRODUCER_SOCK_NAME` or the platform
    /// default.
    pub socket: Option<String>,
    pub shmem_size_hint_kb: u32,
    pub shmem_page_size_hint_kb: u32,
    /// Keep retrying to connect when traced is not (yet) running.
    pub retry_connect: bool,
}

pub fn default_socket() -> String {
    if let Ok(s) = std::env::var("PERFETTO_PRODUCER_SOCK_NAME") {
        return s;
    }
    if cfg!(target_os = "android") {
        "/dev/socket/traced_producer".into()
    } else if std::path::Path::new("/run/perfetto").is_dir() {
        "/run/perfetto/traced-producer.sock".into()
    } else {
        "/tmp/perfetto-producer".into()
    }
}

/// State of one connection to traced.
pub(crate) struct Conn {
    tx: ipc::Sender,
    service_id: u32,
    methods: HashMap<String, u32>,
    next_req: AtomicU64,
    pending: Mutex<Vec<ChunksToMove>>,
    cv: Condvar,
    closed: AtomicBool,
}

impl Conn {
    fn invoke(&self, method: &str, args: Vec<u8>, drop_reply: bool) -> std::io::Result<u64> {
        let req = self.next_req.fetch_add(1, Ordering::Relaxed);
        let Some(&mid) = self.methods.get(method) else {
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, method.to_string()));
        };
        self.tx.send(&IpcFrame {
            request_id: Some(req),
            msg_invoke_method: Some(InvokeMethod {
                service_id: Some(self.service_id),
                method_id: Some(mid),
                args_proto: Some(args),
                drop_reply: Some(drop_reply),
            }),
            ..Default::default()
        })?;
        Ok(req)
    }

    pub(crate) fn queue_commit(&self, page: u32, target_buffer: u32) {
        let mut p = self.pending.lock().unwrap();
        p.push(ChunksToMove { page: Some(page), chunk: Some(0), target_buffer: Some(target_buffer) });
        self.cv.notify_one();
    }

    fn commit(&self, flush_request_id: Option<u64>) {
        let chunks = std::mem::take(&mut *self.pending.lock().unwrap());
        if chunks.is_empty() && flush_request_id.is_none() {
            return;
        }
        let req = CommitDataRequest { chunks_to_move: chunks, flush_request_id };
        let _ = self.invoke("CommitData", req.encode_to_vec(), true);
    }

    fn register_writer(&self, id: u16, target_buffer: u32) {
        let req = RegisterTraceWriterRequest { trace_writer_id: Some(id as u32), target_buffer: Some(target_buffer) };
        let _ = self.invoke("RegisterTraceWriter", req.encode_to_vec(), true);
    }
}

/// The active `track_event` data source instance.
pub(crate) struct Active {
    gen: u64,
    epoch: AtomicU64,
    smb: Arc<Smb>,
    target_buffer: u32,
    conn: Arc<Conn>,
}

static ACTIVE: RwLock<Option<Arc<Active>>> = RwLock::new(None);
static NEXT_GEN: AtomicU64 = AtomicU64::new(1);
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[inline]
pub(crate) fn active() -> Option<Arc<Active>> {
    ACTIVE.read().unwrap().clone()
}

/// Packets dropped because the SMB was full.
pub fn dropped_packets() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// True while traced has our data source started.
pub fn is_tracing() -> bool {
    ACTIVE.read().unwrap().is_some()
}

/// Per-thread system-sink state.
#[derive(Default)]
pub(crate) struct SysWriter {
    pub seq: SeqState,
    smb: Option<Arc<Smb>>,
    conn: Option<Arc<Conn>>,
    cur: Option<ChunkCursor>,
    target_buffer: u32,
    next_chunk_id: u32,
    registered_gen: u64,
    dropping: bool,
}

impl SysWriter {
    pub(crate) fn with_next_chunk_id(next_chunk_id: u32) -> Self {
        SysWriter { next_chunk_id, ..Default::default() }
    }

    pub(crate) fn next_chunk_id(&self) -> u32 {
        self.next_chunk_id
    }

    pub(crate) fn sync(&mut self, st: &Active) {
        self.seq.sync(st.gen, st.epoch.load(Ordering::Relaxed));
        let same_smb = self.smb.as_ref().is_some_and(|s| Arc::ptr_eq(s, &st.smb));
        if !same_smb || self.target_buffer != st.target_buffer {
            // Hand back whatever we had in the previous buffer/session.
            self.complete_current();
            self.smb = Some(st.smb.clone());
            self.target_buffer = st.target_buffer;
        }
        if !self.conn.as_ref().is_some_and(|c| Arc::ptr_eq(c, &st.conn)) {
            self.conn = Some(st.conn.clone());
        }
    }

    fn complete_current(&mut self) {
        if let (Some(c), Some(smb)) = (self.cur.take(), self.smb.as_ref()) {
            smb.complete(&c);
            if let Some(conn) = &self.conn {
                conn.queue_commit(c.page, self.target_buffer);
            }
        }
    }

    pub(crate) fn write_packet(&mut self, writer_id: u16, data: &[u8], st: &Active) {
        if self.registered_gen != st.gen {
            self.registered_gen = st.gen;
            st.conn.register_writer(writer_id, st.target_buffer);
        }
        let smb = self.smb.clone().unwrap();
        let tb = self.target_buffer;
        let conn = &st.conn;
        let ok = smb::write_packet(&smb, &mut self.cur, writer_id, &mut self.next_chunk_id, data, |c| {
            conn.queue_commit(c.page, tb)
        });
        if !ok {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            if !self.dropping {
                self.dropping = true;
                // Descriptors may have been lost; re-emit them.
                self.seq.emitted.clear();
                self.seq.last_track = 0;
            }
        } else {
            self.dropping = false;
        }
    }

    pub(crate) fn flush(&mut self, _writer_id: u16) {
        if self.cur.as_ref().is_some_and(|c| c.count > 0) {
            self.complete_current();
        }
    }
}

struct Producer {
    shutdown: Arc<AtomicBool>,
    conn: Arc<Mutex<Option<Arc<Conn>>>>,
    thread: Option<JoinHandle<()>>,
}

static PRODUCER: Mutex<Option<Producer>> = Mutex::new(None);

/// Starts the producer thread. Returns an error only if `retry_connect` is
/// false and the first connection attempt fails.
pub fn start(args: SystemArgs) -> std::io::Result<()> {
    let mut g = PRODUCER.lock().unwrap();
    if g.is_some() {
        return Ok(());
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let conn_slot: Arc<Mutex<Option<Arc<Conn>>>> = Arc::new(Mutex::new(None));
    let path = args.socket.clone().unwrap_or_else(default_socket);
    let first = if args.retry_connect { None } else { Some(handshake(&path, &args)?) };
    let (sd, slot) = (shutdown.clone(), conn_slot.clone());
    let thread = std::thread::Builder::new()
        .name("rperfetto-prod".into())
        .spawn(move || producer_main(path, args, first, sd, slot))?;
    *g = Some(Producer { shutdown, conn: conn_slot, thread: Some(thread) });
    Ok(())
}

pub fn stop() {
    let p = PRODUCER.lock().unwrap().take();
    if let Some(mut p) = p {
        stop_data_source_locally();
        p.shutdown.store(true, Ordering::SeqCst);
        if let Some(c) = p.conn.lock().unwrap().as_ref() {
            c.commit(None);
            c.tx.shutdown();
        }
        if let Some(t) = p.thread.take() {
            let _ = t.join();
        }
    }
}

fn handshake(path: &str, args: &SystemArgs) -> std::io::Result<(ipc::Reader, Arc<Conn>)> {
    let (mut rx, tx) = ipc::connect(path)?;
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    tx.send(&IpcFrame {
        request_id: Some(1),
        msg_bind_service: Some(BindService { service_name: Some("ProducerPort".into()) }),
        ..Default::default()
    })?;
    let reply = loop {
        let f = rx.next_frame()?;
        if f.request_id == Some(1) {
            break f;
        }
    };
    let bsr = reply.msg_bind_service_reply.ok_or_else(|| bad("BindService failed"))?;
    if bsr.success != Some(true) {
        return Err(bad("BindService rejected"));
    }
    let methods = bsr
        .methods
        .iter()
        .filter_map(|m| Some((m.name.clone()?, m.id?)))
        .collect::<HashMap<_, _>>();
    let conn = Arc::new(Conn {
        tx,
        service_id: bsr.service_id.unwrap_or(0),
        methods,
        next_req: AtomicU64::new(2),
        pending: Mutex::new(Vec::new()),
        cv: Condvar::new(),
        closed: AtomicBool::new(false),
    });

    let call = |rx: &mut ipc::Reader, method: &str, args: Vec<u8>| -> std::io::Result<Vec<u8>> {
        let req = conn.invoke(method, args, false)?;
        loop {
            let f = rx.next_frame()?;
            if f.request_id != Some(req) {
                continue;
            }
            if let Some(e) = f.msg_request_error {
                return Err(bad(&format!("{method}: {}", e.error.unwrap_or_default())));
            }
            let r = f.msg_invoke_method_reply.ok_or_else(|| bad(method))?;
            if r.success != Some(true) {
                return Err(bad(&format!("{method} failed")));
            }
            return Ok(r.reply_proto.unwrap_or_default());
        }
    };

    let init = InitializeConnectionRequest {
        shared_memory_page_size_hint_bytes: Some(args.shmem_page_size_hint_kb * 1024),
        shared_memory_size_hint_bytes: Some(args.shmem_size_hint_kb * 1024),
        producer_name: Some(args.producer_name.clone()),
        sdk_version: Some(concat!("rperfetto-sdk ", env!("CARGO_PKG_VERSION")).into()),
    };
    let r = call(&mut rx, "InitializeConnection", init.encode_to_vec())?;
    let r = InitializeConnectionResponse::decode(&r[..]).unwrap_or_default();
    if r.use_shmem_emulation == Some(true) {
        return Err(bad("traced requested SMB emulation (VM/TCP transport), not supported"));
    }

    let cats = crate::category::registered_categories()
        .into_iter()
        .map(|(n, d)| TrackEventCategory {
            name: Some(n.to_string()),
            description: (!d.is_empty()).then(|| d.to_string()),
            tags: vec![],
        })
        .collect();
    let reg = RegisterDataSourceRequest {
        data_source_descriptor: Some(DataSourceDescriptor {
            name: Some(DATA_SOURCE_NAME.into()),
            will_notify_on_stop: Some(true),
            will_notify_on_start: Some(false),
            handles_incremental_state_clear: Some(true),
            track_event_descriptor: Some(TrackEventDescriptor { available_categories: cats }),
        }),
    };
    conn.invoke("RegisterDataSource", reg.encode_to_vec(), true)?;
    Ok((rx, conn))
}

fn producer_main(
    path: String,
    args: SystemArgs,
    mut first: Option<(ipc::Reader, Arc<Conn>)>,
    shutdown: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<Arc<Conn>>>>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        let (rx, conn) = match first.take() {
            Some(x) => x,
            None => match handshake(&path, &args) {
                Ok(x) => x,
                Err(_) => {
                    for _ in 0..10 {
                        if shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    continue;
                }
            },
        };
        *slot.lock().unwrap() = Some(conn.clone());
        let committer = {
            let c = conn.clone();
            std::thread::Builder::new().name("rperfetto-commit".into()).spawn(move || commit_loop(c)).ok()
        };
        let _ = serve(rx, &conn);
        conn.closed.store(true, Ordering::SeqCst);
        conn.cv.notify_all();
        stop_data_source_locally();
        if let Some(t) = committer {
            let _ = t.join();
        }
        *slot.lock().unwrap() = None;
        if !args.retry_connect {
            return;
        }
    }
}

fn commit_loop(conn: Arc<Conn>) {
    loop {
        {
            let mut p = conn.pending.lock().unwrap();
            while p.is_empty() && !conn.closed.load(Ordering::SeqCst) {
                p = conn.cv.wait(p).unwrap();
            }
            if conn.closed.load(Ordering::SeqCst) {
                return;
            }
        }
        // Batch commits for a few ms.
        std::thread::sleep(Duration::from_millis(2));
        conn.commit(None);
    }
}

fn filter_from(cfg: &Option<DataSourceConfig>) -> CategoryFilter {
    match cfg.as_ref().and_then(|c| c.track_event_config.as_ref()) {
        Some(t) => CategoryFilter { enabled: t.enabled_categories.clone(), disabled: t.disabled_categories.clone() },
        None => CategoryFilter::all(),
    }
}

/// Disables the system sink and pushes out all buffered chunks.
fn stop_data_source_locally() {
    let was = ACTIVE.write().unwrap().take();
    set_sink_filter(SINK_SYSTEM, None);
    if was.is_some() {
        writer::for_each_writer(|w| w.sys.flush(w.id));
    }
}

fn serve(mut rx: ipc::Reader, conn: &Arc<Conn>) -> std::io::Result<()> {
    let async_req = conn.invoke("GetAsyncCommand", GetAsyncCommandRequest {}.encode_to_vec(), false)?;
    let mut smb: Option<Arc<Smb>> = None;
    let mut configs: HashMap<u64, DataSourceConfig> = HashMap::new();
    let mut current: Option<u64> = None;
    loop {
        let f = rx.next_frame()?;
        if f.request_id != Some(async_req) {
            continue; // Replies to our own (non-dropped) calls.
        }
        let Some(r) = f.msg_invoke_method_reply else { continue };
        if r.success != Some(true) {
            return Err(std::io::Error::other("GetAsyncCommand failed"));
        }
        let cmd = GetAsyncCommandResponse::decode(r.reply_proto.as_deref().unwrap_or(&[]))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if let Some(st) = cmd.setup_tracing {
            if let Some(fd) = rx.fds.pop_front() {
                let page = st.shared_buffer_page_size_kb.unwrap_or(4).max(4) as usize * 1024;
                // The mapping stays valid after the fd is closed.
                smb = Some(Arc::new(Smb::map_fd(fd.as_raw_fd(), page)?));
            }
        }
        if let Some(sd) = cmd.setup_data_source {
            if let (Some(id), Some(cfg)) = (sd.new_instance_id, sd.config) {
                configs.insert(id, cfg);
            }
        }
        if let Some(sd) = cmd.start_data_source {
            let id = sd.new_instance_id.unwrap_or(0);
            let cfg = sd.config.or_else(|| configs.get(&id).cloned());
            if let Some(smb) = smb.clone() {
                // A newer session replaces the previous one (single instance).
                stop_data_source_locally();
                let filter = filter_from(&cfg);
                let active = Arc::new(Active {
                    gen: NEXT_GEN.fetch_add(1, Ordering::Relaxed),
                    epoch: AtomicU64::new(0),
                    smb,
                    target_buffer: cfg.as_ref().and_then(|c| c.target_buffer).unwrap_or(0),
                    conn: conn.clone(),
                });
                *ACTIVE.write().unwrap() = Some(active);
                set_sink_filter(SINK_SYSTEM, Some(filter));
                current = Some(id);
            }
        }
        if let Some(fl) = cmd.flush {
            writer::for_each_writer(|w| w.sys.flush(w.id));
            conn.commit(fl.request_id);
        }
        if let Some(cis) = cmd.clear_incremental_state {
            if current.is_some_and(|c| cis.data_source_ids.contains(&c)) {
                if let Some(a) = ACTIVE.read().unwrap().as_ref() {
                    a.epoch.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if let Some(sd) = cmd.stop_data_source {
            let id = sd.instance_id.unwrap_or(0);
            configs.remove(&id);
            if current == Some(id) {
                current = None;
                stop_data_source_locally();
                conn.commit(None);
            }
            let n = NotifyDataSourceRequest { data_source_id: Some(id) };
            let _ = conn.invoke("NotifyDataSourceStopped", n.encode_to_vec(), true);
        }
    }
}
