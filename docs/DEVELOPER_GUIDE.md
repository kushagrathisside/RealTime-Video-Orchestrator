# RVO Developer Guide

This document explains why RVO is built the way it is: the language choice, the system invariant, the crate decomposition, the layered architecture, and the hard design calls with their tradeoffs. It is written for engineers who need a deep understanding of the system, not a tour of the directory tree.

---

## 1. Why Rust

### Predictable latency is the first requirement

A garbage-collected runtime periodically stops the world to reclaim memory. For a realtime video pipeline where the live path runs on a 1 ms tick, a GC pause is not a performance regression — it is a correctness failure. Rust eliminates this by having no runtime allocator overhead on the hot path and no GC.

This is not a theoretical concern. Any language with a managed heap (Go, Java, Python) introduces latency variance that is bounded by the GC's pause time, not by the application's logic. Rust's ownership model forces memory to be reclaimed at a known point — the end of its owner's scope — which is deterministic.

### Ownership makes concurrency correct by construction

RVO shares exactly one piece of mutable state between the live path and the evidence path: the frame buffer, wrapped in `Arc<Mutex<FrameBuffer>>`. Rust's type system enforces that:

- `Arc` is the only way to share ownership across threads.
- `Mutex` is the only way to get a mutable reference to the inner value.
- The borrow checker prevents holding a `MutexGuard` across an `await` or across a blocking call — enforcing the lock discipline that makes the design correct.

In Go or C++, this discipline is a comment. In Rust, it is a compile error.

The `Send` and `Sync` marker traits mean the compiler will reject any type that is not safe to move across thread boundaries. `DetectorNode: Send` is not a convention — it is enforced on every detector implementation.

### Zero-cost abstractions over detectors

`DetectorNode` is a trait object (`Box<dyn DetectorNode>`). Dynamic dispatch has a pointer-indirect call overhead, but no heap allocation per call and no boxing on every invocation. The trait object itself is allocated once at startup. On the hot scheduler path, the cost is one vtable lookup per detector per tick, which is acceptable.

Iterator chains, `map`, `filter_map`, and `try_send` all compile down to the same machine code as a hand-written loop. There is no "cost" to expressing the scheduler logic at the level of the problem.

### Cargo workspace maps to the component model

Each crate in the workspace is a unit of compilation, linkage, and dependency. Adding `rvo-testkit` as a `dev-dependency` of `rvo-scheduler` means test infrastructure never enters the production binary. This is not achievable with a flat module layout in a single crate.

### crossbeam channels over std channels

`crossbeam_channel::bounded` is the backbone of every bounded queue in the system. Compared to `std::sync::mpsc`:

- Both `Sender` and `Receiver` are `Clone` — multiple producers are trivial.
- `try_send` is lock-free on the fast path.
- The bounded channel semantics match what the system needs: reject and count, never block.

---

## 2. What RVO Is

### The problem

Live video AI pipelines have a structural tension. The live path — camera read → frame decode → detector inference → signal publish — must run at a fixed FPS. The evidence path — clip capture → frame encode → file write — is slow and variable. Naive implementations couple these, causing the live path to stall behind a slow encoder or a disk flush.

A second problem: model inference is non-deterministic in latency. A detector that usually runs in 5 ms may occasionally take 50 ms. Without explicit scheduling policy, a slow run cascades into accumulated backlog.

A third problem: single-frame detections are noisy. A human face appearing for one frame is not an event worth capturing. A face present continuously for 3 seconds is.

### The system invariant

> The live path must never wait for slow work.

Everything that could be slow — encoding, file I/O, downstream delivery, post-roll capture — is off the live path. The live path is: camera ingestion → frame buffer push → detector execution → signal publication → event evaluation.

### Core contract

1. Capture frames without blocking; drop when the channel is full.
2. Maintain a bounded rolling window of frames for evidence extraction.
3. Schedule detector nodes under FPS and latency constraints; shed load rather than accumulate backlog.
4. Publish typed signals with TTL freshness semantics.
5. Evaluate temporal conditions over signals; confirm events over time, not on instantaneous matches.
6. Extract evidence asynchronously; the live path never waits for the clip encoder.
7. Expose metrics for every drop, skip, and failure.

### What RVO is not

