# Architecture

This document describes the RVO system design: the invariant it enforces, the
data flow, the component contracts, and the design decisions that follow from
them. For implementation specifics (crate layout, type signatures, exact behavior),
see [CURRENT_IMPLEMENTATION.md](CURRENT_IMPLEMENTATION.md).

## System Invariant

> The live path must never wait for slow work.

The live path: camera ingestion → frame buffer → scheduler → detector execution
→ signal publication → event evaluation. Everything else (encoding, file I/O,
downstream delivery, network calls) is off the live path.

## End-to-End Data Flow

```
Camera / RTSP Stream
    |
    v  (bounded channel, cap 5 — drops on full)
Scheduler.tick()
    |
    +--→ FrameBuffer  (Arc<Mutex<>>, circular, cap 300)
    |
    +--→ DetectorNode(s)  ─────────────────────────────────────┐
    |        |                                                   │
    |        ├── In-process detector                            │
    |        │      execute() runs inline, returns signals       │
    |        │                                                   │
    |        └── Remote gRPC detector (rvo-remote)              │
    |               execute() is NON-BLOCKING:                  │
    |               • writes newest frame to single-slot mailbox│
    |               • returns cached signals (TTL-stamped)      │
    |               worker thread (own Tokio rt, persistent     │
    |               HTTP/2 channel) ←→ external model service   │
    |                                                           │
    |        v  (both paths write to SignalStore)               │
    |    SignalStore  (typed slots, TTL freshness)  ────────────┘
    |
    +--→ EventEngine  (Condition DSL: All/Any over SignalPredicates)
    |        |
    |        v
    |    Vec<Event>
    |
    +--→ EventPublisher  (try_send, cap 64 — drops on full)
    |        |
    |        +--→ stdout logger
    |        +--→ JSON-lines file sink  (optional)
    |
    +--→ ClipManager  (try_send to pending queue, cap 16 — drops on full)
              |
              v  (single worker thread: sleeps until post-roll window closes)
              |  (try_send to encoder, cap 8 — drops on full)
         Encoder Worker
              |
              v
         clips/{type}_{ts}/frame_NNNN.jpg + meta.json
```

## Bounded Everything

Every handoff in the system uses a bounded structure:

| Handoff | Bound | Overflow behavior |
|---|---|---|
| Camera → Scheduler | Channel cap 5 | Drop frame, count `rvo_frame_drops_total` |
| Remote detector mailbox | 1 slot (overwrite-newest) | Stale frame silently replaced |
| Scheduler → EventPublisher | Channel cap 64 | Drop event, count `rvo_event_drops_total` |
| Scheduler → ClipManager (pending queue) | Channel cap 16 | Drop clip job, count `rvo_clip_drops_total` |
| ClipManager worker → Encoder | Channel cap 8 | Drop clip job, count `rvo_clip_drops_total` |
| FrameBuffer | 300 frames (~10 s @ 30 fps) | Overwrite oldest |

No unbounded queue exists anywhere in the live path.

## Frame Buffer Sharing

The frame buffer is the only state shared between the live path and the evidence
pipeline. It uses `Arc<Mutex<FrameBuffer>>`:

- **Scheduler** locks briefly each tick to drain frames and snapshot the newest timestamp, then releases.
- **ClipManager** locks briefly on event to read `newest_instant()` (anchors the clip window), then releases. A single long-lived worker sleeps until the post-roll window closes, then locks briefly to slice frames. The lock is never held while sleeping.

The contention window is one `slice()` call, not the entire post-roll duration.

## Scheduler and DetectorNode Contract

The scheduler is a clock-driven arbiter. It decides when each detector may run.

**Scheduler responsibilities:** enforce FPS caps, check signal dependency freshness,
apply load-shedding backoff, measure execution latency, handle `Failed` health.

**DetectorNode responsibilities:** consume the provided context (timestamp, optional
frame, signal store), produce typed signals and a health status, own no scheduling,
I/O, or downstream delivery logic.

Neither touches the other's domain. This contract holds for both in-process and
remote detectors — `RemoteGrpcDetector` satisfies `DetectorNode` while keeping the
network call off the scheduler tick.

## Remote Detector Isolation

The key design challenge for remote detectors: a network round-trip plus inference
is the slowest thing in the system. A naive blocking `execute()` would either stall
the tick or trip its own backoff on every frame.

