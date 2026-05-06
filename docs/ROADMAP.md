# Roadmap

This roadmap turns the current MVP into the broader RVO platform: a realtime
video AI orchestration runtime for low-latency applications.

For the concrete current-code triage list, see
[Issues And Leftovers](ISSUES_AND_LEFTOVERS.md).

## Phase 1: Correctness And Core Runtime

Priority fixes:

- Replace scheduler `now_ns` with a stable monotonic timestamp.
- Increment `rvo_events_emitted_total` when events are emitted.
- Add all path dependency crates to the workspace members list.
- Make event duration and cooldown tests match real scheduler time behavior.
- Decide whether signal TTL stores a duration or an absolute expiry timestamp.

Core hardening:

- Avoid panicking when clip manager sees an empty frame buffer.
- Add focused tests for scheduler event triggering.
- Add tests for frame buffer slicing and clip job windows.
- Add tests for config reload behavior where possible.

## Phase 2: Real Detector Runtime Contract

Extend `DetectorNode` from the current MVP interface into a production contract.

Add detector metadata:

- unique id
- max FPS
- required signal dependencies
- produced signal types
- optional frame requirement
- cost hint
- enabled flag

Extend `DetectorContext`:

- monotonic timestamp
- optional latest frame handle
- read-only signal view
- runtime config reference if needed

Extend detector results:

- produced signals
- health status
- execution diagnostics
- optional model metadata

Runtime policies:

- no concurrent execution for the same detector
- no catch-up executions
- skip stale dependencies
- disable or degrade failed detectors
- emit per-detector metrics

## Phase 3: Multi-Signal Store

Move from the current single-slot signal store to a fixed signal map.

Target behavior:

- one slot per signal type
- overwrite by signal type
- O(1) read and write
- no hot-path allocation
- TTL freshness checks
- read-only snapshots for detectors and event engine

Example signal types:

- `MOTION_LEVEL`
- `FACE_PRESENT`
- `PERSON_DETECTED`
- `POSE_KEYPOINTS`
- `OBJECT_DETECTED`
- `OCR_TEXT`
- `ANOMALY_SCORE`

## Phase 4: Event Definitions And DSL

Move from one hardcoded dummy condition to configurable event definitions.

Target event definition:

```yaml
events:
  - event_type: HEAD_AWAY_TOO_LONG
    condition:
      all:
        - signal: FACE_PRESENT
          op: eq
          value: true
        - signal: GAZE_STATE
          op: eq
          value: AWAY
    duration_ms: 3000
    cooldown_ms: 5000
```

Required capabilities:

- multiple event definitions
- independent event state per definition
- `all` / `any` conditions
- stale signal handling
- event metadata
- event ids
- event confidence model

## Phase 5: Real Evidence Pipeline

Replace the simulated encoder worker with actual artifact creation.

Capabilities:

- clip file writing
- metadata sidecar files
- deterministic naming
- pre-roll and post-roll support
- dropped frame accounting
- encoder latency metrics
- bounded queue depth metrics

Important design choice:

- For post-roll clips, the system must delay final extraction until the
  post-event window has actually passed, without blocking the scheduler.

## Phase 6: External Interfaces

Add interfaces for applications and distributed deployments.

Event output:

- local IPC publisher
- optional HTTP pull endpoint
- event schema with id, type, timestamp, confidence, metadata, and clip ref

Control:

- reload config
- enable or disable detectors
- adjust FPS caps
- graceful shutdown

Health:

- fast `/health` endpoint
- capture alive status
- scheduler alive status
- degraded status

Security defaults:

- bind local interfaces to localhost
- keep auth outside the realtime runtime unless required by embedding systems

## Phase 7: Load Shedding And Scheduling Policy

Add explicit overload behavior.

Policy inputs:

- CPU load
- detector execution latency
- queue pressure
- frame drop rate
- detector cost hints

Policy actions:

- skip high-cost detectors first
- reduce effective FPS for medium-cost detectors
- preserve low-cost detectors where possible
- emit degradation metrics

Invariant:

> RVO should degrade by doing less fresh work, not by accumulating stale work.

## Phase 8: Multi-Stream And Distributed Runtime

Move from one local camera to multiple streams and distributed components.

Capabilities:

- multiple camera or RTSP sources
- per-stream scheduler instances
- detector worker pools
- process isolation for heavy models
- remote detector workers where latency allows
- shared event schema across streams
- central metrics collection

The distributed architecture should preserve the core RVO contracts:

- bounded message paths
- freshness-aware scheduling
- typed signals
- temporal event confirmation
- best-effort evidence extraction