- A computer vision model or training framework. It runs models; it does not define them.
- A video codec or muxer. It writes JPEG frames; video muxing is future work.
- A general-purpose stream processor like Kafka or Flink. It is a single-process, bounded runtime.
- A durable event log. Events are written to a JSON-lines file; there is no replay or retention guarantee.

---

## 3. Crate Layout

### `rvo-bin`
Entrypoint and wiring. Reads config, starts the metrics server, instantiates the frame buffer, builds detectors and the event engine, starts the camera thread and encoder worker, optionally starts the event file sink, installs the SIGHUP reload handler, then enters the 1 ms tick loop. Nothing in `rvo-bin` is a library — it is all startup sequencing and integration glue.

### `rvo-config`
YAML config loading and validation. The only crate that touches `serde` deserialization. All other crates accept already-validated config structs.

### `rvo-core`
Shared time primitives and the `Frame` type. Kept minimal so every other crate can depend on it without pulling in heavier dependencies.

### `rvo-camera`
Opens a `VideoCapture` from a device index or URI. Produces `Frame` values and `try_send`s them into a bounded channel. On a full channel, it increments the frame drop counter and discards. The camera thread does not panic on read failure; it logs a consecutive-failure count and retries. This crate has no knowledge of the downstream scheduler.

### `rvo-buffer`
`FrameBuffer`: a fixed-capacity circular buffer of `Frame`. `push` is O(1) and overwrites the oldest slot. `slice(start, end)` returns timestamp-ordered frames in the window. Wrapped in `Arc<Mutex<FrameBuffer>>` and shared between the scheduler (writer) and the clip manager (reader). The lock is never held across a blocking operation.

### `rvo-detector`
Defines the `DetectorNode` trait, `DetectorMeta`, `DetectorContext`, and `DetectorResult`. Also contains the synthetic detectors used in production config (`DummyDetector`, `LoadDetector`, `JitterDetector`). This crate is the abstraction boundary between the scheduler and any model implementation.

### `rvo-remote`
Bridges RVO to external model services. `RemoteGrpcDetector` implements `DetectorNode` but sources its signals from a gRPC service (the `rvo.detect.v1.Detector` contract in `proto/detector.proto`) instead of computing them in-process. The gRPC client runs on a dedicated worker thread with its own Tokio runtime and a persistent HTTP/2 channel; `execute()` itself never blocks (see §5.9). This is how a camera frame reaches an external YOLO / image-pipeline service and comes back as a `PersonDetected` or `FacePresent` signal. Codegen uses `tonic-build` with a vendored `protoc`, so no system protobuf compiler is required.

### `rvo-signals`
Defines `SignalType`, `Signal`, and `SignalStore`. The store is a typed blackboard: one slot per `SignalType`, O(1) read and write by type index, TTL freshness enforced at read time. Signals are the shared language between detectors and the event engine.

### `rvo-events`
Implements the condition DSL (`All` / `Any` over `SignalPredicate`), `EventDefinition`, and the temporal state machine per event (`EventMachine`). Also contains `EventPublisher` and the JSON-lines file sink. The event engine consumes the signal store, not raw frames.

### `rvo-scheduler`
The orchestration loop. Each `tick()` drains camera frames into the buffer, snapshots the newest frame, evaluates each detector (FPS gate, backoff gate, dependency gate, frame-required gate), runs eligible detectors, stores produced signals, runs the event engine, and dispatches events to publishers and the clip manager. This is the core of the runtime.

### `rvo-clips`
`ClipManager`: receives clip jobs triggered by the event engine. `on_event` is non-blocking: it anchors the clip window to the newest frame timestamp and enqueues a `PendingJob` into a bounded queue (capacity 16). A single long-lived worker thread drains this queue — for each job it sleeps until the post-roll window closes, then locks the frame buffer briefly to slice frames, and hands them to the encoder via `try_send`. No per-event thread is spawned. The encoder worker writes JPEG frames and a `meta.json` sidecar. All of this is off the live path.

### `rvo-metrics`
Global atomic counters and an HTTP server. `/metrics` serves Prometheus text format. `/health` returns `200 ok` as a liveness check. Counters cover frame drops, scheduler ticks, detector executions, skips, failures, aggregate latency, event emissions, clip drops, and event drops.

