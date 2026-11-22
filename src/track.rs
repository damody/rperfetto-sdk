//! Tracks: where events are drawn on the timeline.
//!
//! Every track has a 64-bit uuid and a `TrackDescriptor`. Descriptors are
//! emitted lazily, once per writer sequence, right before the first event that
//! references the track (parents first).

use crate::platform;
use crate::protos::{CounterDescriptor, ProcessDescriptor, ThreadDescriptor, TrackDescriptor};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Debug)]
pub(crate) enum TrackKind {
    Process,
    Thread { tid: i32 },
    Named,
    Counter,
}

#[derive(Clone, Debug)]
pub(crate) struct TrackInfo {
    pub kind: TrackKind,
    pub name: String,
    pub parent: Option<u64>,
    pub unit_name: Option<String>,
}

fn tracks() -> &'static RwLock<HashMap<u64, TrackInfo>> {
    static T: OnceLock<RwLock<HashMap<u64, TrackInfo>>> = OnceLock::new();
    T.get_or_init(|| RwLock::new(HashMap::new()))
}

pub(crate) fn track_info(uuid: u64) -> Option<TrackInfo> {
    tracks().read().unwrap().get(&uuid).cloned()
}

fn register(uuid: u64, info: TrackInfo) {
    tracks().write().unwrap().insert(uuid, info);
}

/// SplitMix64 finalizer: spreads small inputs over the whole uuid space.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

fn hash_str(seed: u64, s: &str) -> u64 {
    // FNV-1a, then mixed with the seed.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    mix(h ^ mix(seed))
}

/// Process name override set through [`crate::InitArgs`].
pub(crate) static PROCESS_NAME: RwLock<Option<String>> = RwLock::new(None);

/// A track for slices and instants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Track {
    pub(crate) uuid: u64,
}

impl Track {
    /// The track of the calling thread. Events without an explicit track go
    /// here; on the timeline it is the thread's row (merged with its sched
    /// slices in system traces).
    pub fn current_thread() -> Track {
        Track { uuid: thread_uuid(platform::pid(), platform::tid()) }
    }

    /// The process-scoped track (shown on the process row).
    pub fn process() -> Track {
        let pid = platform::pid();
        let uuid = process_uuid(pid);
        if track_info(uuid).is_none() {
            let name = PROCESS_NAME
                .read()
                .unwrap()
                .clone()
                .or_else(platform::process_name)
                .unwrap_or_default();
            register(uuid, TrackInfo { kind: TrackKind::Process, name, parent: None, unit_name: None });
        }
        Track { uuid }
    }

    /// A custom named track under the process. Same name → same track.
    pub fn named(name: &str) -> Track {
        Self::named_with_parent(name, Track::process())
    }

    /// A custom named track nested under `parent`.
    pub fn named_with_parent(name: &str, parent: Track) -> Track {
        let uuid = hash_str(parent.uuid ^ 0x7472_6163_6b00, name);
        if track_info(uuid).is_none() {
            register(
                uuid,
                TrackInfo { kind: TrackKind::Named, name: name.to_string(), parent: Some(parent.uuid), unit_name: None },
            );
        }
        Track { uuid }
    }

    pub fn uuid(&self) -> u64 {
        self.uuid
    }

    /// Looks up a track by uuid (as returned by [`Track::uuid`]); used by the C
    /// API. Returns `None` for unknown uuids.
    pub fn from_uuid(uuid: u64) -> Option<Track> {
        let t = Track { uuid };
        if uuid == Track::current_thread().uuid {
            return Some(t);
        }
        track_info(uuid).map(|_| t)
    }
}

/// A counter track: a time series of numeric values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CounterTrack {
    pub(crate) uuid: u64,
}

impl CounterTrack {
    /// A process-scoped counter track.
    pub fn new(name: &str) -> CounterTrack {
        Self::with_parent(name, Track::process(), None)
    }

    /// A process-scoped counter track with a unit label (e.g. "bytes", "fps").
    pub fn with_unit(name: &str, unit: &str) -> CounterTrack {
        Self::with_parent(name, Track::process(), Some(unit))
    }

    /// A counter track nested under `parent` (e.g. [`Track::current_thread`]
    /// for a thread-scoped counter).
    pub fn with_parent(name: &str, parent: Track, unit: Option<&str>) -> CounterTrack {
        let uuid = hash_str(parent.uuid ^ 0x636f_756e_7400, name);
        if track_info(uuid).is_none() {
            register(
                uuid,
                TrackInfo {
                    kind: TrackKind::Counter,
                    name: name.to_string(),
                    parent: Some(parent.uuid),
                    unit_name: unit.map(str::to_string),
                },
            );
        }
        CounterTrack { uuid }
    }

    pub fn uuid(&self) -> u64 {
        self.uuid
    }

    pub fn from_uuid(uuid: u64) -> Option<CounterTrack> {
        match track_info(uuid) {
            Some(TrackInfo { kind: TrackKind::Counter, .. }) => Some(CounterTrack { uuid }),
            _ => None,
        }
    }
}

pub(crate) fn process_uuid(pid: i32) -> u64 {
    mix(0x7072_6f63_0000_0000 ^ pid as u32 as u64)
}

pub(crate) fn thread_uuid(pid: i32, tid: i32) -> u64 {
    mix(0x7468_7200_0000_0000 ^ ((pid as u32 as u64) << 32) ^ tid as u32 as u64)
}

/// Registers the calling thread's track (called by the thread writer).
pub(crate) fn register_thread(pid: i32, tid: i32, name: &str) -> u64 {
    let uuid = thread_uuid(pid, tid);
    register(
        uuid,
        TrackInfo { kind: TrackKind::Thread { tid }, name: name.to_string(), parent: None, unit_name: None },
    );
    uuid
}

/// Builds the `TrackDescriptor` for `uuid`.
pub(crate) fn descriptor(uuid: u64) -> Option<TrackDescriptor> {
    let info = track_info(uuid)?;
    let pid = platform::pid();
    let mut d = TrackDescriptor { uuid: Some(uuid), ..Default::default() };
    match info.kind {
        TrackKind::Process => {
            d.process = Some(ProcessDescriptor {
                pid: Some(pid),
                process_name: (!info.name.is_empty()).then(|| info.name.clone()),
            });
        }
        TrackKind::Thread { tid } => {
            d.thread = Some(ThreadDescriptor {
                pid: Some(pid),
                tid: Some(tid),
                thread_name: (!info.name.is_empty()).then(|| info.name.clone()),
            });
        }
        TrackKind::Named => {
            d.name = Some(info.name);
            d.parent_uuid = info.parent;
        }
        TrackKind::Counter => {
            d.name = Some(info.name);
            d.parent_uuid = info.parent;
            d.counter = Some(CounterDescriptor { unit_name: info.unit_name, is_incremental: None });
        }
    }
    Some(d)
}

/// `uuid` followed by its ancestors.
pub(crate) fn ancestry(uuid: u64) -> Vec<u64> {
    let mut v = vec![uuid];
    let mut cur = uuid;
    while let Some(p) = track_info(cur).and_then(|i| i.parent) {
        if v.contains(&p) {
            break;
        }
        v.push(p);
        cur = p;
    }
    v
}
