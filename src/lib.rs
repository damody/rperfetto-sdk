//! # rperfetto-sdk
//!
//! Pure-Rust Perfetto TrackEvent producer, embeddable in apps and games.
//!
//! Backends (any combination, chosen at [`init`]):
//! * **In-process** ([`Backends::IN_PROCESS`]): the app records only its own
//!   events into memory and writes a standalone `.pftrace`
//!   ([`TracingSession`]).
//! * **System** ([`Backends::SYSTEM`], Linux/Android): connects to `traced` as
//!   a producer of the `track_event` data source, so events land in the
//!   system trace next to sched/CPU/SurfaceFlinger data.
//! * **ATRACE** ([`Backends::ATRACE`], Linux/Android): writes
//!   `android.os.Trace`-format lines to ftrace's `trace_marker`.
//!
//! ```no_run
//! use rperfetto_sdk::{self as pf, Category, Backends, InitArgs, SessionConfig, TracingSession};
//! static GAME: Category = Category::new("game");
//!
//! pf::init(InitArgs { backends: Backends::IN_PROCESS, ..Default::default() }).unwrap();
//! let session = TracingSession::start(SessionConfig::default()).unwrap();
//! {
//!     let _s = pf::trace_scope!(GAME, "frame", "index" => 1);
//!     pf::instant(&GAME, "input", &[]);
//! }
//! session.stop_and_write("game.pftrace").unwrap();
//! pf::shutdown();
//! ```

mod atrace;
mod category;
mod clock;
mod log;
mod event;
pub mod ffi;
mod inprocess;
pub mod func;
mod platform;
pub mod protos;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub mod system;
mod track;
mod writer;

pub use category::{registered_categories, Category, CategoryFilter};
pub use clock::{Clock, Timestamp};
pub use log::{android_logs, LogEvent, LogPriority};
pub use event::{Arg, ArgValue, CounterValue};
pub use inprocess::{append_compressed, read_packets, Compression, SessionConfig, SessionStats, TracingSession, COMPRESS_CHUNK};
pub use platform::now_ns;
pub use prost;
pub use track::{CounterTrack, Track};

use category::{SINK_ATRACE, SINK_PROTO};
use event::Phase;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use writer::{emit_with_sinks, emit_with_sinks_at, EventSpec};

