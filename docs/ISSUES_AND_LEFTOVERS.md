# Issues And Leftovers

This is the concrete triage list for making the current RVO repository work as
a usable realtime video AI runtime.

It is intentionally different from `ROADMAP.md`:

- `ROADMAP.md` describes the platform direction.
- This file lists current blockers, bugs, incomplete sections, and the minimum
  work needed to make the implementation usable.

## High-To-Low Priority Fix Order

This is the implementation order I would use. Items near the top either block
compilation, break realtime correctness, or prevent RVO from being useful for
real video inference.

1. Fixed: crate exports and imports.
   `rvo-camera` now exports its camera modules, and `DummyDetector` imports now
   use the crate root re-export.

2. Fixed: add all local crates to the workspace.
   Without this, workspace checks and tests will not cover the full repository.

3. Fixed: make reload cross-platform or guard `SIGHUP`.
   The reload thread is now Unix-only, while non-Unix builds skip SIGHUP reload.

4. Fixed: scheduler monotonic time.
   `now.elapsed()` is nearly zero every tick, so event duration and cooldown
   behavior cannot be trusted.

5. Fixed: normalize signal TTL semantics.
   `SignalStore` treats TTL as a duration, while `DummyDetector` writes it like
   an absolute expiry timestamp.

6. Fixed: event tests and zero-duration event behavior.
   Tests should use fresh signals, and the event engine should not divide by
   zero when duration is zero.

7. Fixed: make clip creation safe when the frame buffer is empty.
   Event-triggered clip extraction should drop or defer evidence instead of
   panicking.

8. Fixed: increment emitted-event metrics.
   The metric exists but is not updated when an event fires.

9. Fixed: return frame slices in chronological order.
   Circular-buffer storage order is not the same as timestamp order.

10. Fixed: make config reload non-fatal.
    Invalid reloads should preserve the old runtime instead of panicking.

11. Harden camera failure behavior.
    Camera open/read failures need health reporting, retry/backoff, and no
    tight failure loop.

12. Pass frames or frame handles into `DetectorContext`.
    This is the first major feature required for real DL model integration.

13. Add detector metadata and dependency gating.
    The scheduler needs declared dependencies, frame requirements, output signal
    types, cost hints, and enabled state.

14. Replace the single-slot signal store with typed signal slots.
    Real model orchestration needs multiple fresh signal types, not one global
    latest signal.

15. Expand the event engine beyond one hardcoded dummy condition.
    Support multiple event definitions and predicates over signal types.

16. Use detector health and latency in scheduler policy.
    `DetectorHealth` exists, but it does not currently affect behavior.

17. Implement real evidence output or event-only mode.
    The encoder currently prints and sleeps; a usable MVP needs either actual
    artifacts or a clear external event output path.

18. Add an event publisher for applications.
    RVO needs a way to emit confirmed events to downstream systems.

19. Add load shedding policy.
    Cost hints, queue pressure, and detector latency should drive graceful
    degradation.

20. Add production platform features.
    Multi-stream ingest, model isolation, GPU/runtime abstraction, health
    endpoints, structured logging, control APIs, durable metadata, and CI all
    come after the core runtime works.

## Verification Status

`cargo` and `rustc` are not available on PATH in the current environment, so
these findings are based on source inspection rather than a successful local
build.

The first engineering step should be to run:

```powershell
cargo check
cargo test
```

after Rust is installed or available on PATH.

## P0: Build And Wiring Blockers

These were likely to prevent the current project from compiling or running as
intended. The source-level fixes have been applied, but they still need
`cargo check` verification.

### `rvo-camera` Exports Nothing

Status: fixed in source.

`crates/rvo-camera/src/lib.rs` was empty.

But the binary imports:

```rust
use rvo_camera::{start_camera, CameraConfig};
```

And the scheduler test imports:

```rust
use rvo_camera::mock::start_mock_camera;
```

Applied fix:

```rust
pub mod camera;
pub mod mock;

pub use camera::{start_camera, CameraConfig};
```

### `DummyDetector` Is Imported From The Wrong Module

Status: fixed in source.

`DummyDetector` is re-exported from the `rvo-detector` crate root:

```rust
pub use dummy::DummyDetector;
```

But the binary and scheduler test import it from `rvo_detector::detector`.

Original imports:

```rust
use rvo_detector::detector::{DetectorNode, DummyDetector};
use rvo_detector::detector::DummyDetector;
```

Applied fix:

```rust
use rvo_detector::{DummyDetector};
use rvo_detector::detector::DetectorNode;
```

or:

```rust
use rvo_detector::dummy::DummyDetector;
```

### Workspace Members Are Incomplete

Status: fixed in source.

The root `Cargo.toml` previously listed only:

- `rvo-core`
- `rvo-signals`
- `rvo-detector`
- `rvo-scheduler`
- `rvo-bin`

But the project also contains:

