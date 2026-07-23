//! A stand-in for the kind of crate an avionics team already owns.
//!
//! Nothing here knows about Sigil. It is deliberately shaped like real
//! hardware-facing code:
//!
//!   * `read_imu` / `read_star_tracker` are **blocking** — they talk to a bus
//!     and sleep. Calling either directly from an async task would stall a
//!     runtime worker thread. That is the classic mistake, and it degrades the
//!     scheduler rather than the program, so it survives code review.
//!   * `downlink_packet` is **async** and fallible, like any network call.
//!   * `fuse_attitude` is **pure** — no I/O, cannot fail.
//!
//! Sigil binds to these directly; the generated crate calls them with the
//! correct placement and error mapping, and needs no hand-editing.

#![forbid(unsafe_code)]

use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct ImuFrame {
    pub id: String,
    pub roll_rate: f64,
    pub pitch_rate: f64,
    pub yaw_rate: f64,
    pub valid: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Attitude {
    pub id: String,
    pub roll: f64,
    pub pitch: f64,
    pub yaw: f64,
    pub confidence: f64,
    pub valid: i64,
}

#[derive(Debug)]
pub struct HalError(pub String);

impl std::fmt::Display for HalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn jitter(seed: &str) -> u64 {
    // Deterministic pseudo-jitter so runs are reproducible.
    seed.bytes().map(|b| b as u64).sum::<u64>() % 7
}

/// BLOCKING: reads the inertial measurement unit over a bus.
pub fn read_imu(mut frame: ImuFrame) -> Result<ImuFrame, HalError> {
    std::thread::sleep(Duration::from_micros(200 + jitter(&frame.id) * 50));
    if frame.id.ends_with('7') {
        return Err(HalError("imu: bus timeout".into()));
    }
    frame.roll_rate = 0.01;
    frame.pitch_rate = 0.02;
    frame.yaw_rate = 0.015;
    frame.valid = 1;
    Ok(frame)
}

/// BLOCKING: star tracker read; slower, occasionally unavailable.
pub fn read_star_tracker(mut a: Attitude) -> Result<Attitude, HalError> {
    std::thread::sleep(Duration::from_micros(400 + jitter(&a.id) * 80));
    if a.id.ends_with('3') {
        return Err(HalError("star tracker: no fix".into()));
    }
    a.confidence = 0.98;
    Ok(a)
}

/// PURE: sensor fusion. No I/O, cannot fail, cheap.
pub fn fuse_attitude(mut a: Attitude) -> Attitude {
    a.roll = a.roll * 0.98 + 0.01;
    a.pitch = a.pitch * 0.98 + 0.02;
    a.yaw = a.yaw * 0.98 + 0.015;
    a
}

/// BLOCKING: converts a raw IMU frame into an attitude estimate. Talks to a
/// coprocessor, so it blocks.
pub fn estimate_attitude(f: ImuFrame) -> Result<Attitude, HalError> {
    std::thread::sleep(Duration::from_micros(120 + jitter(&f.id) * 30));
    if f.valid == 0 {
        return Err(HalError("estimator: frame not valid".into()));
    }
    Ok(Attitude {
        id: f.id,
        roll: f.roll_rate * 0.1,
        pitch: f.pitch_rate * 0.1,
        yaw: f.yaw_rate * 0.1,
        confidence: 0.5,
        valid: 1,
    })
}

/// ASYNC: telemetry downlink; fallible, like any network call.
pub async fn downlink_packet(a: Attitude) -> Result<Attitude, HalError> {
    tokio::time::sleep(Duration::from_micros(150)).await;
    if a.id.ends_with("11") {
        return Err(HalError("downlink: window closed".into()));
    }
    Ok(a)
}
