# Spacecraft Attitude Determination & Control

The example that answers "does this actually integrate with code I already
have?" **Every transform is bound to a real function in an ordinary Rust
crate.** The generated crate compiles and runs as-is — there is no stub to
fill in, so regenerating never clobbers hand-written code.

```
SensorBus ──by frame──> Fusion ──> Guidance ──> Telemetry
```

```
cargo run -p sigilc -- examples/avionics/attitude_control.sigil generated/avionics --emit-main --emit-graph --level 4
cd generated/avionics && cargo run --bin demo
```

## The crate being wired

`sensor_hal/` knows nothing about Sigil. It is shaped like real
hardware-facing code:

| Function | Kind | Why it matters |
| -------- | ---- | -------------- |
| `read_imu`, `read_star_tracker`, `estimate_attitude` | **blocking** | talk to a bus and sleep |
| `downlink_packet` | **async**, fallible | ordinary network call |
| `fuse_attitude` | **pure** | no I/O, cannot fail |

## The binding syntax

```
extern crate sensor_hal = path "../../examples/avionics/sensor_hal"

schema ImuFrame = sensor_hal::ImuFrame { id: String, roll_rate: Float, ... }

transform read_imu(f: ImuFrame) -> ImuFrame  = blocking sensor_hal::read_imu
transform downlink(a: Attitude) -> Attitude  = sensor_hal::downlink_packet
transform dead_reckon(a: Attitude) -> Attitude = infallible sensor_hal::fuse_attitude
```

**`blocking` is the one that earns its keep.** Calling a blocking driver
directly from an async handler stalls a runtime worker thread. That failure
degrades the *scheduler*, not the program — so it passes review, passes
tests, and shows up in production as unexplained tail latency. No proof in
this compiler would catch it either, because the driver is opaque. Declaring
it lets codegen place the call correctly:

```rust
async fn read_imu(input: ImuFrame) -> Result<ImuFrame> {
    sigil_rt::chaos::external_stage("read_imu").await?;
    tokio::task::spawn_blocking(move || sensor_hal::read_imu(input))
        .await
        .map_err(|e| SigilError::Transform(format!("read_imu: join: {e}")))?
        .map_err(|e| SigilError::Transform(format!("read_imu: {e}")))
}
```

**Schemas bind too**, and must. A schema that merely *looked* like
`sensor_hal::ImuFrame` would be a different type, and every call into the
crate would fail to typecheck. `schema X = path::Y` re-exports instead of
defining. (The foreign type must derive `Clone`, `Debug`, `Default`.)

**Binding removes the hand-editing, not the obligation.** A bound blocking or
async call still performs real I/O, so it is still external: it still
requires `@recover` or `@error`, and it still counts toward the latency
budget. Delete the failure path from `read_imu` and the build fails.
`infallible` bindings are the exception — declared unable to fail, so they
are legitimate recovery targets and are not fault-injected.

## What the build proves

| Property | Level |
| -------- | ----- |
| `Fusion.fused <= SensorBus.sampled` | 4 |
| `Guidance.commands <= Fusion.fused` | 4 |
| `Telemetry.sent <= Guidance.commands` | 4 |
| `valid_frames <= sampled` (conditional acceptance) | 3 relational |
| `confidence_sum >= 0.0` (two-sided clamp) | 3 inductive |
| end-to-end latency ≤ 250 ms including hand-off | 2 |
| no data races, no shared accumulators, every failure path declared | 1 |

Composed: **no attitude command is issued that was not derived from a sampled
frame, and nothing is downlinked that was not commanded.**

## The SLO conversation happened at build time

The first draft used 40/20/60/30 ms timeouts:

```
error[Level 2 (contracts)]: path_latency is 330ms but require path_latency
<= 250ms (processing + declared hand-off waits)
```

Tightened to 15/10/30/25 ms, totalling 160 ms. On a vehicle that argument
normally happens after an anomaly.

## Back-pressure reflects mission priority

```
send est to Fusion by est.id @deadline(5.ms)    // control path: bounded
send held to Telemetry @shed                    // telemetry: droppable
```

A spacecraft sheds telemetry before it sheds control. That priority is
written in the source, checked by the latency proof, and visible in
`topology.mmd`.

## Measured

Real blocking drivers, 8 shards, 320 frames:

```
[SensorBus] sampled = 320   valid_frames = 320
[Fusion]    fused   = 320   confidence_sum = 298.2
[Guidance]  commands = 320
[Telemetry] sent    = 320
all 5 proven invariant(s) verified at runtime      elapsed = 94ms
```

Under 20% injected faults, on top of the driver's own failure modes:

```
[SensorBus] sampled = 320   [Fusion] fused = 312
[Guidance]  commands = 312  [Telemetry] sent = 312
chaos: 271 injected faults, 306 retries, 239 recoveries, 8 shed
all 5 proven invariant(s) verified at runtime
```

271 driver faults absorbed. Every invariant held, and the degradation is
visible and counted rather than silent.
