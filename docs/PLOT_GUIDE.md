# Benchmark Plot Guide

This document describes the five benchmark figures that the RVO bench suite
produces, what each one proves, and how to recreate them from the CSV files that
`bench.sh` generates.

`scripts/plot.py` is the local plotting script used for paper and report writing.
It is not tracked in the repository. You can recreate it in any tool you prefer
(Python/matplotlib, R, Excel, Observable) — this document gives you everything you
need: CSV schema, axes, and the claim each figure must support.

---

## Running the bench suite

```bash
# on bare-metal Linux with performance governor set
bash scripts/bench.sh            # 30s per scenario, output → target/bench_results/
bash scripts/bench.sh --duration 60  # 60s for more p99.9 samples
```

All output lands in `target/bench_results/` (gitignored). Two file types:

- **`summary.csv`** — one row per scenario, end-of-run aggregates
- **`<scenario>_<duration>s_timeseries.csv`** — per-second samples during the run

---

## CSV schema

### `summary.csv`

| Column | Unit | Description |
|---|---|---|
| `scenario` | string | Scenario name |
| `detector_sleep_ms` | ms | Configured detector artificial latency |
| `input_fps` | fps | Synthetic camera rate |
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
| `total_frame_drops` | count | Frames dropped by the bounded camera channel |

### `*_timeseries.csv`

| Column | Unit | Description |
|---|---|---|
| `elapsed_ms` | ms | Wall time since harness start |
| `ticks_delta` | count | Ticks in this sample interval |
| `execs_delta` | count | execute() calls in this interval |
| `skips_delta` | count | Gate skips in this interval |
| `events_delta` | count | Events emitted in this interval |
| `frame_drops_delta` | count | Frames dropped in this interval |
| `tick_p50_ns` | ns | p50 cumulative tick latency |
| `tick_p99_ns` | ns | p99 cumulative tick latency |
| `exec_p50_ns` | ns | p50 cumulative detector exec latency |
| `exec_p99_ns` | ns | p99 cumulative detector exec latency |
| `staleness_p50_ns` | ns | p50 frame staleness (camera→scheduler) |
| `staleness_p99_ns` | ns | p99 frame staleness |
| `frame_queue_depth` | count | Live frame channel depth at sample time |

---

## The five figures

### Figure 1 — HOL blocking: tick p99 vs detector sleep

**Source:** `summary.csv`, rows where `scenario` starts with `blocking_`

**Axes:**
- X: `detector_sleep_ms`
- Y: `tick_p99_ns / 1e6` (ms) and `tick_p999_ns / 1e6` (ms)
- Reference line: `tick_p99_ns / 1e6` from the `baseline` row

**Claim:** In-process detector latency appears directly in scheduler tick p99 because the
tick loop is synchronous. A 10ms detector produces a ~10ms tick p99.

**What good data looks like:**
- p99 tracks `detector_sleep_ms` closely (linear relationship)
- p99.9 is slightly above p99 (OS scheduling jitter in the tail)
- Baseline reference line is far below the blocking curve

**Red flag:** p99 << detector_sleep_ms at high latencies → detector is being shed
(check that cost_hint is Low in blocking scenarios).

---

### Figure 2 — Load-shedding: time-series

**Source:** `load_shed_<duration>s_timeseries.csv`

**Axes (dual panel):**
- Top: X = `elapsed_s`, Y = `tick_p99_ns / 1e6` (ms)
- Bottom: X = `elapsed_s`, bars for `skips_delta` (orange) and `frame_drops_delta` (red)

**Claim:** With a `High`-cost detector that exceeds its fps budget, the scheduler's backoff
gate sheds it for 500ms windows. The fast detector (DummyDetector) continues running. Tick
p99 stays near-baseline despite the 50ms slow detector existing in the pipeline.

**What good data looks like:**
- Tick p99 stays in the µs range (not approaching 50ms)
- `skips_delta` is non-zero every interval (the slow detector is being skipped in backoff windows)
- `frame_drops_delta` is 0 (the pipeline keeps up — load is shed, not queued)
- `total_ticks` in summary.csv is >> 5000 (fast ticks dominate the count)

**Red flag:** tick p99 ≈ 50ms and `total_ticks` ≈ 600 → shedding is not working. Check
the audit document for diagnosis.

