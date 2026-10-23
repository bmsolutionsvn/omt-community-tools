//! Wall-clock timestamps in OMT's 100 ns units.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// 100 ns ticks per second, the unit of OMT timestamps.
const TICKS_PER_SEC: i128 = 10_000_000;

static FREQUENCY: OnceLock<i64> = OnceLock::new();
static EPOCH_OFFSET: OnceLock<i64> = OnceLock::new();

/// QPC ticks since boot, in 100 ns units.
fn qpc_ticks() -> i64 {
    let freq = *FREQUENCY.get_or_init(|| {
        let mut freq = 0i64;
        unsafe { QueryPerformanceFrequency(&mut freq) }.expect("QueryPerformanceFrequency failed");
        freq.max(1)
    });
    let mut count = 0i64;
    unsafe { QueryPerformanceCounter(&mut count) }.expect("QueryPerformanceCounter failed");
    // i128 keeps this exact past the 25-hour i64 overflow point.
    (i128::from(count) * TICKS_PER_SEC / i128::from(freq)) as i64
}

/// Wall clock since the Unix epoch in 100 ns units.
pub fn now() -> i64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before 1970");
    (since.as_nanos() / 100) as i64
}

/// `ticks` on the epoch basis WGC's frame timestamps and OMT use.
pub fn from_qpc(ticks: i64) -> i64 {
    let offset = *EPOCH_OFFSET.get_or_init(|| now() - qpc_ticks());
    ticks.saturating_add(offset)
}

#[test]
fn qpc_maps_onto_the_wall_clock() {
    let qpc = qpc_ticks();
    let converted = from_qpc(qpc);
    let wall = now();
    // 5 s in 100 ns ticks, generous for clock reads in between.
    assert!(
        (converted - wall).abs() < 50_000_000,
        "off by {} ticks",
        converted - wall
    );
    assert!(from_qpc(qpc + 10_000_000) > from_qpc(qpc));
}