### `rvo-testkit`
Synthetic test infrastructure: `SyntheticCamera`, `ScriptedDetector`, `ProbabilisticDetector`, `LatencyDetector`, `FailingDetector`, `ChainedDetector`, `EventCapture`, `MetricsSnapshot`, `PipelineBuilder`. This crate is a `dev-dependency` only and never enters a production binary.

### `rvo-scenarios`
End-to-end integration tests using `rvo-testkit`. Each scenario builds a complete pipeline and asserts temporal behavior, drop counts, or detector health transitions.

---

## 4. End-to-End Data Flow

```
Camera / RTSP Stream
    |
    v  [bounded channel, cap 5 — drop on full → rvo_frame_drops_total]
Scheduler.tick()  [1 ms loop]
    |
    +--→ FrameBuffer  [Arc<Mutex<>>, circular, cap 300 — overwrite oldest]
    |
    +--→ DetectorNode(s)  [sequential, FPS-gated, backoff-gated, dep-gated]
    |        |
    |        v
    |    SignalStore  [typed slots, TTL freshness, O(1) read/write]
    |
    +--→ EventEngine  [Condition DSL: All/Any over SignalPredicates]
    |        |
    |        v  [EventMachine: Idle → Potential → emit → Cooldown → Idle]
    |    Vec<Event>
    |
    +--→ EventPublisher  [try_send, cap 64 — drop on full → rvo_event_drops_total]
    |        |
    |        +--→ stdout logger
    |        +--→ JSON-lines file sink (if configured)
    |
    +--→ ClipManager  [try_send to pending queue, cap 16 — drop on full → rvo_clip_drops_total]
              |
              v  [single worker thread: sleep until fire_at, lock buffer briefly, slice frames]
              |  [try_send to encoder, cap 8 — drop on full → rvo_clip_drops_total]
         Encoder Worker
              |
              v
         clips/{type}_{ts}/frame_NNNN.jpg + meta.json
```

### Bounded queue table

| Handoff | Capacity | Overflow behavior |
|---|---|---|
| Camera → Scheduler | 5 frames | Drop, count `rvo_frame_drops_total` |
| Scheduler → EventPublisher | 64 events | Drop, count `rvo_event_drops_total` |
| Scheduler → ClipManager (pending queue) | 16 jobs | Drop, count `rvo_clip_drops_total` |
| ClipManager worker → Encoder | 8 jobs | Drop, count `rvo_clip_drops_total` |
| FrameBuffer | 300 frames (~10 s at 30 fps) | Overwrite oldest |

No unbounded queue exists anywhere.

---

## 5. Key Design Decisions and Tradeoffs

### 5.1 Bounded channels and explicit frame drops

Every channel is bounded and uses `try_send`. When a channel is full, the sender discards and increments a metric counter.

The alternative — blocking the producer until the consumer catches up — turns downstream slowness into upstream stalls, which propagates back to the camera thread and breaks the realtime contract. Dropping is the correct choice when freshness matters more than completeness. A 1-second-old frame is worth less than a fresh one; it should be discarded, not queued.

Frame drops are a signal of overload, not a bug. The metrics surface them explicitly so operators can observe and respond.

The capacity choices are deliberate:
- Camera channel at 5: enough to absorb a single slow tick without dropping, not enough to buffer stale frames.
- Event publisher at 64: events are small, and bursts are short. A sink stall should not drop events immediately.
- Clip queue at 8: encoding is slow. A small queue prevents runaway memory growth from queued raw frames.

### 5.2 Frame buffer lock discipline

The `Arc<Mutex<FrameBuffer>>` is the only shared mutable state between the live path and the evidence path. The invariant is: the lock is held only for the duration of a buffer operation, never across a blocking call.

The scheduler holds the lock briefly to push a frame and to read the newest timestamp. When an event fires, `on_event` holds the lock briefly to read `newest_instant()` (anchoring the clip window), then releases immediately — no sleeping is done under the lock. The single post-roll worker holds the lock briefly to call `slice()`, and only after sleeping until the post-roll window closes. The lock is never held while sleeping.

This means there is at most one thread (the worker) competing with the scheduler for the frame buffer lock, and that contention window is a single `slice()` call — not the entire post-roll duration. The previous design (one thread per event) would have had N threads competing simultaneously at the moment their post-roll windows expired; that unbounded concurrency no longer exists.

