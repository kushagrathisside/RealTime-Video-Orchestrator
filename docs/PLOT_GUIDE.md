# Benchmark Plot Guide

This document describes the five benchmark figures that the RVO bench suite
produces, what each one proves, and how to recreate them from the CSV files that
`load_harness --all` generates.

`scripts/plot.py` is the local plotting script used for paper and report writing.
It is not tracked in the repository. You can recreate it in any tool you prefer
(Python/matplotlib, R, Excel, Observable) — this document gives you everything you
need: CSV schema, axes, and the claim each figure must support.

---

## Running the bench suite

```bash
# on bare-metal Linux with performance governor set
cargo build -p rvo-bench --bin load_harness --release

# Single run — 30s per scenario
./target/release/load_harness --all

# 5 runs for variance (recommended for reports)
./target/release/load_harness --all --runs 5

# 60s measurement windows for better p99.9 sample counts
./target/release/load_harness --all --runs 3 --duration-secs 60
```

All output lands in `target/bench_results/` (gitignored). Two file types:

- **`summary.csv`** — one row per scenario per run, end-of-run aggregates
- **`<scenario>_<duration>s_timeseries.csv`** — per-second samples during the run

When using `--runs N`, each run overwrites the timeseries file (only the last run's
timeseries is retained per scenario). The summary.csv accumulates all N rows.

---

## CSV schema

### `summary.csv`

| Column | Unit | Description |
|---|---|---|
| `run_id` | int | Run number (1..=N from `--runs N`; always 1 for single runs) |
| `scenario` | string | Scenario name |
| `detector_sleep_ms` | ms | Configured detector artificial latency (0 for non-blocking) |
| `input_fps` | fps | Configured synthetic camera rate |
| `actual_camera_fps` | fps | Measured camera send rate during the measurement window |
| `duration_secs` | s | Measurement window (after warm-up) |
| `tick_p50_ns` | ns | Median tick duration |
| `tick_p99_ns` | ns | 99th-percentile tick duration |
| `tick_p999_ns` | ns | 99.9th-percentile tick duration |
| `tick_count` | count | Total tick samples in histogram |
| `exec_p50_ns` | ns | Median detector execute() duration |
| `exec_p99_ns` | ns | p99 detector execute() duration |
| `exec_p999_ns` | ns | p99.9 detector execute() duration |
| `total_ticks` | count | Scheduler ticks during measurement |
| `total_execs` | count | Detector execute() calls |
| `total_skips` | count | Detector gate skips (FPS cap + backoff + disabled) |
| `total_events` | count | Events emitted by the event engine |
| `total_frame_drops` | count | Channel-level frame drops (bounded channel full). Stays 0 for overload/fps_* scenarios because the scheduler batch-drains the channel every tick. |
| `effective_fps` | fps | `total_ticks / duration_secs` — scheduler's actual processing rate |
| `frame_loss_rate` | fps | `max(0, actual_camera_fps - effective_fps)` — frames/s lost in ring buffer |

**Important:** for overload and fps_* scenarios, use `frame_loss_rate` (not
`total_frame_drops`) to assess frame loss. The bounded channel never saturates because
the scheduler drains it completely on every tick; ring-buffer overwrites are silent.

### `*_timeseries.csv`

| Column | Unit | Description |
|---|---|---|
| `elapsed_ms` | ms | Wall time since measurement start (0 = end of warm-up) |
| `ticks_delta` | count | Ticks in this sample interval |
| `execs_delta` | count | execute() calls in this interval |
| `skips_delta` | count | Gate skips in this interval |
| `events_delta` | count | Events emitted in this interval |
| `frame_drops_delta` | count | Channel-level frame drops in this interval |
| `tick_p50_ns` | ns | p50 cumulative tick latency (since warmup end) |
| `tick_p99_ns` | ns | p99 cumulative tick latency |
| `exec_p50_ns` | ns | p50 cumulative detector exec latency |
| `exec_p99_ns` | ns | p99 cumulative detector exec latency |
| `staleness_p50_ns` | ns | p50 frame staleness (camera→scheduler) |
| `staleness_p99_ns` | ns | p99 frame staleness |
| `frame_queue_depth` | count | Live frame channel depth at sample time |

---

## The five figures

### Figure 1 — HOL blocking: tick p50 vs detector sleep

**Source:** `summary.csv`, rows where `scenario` starts with `blocking_`

**Axes:**
- X: `detector_sleep_ms`
- Y (primary): `tick_p50_ns / 1e6` (ms)
- Y (secondary): `tick_p99_ns / 1e6` (ms)
- Reference line: `tick_p50_ns / 1e6` from the `baseline` row (~0.005ms)

**Claim:** With `max_fps=10000` (min_interval=0.1ms), the detector runs on every scheduler
tick. `tick_p50` therefore directly tracks detector latency: a 10ms detector produces a
~10ms tick p50. This is the correct metric for demonstrating HOL blocking — the median tick
is as slow as the slowest synchronous step.

