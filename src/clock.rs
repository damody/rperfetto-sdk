//! Clocks that explicit-timestamp events (`*_at`) can be expressed in.

use crate::platform;
use crate::protos;

/// The clock a timestamp was read from. Perfetto converts every clock to the
/// trace clock through the session's clock snapshot(s).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Clock {
    /// The SDK clock ([`crate::now_ns`]): BOOTTIME on Linux/Android, a
    /// monotonic clock elsewhere.
    Sdk,
    /// `CLOCK_REALTIME`: Unix epoch nanoseconds (e.g. device wall-clock time).
    Realtime,
    /// `CLOCK_MONOTONIC`.
    Monotonic,
    /// `CLOCK_BOOTTIME` (Perfetto's default trace clock on Android).
    Boottime,
}

impl Clock {
    /// Builtin clock id in `ClockSnapshot`.
    pub fn builtin_id(self) -> u32 {
        match self {
            Clock::Sdk => platform::TRACE_CLOCK_ID,
            Clock::Realtime => protos::BUILTIN_CLOCK_REALTIME,
            Clock::Monotonic => protos::BUILTIN_CLOCK_MONOTONIC,
            Clock::Boottime => protos::BUILTIN_CLOCK_BOOTTIME,
        }
    }

    /// `TracePacket.timestamp_clock_id` for packets stamped with this clock.
    pub(crate) fn packet_id(self) -> Option<u32> {
        match self {
            Clock::Sdk => platform::SDK_PACKET_CLOCK,
            other => Some(other.builtin_id()),
        }
    }
}

/// A timestamp in nanoseconds of a given [`Clock`]. A bare `u64` converts to
/// a [`Clock::Sdk`] timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    pub ns: u64,
    pub clock: Clock,
}

impl Timestamp {
    pub const fn new(ns: u64, clock: Clock) -> Self {
        Timestamp { ns, clock }
    }
    /// Unix epoch nanoseconds.
    pub const fn realtime(ns: u64) -> Self {
        Timestamp { ns, clock: Clock::Realtime }
    }
    pub const fn boottime(ns: u64) -> Self {
        Timestamp { ns, clock: Clock::Boottime }
    }
    pub const fn monotonic(ns: u64) -> Self {
        Timestamp { ns, clock: Clock::Monotonic }
    }
}

impl From<u64> for Timestamp {
    fn from(ns: u64) -> Self {
        Timestamp { ns, clock: Clock::Sdk }
    }
}
