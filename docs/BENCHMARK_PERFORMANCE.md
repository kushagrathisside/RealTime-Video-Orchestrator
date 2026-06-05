# RVO Benchmark Performance

This document records the methodology behind the benchmark design and the experimental
results that support RVO's architectural claims. It is the companion to
[BENCHMARKING.md](BENCHMARKING.md), which is the operational reference for running the
suite. Read that first.

---

## 1. Methodology

### 1.1 Why tick_p50 is the HOL blocking metric

With `max_fps=10000` (min_interval=0.1ms), the `blocking_*` detectors run on every
scheduler tick. `tick_p50` therefore directly measures detector latency: 10ms detector →
~10ms tick p50. This is the architecturally correct test for HOL blocking — the median
tick is as slow as the synchronous bottleneck.

An earlier design used `max_fps=30`, which caused the detector to fire on only ~1.74% of
ticks. `tick_p50` always reported scheduler overhead (~5µs), hiding the HOL effect
entirely. The change to `max_fps=10000` makes the claim visible in the headline number.

### 1.2 Why frame_loss_rate, not total_frame_drops

The scheduler batch-drains the bounded frame channel on every tick:

```rust
while let Ok(frame) = self.frame_rx.try_recv() {
    buf.push(frame);
}
```

This means the channel never fills — `try_send` always succeeds — so `total_frame_drops`
stays 0 even under heavy overload. Frame loss instead occurs silently in the FrameBuffer
ring buffer: frames that arrive between ticks are overwritten by newer ones before the
detector reads them.

`frame_loss_rate = max(0, actual_camera_fps - effective_fps)` captures this correctly:
if the camera sends frames faster than the scheduler ticks, the difference is frames lost
per second in the ring buffer.

### 1.3 Why actual_camera_fps is measured, not assumed

At high configured fps (2000, 5000), `thread::sleep` granularity caps the camera thread
below its target. A configured 5000fps camera thread achieves ~3841fps in practice due to
OS timer resolution (~200µs sleep limit). If `frame_loss_rate` used the configured fps, it
would report phantom losses for scenarios where the camera never reached the configured
rate. Counting actual frames sent during the measurement window and dividing by duration
gives an accurate rate that correctly computes `frame_loss_rate = 0` when the scheduler
is actually faster than the camera.

### 1.4 Why the throughput ceiling group uses fps_1000/2000/5000

The DummyDetector scheduler ticks at ~1756/s. The fps_* group probes around that ceiling:
one scenario below it (fps_1000, actual ~938fps), one near it (fps_2000, actual ~1772fps),
one well above it (fps_5000, actual ~3841fps). This shows where frame loss begins and how
it scales. The group is the paired control for the overload group: at fps_5000 the
fast-detector scheduler still manages its ceiling (1756/s), while the slow-detector
overload scenarios cap at only ~176/s — isolating the slow detector as the cause of
the earlier overload onset.

### 1.5 Why overload uses Low cost, not High

`LatencyDetector(5ms, Low, 1000fps)` in the overload group is `Low` cost deliberately.
`Low` cost is never backed off by the scheduler. If it were `High`, the backoff mechanism
would shed it — which is exactly what `load_shed` demonstrates. The overload group wants a
slow tick on every call to cap the scheduler at ~176/s so the camera can outpace it. The
`load_shed` group wants the same slow detector to be shed so the fast path stays free.
Different cost hints, different outcomes, different claims.

### 1.6 Multi-run design

Each run appends a row with a distinct `run_id` to `summary.csv`. Grouping by `scenario`
and computing median across run IDs gives variance-aware results without manual file
management. `--runs 5` is the recommended setting for report-quality data.

---

## 2. Measured Results