The solution decouples inference from the tick:

```
scheduler tick (hot)          worker thread (persistent HTTP/2 channel)
────────────────────          ──────────────────────────────────────────
execute(ctx):                 loop:
  write frame → mailbox  →     take newest frame
  read cached result     ←     JPEG-encode + Detect() over gRPC
  return signals (TTL)         store (value, produced_at)
```

`execute()` never waits on the network. Staleness is handled by the existing TTL
mechanism: if the worker falls behind, the cached result expires in `SignalStore`
exactly as an in-process signal would. Failure accumulates to a threshold, then
`DetectorHealth::Failed` — the scheduler disables the detector and the pipeline
continues.

## Signal Store

The signal store is a typed blackboard, not a queue. It answers:

> What is the latest valid value of signal X right now?

One slot per `SignalType`. Reads and writes are O(1) by type index. Freshness is
enforced at read time via TTL: `signal.ts_ns + signal.ttl_ns < now_ns` → absent.

A missing or stale signal is treated as absent — dependent detectors are skipped,
predicates referencing it evaluate false.

## Condition DSL

Event conditions are a tree of signal predicates:

```
Condition
  ├── All(predicates)    →  AND semantics
  └── Any(predicates)    →  OR semantics

SignalPredicate
  ├── signal_type: SignalType
  ├── op: CompareOp (Gte | Gt | Eq | Lt | Lte)
  └── value: u64
```

Each `EventMachine` evaluates its own `Condition` independently per tick.

## Event Engine

Temporal state machine per event definition:

```
Idle → (condition first true) → Potential { start_ns }
     → (condition still true, elapsed >= duration_ns) → emit Event
     → Cooldown { until_ns }
     → (now >= until_ns) → Idle

     (condition becomes false while Potential) → back to Idle
```

Event emission is separate from evidence capture. An event is meaningful even if
clip extraction fails or is dropped.

## Evidence Pipeline

The evidence pipeline is explicitly best-effort:

- Clip jobs use `try_send` — never blocks the live path.
- A single long-lived worker thread (not per-event threads) processes jobs sequentially, keeping thread count bounded regardless of event burst size.
- Failed or slow encoding does not affect the scheduler.

## Load Shedding

When a detector's execution time exceeds 2× its FPS budget, it enters backoff:

```
cost_hint → backoff duration
Low       → 0 ms   (always recovers immediately)
Medium    → 100 ms
High      → 500 ms
```

During backoff the detector is skipped entirely — no missed executions are queued.
RVO degrades by doing less work per unit time, not by accumulating stale work.

## Front-Ends

All three front-ends run the same runtime:

- **`rvo`** — headless CLI for servers and scripted use.
- **`rvo-tui`** — interactive terminal dashboard: signal blackboard, metrics, events.
- **`rvo-web`** — browser dashboard; supports live model node addition via `POST /api/nodes` without restart.

## Observability

Every significant state change produces a metric increment. Prometheus-style counters
at `/metrics`; process-alive check at `/health`. Key families: frame drops, scheduler
tick rate, detector exec/skip/latency, event counts, clip drops.

The load harness (`rvo-bench`) provides HDR histogram latency measurements (p50/p99/p99.9)
for the scheduler and detector paths across 13 benchmark scenarios. See
[BENCHMARK_PERFORMANCE.md](BENCHMARK_PERFORMANCE.md) for measured results.

## Design Boundaries

RVO is **not**:
- a computer vision model or training framework
- a video codec or muxer
- a general-purpose distributed compute engine
- a durable event log

RVO **is**:
- a realtime orchestration runtime for video AI models
- a bounded message path for frames and signals
- a time-aware scheduler for model execution
- a temporal event confirmation engine
- a best-effort evidence pipeline

## Scaling

**Today:** single process per camera stream. Remote detectors (`rvo-remote`) already
move the model execution out-of-process, behind a stable gRPC contract. Each remote
detector is a worker thread and a persistent channel — adding a model node is O(1)
on the hot path.

**Future:** the component boundaries (ingest, scheduler, detector pools, event
publisher, clip encoder, metrics) are chosen so each can move across a process or
host boundary. The core contracts — bounded queues, freshness-first scheduling,
typed signals, temporal event semantics, no slow work on the live path — should hold
in the distributed form as they do today.