**What good data looks like:**
- `tick_p50_ns / 1e6` closely tracks `detector_sleep_ms` (linear, near-unity slope).
- `tick_p999_ns` is slightly above `tick_p99_ns` (OS jitter in the tail, not systematic).
- The baseline reference line is at ~0.005ms — well below all blocking curves.
- `blocking_50ms` shows `effective_fps` ≈ 19.7/s and `frame_loss_rate` ≈ 10/s (tick rate
  fell below the 30fps camera rate, causing ring-buffer overwrites).

**Red flag:** `tick_p50_ns` ≈ baseline for all blocking scenarios → detector is not
running every tick. Check that `max_fps=10000` (not 30) in `detectors_for()`.

---

### Figure 2 — Load-shedding: time-series

**Source:** `load_shed_<duration>s_timeseries.csv`

**Axes (dual panel):**
- Top: X = `elapsed_ms / 1000` (s), Y = `tick_p99_ns / 1e6` (ms)
- Bottom: X = `elapsed_ms / 1000` (s), bars for `skips_delta` (orange) and
  `frame_drops_delta` (red)

**Claim:** The `High`-cost 50ms LatencyDetector exceeds its 33ms budget (1/60fps × 2) and
is backed off for 500ms windows. DummyDetector continues running freely. Tick p99 stays
near-baseline despite the 50ms detector existing in the pipeline.

**What good data looks like:**
- `tick_p99_ns / 1e6` stays in the µs range (not approaching 50ms).
- `skips_delta` is non-zero every interval (slow detector skipped during backoff windows).
- `frame_drops_delta` is 0 (fast path keeps up; load is shed, not queued).
- `total_ticks` in summary.csv >> 5000 for a 30s run (~47,000 typical).
- `total_execs` ≈ 900 (DummyDetector at its 30fps cadence × 30s).

**Red flag:** `tick_p99_ns ≈ 50ms` and `total_ticks ≈ 600` → shedding not working.
Check `cost_hint=High` and `max_fps=60.0` for the LatencyDetector in `load_shed`.

---

### Figure 3 — Graceful degradation: overload raises frame_loss_rate, tick p99 stays bounded

**Source:** `summary.csv`, rows where `scenario` starts with `overload_`

**Axes (dual Y):**
- X: `actual_camera_fps`
- Left Y: `frame_loss_rate` (fps, red) — NOT `total_frame_drops`
- Right Y: `tick_p99_ns / 1e6` (ms, blue)
- Rows: `overload_threshold`, `overload_moderate`, `overload_severe`

**Claim:** A 5ms detector (max_fps=1000) caps the scheduler at ~176/s. Cameras above this
rate produce frames faster than the scheduler can process them; excess frames are
overwritten in the ring buffer. `tick_p99` stays flat — drops absorb the overload.

**What good data looks like:**
- `overload_threshold` (~118fps actual < 176/s): `frame_loss_rate` = 0.
- `overload_moderate` (~294fps actual > 176/s): `frame_loss_rate` ≈ 118/s.
- `overload_severe` (~577fps actual > 176/s): `frame_loss_rate` ≈ 401/s.
- `tick_p99_ns / 1e6` ≈ 5ms for all three rows (the detector sleep).
- `total_frame_drops` = 0 for all three (channel never saturates).

**Plot note:** use `actual_camera_fps` on the X-axis, not `input_fps`. The camera thread
may not achieve the configured rate at high fps due to `thread::sleep` granularity.

**Red flag:** `frame_loss_rate` = 0 for `overload_moderate` → effective tick rate >= camera
fps. Check `max_fps=1000` for the LatencyDetector in overload scenarios. Harness
validation also exits 1 with a diagnostic in this case.

---

### Figure 4 — Tick CDF: tail latency distribution

**Source:** `summary.csv`, rows for `baseline`, `inproc_low`, `blocking_3ms`, `blocking_10ms`

**Axes:**
- X: interpolated tick latency (ms) using the three percentile points (p50/p99/p99.9)
- Y: percentile (50th–99.9th)

**Note:** this is an approximation from three percentile points, not a full empirical CDF.
Label it clearly in any paper.

**Claim:** The CDF curves separate cleanly by detector sleep. With `max_fps=10000`, the
curves are steep — the sleep is deterministic so ticks cluster tightly around p50.

**What good data looks like:**
- `baseline` and `inproc_low` curves nearly overlap (DummyDetector adds ~0µs overhead
  at baseline tick rates), shifted far left.
- `blocking_3ms` and `blocking_10ms` curves are shifted right by exactly their sleep time
  relative to baseline.
- Curves are steep (small spread between p50 and p99.9) — deterministic sleep has low jitter.

---

### Figure 5 — Throughput ceiling: fps_* scenarios

**Source:** `summary.csv`, rows where `scenario` starts with `fps_`

