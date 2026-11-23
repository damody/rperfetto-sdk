//! Clocks, pid/tid and thread names.

/// Timestamp in nanoseconds of the clock used for every event.
///
/// Linux/Android: `CLOCK_BOOTTIME`, which is Perfetto's default trace clock
/// (builtin clock id 6), so events line up with ftrace/sched without any clock
/// conversion. Other platforms: a monotonic clock (declared as
/// `BUILTIN_CLOCK_MONOTONIC` in the in-process clock snapshot).
#[inline]
pub fn now_ns() -> u64 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts);
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static BASE: OnceLock<Instant> = OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        // Offset so that the first timestamp is not 0 (0 is treated as unset).
        1_000_000_000 + base.elapsed().as_nanos() as u64
    }
}

/// The builtin clock id `now_ns()` reads.
pub const TRACE_CLOCK_ID: u32 = if cfg!(any(target_os = "linux", target_os = "android")) {
    crate::protos::BUILTIN_CLOCK_BOOTTIME
} else {
    crate::protos::BUILTIN_CLOCK_MONOTONIC
};

/// Snapshot of (clock id, timestamp) pairs for the in-process trace header.
pub fn clock_snapshot() -> Vec<(u32, u64)> {
    #[allow(unused_mut)]
    let mut v = vec![(TRACE_CLOCK_ID, now_ns())];
    // TrackEvent packets carry no clock id, and trace_processor reads them as BOOTTIME.
    // Without a BOOTTIME entry it cannot convert them and drops every event
    // (`clock_sync_failure_unknown_source_clock`), so alias the monotonic clock.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    v.push((crate::protos::BUILTIN_CLOCK_BOOTTIME, v[0].1));
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        for (id, clk) in [
            (crate::protos::BUILTIN_CLOCK_MONOTONIC, libc::CLOCK_MONOTONIC),
            (crate::protos::BUILTIN_CLOCK_REALTIME, libc::CLOCK_REALTIME),
        ] {
            let mut ts: libc::timespec = std::mem::zeroed();
            libc::clock_gettime(clk, &mut ts);
            v.push((id, ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64));
        }
    }
    v
}

pub fn pid() -> i32 {
    std::process::id() as i32
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn tid() -> i32 {
    unsafe { libc::gettid() as i32 }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn tid() -> i32 {
    use std::sync::atomic::{AtomicI32, Ordering};
    static NEXT: AtomicI32 = AtomicI32::new(1);
    thread_local!(static TID: i32 = NEXT.fetch_add(1, Ordering::Relaxed));
    TID.with(|t| *t)
}

/// Name of the calling thread (kernel `comm` on Linux/Android).
pub fn current_thread_name() -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if let Ok(s) = std::fs::read_to_string(format!("/proc/self/task/{}/comm", tid())) {
            let s = s.trim_end();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    std::thread::current().name().map(str::to_string)
}

pub fn process_name() -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if let Ok(b) = std::fs::read("/proc/self/cmdline") {
            let first = b.split(|&c| c == 0).next().unwrap_or(&[]);
            if !first.is_empty() {
                return Some(String::from_utf8_lossy(first).into_owned());
            }
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
}