### 5.3 Sequential detector execution

All detectors run sequentially within a single `tick()` call. This is a deliberate simplification: no thread pool, no work stealing, no concurrent signal writes.

The tradeoff: a single slow detector delays all subsequent detectors in the tick. The mitigation is load shedding — slow detectors are backed off, so they do not run every tick. For a pipeline where each detector is expected to run in milliseconds, sequential execution is simpler and has lower overhead than the coordination cost of a thread pool.

Parallel execution would require concurrent signal writes, which would require the signal store to use atomics or fine-grained locking rather than the current `&mut self` borrow model. That complexity is reserved for a future multi-stream or distributed phase.

### 5.4 Signal store as typed blackboard with TTL freshness

The signal store is not a queue. It answers one question: *What is the latest valid value of signal X right now?* There is one slot per `SignalType`, and a new write overwrites the previous value. Reads are O(1) by type index.

Freshness is enforced via TTL at read time: `signal.ts_ns + signal.ttl_ns < now_ns` means absent. This prevents stale signals from triggering events long after their detector stopped running. A signal that is not refreshed within its TTL becomes invisible to the event engine.

The store uses a seqlock-style version counter on each slot. Writes increment the version before and after the value update. Reads check that the version is even (no write in progress) and consistent across the read. Currently, writes are serialized by the `&mut self` borrow on `SignalStore`, so the seqlock is defensive against a future move to concurrent writes. If that changes, the memory ordering on the version counter must be reviewed carefully — seqlocks require at minimum `Release` on write and `Acquire` on read.

### 5.5 Temporal event confirmation

Events are confirmed over time, not on instantaneous signal matches. The state machine per event definition:

```
Idle
  → (condition first evaluates true) → Potential { start_ns }
  → (condition still true, elapsed >= duration_ns) → emit Event + Cooldown { until_ns }
  → (cooldown elapsed) → Idle

  (condition becomes false while Potential) → Idle
```

This eliminates the noise problem: a one-frame detection never fires an event. The event fires only if the condition holds continuously for `duration_ns`. The cooldown prevents re-triggering on the same sustained condition.

The tradeoff: added complexity in the event engine, and tests that validate event timing must run with real elapsed time (no clock injection). The correctness benefit — eliminating noisy one-shot events — is the core value proposition of the temporal confirmation layer.

### 5.6 Load shedding via cost hints and overrun detection

When a detector's last execution time exceeds 2× its FPS-budget interval, the scheduler applies a backoff:

- `CostHint::Low`: no backoff — always allowed to recover immediately.
- `CostHint::Medium`: 100 ms backoff.
- `CostHint::High`: 500 ms backoff.

During backoff, the detector is skipped entirely. The scheduler does not queue missed executions. This keeps the live path from falling behind when a model runs slow: RVO degrades by doing less work per unit time, not by accumulating a backlog of stale work.

The cost hint is self-declared by the detector, not measured. This is intentional: the scheduler needs the hint before execution to make admission decisions. The overrun check corrects for detectors that underestimate their cost.

A known limitation: the current load shedding policy only uses execution latency as input. It does not account for downstream queue depth (clip queue depth, event queue depth). A detector that produces many events per tick could saturate the event publisher even under acceptable execution latency.

### 5.7 Evidence pipeline as best-effort

The clip pipeline is explicitly best-effort. `ClipManager::on_event` uses `try_send` and drops on failure. The encoder worker thread runs at its own pace and does not signal back to the scheduler. A failed or slow encoding does not affect the scheduler tick rate.

The philosophical point: an event is meaningful even if the evidence capture fails. The event itself is the ground truth; the clip is supporting material. Coupling event reliability to encoding reliability would be the wrong abstraction boundary.

### 5.8 Hot reload and event machine state

SIGHUP (Unix only) reloads config, rebuilds detectors and the event engine, and swaps them into the running scheduler. Invalid configs preserve the live runtime.

A consequence: the event state machines are rebuilt from scratch on reload. Any in-progress `Potential` state is lost. Events that were building toward confirmation are reset to `Idle`. This is acceptable for config changes but should be documented as a known behavior.

### 5.9 Remote detectors over gRPC (decoupled inference)

