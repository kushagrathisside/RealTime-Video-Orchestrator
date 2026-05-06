# Architecture

This document describes the intended RVO architecture. Some parts already exist
in the current Rust MVP, while others are platform goals.

## System View

RVO is built around a simple realtime invariant:

> The live path must never wait for slow work.

The live path includes camera ingestion, frame freshness, detector scheduling,
signal publication, and event evaluation. Slow work such as encoding, disk IO,
uploads, and external consumers belongs off the live path.

## End-to-End Flow

```text
Camera / Stream
    |
    v
Bounded Frame Channel
    |
    v
Rolling Frame Buffer
    |
    v
Scheduler
    |
    +--> DetectorNode(s)
    |        |
    |        v
    |   SignalStore
    |
    v
EventEngine
    |
    v
ClipManager / Event Publisher
    |
    v
Async Workers / Storage / Downstream Apps
```

## Runtime Components

### Frame Bus

The frame bus accepts live frames and keeps the latest data moving. It should be
bounded, lossy under pressure, and optimized for freshness.

Core rule:

> A fresh frame is usually more valuable than an old frame processed late.

### Scheduler

The scheduler is a clock-driven arbiter. It decides which detector nodes are
allowed to run now.

It should consider:

- detector FPS caps
- dependency freshness
- frame availability
- model cost hints
- current load
- detector health

It should not create a backlog of missed detector executions.

### DetectorNode

A detector node is a time-governed signal producer.

It should not own scheduling, disk IO, or downstream delivery. Its job is to
consume the context made available by the runtime and produce bounded outputs.

Conceptual interface:

```text
DetectorNode
  id
  metadata
  init(config)
  execute(context) -> result
  shutdown()
```

Conceptual metadata:

```text
DetectorMeta {
  id
  max_fps
  dependencies
  output_signals
  cost_hint
  enabled
}
```

Conceptual execution context:

```text
DetectorContext {
  now_ts
  frame
  signals
}
```

### Signal Store

The signal store is a realtime blackboard.

It should answer:

> What is the latest valid signal of type X right now?

It is not a queue, event log, or pub-sub system. It should store the latest
signal per type and use TTLs to prevent stale dependency usage.

Conceptual signal:

```text
Signal {
  type
  value
  ts
  ttl
}
```

Freshness rule:

```text
signal.ts + signal.ttl >= now
```

If a signal is missing or stale, it should be treated as absent.

### Event Engine

The event engine converts signal conditions into temporal events.

It should not read frames, run models, write files, or call external systems.
It should evaluate conditions over fresh signals and update small state
machines.

Conceptual event states:

```text
Idle -> Potential -> Confirmed -> Cooldown -> Idle
```

The current implementation uses `Idle`, `Potential`, and `Cooldown`, with event
emission happening at the transition into cooldown.

### Evidence Pipeline

The evidence pipeline turns events into optional artifacts such as clips,
thumbnails, metadata files, or downstream messages.

This pipeline is best-effort:

- bounded queues
- async workers
- drop-on-overload
- no backpressure into the live path

Events should remain meaningful even if evidence extraction fails.

### Observability

RVO should expose enough metrics to prove that realtime behavior is preserved.

Important metric families:

- capture FPS and frame drops
- scheduler tick rate
- detector executions, skips, and latency
- signal freshness and missing dependencies
- event counts and event latency
- clip jobs accepted, dropped, and encoded
- queue depths and overload state

## Design Boundaries

RVO is not:

- a computer vision model
- a model training framework
- a general-purpose distributed compute engine
- a video codec
- a durable event log

RVO is:

- a realtime orchestration layer
- a bounded message path for frames and signals
- a scheduler for video AI models
- a temporal event engine
- a best-effort evidence pipeline

## Distributed System Direction

The current code runs as one process. The architecture should allow future
modules to move across process or machine boundaries.

Natural boundaries:

- stream ingest
- scheduler runtime
- detector workers
- event publisher
- clip encoder
- artifact storage
- metrics collection

The key is to preserve the same contracts:

- bounded queues
- freshness over completeness
- typed signals
- temporal event semantics
- no slow work on the live path

