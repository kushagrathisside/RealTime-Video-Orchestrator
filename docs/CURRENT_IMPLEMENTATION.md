# Current Implementation

This document describes what the current Rust code does today. It is separate
from the broader RVO product vision so the implementation status stays clear.

## Repository Shape

The project is a Rust workspace with multiple crates under `crates/`.

Current top-level crates and responsibilities:

- `rvo-bin`: application entrypoint and runtime wiring
- `rvo-config`: YAML configuration loading and validation
- `rvo-camera`: OpenCV camera capture and mock camera helper
- `rvo-buffer`: bounded frame buffer
- `rvo-detector`: detector trait and synthetic detectors
- `rvo-signals`: signal storage
- `rvo-events`: temporal event engine
- `rvo-scheduler`: main orchestration loop
- `rvo-clips`: clip job creation and simulated encoder worker
- `rvo-metrics`: metrics counters and HTTP endpoint
- `rvo-core`: small shared time helper

Note: the root `Cargo.toml` currently lists only a subset of these crates as
workspace members, while other crates are pulled in through path dependencies.

## Entrypoint

The process starts in `crates/rvo-bin/src/main.rs`.

Startup flow:

1. Start the metrics server on `127.0.0.1:9090`.
2. Load `config/rvo.yaml`.
3. Build detector nodes from config.
4. Build the event engine from config.
5. Start the OpenCV camera thread.
6. Start the clip encoder worker thread.
7. Create the scheduler.
8. Start a SIGHUP reload thread.
9. Enter the scheduler tick loop.

The main loop calls `scheduler.tick()` and sleeps for 1 ms.

## Configuration

The active config is `config/rvo.yaml`.

Current detector types:

- `dummy`: emits a simple signal
- `load`: burns CPU for a configured number of nanoseconds
- `jitter`: burns a random amount of CPU time, currently disabled in config

Current event support:

- `DummyEvent`

The config format allows multiple events, but the current binary uses only the
first event definition with `cfg.events[0]`.

## Camera Path

`rvo-camera` starts a camera thread using OpenCV `VideoCapture`.

For every successful camera read:

1. A `Frame` is created with:
   - `Instant` timestamp
   - incrementing frame id
   - OpenCV `Mat`
2. The frame is sent through a bounded channel with `try_send`.
3. If the channel is full, the frame is dropped.

This keeps camera capture from blocking on the scheduler.

## Frame Buffer

`rvo-buffer` stores recent frames in a fixed-size buffer.

Important behavior:

- `push(frame)` overwrites the next slot.
- `slice(start, end)` scans the buffer and clones frames whose timestamps are
  inside the requested window.
- `newest_instant()` returns the newest frame timestamp and panics if the buffer
  is empty.

The scheduler creates the frame buffer with capacity `300`, intended to be
about 10 seconds at 30 FPS.

## Detector Model

The current detector interface is:

```rust
pub trait DetectorNode: Send {
    fn id(&self) -> &'static str;
    fn max_fps(&self) -> f64;
    fn execute(&mut self, ctx: &DetectorContext) -> DetectorResult;
}
```

Current `DetectorContext` contains only:

```rust
pub struct DetectorContext {
    pub now_ns: u64,
}
```

Current detectors:

- `DummyDetector`: runs at up to 30 FPS and emits one signal with value `1`.
- `LoadDetector`: runs at up to 10 FPS and busy-spins for `busy_ns`.
- `JitterDetector`: runs at up to 30 FPS and busy-spins for a random duration.

The current detector interface does not yet include:

- declared dependencies
- cost hints
- frame handles
- signal snapshots
- lifecycle hooks
- per-detector configuration objects

## Scheduler

`rvo-scheduler` is the current orchestration core.

Each `tick()`:

1. Drains all available frames from the camera channel into the frame buffer.
2. Increments the scheduler tick metric.
3. For each detector:
   - checks whether enough time has elapsed since the detector last ran
   - skips the detector if the FPS cap has not elapsed
   - executes the detector otherwise
   - stores produced signals
4. Updates the event engine.
5. If an event is produced, passes it to the clip manager.

Execution guarantees currently present:

- detectors do not run concurrently inside the scheduler
- detector execution is capped by `max_fps`
- skipped detector executions are not queued
- frames are drained without blocking

Execution guarantees not yet implemented:

- dependency freshness gating
- load shedding by cost
- disabling failed detectors
- detector timeout enforcement
- per-detector health policy

## Signal Store

`rvo-signals` currently implements a single-slot signal store.

The implementation uses a version counter around writes, similar to a seqlock:

1. Increment version before writing.
2. Write the signal.
3. Increment version after writing.

Reads check the version before and after reading. If the version is odd or
changes during the read, the signal is treated as absent.

The current store supports:

- one latest signal slot
- overwrite behavior
- TTL-based freshness checks

It does not yet support:

- a fixed map of signal slots by `SignalType`
- multiple signal types
- explicit snapshot views
- true multi-detector signal routing

## Event Engine

`rvo-events` converts signal state into events.

The current event engine is a single temporal state machine with states:

- `Idle`
- `Potential`
- `Cooldown`

Current condition:

```text
latest signal value >= configured signal_threshold
```

If the condition remains true for `duration_ns`, the engine emits one event and
enters cooldown for `cooldown_ns`.

Current limitations:

- only one event definition is active
- only one hardcoded signal condition exists
- no event DSL exists yet
- no event metadata exists yet
- update returns `Option<Event>`, not a list of events

## Clip Path

`rvo-clips` currently creates best-effort clip jobs when an event fires.

Current flow:

1. `ClipManager::on_event` finds the newest frame timestamp.
2. It computes a time window using configured `before` and `after` durations.
3. It slices frames from the current frame buffer.
4. It sends `(ClipJob, Vec<Frame>)` to a bounded encoder queue.
5. The encoder worker prints a message and sleeps for 200 ms.

This proves the asynchronous shape of the evidence pipeline, but it does not
yet write video files.

Important limitation: post-event frames are not really captured yet. The clip
manager computes `event_ts + after`, but immediately slices the current buffer,
so future frames are not available at that moment.

## Metrics

`rvo-metrics` exposes a Prometheus-style endpoint at:

```text
http://127.0.0.1:9090/metrics
```

Current counters:

- `rvo_scheduler_ticks`
- `rvo_detector_exec_total`
- `rvo_detector_skip_total`
- `rvo_events_emitted_total`

Current limitation: `rvo_events_emitted_total` exists but is not incremented in
the scheduler today.

## Hot Reload

The binary listens for `SIGHUP`.

On reload:

1. Reload `config/rvo.yaml`.
2. Rebuild detectors.
3. Rebuild the event engine.
4. Swap the scheduler runtime.

This supports runtime detector and event config changes without restarting the
process.

## Known Correctness Issue

The scheduler currently computes `now_ns` like this:

```rust
let now = Instant::now();
let now_ns = now.elapsed().as_nanos() as u64;
```

Because `now` was just created, `now.elapsed()` is nearly zero every tick. The
event engine expects a monotonic timestamp that advances across ticks, so this
should be replaced with a stable monotonic origin or a shared time source.

This is the most important issue to fix before relying on event duration or
cooldown behavior.