A detector does not have to compute in-process. `RemoteGrpcDetector` (in `rvo-remote`) wraps an external model service: each camera frame is JPEG-encoded and sent over gRPC, and the reply is mapped back into a `Signal`. This is how RVO fans frames out to a YOLO service, a face/emotion service, or any process speaking the `rvo.detect.v1.Detector` contract — without coupling the orchestrator to a model runtime or language.

The hard constraint is the system invariant from §2: *the live path must never wait for slow work.* A network round-trip plus model inference is the slowest thing in the system, and the scheduler times every `execute()` call and backs off detectors that overrun (§5.6). So a naive "call gRPC inside `execute()` and block" would either stall the 1 ms tick or trip its own backoff every frame. The design decouples inference from the tick:

```
scheduler tick (hot)          worker thread (own Tokio runtime + persistent channel)
────────────────────          ─────────────────────────────────────────────────────
execute(ctx):                 loop:
  publish newest frame   →      take newest frame, JPEG-encode
  read cached result     ←      Detect() over the persistent HTTP/2 channel
  return signals (w/ ttl)       store (value, produced_at)
```

- **`execute()` is non-blocking.** It writes the newest frame into a single-slot mailbox (overwrite-newest, the same discipline as the camera→buffer handoff) and returns the most recently cached result. The network never touches the hot path.
- **Staleness is handled by the existing TTL mechanism, not a new one.** The cached result is stamped `ts_ns = now_ns − result_age`, so if the worker falls behind, the signal expires in the `SignalStore` (§5.4) exactly as an in-process signal would. Events never fire on a stale remote detection.
- **The channel is persistent and dialed lazily.** `connect_lazy` means construction never blocks; tonic reconnects transparently, so a service that is down at startup or bounces mid-run does not tear down the detector.
- **Failure is bounded, then terminal.** Transient RPC errors/timeouts are tolerated; only after a threshold of consecutive failures does the detector report `DetectorHealth::Failed`, at which point the scheduler disables it (§5.6 health gate) and the rest of the pipeline keeps running. This is the "kill a model service, the pipeline survives" property.

Tradeoffs: signals lag real-world state by roughly one round-trip, and the worker introduces one thread + one Tokio runtime per remote detector. Both are acceptable for the value — arbitrary models in any language, hot-swappable behind a stable proto, with the orchestrator's latency guarantees intact. The mapping is deliberately one detector ↔ one `SignalType`, so adding a second model is just a second `remote_grpc` entry (see §7).

---

## 6. Known Caveats and Open Problems

### Clock injection is absent

The scheduler uses `Instant::now()` directly, not an injected clock. This means tests that validate temporal behavior — event confirmation, TTL expiry, backoff duration — must run with real elapsed time. A test validating a 500 ms backoff must actually wait 500 ms. This makes temporal tests inherently slower and introduces flakiness on a loaded CI machine.

The fix is a `Clock` trait that the scheduler accepts, with a `MockClock` in testkit. This is a non-trivial refactor because `Instant` is used in many places across the scheduler and event engine.

### Sequential detector execution creates a bottleneck

All detectors share the same scheduler tick thread. A detector that takes 30 ms on a given tick delays every subsequent detector in that tick by 30 ms, even if their individual FPS budgets would have allowed them to run. Load shedding mitigates this by backing off the slow detector on the next tick, but it does not address the current tick's delay.

In a pipeline where high-cost detectors run at low FPS alongside low-cost detectors at high FPS, this can cause the low-cost detectors to miss their timing windows on ticks where a high-cost detector runs.

### Bounded pending-queue depth limits burst clip capture

`ClipManager` uses a single worker thread and a bounded pending queue (capacity 16). A burst of more than 16 confirmed events in flight simultaneously will start dropping clip jobs, counted as `rvo_clip_drops_total`. This is intentional — the old design spawned one thread per event, which was the only unbounded resource path in the system. The current design trades worst-case clip capture completeness for strict resource bounds. If 16 is too small for a high-event workload, `PENDING_CAP` can be increased, but memory growth is proportional to that capacity times the post-roll duration.

### Global metric atomics in parallel tests

`rvo-metrics` uses global atomic counters shared across all test threads. In a parallel test run, counter deltas from different tests intermingle. A test that asserts `rvo_frame_drops_total == 2` may see a higher value because another test dropped frames concurrently.