#[derive(Debug)]
pub enum Error {
    /// The requested backend is not available on this platform.
    Unsupported(&'static str),
    /// [`TracingSession::start`] without the in-process backend.
    BackendNotInitialized,
    /// An in-process session is already running.
    SessionActive,
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
            Error::BackendNotInitialized => write!(f, "backend not initialized"),
            Error::SessionActive => write!(f, "an in-process session is already active"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Set of backends (bit flags; same values as the C API).
/// cbindgen:ignore
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Backends(pub u32);

impl Backends {
    pub const IN_PROCESS: Backends = Backends(1);
    pub const SYSTEM: Backends = Backends(2);
    pub const ATRACE: Backends = Backends(4);

    pub fn contains(self, o: Backends) -> bool {
        self.0 & o.0 == o.0
    }
}

impl std::ops::BitOr for Backends {
    type Output = Backends;
    fn bitor(self, o: Backends) -> Backends {
        Backends(self.0 | o.0)
    }
}

#[derive(Clone, Debug)]
pub struct InitArgs {
    pub backends: Backends,
    /// Process name in track descriptors (default: from /proc/self/cmdline).
    pub process_name: Option<String>,
    /// System backend: producer name reported to traced.
    pub producer_name: String,
    /// System backend: producer socket (default: platform default or
    /// `$PERFETTO_PRODUCER_SOCK_NAME`).
    pub producer_socket: Option<String>,
    /// System backend: shared memory size hint (KiB), default 1024.
    pub shmem_size_hint_kb: u32,
    /// System backend: page size hint (KiB), default 4.
    pub shmem_page_size_hint_kb: u32,
    /// System backend: if true (default), `init` succeeds even when traced is
    /// not running and the SDK connects in the background once it is.
    pub retry_connect: bool,
    /// ATRACE backend: explicit `trace_marker` path.
    pub atrace_marker_path: Option<String>,
}

impl Default for InitArgs {
    fn default() -> Self {
        InitArgs {
            backends: Backends::IN_PROCESS,
            process_name: None,
            producer_name: platform::process_name().unwrap_or_else(|| "rperfetto-sdk".into()),
            producer_socket: None,
            shmem_size_hint_kb: 1024,
            shmem_page_size_hint_kb: 4,
            retry_connect: true,
            atrace_marker_path: None,
        }
    }
}

static INITIALIZED: Mutex<Backends> = Mutex::new(Backends(0));

/// Initializes the requested backends. Can be called again to add backends.
pub fn init(args: InitArgs) -> Result<(), Error> {
    let mut cur = INITIALIZED.lock().unwrap();
    if let Some(n) = &args.process_name {
        track::set_process_name(n);
    }
    let b = args.backends;
    if b.contains(Backends::IN_PROCESS) {
        inprocess::ENABLED.store(true, Ordering::Release);
        cur.0 |= Backends::IN_PROCESS.0;
    }
    if b.contains(Backends::ATRACE) && !cur.contains(Backends::ATRACE) {
        if !cfg!(any(target_os = "linux", target_os = "android")) {
            return Err(Error::Unsupported("ATRACE needs Linux/Android ftrace"));
        }
        atrace::open(args.atrace_marker_path.as_deref())?;
        category::set_sink_filter(SINK_ATRACE, Some(CategoryFilter::all()));
        cur.0 |= Backends::ATRACE.0;
    }
    if b.contains(Backends::SYSTEM) && !cur.contains(Backends::SYSTEM) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            system::start(system::SystemArgs {
                producer_name: args.producer_name.clone(),
                socket: args.producer_socket.clone(),
                shmem_size_hint_kb: args.shmem_size_hint_kb,
                shmem_page_size_hint_kb: args.shmem_page_size_hint_kb,
                retry_connect: args.retry_connect,
            })?;
            cur.0 |= Backends::SYSTEM.0;
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        return Err(Error::Unsupported("system backend needs traced (Linux/Android)"));
    }
    Ok(())
}

/// Backends initialized so far.
pub fn initialized_backends() -> Backends {
    *INITIALIZED.lock().unwrap()
}

/// Flushes everything and tears down all backends.
pub fn shutdown() {
    let mut cur = INITIALIZED.lock().unwrap();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if cur.contains(Backends::SYSTEM) {
        system::stop();
    }
    if cur.contains(Backends::ATRACE) {
        category::set_sink_filter(SINK_ATRACE, None);
    }
    inprocess::ENABLED.store(false, Ordering::Release);
    *cur = Backends(0);
}

/// Pushes the calling thread's buffered events to the backends. Data from all
/// threads is also flushed on session stop / traced flush requests, and when a
/// thread exits.
pub fn flush_current_thread() {
    writer::with_writer(|w| w.flush());
}

/// Names the calling thread in the trace.
pub fn set_thread_name(name: &str) {
    writer::set_thread_name(name);
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[inline]
fn spec<'a>(cat: &'static Category, phase: Phase, track: Option<u64>, name: Option<&'a str>, args: &'a [Arg<'a>]) -> EventSpec<'a> {
    EventSpec { cat, phase, track_uuid: track, name, args, counter: None, flows: &[], terminating_flows: &[] }
}

/// Begins a slice on the calling thread's track.
#[inline]
pub fn slice_begin(cat: &'static Category, name: &str, args: &[Arg]) {
    let s = cat.sinks();
    if s != 0 {
        emit_with_sinks(s, &spec(cat, Phase::Begin, None, Some(name), args));
    }
}

/// Ends the innermost slice on the calling thread's track.
#[inline]
pub fn slice_end(cat: &'static Category) {
    let s = cat.sinks();
    if s != 0 {
        emit_with_sinks(s, &spec(cat, Phase::End, None, None, &[]));
    }
}

/// Begins a slice on `track`.
#[inline]
pub fn slice_begin_on(cat: &'static Category, track: Track, name: &str, args: &[Arg]) {
    let s = cat.sinks();
    if s != 0 {
        emit_with_sinks(s, &spec(cat, Phase::Begin, Some(track.uuid), Some(name), args));
    }
}

/// Ends the innermost slice on `track`.
#[inline]
pub fn slice_end_on(cat: &'static Category, track: Track) {
    let s = cat.sinks();
    if s != 0 {
        emit_with_sinks(s, &spec(cat, Phase::End, Some(track.uuid), None, &[]));
    }
}

/// A zero-duration event on the calling thread's track.
#[inline]
pub fn instant(cat: &'static Category, name: &str, args: &[Arg]) {
    let s = cat.sinks();
    if s != 0 {
        emit_with_sinks(s, &spec(cat, Phase::Instant, None, Some(name), args));
    }
}

/// A zero-duration event on `track`.
#[inline]
pub fn instant_on(cat: &'static Category, track: Track, name: &str, args: &[Arg]) {
    let s = cat.sinks();
    if s != 0 {
        emit_with_sinks(s, &spec(cat, Phase::Instant, Some(track.uuid), Some(name), args));
    }
}

/// Records a counter sample.
#[inline]
pub fn counter(cat: &'static Category, track: CounterTrack, value: impl Into<CounterValue>) {
    let s = cat.sinks();
    if s != 0 {
        let mut e = spec(cat, Phase::Counter, Some(track.uuid), None, &[]);
        e.counter = Some(value.into());
        emit_with_sinks(s, &e);
    }
}

/// Begins a slice that is linked by flow arrows: `flows` connect this slice to
/// other slices carrying the same ids; `terminating_flows` end a flow here.
pub fn slice_begin_with_flows(
    cat: &'static Category,
    track: Option<Track>,
    name: &str,
    args: &[Arg],
    flows: &[u64],
    terminating_flows: &[u64],
) {
    let s = cat.sinks() & SINK_PROTO;
    if s != 0 {
        let mut e = spec(cat, Phase::Begin, track.map(|t| t.uuid), Some(name), args);
        e.flows = flows;
        e.terminating_flows = terminating_flows;
        emit_with_sinks(s, &e);
    }
    if cat.sinks() & SINK_ATRACE != 0 {
        emit_with_sinks(SINK_ATRACE, &spec(cat, Phase::Begin, track.map(|t| t.uuid), Some(name), args));
    }
}

// ---------------------------------------------------------------------------
// Events with explicit timestamps
//
// For data that was sampled earlier (e.g. replaying recorded metrics into a
// trace). `ts` is a bare `u64` in the clock of [`now_ns`], or a [`Timestamp`]
// in another clock (e.g. device REALTIME); events on one track should be
// emitted in timestamp order. Only the in-process and system backends record
// these; ATRACE cannot back-date events and ignores them.
// ---------------------------------------------------------------------------

/// [`slice_begin_on`] at timestamp `ts`.
#[inline]
pub fn slice_begin_on_at(cat: &'static Category, track: Track, ts: impl Into<Timestamp>, name: &str, args: &[Arg]) {
    let s = cat.sinks() & SINK_PROTO;
    if s != 0 {
        emit_with_sinks_at(s, ts.into(), &spec(cat, Phase::Begin, Some(track.uuid), Some(name), args));
    }
}

/// [`slice_end_on`] at timestamp `ts`.
#[inline]
pub fn slice_end_on_at(cat: &'static Category, track: Track, ts: impl Into<Timestamp>) {
    let s = cat.sinks() & SINK_PROTO;
    if s != 0 {
        emit_with_sinks_at(s, ts.into(), &spec(cat, Phase::End, Some(track.uuid), None, &[]));
    }
}

/// [`instant_on`] at timestamp `ts`.
#[inline]
pub fn instant_on_at(cat: &'static Category, track: Track, ts: impl Into<Timestamp>, name: &str, args: &[Arg]) {
    let s = cat.sinks() & SINK_PROTO;
    if s != 0 {
        emit_with_sinks_at(s, ts.into(), &spec(cat, Phase::Instant, Some(track.uuid), Some(name), args));
    }
}

/// [`counter`] at timestamp `ts`.
#[inline]
pub fn counter_at(cat: &'static Category, track: CounterTrack, ts: impl Into<Timestamp>, value: impl Into<CounterValue>) {
    let s = cat.sinks() & SINK_PROTO;
    if s != 0 {
        let mut e = spec(cat, Phase::Counter, Some(track.uuid), None, &[]);
        e.counter = Some(value.into());
        emit_with_sinks_at(s, ts.into(), &e);
    }
}

/// Ends the slice it was created for when dropped. The end is sent to the same
/// backends that recorded the begin, even if the category changed meanwhile.
#[must_use = "the slice ends when the guard is dropped"]
pub struct SliceGuard {
    cat: &'static Category,
    sinks: u8,
    track: Option<u64>,
}

impl Drop for SliceGuard {
    #[inline]
    fn drop(&mut self) {
        if self.sinks != 0 {
            emit_with_sinks(self.sinks, &spec(self.cat, Phase::End, self.track, None, &[]));
        }
    }
}

/// Begins a slice on the calling thread and returns a guard that ends it.
#[inline]
pub fn scoped_slice(cat: &'static Category, name: &str, args: &[Arg]) -> SliceGuard {
    let sinks = cat.sinks();
    if sinks != 0 {
        emit_with_sinks(sinks, &spec(cat, Phase::Begin, None, Some(name), args));
    }
    SliceGuard { cat, sinks, track: None }
}

/// Like [`scoped_slice`] on an explicit track.
#[inline]
pub fn scoped_slice_on(cat: &'static Category, track: Track, name: &str, args: &[Arg]) -> SliceGuard {
    let sinks = cat.sinks();
    if sinks != 0 {
        emit_with_sinks(sinks, &spec(cat, Phase::Begin, Some(track.uuid), Some(name), args));
    }
    SliceGuard { cat, sinks, track: Some(track.uuid) }
}

/// `trace_scope!(CAT, "name", "arg" => value, ...)` → [`SliceGuard`].
/// Arguments are only evaluated when the category is enabled.
#[macro_export]
macro_rules! trace_scope {
    ($cat:expr, $name:expr $(, $k:expr => $v:expr)* $(,)?) => {{
        if $cat.is_enabled() {
            $crate::scoped_slice(&$cat, $name, &[$($crate::Arg::new($k, $v)),*])
        } else {
            $crate::scoped_slice(&$cat, $name, &[])
        }
    }};
}

/// `trace_instant!(CAT, "name", "arg" => value, ...)`.
#[macro_export]
macro_rules! trace_instant {
    ($cat:expr, $name:expr $(, $k:expr => $v:expr)* $(,)?) => {{
        if $cat.is_enabled() {
            $crate::instant(&$cat, $name, &[$($crate::Arg::new($k, $v)),*]);
        }
    }};
}

/// `trace_counter!(CAT, "track name", value)`; the counter track is created
/// once per call site.
#[macro_export]
macro_rules! trace_counter {
    ($cat:expr, $track:expr, $value:expr) => {{
        if $cat.is_enabled() {
            static T: ::std::sync::OnceLock<$crate::CounterTrack> = ::std::sync::OnceLock::new();
            let t = *T.get_or_init(|| $crate::CounterTrack::new($track));
            $crate::counter(&$cat, t, $value);
        }
    }};
}
