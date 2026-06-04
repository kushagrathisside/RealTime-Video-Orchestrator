# Architecture

This document describes the RVO architecture. The core design is implemented
in the current codebase. Platform-level extensions are noted where applicable.

## System Invariant

> The live path must never wait for slow work.

The live path: camera ingestion → frame buffer → scheduler → detector execution
→ signal publication → event evaluation. Everything else (encoding, file I/O,
downstream delivery) is off the live path.

## End-to-End Data Flow

```
Camera / RTSP Stream
    |
    v  (bounded channel, capacity 5 — drops on full)
Scheduler.tick()
    |
    +--→ FrameBuffer  (Arc<Mutex<>>, circular, capacity 300)
    |
    +--→ DetectorNode(s)
    |        |
    |        v
    |    SignalStore  (typed slots, TTL freshness)
    |
    +--→ EventEngine
    |        |  (Condition DSL: All/Any over SignalPredicates)
    |        v
    |    Vec<Event>
    |
    +--→ EventPublisher  (try_send — drops on full)
    |        |
    |        +--→ stdout logger
    |        +--→ JSON-lines file sink  (optional)
    |
    +--→ ClipManager  (try_send to pending queue, cap 16 — non-blocking, drops on full)
              |
              v  (single worker thread: sleeps until post-roll window closes, slices buffer)
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
| Scheduler → ClipManager (pending queue) | Channel cap 16 | Drop clip job, count `rvo_clip_drops_total` |
| ClipManager worker → Encoder | Channel cap 8 | Drop clip job, count `rvo_clip_drops_total` |
| Scheduler → EventPublisher | Channel cap 64 | Drop event, count `rvo_event_drops_total` |
| FrameBuffer | 300 frames (~10 s @ 30 fps) | Overwrite oldest |

No unbounded queue exists anywhere in the live path.

## Frame Buffer Sharing

The frame buffer is the only state shared between the live path and the
evidence pipeline. It uses `Arc<Mutex<FrameBuffer>>`:

- **Scheduler** holds a clone of the Arc. During each tick, it locks briefly
  to drain frames and snapshot the newest frame, then releases.
- **ClipManager** holds a clone of the Arc. When an event fires, `on_event`
  locks briefly to read `newest_instant()` (anchoring the clip window), then
  releases. A single long-lived worker thread processes all pending jobs
  sequentially: it sleeps until each job's post-roll window closes, then locks
  briefly to slice frames, then releases. The lock is never held while sleeping.

The lock is never held across a blocking operation, so the scheduler tick and
post-roll threads contend only on brief read/write windows.

## Scheduler And DetectorNode Contract

The scheduler is a clock-driven arbiter. It decides when each detector may run.

Scheduler responsibilities:
- enforce FPS caps
- check signal dependency freshness
- apply load-shedding backoff
- measure execution latency
- handle Failed health

DetectorNode responsibilities:
- consume the provided context (timestamp, optional frame, signal store)
- produce typed signals and a health status
- not own scheduling, I/O, or downstream delivery

Neither touches the other's domain.

## Signal Store

The signal store is a typed blackboard, not a queue. It answers:

> What is the latest valid value of signal X right now?

One slot per `SignalType`. Reads and writes are O(1). A seqlock-style version
counter guards the read side. Freshness is enforced via TTL at read time.

If a signal is missing or stale, it is treated as absent — the scheduler will
skip dependent detectors, and predicates that reference it will evaluate false.

## Condition DSL

Event conditions are defined as a tree of signal predicates:

```
Condition
  ├── All(predicates)    →  AND semantics
  └── Any(predicates)    →  OR semantics

SignalPredicate
  ├── signal_type: SignalType
  ├── op: CompareOp (Gte | Gt | Eq | Lt | Lte)
  └── value: u64
```

This replaces the original single `signal >= threshold` condition. Each
`EventMachine` evaluates its own `Condition` independently per tick.

## Event Engine

Temporal state machine per event definition:

```
Idle → (condition first true) → Potential { start_ns }
     → (condition still true, elapsed >= duration_ns) → emit Event
     → Cooldown { until_ns }
     → (now >= until_ns) → Idle
     
     (condition becomes false while Potential) → back to Idle
```

Event emission is separate from evidence capture. An event is meaningful even
if clip extraction fails or is dropped.

## Evidence Pipeline

The evidence pipeline is explicitly best-effort:

- Clip jobs use `try_send` — never blocks the live path.
- The encoder thread works at its own pace.
- Failed or slow encoding does not affect the scheduler.
- Post-roll frames are captured by a single long-lived worker thread, not per-event threads. The worker sleeps until each job's post-roll window closes, keeping thread count bounded regardless of event burst size.

Evidence artifacts (JPEGs + meta.json) are written to `clips_dir`. Future work:
video muxing into MP4/MKV, thumbnail extraction, metadata store.

## Load Shedding

When a detector's execution time exceeds 2× its FPS budget, it enters backoff:

```
cost_hint → backoff duration
Low       → 0 ms   (always recovers)
Medium    → 100 ms
High      → 500 ms
```

The invariant: RVO degrades by doing less work per unit time, not by
accumulating a backlog of stale work.

## Observability

Every significant state change produces a metric increment. Prometheus-style
text at `/metrics`; process-alive check at `/health`.

Key metric families: frame drops, scheduler tick rate, detector executions and
skips and latency, event counts, clip drops.

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

## Distributed Future

The current code runs as a single process. The component boundaries are chosen
so that each can later move across a process or host boundary:

- stream ingest
- scheduler runtime
- detector worker pools
- event publisher
- clip encoder
- artifact storage
- metrics collection

The core contracts (bounded queues, freshness-first, typed signals, temporal
event semantics, no slow work on the live path) should hold in the distributed
form exactly as they do today.