The workaround is `cargo test -- --test-threads=1`. The fix requires per-instance metrics objects rather than global atomics, which is a larger refactor that affects the metric API across all crates.

### Post-roll clip timing requires test accommodation

`ClipManager` uses a single worker thread that sleeps until each job's `fire_at` before slicing the buffer. Any test that reads `clip_rx` must wait longer than `clip_after` before asserting, since there is no signal back to the test when the worker finishes sleeping.

Until clock injection is implemented, tests must use a short but non-zero `clip_after` and sleep at least that long before asserting on clip output.

### Load shedding ignores queue depth

The backoff policy uses only execution latency as input. A detector that runs fast but produces a burst of events can saturate the event publisher channel (capacity 64) without triggering any backoff. Similarly, a ClipManager that spawns many post-roll threads can exhaust the encoder queue (capacity 8) without affecting detector scheduling.

Incorporating downstream queue depth into the shedding policy would make the system more holistically self-limiting under load. For example, a detector that fires many events per tick could saturate the EventPublisher channel (capacity 64) or fill the ClipManager pending queue (capacity 16) without affecting detector scheduling.

### Static lifetime on `DetectorMeta` output lists

`DetectorMeta.output_signals` and `dependencies` are typed as `&'static [SignalType]`. This is correct for detectors whose signal lists are compile-time constants. For test detectors with dynamic output lists, the only options are:

1. Declare the slice as a module-level `static`.
2. Leak a `Box<[SignalType]>` to obtain a `&'static` reference.

Option 2 is acceptable in test-only code but is a memory leak that grows with the number of test detector instances. If the API is ever extended to support dynamically configured detectors, the lifetime constraint must be relaxed, likely by switching to a `Vec<SignalType>` owned by the `DetectorMeta` struct.

---

## 7. Running RVO: the CLI and the TUI

RVO ships two front-ends, both in `rvo-bin`: a headless **CLI** (`rvo`, the default binary) for servers and scripted demos, and an interactive **TUI** (`rvo-tui`) for driving and watching the pipeline live. Both run the same runtime — they only differ in how you start it and what you see.

### 7.1 The CLI app (`rvo`)

#### How to use it

```sh
# Default: read config/rvo.yaml (or $RVO_CONFIG), run until killed.
cargo run -p rvo-bin

# Discover camera options before starting.
cargo run -p rvo-bin -- --list-cameras
#   device 0  (use: --camera-device 0)

# Pick a camera and attach remote model services from the command line.
cargo run -p rvo-bin -- \
  --camera-device 0 \
  --detector http://localhost:50051=PersonDetected \
  --detector http://localhost:50052=FacePresent \
  --clips-dir clips/demo
```

| Flag | Purpose |
|---|---|
| `--config <PATH>` | Config file path (overrides `$RVO_CONFIG`). |
| `--camera-device <N>` | Use local device index `N`. |
| `--camera-uri <URI>` | Use an RTSP/file/MJPEG URI (mutually exclusive with `--camera-device`). |
| `--detector ENDPOINT=SIGNAL` | Add a `remote_grpc` detector. Repeatable. |
| `--clips-dir <DIR>` | Output directory for evidence clips. |
| `--metrics-port <PORT>` | Prometheus/health server port (default 9090). |
| `--list-cameras` | Probe device indices 0–9, print which open, exit. |

#### How it works

`main()` parses flags with `clap`, then: resolves the config path (`--config` → `$RVO_CONFIG` → `config/rvo.yaml`), loads and validates the YAML, and applies CLI overrides on top of the parsed `RvoConfig` — camera source, clips dir, and any `--detector` specs appended as `remote_grpc` `DetectorConfig` entries. The merged config then flows through the normal wiring (`build_detectors`, `build_event_engine`) into the scheduler's 1 ms tick loop.

The division of labour is deliberate: **CLI flags augment the camera and add detectors; events always come from the config file.** This keeps the event rules (which need durations, cooldowns, thresholds) declarative while letting you point the binary at a different camera or model service without editing YAML. `--list-cameras` is backed by `rvo_camera::list_cameras`, which best-effort-opens each device index so you know what `--camera-device` values are valid. Note that SIGHUP reload re-reads the file and therefore drops CLI overrides — the file is the source of truth on reload.

