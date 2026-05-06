# RVO: Realtime Video Orchestration

RVO is low-latency data infrastructure for realtime video AI systems.

It is designed for applications where live video streams need to be processed by
multiple AI or deep-learning models without allowing slow inference, storage,
encoding, or downstream consumers to stall the realtime path.

In short:

> RVO is a realtime orchestration runtime for video streams. It ingests live
> streams, schedules models under latency budgets, moves results through bounded
> message paths, fuses model outputs into temporal events, and emits clips or
> metadata without blocking the live pipeline.

## Why RVO Exists

Modern video AI systems are not limited only by model accuracy. In production,
they are often limited by orchestration:

- Frame backlogs create stale inference.
- Slow models block fast models.
- Unbounded queues hide latency until the system falls behind.
- Ad hoc glue code makes detector dependencies hard to reason about.
- One-frame detections create noisy events.
- Clip extraction and storage compete with realtime inference.
- Production teams lack clear metrics for latency, drops, and overload.

RVO treats realtime video AI as a data systems problem.

Instead of asking every application team to reinvent capture loops, model
schedulers, signal freshness, event debouncing, and evidence capture, RVO
provides a common runtime for those concerns.

## Product Framing

RVO can be thought of as:

```text
Realtime Frame Bus
+ Model Orchestrator
+ Signal Store
+ Temporal Event Engine
+ Evidence Pipeline
+ Observability Layer
```

Or, more simply:

> Message Queue + Stream Processor + Model Scheduler + Temporal Event Engine
> for realtime video AI.

RVO does not aim to replace Kafka, Ray, GStreamer, or model-serving systems.
Instead, it sits in the missing middle layer for live video inference:

- Cameras and streams produce fast, lossy, time-sensitive data.
- AI models consume selected frames or signals at controlled rates.
- Applications need reliable events, metadata, and clips.
- The system must stay realtime even when parts of the pipeline slow down.

## Core Principles

### Bounded Everything

Frame buffers, channels, job queues, and signal state should have fixed bounds.
RVO should not hide overload behind unbounded memory growth.

### No Stale Work

Old frames are less valuable than fresh frames in realtime systems. If the
system falls behind, RVO should drop stale work instead of processing it late.

### Time-Aware Scheduling

Models should run according to time budgets, FPS caps, dependencies, and load
policy. The goal is not to run every model on every frame. The goal is to run
the right model at the right time with bounded latency.

### Signal-Oriented Modularity

Models should not call each other directly. They should emit typed signals that
other modules can consume if the signals are still fresh.

### Temporal Correctness

Events should be confirmed over time, not triggered by every noisy frame-level
output. A sustained condition is more useful than a raw detection spike.

### Failure Isolation

Slow encoding, disk writes, storage uploads, or downstream consumers must not
block capture or scheduling. Evidence is valuable, but realtime behavior is the
primary invariant.

### Production Observability

A realtime runtime should prove its behavior with metrics: frame drops, model
latency, scheduler ticks, detector skips, event counts, queue pressure, and clip
pipeline health.

## Target Users

RVO is intended for teams building low-latency video systems where data flow,
distributed modules, and predictable degradation matter.

Example domains:

- Retail analytics
- Smart cameras
- Industrial safety
- Warehouse and logistics monitoring
- Traffic analytics
- Sports highlight generation
- Robotics perception
- Healthcare safety monitoring
- Proctoring and interview monitoring
- Edge AI and multimodal sensor systems

The common pattern is the same:

> Multiple models need to run over live video, their outputs need to be fused
> over time, and the application cannot afford unbounded latency.

## The Runtime Contract

The long-term RVO contract is:

1. Ingest live frames without blocking capture.
2. Keep a bounded rolling memory of recent frames.
3. Schedule model nodes using time-aware policies.
4. Publish model outputs as fresh typed signals.
5. Convert signal conditions into temporal events.
6. Extract evidence asynchronously when possible.
7. Expose metrics that make realtime behavior measurable.

This contract lets application developers focus on domain-specific models and
events instead of rebuilding realtime orchestration for every product.

## Current Repository Status

This repository currently contains a Rust MVP of the RVO runtime. It already
has the basic shape of the system:

- OpenCV camera capture
- Bounded frame channel
- Rolling frame buffer
- Time-gated detector execution
- Synthetic detector nodes
- Signal store
- Temporal event engine
- Background clip job worker
- Prometheus-style metrics endpoint
- YAML configuration
- SIGHUP config reload

Some platform-level ideas are still aspirational and are tracked separately in
[Roadmap](ROADMAP.md).

For the current code-level behavior, see [Current Implementation](CURRENT_IMPLEMENTATION.md).

For the concrete implementation issues and leftovers needed to make the MVP
work, see [Issues And Leftovers](ISSUES_AND_LEFTOVERS.md).