Results below are from a single representative 30-second run per scenario on this machine.
For variance data, run `./target/release/load_harness --all --runs 5` and compute per the
snippet in [BENCHMARKING.md §9](BENCHMARKING.md#9-statistical-rigour-checklist).

### 2.1 Scheduler overhead (baseline)

| metric | value |
|---|---|
| tick_p50 | 4.8µs |
| tick_p99 | 11.3µs |
| tick_p999 | 26.0µs |
| tick rate | 1756/s |
| frame_drops | 0 |

The ~5µs p50 is the floor: lock acquisition on the frame buffer, ring-buffer push,
HDR histogram record, and loop overhead. Everything above this in other scenarios is
attributable to detector cost.

### 2.2 HOL blocking

| scenario | tick_p50 | tick_p99 | effective_fps | frame_loss_rate |
|---|---|---|---|---|
| inproc_low (DummyDetector) | 4.8µs | 11.4µs | 1759/s | 0 |
| blocking_1ms | 1.07ms | 1.15ms | 610/s | 0 |
| blocking_3ms | 3.07ms | 3.16ms | 275/s | 0 |
| blocking_10ms | 10.10ms | 10.34ms | 93/s | 0 |
| blocking_50ms | 50.20ms | 50.36ms | 19.7/s | 10.2/s |

`tick_p50` tracks the injected sleep ms-for-ms with low jitter (p50≈p99 because the sleep
is deterministic). `blocking_50ms` is the only one where frame loss occurs — the 50ms
tick interval puts the drain rate (19.7/s) below the 30fps camera rate.

### 2.3 Load-shedding

| metric | value |
|---|---|
| tick_p50 | 4.9µs (near-baseline) |
| tick_p99 | 13.8µs |
| tick_p999 | 50.1ms (rare: first execution before backoff fires) |
| total_ticks (30s) | 47,343 |
| total_execs | 900 (DummyDetector at 30fps cadence) |
| total_skips | 93,786 (slow detector shed during 500ms backoff windows) |
| frame_loss_rate | 0 |

The slow detector fires once, exceeds its 33ms budget (50ms > 33ms), and is parked for
500ms. DummyDetector continues at ~30fps. The tick_p50 stays within 2.5% of baseline.
`total_skips` (93,786) >> `total_execs` (900) confirms the slow detector is shed, not
running. The occasional 50ms in tick_p999 is the first execution of the slow detector each
time its backoff expires — expected behaviour.

### 2.4 Overload (graceful degradation)

| scenario | actual_camera_fps | effective_fps | frame_loss_rate | tick_p99 |
|---|---|---|---|---|
| overload_threshold (ref) | 118fps | 176/s | 0 | 5.30ms |
| overload_moderate | 294fps | 176/s | 117.5/s | 5.21ms |
| overload_severe | 577fps | 176/s | 401.1/s | 5.22ms |

Tick p99 is stable across all three (5.2–5.3ms, equal to the 5ms sleep + overhead).
Frame loss scales with the excess camera rate above the 176/s scheduler ceiling.
`total_frame_drops` = 0 for all three — the channel never saturates.

### 2.5 Throughput ceiling

| scenario | actual_camera_fps | effective_fps | frame_loss_rate | tick_p99 |
|---|---|---|---|---|
| fps_1000 | 938fps | 1757/s | 0 | 12.1µs |
| fps_2000 | 1772fps | 1758/s | 14.7/s | 13.0µs |
| fps_5000 | 3841fps | 1755/s | 2086/s | 13.7µs |

The DummyDetector scheduler ceiling is ~1756/s. At fps_1000 (actual 938fps) the scheduler
is faster than the camera — no loss. At fps_2000 (actual 1772fps) the camera marginally
exceeds the ceiling — minimal loss. At fps_5000 (actual 3841fps) the ceiling is
significantly exceeded — heavy loss. Tick p99 stays flat (~13µs) because the DummyDetector
imposes no latency.

Contrast with the overload group: the same 3841fps would produce ~3665 frames/s loss
against the overload ceiling of ~176/s. The slow detector is the difference — it lowers
the ceiling by 10×, not the fps itself.

---

## 3. Architectural Claims Confirmed

| Claim | Evidence |
|---|---|
| HOL blocking: synchronous detector latency appears directly in tick_p50 | blocking_* scenarios: tick_p50 = detector_sleep_ms ± <5% |
| Load-shedding: High-cost detector shed, fast path unaffected | load_shed: tick_p50 near-baseline, 93,786 skips vs 900 execs |
| Graceful degradation: overload raises frame_loss_rate, not tick latency | overload_moderate/severe: frame_loss_rate rises, tick_p99 flat at 5.2ms |
| Throughput ceiling: frame loss begins at actual_camera_fps > scheduler tick rate | fps_1000: 0 loss; fps_2000: 14.7/s loss; fps_5000: 2086/s loss |
| Panic isolation: panicking detector disabled, pipeline survives | unit test: `panicking_detector_does_not_kill_scheduler` |
| Bounded clip threads: event burst does not spawn unbounded threads | unit test: `event_burst_is_bounded` |
| Non-blocking gRPC path: scheduler tick does not block on network | integration test: `grpc_pipeline` |
| Signal TTL: stale signals do not trigger events | unit test: `expired_signal_is_absent` |

---

## 4. Limitations

### Coordinated omission in tick latency

Tick latency is measured from the moment `scheduler.tick()` is called, not from when the
frame was due. Under overload, frames accumulate in the ring buffer between ticks; the
queuing delay is not captured in `tick_p99`. The headline latency figure is the
control-loop processing time, not the end-to-end camera-to-signal latency. Frame staleness
(`staleness_p50_ns` in the timeseries CSV) measures the latter separately.

### thread::sleep is not a real model

`LatencyDetector` injects deterministic latency via `thread::sleep`. Real inference has
non-uniform latency (GPU sync, CUDA malloc, cold-start effects, memory pressure). The
blocking and load-shedding experiments are controlled proxies that isolate the scheduling
mechanism, not production model measurements.

### Harness-imposed tick ceiling

A `thread::sleep(500µs)` between ticks caps the scheduler at ~1756/s. A production
deployment using a monotonic timer without artificial sleep would achieve a higher
throughput ceiling. The ~1756/s figure characterises the harness, not the hardware limit.

### actual_camera_fps at high configured rates

`thread::sleep` granularity limits the camera thread. Configured 5000fps produces only
~3841fps actual. The `frame_loss_rate` column uses `actual_camera_fps` to remain accurate,
but the configured rate cannot be independently verified without a spin-loop camera (which
would consume a full CPU core and distort the benchmark).

### Single-machine, single-process

All results are from one machine. Distributed scheduling, NUMA effects, multiple concurrent
camera streams, and gRPC network jitter are not covered by the load harness. The gRPC path
is exercised separately by `grpc_pipeline` integration tests.