### 7.2 The TUI app (`rvo-tui`)

#### How to use it

```sh
cargo run -p rvo-bin --bin rvo-tui
# or against the gRPC demo config:
RVO_CONFIG=config/rvo-remote.yaml cargo run -p rvo-bin --bin rvo-tui
```

Two phases:

- **Menu** — pick a camera source. Probed local devices and the config default are listed; `↑/↓` (or `j/k`) to move, `Enter` to start, `q` to quit.
- **Dashboard** — a live view while the pipeline runs: configured **services**, the **signal** blackboard (each `SignalType` shown present/absent with its current value), aggregate **metrics** (ticks, detector exec/skip/fail, average exec latency, events, drops), and the **recent events** stream. `q`/`Esc` quits.

#### How it works

The detectors and events still come entirely from config; the menu only chooses the camera. On start, the TUI builds the same runtime as the CLI and spawns the scheduler tick loop on a background thread, then the UI thread renders at ~7 fps using `ratatui`. It reads three live sources without disturbing the hot path:

- **Metrics** — the global atomic counters in `rvo-metrics` (`METRICS`), read directly each frame.
- **Signals** — `Scheduler::signal_snapshot()` briefly locks the scheduler to read each slot's non-expired value (the same TTL check the event engine uses).
- **Events** — a tap thread drains the event channel into a 50-entry ring buffer the dashboard renders from, instead of the stdout/file sink the CLI uses.

One detail worth noting: the TUI sets `RVO_REMOTE_SILENT` so a remote detector whose service is down cannot spam stderr over the alternate screen. The same failure still surfaces — as a rising `detector_fail` count and, after the failure threshold, the detector dropping out — just without corrupting the display.

### 7.3 The end-to-end gRPC demo

`demo/` wires the whole story together with two trivial stub model services (see `demo/README.md`):

```sh
pip install -r demo/requirements.txt
bash demo/run_demo.sh        # codegen stubs, start both services, run RVO
```

Then cover/uncover the camera to drive the signals and observe `curl :9090/metrics`, `tail -f events.jsonl`, and `ls clips/demo/`. The stub services implement the same `rvo.detect.v1.Detector` proto as a real model would, so the demo exercises the actual remote-detector path (§5.9), not a mock.

### 7.4 The web POC (`rvo-web`)

For the most legible "what does RVO do" demo — and to make the pluggable model story tangible to a new user or an interviewer — `rvo-web` serves a browser dashboard:

```sh
cargo run -p rvo-bin --bin rvo-web        # then open http://127.0.0.1:8080
# RVO_CONFIG=config/rvo-remote.yaml RVO_WEB_PORT=9000 cargo run -p rvo-bin --bin rvo-web
```

The page shows the camera source, the live signal blackboard (chips that light up green when present), aggregate metrics, registered model nodes, and recent events — polling `GET /api/state` every 700 ms. The key interaction is **adding a model node from the browser**: enter a gRPC `endpoint` + the `signal` it produces and submit, which `POST /api/nodes` turns into a `RemoteGrpcDetector` injected into the running scheduler via `Scheduler::add_detector` (§7 wiring). No restart — the node appears and its signal lights up as soon as the service responds. This is the camera→RVO→models fan-out made visible.

How it works: `rvo-web` builds the same runtime as the CLI, runs the scheduler tick loop on a background thread, and reuses the `tiny_http` server (no new web framework). It reads live state from `Scheduler::signal_snapshot()`, the global `METRICS`, and an event-tap ring buffer; the frontend is a single embedded HTML/JS page with no external assets, so it works offline. Routes: `GET /` (page), `GET /api/state` (JSON), `POST /api/nodes` (add a node), `GET /metrics` (Prometheus, same as the CLI server).

Because a model node requires camera frames to act on, a webcam-less host should point `camera.source_uri` at a video file in the config so frames flow and added nodes have something to score.

---

## 8. Practical Dev Workflow

### Build and run

```sh
cargo build -p rvo-bin
RVO_CONFIG=config/rvo.yaml cargo run -p rvo-bin
```

### Observe the running system

```sh
curl http://127.0.0.1:9090/metrics
curl http://127.0.0.1:9090/health
tail -f events.jsonl          # if event_log is set in config
ls clips/                     # evidence output directories
```