**Axes (dual Y):**
- X: `actual_camera_fps` (use this, not `input_fps`)
- Left Y: `frame_loss_rate` (fps, red)
- Right Y: `tick_p99_ns / 1e6` (ms, blue)
- Rows: `fps_1000`, `fps_2000`, `fps_5000`

**Claim:** With DummyDetector, the scheduler ticks at ~1756/s. Below this ceiling
(`fps_1000`, actual ≈ 938fps) there is no frame loss. At and above the ceiling
(`fps_2000` actual ≈ 1772fps, `fps_5000` actual ≈ 3841fps), loss scales with the excess.
This is the fast-path control experiment for Figure 3: it shows that the scheduler's tick
rate (not the fps itself) is the limiting factor, and that a fast detector eliminates the
early overload seen in Figure 3.

**What good data looks like:**
- `fps_1000`: `frame_loss_rate` = 0 (scheduler is faster than camera).
- `fps_2000`: small positive `frame_loss_rate` (actual camera ≈ 1772fps is just above the
  ~1756/s scheduler ceiling).
- `fps_5000`: `frame_loss_rate` ≈ 2086/s (actual camera ≈ 3841fps, scheduler at 1755/s).
- `tick_p99_ns` stays flat across all three — the fast detector is not the bottleneck.

**Contrast with Figure 3:** in Figure 3, frame loss begins at ~176fps effective rate
(because the 5ms detector slows every tick). In Figure 5, frame loss begins only at ~1756fps
(because DummyDetector imposes no tick overhead). This paired comparison isolates the slow
detector as the cause of the early overload in Figure 3.

---

## Plotting recipe (any tool)

All figures follow the same data flow:

1. Load `target/bench_results/summary.csv`.
2. If using `--runs N`, aggregate across `run_id`: compute median (or mean) per `scenario`.
3. Filter rows by `scenario` prefix.
4. Convert `*_ns` columns to ms: divide by `1e6`.
5. Plot the columns listed in each figure's schema above.

For Figure 2 (time-series), load `load_shed_<duration>s_timeseries.csv` and plot
`elapsed_ms / 1000` on the X-axis.

```python
import pandas as pd

df = pd.read_csv("target/bench_results/summary.csv")
df["tick_p50_ms"]  = df["tick_p50_ns"]  / 1e6
df["tick_p99_ms"]  = df["tick_p99_ns"]  / 1e6
df["tick_p999_ms"] = df["tick_p999_ns"] / 1e6

# Aggregate across runs (if --runs N was used)
agg = df.groupby("scenario").agg(
    tick_p50_ms_median=("tick_p50_ms", "median"),
    tick_p99_ms_median=("tick_p99_ms", "median"),
    frame_loss_rate_median=("frame_loss_rate", "median"),
    actual_camera_fps_median=("actual_camera_fps", "median"),
).reset_index()
```

In R:
```r
df <- read.csv("target/bench_results/summary.csv")
df$tick_p50_ms <- df$tick_p50_ns / 1e6
df$tick_p99_ms <- df$tick_p99_ns / 1e6
agg <- aggregate(cbind(tick_p50_ms, tick_p99_ms, frame_loss_rate, actual_camera_fps)
                 ~ scenario, data=df, FUN=median)
```

In Excel: open the CSV, add computed columns `=tick_p50_ns/1000000`, etc.

---

## Interpreting validation output

The harness prints a `[BENCH VALIDATION OK/WARN/NOTE/FAIL]` line after each scenario.
`FAIL` exits 1 and stops the run. Examples:

```
[BENCH VALIDATION OK] blocking_10ms: tick_p50=10.10ms ≈ 10ms injected sleep, 0 frame drops
[BENCH VALIDATION NOTE] blocking_50ms: effective 19.7/s < camera 30fps
    (~10 frames/s lost in ring buffer — expected for 50ms HOL scenario). tick_p99=50.36ms
[BENCH VALIDATION OK] load_shed: 47343 ticks, tick_p99=0.01ms (backoff active, fast detector running freely)
[BENCH VALIDATION OK] overload_moderate: effective 176.5/s < camera 294.0fps
    (~118 frames/s lost in ring buffer, as expected)

[BENCH VALIDATION FAIL] load_shed: 612 ticks recorded (expected >> 5000).
    tick_p99=49.87ms. Load-shedding did not activate — check cost_hint and overrun budget.
[BENCH VALIDATION FAIL] overload_moderate: effective tick rate 1756.0/s >= camera 294.0fps
    — scheduler is keeping up, no frame loss. Check that the slow detector runs on every tick.
```

A `FAIL` means the numbers exist in the CSV but do not prove the intended claim.
Do not quote those numbers in a report without investigating the root cause.

A `WARN` means a reference scenario produced unexpected data (e.g. frame drops on a
fast-path scenario). Investigate but the run continues.

A `NOTE` is informational — the scenario behaved as expected but the outcome involves
trade-offs worth noting (e.g. `blocking_50ms` causing ring-buffer loss).
