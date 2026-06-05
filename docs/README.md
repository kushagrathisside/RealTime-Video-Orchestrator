# RVO: Realtime Video Orchestration

RVO is low-latency scheduling infrastructure for realtime video AI. It ingests live streams, schedules models under latency budgets, routes results through bounded message paths, fuses outputs into temporal events, and extracts evidence clips — without the live path ever waiting for slow work.

## Why RVO Exists

Modern video AI systems are not limited only by model accuracy. In production, they are often limited by orchestration:

- Frame backlogs create stale inference.
- Slow models block fast models.
- Unbounded queues hide latency until the system falls behind.
- Ad hoc glue code makes detector dependencies hard to reason about.
- One-frame detections create noisy events.
- Clip extraction and storage compete with realtime inference.
- Production teams lack clear metrics for latency, drops, and overload.

RVO treats realtime video AI as a data systems problem. Instead of asking every application team to reinvent capture loops, model schedulers, signal freshness, event debouncing, and evidence capture, RVO provides a common runtime for those concerns.

## What It Does

```
Realtime Frame Bus         bounded camera ingestion, circular frame buffer
Model Orchestrator         FPS-gated, cost-aware, dependency-chained scheduler
Signal Store               typed, TTL-bounded blackboard between models
Temporal Event Engine      condition DSL, Idle→Potential→Confirmed state machine
Evidence Pipeline          post-roll clip capture, JPEG+metadata, best-effort
Remote Model Gateway       non-blocking gRPC fan-out to external model services
Observability Layer        Prometheus counters, /health, load harness with HDR histograms
```

## Core Principles

**Bounded Everything** — Frame buffers, channels, job queues, and signal state have fixed bounds. RVO does not hide overload behind unbounded memory growth.

**No Stale Work** — Old frames are less valuable than fresh frames. If the system falls behind, RVO drops stale work instead of processing it late.

**Time-Aware Scheduling** — Models run according to time budgets, FPS caps, dependency freshness, and load policy. The goal is not to run every model on every frame; it is to run the right model at the right time with bounded latency.

**Signal-Oriented Modularity** — Models emit typed signals that other modules consume if fresh. Models do not call each other directly.

**Temporal Correctness** — Events are confirmed over time, not triggered by every raw detection. A sustained condition is more useful than a noisy spike.

**Failure Isolation** — Slow encoding, disk writes, or downstream consumers never block capture or scheduling. Evidence is best-effort.

**Production Observability** — A realtime runtime proves its behavior with metrics: frame drops, model latency, scheduler ticks, event counts, clip pipeline health, and queue pressure.

## Measured Performance

5 runs × 30 s each on bare-metal Linux, performance governor. Median across runs.

| Scenario | tick_p50 | tick_p99 | frame_loss_rate |
|---|---|---|---|
| Baseline (no detectors) | 4.8 µs | 10.8 µs | 0 |
| HOL blocking — 10 ms detector, every tick | 10.16 ms | 10.34 ms | 0 |
| Load-shedding — 50 ms detector shed by backoff | 4.9 µs | 13 µs | 0 |
| Overload — 577 fps camera, 5 ms detector | 5.08 ms | 5.21 ms | 401/s |
| Throughput ceiling — 3841 fps actual (DummyDetector) | 5.8 µs | 13.7 µs | 2087/s |

HOL blocking appears directly in tick p50. Load-shedding keeps p50 at baseline despite a 50 ms detector. Under overload, p99 stays bounded while ring-buffer loss absorbs excess frames. See [BENCHMARK_PERFORMANCE.md](BENCHMARK_PERFORMANCE.md) for full results, methodology, and all architectural claims confirmed by measurement.

## Current State

Working Rust implementation, 15 crates, CI passing.

**Core runtime**
- OpenCV camera capture with RTSP/URI support
- Bounded frame channel (cap 5) + circular frame buffer (cap 300, ~10 s at 30 fps)
- Time-gated, cost-aware, dependency-chained scheduler with HOL backoff (Medium 100 ms / High 500 ms)
- Typed signal store (Dummy, MotionLevel, FacePresent, PersonDetected) with TTL freshness
- Composable condition DSL (`All`/`Any` over typed signal predicates)
- Temporal event engine (Idle → Potential → Confirmed → Cooldown)
- Post-roll clip capture — JPEG frames + JSON metadata sidecar, best-effort, bounded thread

**External model integration**
- `RemoteGrpcDetector` — non-blocking gRPC fan-out to any model service speaking the `rvo.detect.v1.Detector` proto
- Persistent HTTP/2 channels, lazy dial, TTL-bounded cached results
- Failure threshold → `DetectorHealth::Failed` → scheduler disables detector, pipeline continues

**Front-ends**
- `rvo` CLI — headless runtime; `--detector ENDPOINT=SIGNAL`, `--camera-device`, `--list-cameras`
- `rvo-tui` — interactive terminal dashboard: signals, metrics, events
- `rvo-web` — browser dashboard with live node-add API at `POST /api/nodes`

**Testing and benchmarking**
- `rvo-testkit` — synthetic cameras, scripted / latency / failing / chained detectors, full pipeline builder
- `rvo-scenarios` — 12 end-to-end integration scenarios
- `rvo-bench` — load harness: 13 scenarios, HDR latency histograms, multi-run CSV output
- CI: check, test, clippy, fmt across all 15 crates

See [CURRENT_IMPLEMENTATION.md](CURRENT_IMPLEMENTATION.md) for the precise behavior of the running code.

## Quick Start

```sh
# Requires: Rust stable, OpenCV system libraries
cargo build --release -p rvo-bin

# Discover cameras
./target/release/rvo --list-cameras

# Run headless
RVO_CONFIG=config/rvo.yaml ./target/release/rvo

# Interactive TUI
cargo run -p rvo-bin --bin rvo-tui

# Browser dashboard (open http://127.0.0.1:8080)
cargo run -p rvo-bin --bin rvo-web

# Point at an external model service
./target/release/rvo --detector http://localhost:50051=PersonDetected

# Run benchmarks (5 runs × 30 s each → 65-row CSV)
cargo build --release -p rvo-bench --bin load_harness
./target/release/load_harness --all --runs 5

# Observe
curl http://127.0.0.1:9090/metrics
curl http://127.0.0.1:9090/health
tail -f events.jsonl
ls clips/
```

## Documentation

| Document | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Data flow, component contracts, design boundaries |
| [DEVELOPER_GUIDE.md](DEVELOPER_GUIDE.md) | Language choice, crate layout, design decisions and tradeoffs |
| [CURRENT_IMPLEMENTATION.md](CURRENT_IMPLEMENTATION.md) | What the running code does today |
| [BENCHMARKING.md](BENCHMARKING.md) | How to run the benchmark suite, scenario definitions, CSV schema |
| [BENCHMARK_PERFORMANCE.md](BENCHMARK_PERFORMANCE.md) | Methodology rationale, measured results, architectural claims |
| [PLOT_GUIDE.md](PLOT_GUIDE.md) | Figure descriptions, axes, plotting recipes |
| [LOW_LATENCY_SYSTEMS.md](LOW_LATENCY_SYSTEMS.md) | Domain primer: latency, queuing, scheduling, observability |