### Run tests

```sh
cargo test --workspace
cargo test -p rvo-scenarios   # integration scenarios only
cargo test -- --test-threads=1  # if metric assertions are sensitive to parallel runs
```

### Add a detector

1. Implement `DetectorNode` in `rvo-detector` (production) or `rvo-testkit` (test-only).
2. Declare `meta()` with `id`, `max_fps`, `output_signals`, `dependencies`, `cost_hint`, and `requires_frame`.
3. Wire it into `rvo-bin` or config.
4. Write a scenario test in `rvo-scenarios` if it affects event behavior.

### Add a remote model service

1. Implement the `rvo.detect.v1.Detector` gRPC contract in any language (see `demo/services/model_service.py` for a reference). Return `SignalOut` entries whose `signal_type` matches a known `SignalType` name.
2. Add a `remote_grpc` detector to config (`endpoint`, `output_signal`, optional `max_fps`/`timeout_ms`/`ttl_ms`) or pass `--detector ENDPOINT=SIGNAL` on the CLI.
3. No Rust changes are needed for a new model — the proto is the boundary. To validate the integration deterministically, add a test against an in-process tonic mock server (see `crates/rvo-remote/tests/grpc_pipeline.rs`).

### Add a signal type

1. Extend `SignalType` in `rvo-signals` (add to `ALL` and `name()` too).
2. Update condition builders or event definitions that reference the new type.
3. Verify freshness and TTL semantics in a scenario test.

### Add an event rule

1. Define `EventDefinition` with `condition`, `duration_ns`, and `cooldown_ns`.
2. Register it in `EventEngine::new_many(...)`.
3. Write a scenario that validates both the confirmation duration and the cooldown.

---

## 9. Repo Structure

### Proto file sync

The `rvo.detect.v1.Detector` proto is defined in two locations:

- **Canonical**: `crates/rvo-remote/proto/detector.proto` — used by the Rust `tonic-build` codegen.
- **Demo copy**: `demo/proto/detector.proto` — used by the Python stub services in `demo/services/`.

The demo copy carries a comment (`// Keep in sync with crates/rvo-remote/proto/detector.proto`) but sync is currently manual. When changing the proto, update both files. The canonical copy has full doc comments; the demo copy intentionally has minimal comments. A CI check enforcing field-level equivalence is future work.

---

```
Cargo.toml                      workspace members
config/
  rvo.yaml                      default runtime config
  rvo-remote.yaml               demo config: two remote_grpc detectors
crates/
  rvo-bin/                      entrypoint
    src/main.rs                 rvo CLI (default binary)
    src/bin/rvo-tui.rs          rvo-tui interactive dashboard
    src/bin/rvo-web.rs          rvo-web browser dashboard + node-add API
    examples/                   synthetic_demo, rtsp_demo
  rvo-config/                   YAML loading
  rvo-core/                     Frame, time primitives
  rvo-camera/                   capture (+ list_cameras probe)
  rvo-buffer/                   circular frame buffer
  rvo-detector/                 trait and synthetic detectors
  rvo-remote/                   gRPC remote detector (proto + tonic client)
  rvo-signals/                  typed signal store
  rvo-events/                   condition DSL, event engine, publishers
  rvo-scheduler/                orchestration loop
  rvo-clips/                    evidence pipeline
  rvo-metrics/                  Prometheus counters, HTTP
  rvo-testkit/                  test-only infrastructure
  rvo-scenarios/                integration tests
demo/                           end-to-end gRPC demo
  proto/detector.proto          shared service contract
  services/model_service.py     trivial stub model service
  run_demo.sh, gen_protos.sh    harness
docs/
  ARCHITECTURE.md               data flow, component contracts, design boundaries
  BENCHMARKING.md               benchmark suite reference (how to run, scenarios, CSV schema)
  BENCHMARK_PERFORMANCE.md      methodology rationale, measured results, architectural claims
  CURRENT_IMPLEMENTATION.md     what the running code does today
  DEVELOPER_GUIDE.md            this file
  LOW_LATENCY_SYSTEMS.md        domain primer: latency, queuing, scheduling, observability
  PLOT_GUIDE.md                 figure descriptions, axes, and plotting recipes
  README.md                     project overview and quick start
```