---

### Figure 3 — Graceful degradation: overload raises drops, tick p99 stays bounded

**Source:** `summary.csv`, rows where `scenario` starts with `overload_`

**Axes (dual Y):**
- X: `input_fps`
- Left Y: `total_frame_drops` (red)
- Right Y: `tick_p99_ns / 1e6` (ms, blue)
- Rows: `overload_threshold` (120fps), `overload_moderate` (300fps), `overload_severe` (600fps)

**Claim:** When a slow in-process detector caps the scheduler's effective tick rate to
~182/s, a camera producing > 182fps overflows the bounded frame channel. Excess frames are
dropped by `try_send`, keeping tick latency bounded — the live path is protected.

**What good data looks like:**
- `overload_threshold` (120fps < 182/s): frame_drops ≈ 0
- `overload_moderate` (300fps > 182/s): frame_drops >> 0, rising sharply
- `overload_severe` (600fps): frame_drops >> moderate
- tick_p99 stays roughly flat across all three rows (drops absorb the overload)

**Red flag:** frame_drops = 0 for overload_moderate → drain rate >= 300fps → slow detector
is not running every tick (check max_fps=1000 for the LatencyDetector).

---

### Figure 4 — Tick CDF: tail latency distribution

**Source:** `summary.csv`, rows for `baseline`, `inproc_low`, `blocking_3ms`, `blocking_10ms`

**Axes:**
- X: interpolated tick latency (ms) from p50/p99/p99.9
- Y: percentile (50th–99.9th)

**Note:** This is an approximation from three percentile points, not a full empirical CDF.
Label it clearly in any paper.

**Claim:** The CDF curves separate clearly by scenario. The gap between p99 and p99.9
quantifies tail jitter. Baseline stays flat near zero.

**What good data looks like:**
- Curves are well separated and do not cross
- baseline curve is near the Y-axis (very low latency)
- blocking_10ms curve is shifted right by ~10ms relative to baseline

---

### Figure 5 — fps reference: fast pipeline, no drops

**Source:** `summary.csv`, rows where `scenario` starts with `fps_`

**Axes (dual Y, same structure as Figure 3):**
- X: `input_fps`
- Left Y: `total_frame_drops` (red)
- Right Y: `tick_p99_ms` (blue)
- Rows: `fps_30`, `fps_60`, `fps_120`, `fps_300`

**Claim:** With a fast detector (DummyDetector, ~µs cost), the scheduler drains at ~2000
ticks/s and no frame is dropped even at 300fps. This is the paired control experiment for
Figure 3: it isolates that *the slow detector* causes overload, not the fps itself.

**What good data looks like:**
- frame_drops = 0 at all fps (the drop line hugs the X-axis)
- tick_p99 stays in µs range across all fps values

---

## Plotting recipe (any tool)

All figures follow the same data flow:

1. Load `target/bench_results/summary.csv`
2. Filter rows by `scenario` prefix
3. Convert `*_ns` columns to ms: divide by `1e6`
4. Plot the columns listed in each figure's schema above

For Figure 2 (time-series), load `load_shed_<duration>s_timeseries.csv` instead and plot
`elapsed_ms / 1000` on the X-axis.

In Python:
```python
import pandas as pd
df = pd.read_csv("target/bench_results/summary.csv")
df["tick_p99_ms"] = df["tick_p99_ns"] / 1e6
```

In R:
```r
df <- read.csv("target/bench_results/summary.csv")
df$tick_p99_ms <- df$tick_p99_ns / 1e6
```

In Excel: open the CSV, add a computed column `=tick_p99_ns/1000000`.

---

## Interpreting validation output

The harness prints a `[BENCH VALIDATION OK/FAIL]` line after each load_shed and overload
scenario. If a scenario fails validation, it exits 1 and the failure reason is printed:

```
[BENCH VALIDATION FAIL] load_shed: 612 ticks recorded (expected >> 5000).
    tick_p99=49.87ms. Load-shedding did not activate — check cost_hint and overrun budget.

[BENCH VALIDATION FAIL] overload_moderate: zero frame drops.
    Queue never saturated — check that slow detector is running on every tick.
```

A validation failure means the numbers exist in the CSV but do not prove the intended claim.
Do not use those numbers in a report or paper without investigating the root cause first.
