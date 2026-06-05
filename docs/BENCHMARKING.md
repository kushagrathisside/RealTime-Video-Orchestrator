# RVO Benchmarking Guide

This document is the end-to-end reference for running, reading, and extending the RVO
benchmark suite. It covers both the micro-benchmarks (per-operation service times) and the
macro load harness (sustained-throughput, tail-latency, and graceful-degradation curves),
plus the plotting pipeline and how to use the numbers in the Stage 3 tech report.

For methodology rationale, measured results, and architectural claims, see
[BENCHMARK_PERFORMANCE.md](BENCHMARK_PERFORMANCE.md).

---

## Table of Contents

1. [Why benchmarking matters here](#1-why-benchmarking-matters-here)
2. [Suite overview](#2-suite-overview)
3. [Environment requirements](#3-environment-requirements)
4. [Running the micro-benchmarks](#4-running-the-micro-benchmarks)
5. [Running the macro load harness](#5-running-the-macro-load-harness)
6. [Output files — schema and meaning](#6-output-files--schema-and-meaning)
7. [Generating figures](#7-generating-figures)
8. [Interpreting each figure](#8-interpreting-each-figure)
9. [Statistical rigour checklist](#9-statistical-rigour-checklist)
10. [Adding a new scenario](#10-adding-a-new-scenario)
11. [Common pitfalls](#11-common-pitfalls)
12. [Using the numbers in the tech report](#12-using-the-numbers-in-the-tech-report)

---

## 1. Why benchmarking matters here

RVO's core claim is that bounded queues + cost-hint load-shedding + decoupled gRPC inference
keep the control-loop tail latency flat even when detectors are slow or the camera is
overloaded. Numbers are required to substantiate that claim in any interview, tech report, or
paper. Without them the architecture is a design argument, not an evaluated system.

The three specific properties that must be demonstrated:

| Property | Measured by |
|---|---|
| HOL blocking: in-process detector latency appears directly in tick p50 and p99 | Fig 1 — tick p50 tracks `blocking_*` detector sleep ms-for-ms |
| Load-shedding: High-cost overrunning detector is backed off, tick p99 stays near-baseline | Fig 2 — `load_shed` time-series: skips rise, tick p99 stays low |
| Graceful degradation: overload raises frame loss, not latency | Fig 3 — `overload_*`: frame_loss_rate rises with fps, tick p99 stays bounded |

Fig 4 (CDF) and Fig 5 (throughput ceiling) provide supporting context.

---

## 2. Suite overview

```
crates/rvo-bench/
  benches/micro.rs          <- criterion micro-benchmarks (per-op service times)
  src/bin/load_harness.rs   <- macro load harness (sustained run, CSV output)
  src/lib.rs                <- HistSummary, CounterSnapshot, CsvWriter shared by harness

target/bench_results/       <- harness output (CSVs); gitignored
target/criterion/           <- criterion HTML reports; created by cargo bench
docs/report/figures/        <- generated figures; gitignored
```

**Micro-benchmarks** (criterion) measure the cost of a single operation in isolation:
`SignalStore::upsert`, `FrameBuffer::push`, `EventEngine::update`, `Scheduler::tick` with
0/1/4/8 null detectors. These establish the per-operation service times that feed a
back-of-envelope capacity model.

**Macro load harness** drives the full scheduler at configurable fps with configurable
detector mixes for a sustained duration (default 30s + 5s warm-up). It captures all
histogram percentiles and counter deltas into per-interval time-series CSVs and a final
summary CSV.

---

## 3. Environment requirements

**WSL2 is not valid for p99/p99.9 numbers.** The hypervisor scheduler and the lack of
CPU isolation make tail latencies meaningless — values can be 10× higher than bare-metal
and are not reproducible. Develop and iterate on WSL; run the headline benchmarks
on bare-metal Linux or a dedicated VM.

### Mandatory before any bench run

```bash
# 1. Set CPU governor to performance (prevents freq scaling jitter)
sudo cpupower frequency-set -g performance

# 2. Verify governor is set on all cores
cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor | sort -u
# expected output: performance

# 3. Disable turbo boost (reduces variance in p99.9)
#    Intel:
echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo
#    AMD:
echo 0 | sudo tee /sys/devices/system/cpu/cpufreq/boost

# 4. Optional: pin the harness to isolated cores (strongest isolation)
# First isolate cores at boot via isolcpus=2,3 in GRUB, then:
taskset -c 2,3 ./target/release/load_harness --scenario baseline
```

### Recommended: document your hardware

Record the following in your tech report's evaluation section before quoting any number:

```
CPU:     <model, GHz, core count>
RAM:     <GB, speed>
OS:      <distro, kernel version>
Rust:    <rustc --version>
Profile: release, LTO=true, codegen-units=1
Governor: performance, turbo: disabled
```

---

## 4. Running the micro-benchmarks

```bash
# Build and run all micro-benchmarks (release mandatory)
cargo bench -p rvo-bench --bench micro

# Run only the signal_store group
cargo bench -p rvo-bench --bench micro -- signal_store

# Run only the scheduler_tick group
cargo bench -p rvo-bench --bench micro -- scheduler_tick
```

HTML reports land in `target/criterion/`. Open `target/criterion/report/index.html` in a
browser for full violin plots and regression history.

### What each group measures

| Group | Benchmarks | What it tells you |
|---|---|---|
| `signal_store` | `upsert`, `get_hit`, `get_miss_expired` | Cost of the seqlock-protected slot read/write on the hot path |
| `frame_buffer` | `push_300`, `slice_window_10s` | Cost of ring-buffer append and clip-window slice |
| `event_engine` | `update_no_fire`, `update_fires` | Cost of temporal state machine evaluation per tick |
| `scheduler_tick` | `no_detectors`, `null_detectors/1/4/8` | Pure scheduler overhead + linear cost per detector gate check |

The scheduler tick numbers are the capacity model anchor: if `null_detectors/8` costs X µs,
adding 8 real detectors costs at minimum X µs/tick of overhead (detector work is additive).

---

## 5. Running the macro load harness

### One command: all 13 scenarios, single run

```bash
cargo build -p rvo-bench --bin load_harness --release
./target/release/load_harness --all
```

This runs all 13 scenarios sequentially (30s measurement + 5s warm-up each), writing each
result to `target/bench_results/summary.csv`. Any stale `summary.csv` is removed
automatically before the first scenario starts.

### Multiple runs for variance (recommended for reports)

```bash
# 5 repeats of every scenario — produces 65 rows in summary.csv with a run_id column
./target/release/load_harness --all --runs 5

# 3 repeats with 60-second measurement windows (better p99.9 sample count)
./target/release/load_harness --all --runs 3 --duration-secs 60
```

Each repeat appends rows with a distinct `run_id` (1..=N). Group by `scenario` and compute
mean ± stddev across run_id values to get variance-aware results.

### Custom duration / single scenario

```bash
# 60-second runs (better p99.9 sample count)
./target/release/load_harness --all --duration-secs 60

# Single scenario
./target/release/load_harness --scenario blocking_10ms --duration-secs 30

# Single scenario, custom output dir
./target/release/load_harness --scenario load_shed --out-dir /tmp/bench
```

### The 13 scenarios

#### HOL-blocking group — no shedding (cost=Low), detector runs every tick

`max_fps=10000` (min_interval = 0.1ms) ensures the detector runs on every scheduler tick.
`tick_p50` therefore directly measures detector latency — not scheduler overhead. This is
the correct metric for HOL blocking: a 10ms detector produces a 10ms tick p50.

| Scenario | Detector | tick_p50 expected | Purpose |
|---|---|---|---|
| `baseline` | none | ~5µs | Pure scheduler overhead — the floor |
| `inproc_low` | DummyDetector (~0ms) | ~5µs | Cheap in-process baseline |
| `blocking_1ms` | LatencyDetector(1ms, Low, 10000fps) | ~1ms | HOL blocking — mild |
| `blocking_3ms` | LatencyDetector(3ms, Low, 10000fps) | ~3ms | HOL blocking — moderate |
| `blocking_10ms` | LatencyDetector(10ms, Low, 10000fps) | ~10ms | HOL blocking — heavy |
| `blocking_50ms` | LatencyDetector(50ms, Low, 10000fps) | ~50ms | HOL blocking — severe; also causes ring-buffer frame loss |

`blocking_50ms` is special: with tick_p50 ≈ 50ms the scheduler drains at only ~19.7/s,
which is below the 30fps camera rate. Ring-buffer overwrites occur (~10 frames/s lost).
`frame_loss_rate` in the CSV captures this.

#### Load-shedding group — backoff active (cost=High)

| Scenario | Detectors | Camera fps | Purpose |
|---|---|---|---|
| `load_shed` | DummyDetector + LatencyDetector(50ms, **High**, **60fps**) | ~30fps | Backoff in action: tick p99 near-baseline |

Why 60fps for the slow detector: `budget = (1/60) × 2 = 33ms`. The 50ms sleep exceeds
33ms → overrun fires → `apply_backoff(High)` → 500ms backoff. During backoff,
DummyDetector continues running freely. Tick p99 stays near-baseline (~5µs);
`total_ticks` >> 5000 in 30s (a slow-only scheduler would reach ~600).

#### Overload group — slow detector, camera fps sweeps above scheduler ceiling

All three use `LatencyDetector(5ms, Low, 1000fps)` (min_interval=1ms, runs every tick):

```
tick cost     = 5ms detector + 0.5ms inter-tick sleep ≈ 5.5ms/tick
effective rate ≈ 176/s

overload_threshold:  120fps camera  < 176/s → no frame loss (reference)
overload_moderate:   300fps camera  > 176/s → ~120 frames/s lost in ring buffer
overload_severe:     600fps camera  > 176/s → ~400 frames/s lost in ring buffer
```

Frame loss is captured by `frame_loss_rate = actual_camera_fps - effective_fps` in the
CSV, not by `total_frame_drops` (which stays 0 — the bounded channel never saturates
because the scheduler batch-drains all pending frames per tick; ring-buffer overwrites are
silent by design).

| Scenario | Camera fps | Expected effective_fps | Expected frame_loss_rate |
|---|---|---|---|
| `overload_threshold` | 120fps | ~176/s | 0 |
| `overload_moderate` | 300fps | ~176/s | ~124/s |
| `overload_severe` | 600fps | ~176/s | ~424/s |

#### Throughput ceiling group — fast detector, camera fps probes scheduler ceiling

DummyDetector tick rate ≈ 1756/s. Scenarios probe below and above that ceiling to show
when the ring buffer starts losing frames. `actual_camera_fps` is measured (not assumed)
because `thread::sleep` granularity caps the camera thread below its configured rate at
high fps.

| Scenario | Configured fps | Typical actual fps | Expected outcome |
|---|---|---|---|
| `fps_1000` | 1000fps | ~938/s | Below ceiling → 0 frame loss (reference) |
| `fps_2000` | 2000fps | ~1772/s | Near ceiling → marginal loss (~14/s) |
| `fps_5000` | 5000fps | ~3841/s | Above ceiling → heavy loss (~2086/s) |

`frame_loss_rate = (actual_camera_fps - effective_fps).max(0)`. If the camera thread
cannot reach its configured rate, `frame_loss_rate` correctly reports 0 rather than
attributing phantom losses.

### What the harness prints during a run

```
[harness] scenario=blocking_10ms duration=30s warmup=5s sample=1000ms
[harness] warming up for 5s ...
[harness] warm-up done, measuring ...
[harness] t=6.0s  tick_p99=10.10ms  skips/s=0  frame_drops/s=0
...
[harness] DONE  tick_p50=10.10ms  tick_p99=10.34ms  tick_p999=10.39ms  ticks=2793  frame_drops=0
[BENCH VALIDATION OK] blocking_10ms: tick_p50=10.10ms ≈ 10ms injected sleep, 0 frame drops
```

For blocking scenarios, `tick_p99` should be close to the detector sleep. For
`blocking_50ms` with ring-buffer loss, a NOTE is printed showing the frame loss rate.

---

## 6. Output files — schema and meaning

### `target/bench_results/summary.csv`

One row per scenario per run. The single source of truth for Figures 1, 3, and 4.

| Column | Unit | Meaning |
|---|---|---|
| `run_id` | int | Run number (1..=N from `--runs N`; always 1 for single runs) |
| `scenario` | string | Scenario name |
| `detector_sleep_ms` | ms | Configured detector latency (0 for non-blocking) |
| `input_fps` | fps | Configured synthetic camera rate |
| `actual_camera_fps` | fps | Measured camera send rate during the measurement window (may be below `input_fps` at high rates due to `thread::sleep` granularity) |
| `duration_secs` | s | Measurement window (after warm-up) |
| `tick_p50_ns` | ns | Median tick duration over the measurement window |
| `tick_p99_ns` | ns | 99th percentile tick duration |
| `tick_p999_ns` | ns | 99.9th percentile tick duration |
| `tick_count` | count | Total tick samples in the histogram |
| `exec_p50_ns` | ns | Median detector execute() duration (all detectors combined) |
| `exec_p99_ns` | ns | p99 detector execute() duration |
| `exec_p999_ns` | ns | p99.9 detector execute() duration |
| `total_ticks` | count | Scheduler ticks fired during measurement |
| `total_execs` | count | Detector execute() calls |
| `total_skips` | count | Detector gate skips (FPS cap + backoff + disabled) |
| `total_events` | count | Events emitted by the event engine |
| `total_frame_drops` | count | Frames dropped at the camera channel level (bounded channel full). This stays 0 for overload scenarios because the scheduler batch-drains the channel on every tick; ring-buffer losses are captured in `frame_loss_rate` instead. |
| `effective_fps` | fps | `total_ticks / duration_secs` — the scheduler's actual processing rate |
| `frame_loss_rate` | fps | `max(0, actual_camera_fps - effective_fps)` — frames/s lost in the ring buffer because the camera outpaced the scheduler |

**Reading `frame_loss_rate`:** a non-zero value means the scheduler cannot process every
frame the camera produces. Frames are silently overwritten in the ring buffer (newest
survives). This is expected for `blocking_50ms`, `overload_moderate`, `overload_severe`,
`fps_2000`, and `fps_5000`.

**Minimum sample count for valid p99.9:** the HDR histogram needs ≥1000 samples per
percentile decade, so ≥10,000 tick samples for a reliable p99.9. At the default ~1756 Hz
tick ceiling, 30s gives ~50,000 samples — sufficient for fast scenarios. For slow
scenarios (`blocking_50ms`: ~590 ticks in 30s), p99.9 is the same as p99 (small sample
counts compress the tail). Extend `--duration-secs` or do not report p99.9 for slow
scenarios.

### `target/bench_results/<scenario>_<duration>s_timeseries.csv`

One row per sample interval (default 1s). Source for Figure 2 (load-shedding time-series).
When `--runs N` is used, each run overwrites the timeseries file; only the last run's
timeseries is kept per scenario. Use `--runs 1` if you need all timeseries files.

| Column | Unit | Meaning |
|---|---|---|
| `elapsed_ms` | ms | Wall time since measurement start (0 = end of warm-up) |
| `ticks_delta` | count | Ticks fired in this interval |
| `execs_delta` | count | execute() calls in this interval |
| `skips_delta` | count | Gate skips in this interval — rising = load-shedding active |
| `events_delta` | count | Events emitted in this interval |
| `frame_drops_delta` | count | Channel-level frame drops in this interval |
| `tick_p50_ns` | ns | p50 tick over the full run so far (HDR is cumulative) |
| `tick_p99_ns` | ns | p99 tick over the full run so far |
| `exec_p50_ns` | ns | p50 detector exec over the full run so far |
| `exec_p99_ns` | ns | p99 detector exec |
| `staleness_p50_ns` | ns | p50 frame staleness (camera→scheduler latency) |
| `staleness_p99_ns` | ns | p99 frame staleness |
| `frame_queue_depth` | count | Live frame channel depth at sample time |

**Note:** because the HDR histogram is cumulative (not windowed), p99 in the timeseries
represents the p99 of all ticks since warm-up, not just the last interval. Use the
time-series primarily for observing `skips_delta` trends; use the summary CSV for
end-of-run percentile comparisons.

---

## 7. Generating figures

See [PLOT_GUIDE.md](PLOT_GUIDE.md) for:
- The complete CSV column schema
- What each of the five figures shows, what axes to use, and what "good data" looks like
- A plotting recipe that works in Python, R, or Excel
- How to interpret the harness validation output

Five figures are produced:

| Figure | File | Claim it supports |
|---|---|---|
| Fig 1 | `fig1_tick_p50_vs_detector_latency.pdf` | HOL blocking: tick p50 tracks detector sleep ms-for-ms |
| Fig 2 | `fig2_load_shedding.pdf` | Backoff keeps tick fast while shedding slow detector |
| Fig 3 | `fig3_overload_graceful_degradation.pdf` | Frame loss rises under overload; tick p99 stays bounded |
| Fig 4 | `fig4_tick_cdf.pdf` | Tail latency distribution per scenario |
| Fig 5 | `fig5_throughput_ceiling.pdf` | Fast pipeline: frame loss appears only above the scheduler tick-rate ceiling |

---

## 8. Interpreting each figure

### Figure 1 — HOL blocking: tick p50 vs detector latency

**What it shows:** `tick_p50` (and `tick_p99`) on the Y-axis vs configured detector sleep
on the X-axis, with a horizontal reference line at the `baseline` (no detectors) value.

**What to look for:**
- `tick_p50` tracks `detector_sleep_ms` linearly and tightly.
- `tick_p99` and `tick_p999` are close to p50 (the sleep is deterministic).
- The baseline reference line is flat at ~5µs, well below all blocking curves.

**Key claim supported:** "an in-process detector that runs synchronously on every tick
imposes its full execution time as direct tick latency — there is no buffering."

**Red flags in the data:**
- `tick_p50` equals the baseline (~5µs) for all blocking scenarios — the detector is not
  running every tick. Check that `max_fps=10000` in `detectors_for()`.
- Baseline `tick_p99` > 1ms — system noise. Check governor and other processes.

---

### Figure 2 — Load-shedding: time-series

**What it shows:** dual-axis time-series for the `load_shed` scenario. Top panel: rolling
tick p99. Bottom panel: `skips_delta` (orange bars) and `frame_drops_delta` (red bars)
per sample interval.

**What to look for:**
- `skips_delta` is non-zero from the start and is consistently non-zero throughout the run
  (the 50ms detector is shed on its first execution, then backed off for 500ms windows).
- `frame_drops_delta` stays at 0 — the fast path is not overwhelmed.
- `tick_p99` stays near-baseline (~10µs), not near 50ms.
- `total_ticks` in summary.csv >> 5000 (fast DummyDetector ticks dominate the count).
- `total_execs` ≈ 900 over 30s — DummyDetector running at its 30fps cadence.

**Key claim supported:** "the backoff mechanism sheds the slow detector without starving
the fast path — DummyDetector continues running freely between backoff windows."

**Red flags:**
- `skips_delta` is zero — backoff never triggered. Verify `cost_hint = High` and
  `max_fps = 60.0` for the LatencyDetector in `load_shed` (budget = 33ms < 50ms sleep).
- `tick_p99` approaches 50ms — load-shedding is not effective.
- `total_ticks` ≈ 600 — the scheduler ran at the slow detector's pace, not the fast one.

---

### Figure 3 — Graceful degradation: overload_* scenarios

**What it shows:** `frame_loss_rate` (left Y, red) and `tick_p99` (right Y, blue) vs
`actual_camera_fps`, for the three overload scenarios.

**What to look for:**
- `overload_threshold` (camera ≈ 118fps < effective 176/s): `frame_loss_rate` = 0.
- `overload_moderate` (camera ≈ 294fps > 176/s): `frame_loss_rate` ≈ 118/s.
- `overload_severe` (camera ≈ 577fps > 176/s): `frame_loss_rate` ≈ 401/s.
- `tick_p99` stays roughly constant across all three (≈ 5ms, the detector sleep).
- `total_frame_drops` is 0 for all three — losses happen in the ring buffer, not the channel.

**Key claim supported:** "bounded queues degrade gracefully: ring-buffer overwrites absorb
excess frames, keeping tick latency predictable under overload."

**Note on validation:** the harness validates overload scenarios by checking that
`effective_fps < actual_camera_fps` for moderate/severe. If this check fails (e.g. the
slow detector is not running every tick), the harness exits 1 with a diagnostic.

**Red flags:**
- `frame_loss_rate` = 0 for `overload_moderate` — effective tick rate >= camera fps.
  Check that `max_fps=1000` (not 30) for the overload LatencyDetector.
- `tick_p99` grows sharply with fps — something is queuing latency, not dropping frames.

---

### Figure 4 — Tick CDF

**What it shows:** approximate CDF of tick duration for four scenarios
(`baseline`, `inproc_low`, `blocking_3ms`, `blocking_10ms`). X-axis: latency in ms.
Y-axis: percentile (50th to 99.9th).

**Note on approximation:** the CDF is interpolated from three reported percentiles
(p50/p99/p99.9). It is an approximation — not a full empirical CDF — because the raw
histogram buckets are not exported to CSV. Label it as such in any paper.

**What to look for:**
- Curves separate cleanly: baseline lowest, then inproc_low, then blocking_3ms, then
  blocking_10ms.
- The p50 of each blocking curve aligns with the injected sleep (3ms and 10ms).
- The curves are steep (low jitter) — the sleep is deterministic so most ticks cluster
  tightly around the p50 value.

---

### Figure 5 — Throughput ceiling: fps_* scenarios

**What it shows:** `frame_loss_rate` (left Y, red) and `tick_p99` (right Y, blue) vs
`actual_camera_fps`, for `fps_1000`, `fps_2000`, `fps_5000`.

**What to look for:**
- `fps_1000` (actual ≈ 938fps < 1756/s ceiling): `frame_loss_rate` = 0.
- `fps_2000` (actual ≈ 1772fps ≈ ceiling): marginal loss (~15/s).
- `fps_5000` (actual ≈ 3841fps > ceiling): heavy loss (~2086/s).
- `tick_p99` stays flat across all three — the fast detector does not slow down as camera
  rate rises.

**Key claim:** this figure identifies the DummyDetector scheduler's throughput ceiling
(~1756/s) and shows how frame loss scales when the camera exceeds it. Compare with
Figure 3: in Figure 3 the ceiling is much lower (~176/s) because the slow detector consumes
each tick for 5ms. The paired comparison isolates the detector as the cause of Figure 3's
earlier onset of frame loss.

**Note on actual vs configured fps:** `actual_camera_fps` is measured per-run. At
configured 5000fps the camera thread achieves only ~3841fps due to `thread::sleep`
granularity. The `frame_loss_rate` column uses `actual_camera_fps` so the reported loss
is accurate (not inflated by a configured rate the camera never reached).

---

## 9. Statistical rigour checklist

Before quoting any number in a report or paper:

- [ ] **≥ 5 runs per scenario** on the same machine, same governor setting. Run with
      `--runs 5`. Report median ± 95% CI, not a single run.
- [ ] **Warm-up window excluded.** Default is 5s. For slow-converging scenarios
      (`blocking_50ms`), consider `--warmup-secs 10`.
- [ ] **≥ 10,000 tick samples for p99.9.** Check `tick_count` in summary.csv. Fast
      scenarios at 30s give ~50,000 samples — fine. `blocking_50ms` produces only ~590
      ticks in 30s; do not report p99.9 for it or extend `--duration-secs`.
- [ ] **Baseline recorded in the same session.** `--all` runs baseline immediately before
      other scenarios, keeping hardware state (caches, TLB) consistent.
- [ ] **No other significant load on the machine.** Check with `htop` before starting.
- [ ] **Hardware spec documented.** Every quoted number must name the CPU, RAM, OS kernel,
      and Rust version.
- [ ] **`actual_camera_fps` vs `input_fps` noted for fps_* scenarios.** The configured
      fps and the achieved fps diverge at high rates. Quote `actual_camera_fps` in reports,
      not `input_fps`.
- [ ] **Coordinated omission acknowledged.** Tick latency is measured from when
      `scheduler.tick()` is called, not from when the frame was due. Queuing delay at the
      camera channel is not captured. This is acceptable for a control-loop latency claim
      but must be noted as a limitation.

### Computing mean ± CI from multi-run output

```python
import pandas as pd, scipy.stats as st

df = pd.read_csv("target/bench_results/summary.csv")
results = []
for scenario, group in df.groupby("scenario"):
    tick_p99_ms = group["tick_p99_ns"] / 1e6
    n = len(tick_p99_ms)
    mean = tick_p99_ms.mean()
    sem = tick_p99_ms.sem()
    ci95 = st.t.ppf(0.975, df=n-1) * sem if n > 1 else float("nan")
    results.append({"scenario": scenario, "n": n,
                    "tick_p99_ms_mean": mean, "tick_p99_ms_ci95": ci95,
                    "frame_loss_mean": group["frame_loss_rate"].mean()})
print(pd.DataFrame(results).to_string(index=False))
```

---

## 10. Adding a new scenario

1. Add the scenario name and detector list to `detectors_for()` in
   [load_harness.rs](../crates/rvo-bench/src/bin/load_harness.rs).
2. If it needs a different camera fps, add it to `camera_fps_for()`.
3. If it has a meaningful `detector_sleep_ms`, add it to the match near the end of `run()`.
4. Add the scenario name to `ALL_SCENARIOS` so `--all` includes it.
5. Add a `validate_scenario()` case if the scenario should self-check that its intended
   mechanism fired.
6. Update [PLOT_GUIDE.md](PLOT_GUIDE.md) with the scenario's axes and expected output.

---

## 11. Common pitfalls

### `tick_p50` stays at ~5µs for blocking scenarios

The detector is not running every tick. Verify that `max_fps=10000` (not 30) for
`blocking_1/3/10ms` scenarios in `detectors_for()`. With `max_fps=30`, the detector fires
on only ~1.74% of ticks and tick p50 always reports scheduler overhead, hiding the HOL effect.

### `frame_loss_rate` is 0 for overload_moderate

The effective tick rate is >= the actual camera fps. Most likely the overload LatencyDetector
has the wrong `max_fps` (should be 1000 so it runs every tick). Harness validation will
also emit `[BENCH VALIDATION FAIL]` and exit 1.

### `load_shed` shows `total_ticks` ≈ 600

Backoff is not activating. Check:
1. The LatencyDetector in `load_shed` must use `DetectorCostHint::High` and `max_fps=60.0`.
   Budget = (1/60)×2 = 33ms. The 50ms sleep must exceed this.
2. `total_skips` in summary.csv must be > 0. Zero skips means the overrun check never fired.
3. Running on WSL2 can cause timing jitter that prevents the 50ms sleep from reliably
   exceeding 33ms in the scheduler's measurement window. Run on bare-metal.

### summary.csv accumulates rows across multiple --all runs

`--all` removes `summary.csv` before the first scenario starts. Single-scenario runs
(`--scenario foo`) do not clear the file — they append. Clear manually before a clean
session if needed:

```bash
rm -f target/bench_results/summary.csv
```

### Criterion shows high variance

Usually the CPU governor is not set, or turbo is still enabled. Verify:

```bash
cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor   # must print: performance
cat /sys/devices/system/cpu/intel_pstate/no_turbo            # must print: 1
```

---

## 12. Using the numbers in the tech report

### 12.1 Experimental setup subsection

```
Hardware: <CPU, RAM>
OS: <kernel>
Toolchain: Rust 1.XX, LTO=true, codegen-units=1
Governor: performance, turbo disabled
Warm-up: 5s excluded from all reported measurements
Runs: 5 per scenario (run_id 1–5), median reported
Sample count: ≥50,000 tick samples per scenario at default tick rate
```

### 12.2 Micro-benchmark table

Pull p50 and p99 from criterion HTML reports:

| Operation | p50 (ns) | p99 (ns) |
|---|---|---|
| SignalStore::upsert | — | — |
| SignalStore::get (hit) | — | — |
| FrameBuffer::push | — | — |
| EventEngine::update | — | — |
| Scheduler::tick (0 detectors) | — | — |
| Scheduler::tick (8 null detectors) | — | — |

### 12.3 Key results paragraph (fill in from your run)

> With no detectors, tick p50/p99 are X/Y µs (scheduler overhead floor). Adding a 10ms
> in-process LatencyDetector raises tick p50 to ~10ms — every tick is delayed by the full
> detector latency with no buffering (HOL blocking on the synchronous execution path). The
> `load_shed` scenario runs the same 50ms detector alongside DummyDetector with
> `cost_hint=High`: after the first overrun the scheduler backs off the slow detector for
> 500ms windows, and tick p99 returns to ~5µs (within Z% of baseline) while skips average
> W/s. Under camera overload at 294fps, with the scheduler capped at ~176/s by the 5ms
> detector, the ring buffer loses ~118 frames/s while tick p99 stays within T ms —
> frame loss absorbs the burst without latency explosion.

### 12.4 Figures

Embed all five PDFs from `docs/report/figures/` in the LaTeX source. Each figure caption
must explicitly name what X and Y axes measure and state the key takeaway. Do not embed
raw CSV values in captions — state the claim the figure supports.

### 12.5 Limitations to state honestly

- The harness imposes a `thread::sleep(500µs)` between ticks, capping tick rate at ~1756/s.
  This is a harness-imposed ceiling, not the hardware limit. A production scheduler with a
  monotonic timer and no artificial sleep would achieve a higher rate; do not quote this
  ceiling as a deployment capacity figure.
- Coordinated omission: latency is measured from tick-start, not from frame arrival. Queuing
  delay at the camera channel is not captured in `tick_p99`.
- `actual_camera_fps` for `fps_5000` is ~3841fps (not 5000fps) because `thread::sleep` at
  200µs is at the OS timer resolution limit. The configured fps is an upper bound; the
  measured `actual_camera_fps` is the scientifically valid number.
- Single-machine, single-process evaluation. Distributed scheduling, NUMA effects, and
  multiple camera streams are not tested.
- `LatencyDetector` uses `thread::sleep` for artificial latency. Real inference has a
  non-uniform latency distribution with cache and GPU effects absent from a sleep.

---

*Update the "fill in from your run" placeholders once you have real numbers from your
bare-metal run. Run with `--runs 5 --duration-secs 60` for report-quality data.*