- `rvo-buffer`
- `rvo-camera`
- `rvo-clips`
- `rvo-config`
- `rvo-events`
- `rvo-metrics`

Path dependencies can still compile, but leaving these crates out of the
workspace means `cargo test --workspace` will not directly cover all local
crates. Every local crate is now listed as a workspace member.

### `SIGHUP` Is Unix-Oriented

Status: fixed in source with platform gating.

The binary used:

```rust
use signal_hook::consts::SIGHUP;
```

That reload mechanism is natural on Linux, but the current development
environment is Windows. The reload thread is now compiled only on Unix, and
non-Unix platforms print that SIGHUP reload is disabled.

Future options:

- compile SIGHUP reload only on Unix
- add a control endpoint for reload
- add file watching for local development

## P1: Runtime Correctness Blockers

These issues affect whether RVO behaves correctly even after it compiles.

### Scheduler Time Does Not Advance Correctly

Status: fixed in source.

Original code:

```rust
let now = Instant::now();
let now_ns = now.elapsed().as_nanos() as u64;
```

Because `now` was just created, `now.elapsed()` was nearly zero every tick. The
event engine needs a monotonic timestamp that advances across scheduler ticks.

Applied fix:

- store a stable `started_at: Instant` in `Scheduler`
- compute `now_ns = now.duration_since(started_at).as_nanos() as u64`
- use the same monotonic basis for detector signals and event evaluation

### Signal TTL Semantics Are Inconsistent

Status: fixed in source.

`SignalStore::get` treats `ttl_ns` as a duration:

```rust
if sig.ts_ns + sig.ttl_ns < now_ns {
    None
}
```

But `DummyDetector` writes:

```rust
ttl_ns: ctx.now_ns + 1_000_000_000
```

That looks like an absolute expiry timestamp, not a duration.

Applied fix:

- choose one meaning
- recommended: `ttl_ns` should be a duration
- `DummyDetector` should write `ttl_ns: 1_000_000_000`
- tests should use realistic timestamps and TTLs

### Event Tests Do Not Match Signal Freshness

Status: fixed in source.

The event engine tests previously inserted:

```rust
Signal {
    value: 1,
    ts_ns: 1,
    ttl_ns: 1,
}
```

Then they evaluate at much later timestamps. With duration-style TTLs, this
signal is stale almost immediately, so event assertions become invalid.

Applied fix:

- use a TTL large enough for the simulated event duration
- or update the signal at each simulated tick

### Zero Duration Event Can Produce Bad Confidence

Status: fixed in source.

The test config uses `duration_ns: 0`.

The event engine computes:

```rust
(now_ns - start_ns) as f64 / self.def.duration_ns as f64
```

Production config validation rejects zero durations, but tests can still create
zero-duration definitions directly.

Applied fix:

- guard zero duration in `EventEngine`
- or make `EventDefinition` construction validate duration and cooldown

### Clip Manager Can Panic On Empty Frame Buffer

Status: fixed in source.

`ClipManager::on_event` calls:

```rust
let event_ts = buffer.newest_instant();
```

`FrameBuffer::newest_instant()` panics if the buffer is empty.

This can happen if:

- the camera has not produced frames yet
- the camera failed to open
- a detector emits signals without requiring frames

Applied fix:

- return `Option<Instant>` from `newest_instant`
- drop clip creation if no frames are available

Remaining:

- emit a metric for missed evidence

### Post-Roll Clips Are Not Actually Captured

The clip manager computes:

```rust
let end = event_ts + self.after;
let frames = buffer.slice(start, end);
```

But it slices immediately. Future post-event frames are not in the buffer yet.

Required fix:

- enqueue a delayed clip extraction job
- wait until `after` has elapsed in an async worker
- then slice the buffer for the full window

The scheduler must not wait for post-roll.

### Events Metric Is Not Incremented

Status: fixed in source.

`rvo_events_emitted_total` exists, and the scheduler now increments it when an
event is emitted.

Applied fix:

- increment `METRICS.events_emitted` inside the `if let Some(event)` block

### Frame Buffer Slice Is Not Chronologically Ordered

Status: fixed in source.

`FrameBuffer::slice` scans slots in storage order, not time order. Since the
buffer is overwritten circularly, returned frames may not be ordered by
timestamp.

Applied fix:

- sort the output by `Frame.ts`
- or iterate from the oldest slot to newest slot

### Config Reload Can Panic Instead Of Preserving Old Config

Status: fixed in source.

Original issue: `load_config` used `expect`, and detector/event factories used
`panic!` on unknown values.

On reload, invalid config should not destabilize the running system.

Applied fix:

- make config loading return `Result`
- validate fully before swapping runtime
- keep old runtime active if reload fails
- emit a reload failure log

Remaining:

- emit a reload failure metric

### Camera Failure Path Can Spin Or Die Quietly

Camera startup uses `expect` inside the spawned thread. If the camera fails to
open, the camera thread panics while the rest of the process can keep running.

