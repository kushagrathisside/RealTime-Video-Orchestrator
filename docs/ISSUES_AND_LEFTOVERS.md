# Issues And Leftovers

Concrete triage list. `ROADMAP.md` describes the platform direction; this file
tracks what is resolved vs. still open in the current codebase.

## Resolved

All items below have been fixed in source and are reflected in the current code.

### Verification

- **`cargo check --workspace` / `cargo test` / `cargo clippy` verified** — CI
  passes on all commits on `main`. The full workspace compiles and all tests
  pass. `serde` derives, `Condition` DSL `Clone` propagation, and the
  `imgcodecs` feature are all confirmed working.

### Build and Wiring

- **rvo-camera exports** — `lib.rs` re-exports `start_camera`, `CameraConfig`,
  and the `mock` module.
- **DummyDetector import path** — imports now use the crate-root re-export.
- **Workspace members** — all eleven crates are listed in the root `Cargo.toml`.
- **SIGHUP platform guard** — reload thread compiles only on Unix; non-Unix
  builds print a notice and skip.
- **Unused dashmap dep** — removed from `rvo-signals` (the store uses a fixed
  `Vec`, not a hash map).

### Runtime Correctness

- **Scheduler monotonic time** — `started_at: Instant` is captured at
  construction; `now_ns = now.duration_since(started_at)` advances correctly
  across ticks.
- **Signal TTL semantics** — `ttl_ns` is treated as a duration everywhere;
  `DummyDetector` writes `ttl_ns: 1_000_000_000` (1 second).
- **Event tests and zero-duration behavior** — tests use realistic TTLs;
  zero-duration event yields `confidence = 1.0` without divide-by-zero.
- **Clip manager panic on empty buffer** — `newest_instant()` returns `Option`;
  clip creation is skipped gracefully.
- **Events metric** — `rvo_events_emitted_total` is incremented in the
  scheduler when events fire.
- **Frame buffer slice order** — `slice()` sorts output by `Frame.ts`.
- **Config reload non-fatal** — reload preserves the current runtime on any
  parse or validation error.
- **Camera failure loop** — open failure exits cleanly; read failures use
  consecutive-failure logging with rate limiting.
- **Config path hardcoded** — resolved: reads `RVO_CONFIG` env var at startup.

### MVP Features

- **Detectors receive frames** — `DetectorContext` carries
  `frame: Option<&Frame>`; the scheduler snapshots `newest_frame()` once per
  tick.
- **Detector dependency model** — `DetectorMeta` declares `dependencies` and
  `output_signals`; scheduler gates execution on fresh signal dependencies.
- **Multi-slot signal store** — `SignalType` has four variants (`Dummy`,
  `MotionLevel`, `FacePresent`, `PersonDetected`); each has its own slot.
- **Event engine beyond one hardcoded condition** — `EventDefinition` uses a
  `Condition` DSL (`All`/`Any` over `SignalPredicate`); multiple definitions
  update independently.
- **Detector health used** — `Failed` health disables a detector; aggregate
  execution nanoseconds are tracked.
- **Execution latency measured** — per-tick `detector_exec_ns_total` metric.
- **Real evidence output** — encoder writes JPEG frames and a `meta.json`
  sidecar into a per-clip directory under `clips_dir`.
- **Event publisher** — `EventPublisher` channel publishes to stdout logger
  and optionally to a JSON-lines file sink (`event_log` config field).
- **Load shedding** — cost-aware backoff: Medium detectors back off 100 ms,
  High detectors back off 500 ms, when last execution exceeded 2× FPS budget.
- **Post-roll clips** — `ClipManager` stores an `Arc<Mutex<FrameBuffer>>`;
  `on_event` spawns a thread that sleeps `after` duration before slicing, so
  post-roll frames are actually captured.
- **Drop metrics wired** — `rvo_frame_drops_total`, `rvo_clip_drops_total`,
  `rvo_event_drops_total` are incremented at every drop site.
- **RTSP/URI source** — `CameraConfig` accepts `source_uri: Option<String>` in
  addition to `device_index`; OpenCV `VideoCapture` opens either form.
- **`/health` endpoint** — metrics server returns `200 ok` at `/health`.
- **CI workflow** — `.github/workflows/ci.yml` runs check, test, clippy, fmt.
- **`rvo-core/frame.rs`** — exposed as a module with a doc comment explaining
  the planned Frame type unification.

## Open Items

### Evidence (P1)

**No video file output.** The encoder writes individual JPEG frames, not a
video stream. For a production clip, frames need to be muxed into an MP4/MKV
via GStreamer, ffmpeg-sys, or an external post-processing script.

**Post-roll accuracy is best-effort.** The delayed post-roll thread reads from
the circular buffer after sleeping `after` duration. If the buffer capacity is
exceeded during that window (at 300 frames / 30fps ≈ 10 s), old frames are
overwritten. For clips with `after` > 10 s, the buffer capacity should be
increased or post-roll should be stored differently.

### Event System (P1)

**Config-level condition DSL not yet parsed.** The `Condition` DSL types exist
in `rvo-events` and are usable programmatically. The config YAML does not yet
parse an `all`/`any` block directly; events configured in YAML still use the
`signal_type` + `signal_threshold` shorthand that maps to
`Condition::single_gte`. Full YAML DSL support is Phase 4 work.

**Only `DummyEvent` exists.** The `EventType` enum and the config validation
both only recognize `DummyEvent`. Adding a real event type requires: a new
`EventType` variant, a KNOWN_EVENT_TYPES entry, and condition definition.

### Scheduling (P2)

**No per-detector load shedding metrics.** The scheduler tracks aggregate
execution nanoseconds but not per-detector latency histograms. Identifying
which specific detector is causing overload requires a per-detector latency
metric.

**No graceful shutdown.** The main loop runs forever. Clean shutdown
(drain in-flight clips, flush event log, join threads) is not implemented.

### External Interfaces (P2)

**Event file sink only.** Downstream applications can read `events.jsonl`
but there is no push interface (IPC socket, webhook, gRPC stream). This is
acceptable for single-host deployments but limits integration options.

**No control API.** Enabling/disabling individual detectors, adjusting FPS
caps, or triggering a config reload from a non-Unix signal requires a control
endpoint.

### Platform (P3)

**Single camera, single process.** Multi-stream ingest, RTSP muxing, and
distributed detector workers are roadmap items.

**No GPU abstraction.** Detectors run on CPU. GPU model runtimes (CUDA,
CoreML, TensorRT) require a model execution backend abstraction layer.

**No structured logging.** The binary uses `println!` / `eprintln!`. A
proper logging framework (`tracing` or `log` with a JSON formatter) is needed
for production deployments.

**`load_config` exported but unused by the binary.** The `rvo-config` crate
exports both `try_load_config` (used) and `load_config` (panicking wrapper,
not used). Keep for embedding convenience or remove if it creates confusion.