Frame read failures use `continue`, which can become a tight loop if the camera
keeps failing.

Required fix:

- surface camera health
- add retry/backoff
- avoid tight looping on repeated read failure
- expose capture-alive metrics

## P2: Missing MVP Features For Real DL Workloads

These are needed before RVO can be used directly by applications that run
deep-learning models on live video.

### Detectors Do Not Receive Frames

`DetectorContext` currently contains only:

```rust
pub now_ns: u64
```

A real model needs access to at least the latest frame or a frame handle.

Required fix:

- include optional frame access in `DetectorContext`
- decide whether detectors get cloned `Mat`, borrowed frame views, or an
  immutable frame handle
- avoid unbounded copies on the hot path

### No Detector Dependency Model

The scheduler currently gates detectors only by `max_fps`.

For model orchestration, it needs dependency checks such as:

- run gaze only when face is present
- run OCR only when document region exists
- run expensive classifier only when object detector has a fresh match

Required fix:

- add detector metadata
- add dependency signal types
- skip execution when dependencies are absent or stale

### SignalStore Supports Only One Signal Slot

The current signal store cannot represent multiple typed model outputs.

Required fix:

- add signal type to `Signal`
- store one latest slot per `SignalType`
- make `get(type, now)` the main read API

### Event Engine Supports Only One Hardcoded Condition

Current event condition is:

```text
latest signal value >= threshold
```

Required fix:

- support multiple event definitions
- support conditions over signal types
- support `all` and `any` predicates
- return a list of events per tick

### Detector Health Is Ignored

`DetectorResult` includes `health`, but the scheduler ignores it.

Required fix:

- record health metrics
- degrade or disable failed detectors
- decide policy for `DEGRADED` and `FAILED`

### No Execution Latency Measurement

The scheduler counts executions and skips, but does not measure execution time.

Required fix:

- record per-detector execution latency
- use latency for load shedding and observability

### No Real Encoder Or Artifact Store

The encoder worker currently prints and sleeps.

Required fix:

- write actual clips
- write metadata sidecars
- track dropped frames and encoding latency
- handle disk failures without affecting the live path

### No Event Publisher

Events currently only trigger clip jobs. There is no external event stream for
applications.

Required fix:

- add an event publisher interface
- start with local IPC or an in-process channel
- include event id, type, timestamp, confidence, metadata, and optional clip ref

### No Load Shedding Policy

The current scheduler has no CPU, queue pressure, or model-cost policy.

Required fix:

- add cost hints
- add effective FPS adjustment
- skip high-cost detectors first under load
- emit overload/degradation metrics

## P3: Platform And Production Leftovers

These are not needed for the first working demo, but they are needed for the
company-facing infrastructure vision.

- Multi-stream ingestion
- RTSP or file stream support
- Detector worker isolation
- GPU/model runtime abstraction
- Backpressure metrics for all bounded queues
- Health endpoint
- Graceful shutdown
- Configurable camera source
- Configurable clip windows
- Configurable metrics port
- Structured logging
- Event ids and session ids
- Durable metadata storage
- Control API for reload and detector enable/disable
- Per-detector config schema
- Per-event confidence models
- Distributed detector workers
- Cross-platform reload behavior
- CI workflow for `cargo check`, `cargo test`, and formatting

## Leftover Code Sections And Placeholders

These are visible placeholders or unfinished areas in the codebase.

### Empty Files

- `crates/rvo-camera/src/lib.rs`: should export camera modules.
- `crates/rvo-core/src/frame.rs`: empty and not wired into `rvo-core/src/lib.rs`.

### Placeholder Comments

- `crates/rvo-clips/src/encoder.rs`: `// Later:` lists real encoding/file work.
- `crates/rvo-bin/src/main.rs`: `// reload thread (Section 1.4)` looks copied
  from an earlier outline and should be renamed to a normal code comment.

### Unused Or Underused Types

- `SignalType` exists but is not used by `Signal` or `SignalStore`.
- `DetectorHealth` exists but is not used by scheduler policy.
- `rvo-core::time::MonoTime` exists but the scheduler uses raw `Instant`
  directly.

## Minimum Path To A Working MVP

This is the shortest practical sequence to make RVO usable as a demo runtime.

1. Fix crate exports and imports so the project compiles.
2. Add all crates to the workspace.
3. Fix scheduler monotonic time.
4. Normalize signal TTL semantics.
5. Fix event tests around signal freshness and zero durations.
6. Make clip manager safe when no frames are available.
7. Pass latest frame or frame handle into `DetectorContext`.
8. Add at least one real frame-consuming detector stub.
9. Increment event metrics.
10. Sort frame slices chronologically.
11. Make config reload non-fatal.
12. Replace simulated encoding with at least a basic artifact writer or a clear
    event-only output mode.

After these steps, the system can be honestly described as a working realtime
video orchestration MVP.
