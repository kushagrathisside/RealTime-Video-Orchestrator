# RVO Benchmarking Guide

This document is the end-to-end reference for running, reading, and extending the RVO
benchmark suite. It covers both the micro-benchmarks (per-operation service times) and the
macro load harness (sustained-throughput, tail-latency, and graceful-degradation curves),
plus the plotting pipeline and how to use the numbers in the Stage 3 tech report.

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
| HOL blocking: in-process detector latency appears directly in tick p99 | Fig 1 — tick p99 tracks `blocking_*` detector sleep |
| Load-shedding: High-cost overrunning detector is backed off, tick p99 stays near-baseline | Fig 2 — `load_shed` time-series: skips rise, tick p99 stays low |
| Graceful degradation: overload raises drops, not latency | Fig 3 — `overload_*`: frame drops rise with fps, tick p99 stays bounded |

Fig 4 (CDF) and Fig 5 (fps reference) provide supporting context.
See [BENCHMARK_REMEDIATION_REPORT.md](BENCHMARK_REMEDIATION_REPORT.md) for an explanation
of why earlier scenario designs did not exercise these paths and what was changed.

---

## 2. Suite overview

```
crates/rvo-bench/
  benches/micro.rs          ← criterion micro-benchmarks (per-op service times)
  src/bin/load_harness.rs   ← macro load harness (sustained run, CSV output)
  src/lib.rs                ← HistSummary, CounterSnapshot, CsvWriter shared by harness

scripts/
  bench.sh                  ← runs all 15 load-harness scenarios, produces CSVs

target/bench_results/       ← harness output (CSVs); gitignored
target/criterion/           ← criterion HTML reports; created by cargo bench
docs/report/figures/        ← generated figures; gitignored
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

### One command: all 11 scenarios

```bash
bash scripts/bench.sh
```

This builds the harness in release (enforces LTO + codegen-units=1), runs all 11 scenarios
with 30s duration + 5s warm-up, and appends each result to `target/bench_results/summary.csv`.

### Custom duration / scenario subset

```bash
# 60-second runs (better p99.9 sample count)
bash scripts/bench.sh --duration 60

# Specific scenarios only
bash scripts/bench.sh --scenarios "baseline blocking_3ms blocking_50ms load_shed"

# Single scenario manually (useful during iteration)
./target/release/load_harness \
  --scenario blocking_10ms \
  --duration-secs 30 \
  --warmup-secs 5 \
  --sample-ms 1000 \
  --out-dir target/bench_results
```

### The 15 scenarios

#### HOL-blocking group — no shedding (cost=Low)

| Scenario | Detectors | Frame rate | Purpose |
|---|---|---|---|
| `baseline` | none | ~30fps | Pure scheduler overhead — the floor |
| `inproc_low` | DummyDetector (~0ms) | ~30fps | Cheap in-process baseline |
| `blocking_1ms` | LatencyDetector(1ms, Low, 30fps) | ~30fps | HOL blocking — mild |
| `blocking_3ms` | LatencyDetector(3ms, Low, 30fps) | ~30fps | HOL blocking — moderate |
| `blocking_10ms` | LatencyDetector(10ms, Low, 30fps) | ~30fps | HOL blocking — heavy |
| `blocking_50ms` | LatencyDetector(50ms, Low, 30fps) | ~30fps | HOL blocking — severe |

#### Load-shedding group — backoff active (cost=High)

| Scenario | Detectors | Frame rate | Purpose |
|---|---|---|---|
| `load_shed` | DummyDetector + LatencyDetector(50ms, **High**, **60fps**) | ~30fps | Backoff in action: tick p99 near-baseline |

Why 60fps for the slow detector: budget = (1/60)×2 = 33ms. 50ms > 33ms → overrun fires → 500ms backoff.

#### Overload group — slow tick, high fps (cost=Low)

| Scenario | Detectors | Camera fps | Purpose |
|---|---|---|---|
| `overload_threshold` | LatencyDetector(5ms, Low, 1000fps) | 120fps | No drops — reference point |
| `overload_moderate` | LatencyDetector(5ms, Low, 1000fps) | 300fps | Moderate drops (300 > ~182 tick/s) |
| `overload_severe` | LatencyDetector(5ms, Low, 1000fps) | 600fps | Heavy drops |

Effective tick rate ≈ 182/s (5ms detector + 0.5ms sleep). Low cost ensures the detector is never shed — we want it to slow the tick, not be skipped.

#### fps reference group — fast detector, no drops

| Scenario | Detectors | Camera fps | Purpose |
|---|---|---|---|
| `fps_30` | DummyDetector | 30fps | Fast-pipeline reference |
| `fps_60` | DummyDetector | 60fps | 2× baseline |
| `fps_120` | DummyDetector | 120fps | 4× baseline |
| `fps_300` | DummyDetector | 300fps | 10× baseline (still no drops) |

`LatencyDetector` wraps a real detector and adds a deterministic `thread::sleep`. The
`cost_hint` and `max_fps` fields are now explicit constructor arguments — they are not
inherited from the inner detector.

### What the harness prints during a run

```
[harness] scenario=blocking_10ms duration=30s warmup=5s sample=1000ms
[harness] warming up for 5s ...
[harness] warm-up done, measuring ...
[harness] t=6.0s  tick_p99=10.23ms  skips/s=42  frame_drops/s=0
[harness] t=7.0s  tick_p99=10.18ms  skips/s=38  frame_drops/s=0
...
[harness] DONE  tick_p50=10.02ms  tick_p99=10.23ms  tick_p999=10.41ms  ticks=2891  frame_drops=0
```

`tick_p99` should be close to the detector sleep for blocking scenarios. If it is much
higher, you have system noise (check governor, other processes on the machine).

---

## 6. Output files — schema and meaning

### `target/bench_results/summary.csv`

One row per scenario. The single source of truth for Figures 1, 3, and 4.

| Column | Unit | Meaning |
|---|---|---|
| `scenario` | string | Scenario name |
| `detector_sleep_ms` | ms | Configured detector latency (0 for non-blocking) |
| `input_fps` | fps | Synthetic camera rate for fps_* scenarios; 30 otherwise |
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
| `total_frame_drops` | count | Frames dropped by the bounded camera channel |

**Minimum sample count for valid p99.9:** the HDR histogram needs ≥1000 samples per
percentile decade, so ≥10,000 tick samples for a reliable p99.9. At the default ~2 kHz
tick ceiling, 30s ≈ 60,000 samples — sufficient. At slower tick rates, extend `--duration`.

### `target/bench_results/<scenario>_<duration>s_timeseries.csv`

One row per sample interval (default 1s). Source for Figure 2 (load-shedding time-series).

| Column | Unit | Meaning |
|---|---|---|
| `elapsed_ms` | ms | Wall time since harness start |
| `ticks_delta` | count | Ticks fired in this interval |
| `execs_delta` | count | execute() calls in this interval |
| `skips_delta` | count | Gate skips in this interval — rising = load-shedding |
| `events_delta` | count | Events emitted in this interval |
| `frame_drops_delta` | count | Frames dropped in this interval — rising = camera overload |
| `tick_p50_ns` | ns | p50 tick over the full run so far (HDR is cumulative) |
| `tick_p99_ns` | ns | p99 tick over the full run so far |
| `exec_p50_ns` | ns | p50 detector exec over the full run so far |
| `exec_p99_ns` | ns | p99 detector exec |
| `staleness_p50_ns` | ns | p50 frame staleness (camera→scheduler latency) |
| `staleness_p99_ns` | ns | p99 frame staleness |
| `frame_queue_depth` | count | Live frame channel depth at sample time (saturation gauge) |

**Note:** because the HDR histogram is cumulative (not windowed), p99 in the timeseries
represents the p99 of all ticks *since warm-up*, not just the last interval. Use the
time-series primarily for observing `skips_delta` and `frame_drops_delta` trends; use the
summary CSV for end-of-run percentile comparisons.

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
| Fig 1 | `fig1_tick_p99_vs_detector_latency.pdf` | HOL blocking: tick p99 tracks detector sleep |
| Fig 2 | `fig2_load_shedding.pdf` | Backoff keeps tick fast while shedding slow detector |
| Fig 3 | `fig3_overload_graceful_degradation.pdf` | Drops rise under overload; tick p99 stays bounded |
| Fig 4 | `fig4_tick_cdf.pdf` | Tail latency distribution per scenario |
| Fig 5 | `fig5_fps_reference.pdf` | Fast pipeline: no drops at any fps (control experiment for Fig 3) |

---

## 8. Interpreting each figure

### Figure 1 — HOL blocking: tick p99 vs detector latency

**What it shows:** tick p99 and p99.9 on the Y-axis vs configured in-process detector sleep
on the X-axis, with a horizontal dashed line at the baseline (no detectors).

**What to look for:**
- In an unprotected scheduler (no load-shedding), tick p99 would track the detector sleep
  linearly: 10ms detector → ~10ms tick.
- With RVO's FPS-cap + backoff gates, the tick p99 for a properly shedding scenario should
  rise more slowly and plateau, because the scheduler skips overrun detectors.
- The gap between `tick_p99` and `tick_p999` indicates jitter in the tail.

**Key claim supported:** "cost-hint load-shedding prevents a slow detector from
proportionally degrading the control loop."

**Red flags in the data:**
- tick p99 exactly equals `detector_sleep_ms` × 1ms at every data point — means load-shedding
  is not activating (check the `total_skips` column in summary.csv).
- Baseline tick p99 > 1ms — system noise, check governor.

---

### Figure 2 — Load-shedding: time-series

**What it shows:** dual-axis time-series for the `load_shed` scenario (DummyDetector +
50ms LatencyDetector running together). Top panel: rolling tick p99. Bottom panel:
`skips_delta` (orange bars) and `frame_drops_delta` (red bars) per sample interval.

**What to look for:**
- `skips_delta` should be non-zero from the start and rise as the 50ms detector is
  repeatedly backed off — this is the load-shedder working.
- `frame_drops_delta` should stay near zero — the slow detector's backoff prevents it from
  monopolising tick time, keeping the camera channel draining.
- `tick_p99` should stay bounded (not track 50ms) — because the 50ms detector is shed.

**Key claim supported:** "the DummyDetector keeps running at its cadence; the slow detector
is shed without starving the fast path."

**Red flags:**
- `skips_delta` is zero — backoff logic not triggering. Increase OVERRUN_FACTOR or check
  that `cost_hint = High` for the LatencyDetector.
- `tick_p99` approaches 50ms — load-shedding is not effective.

---

### Figure 3 — Graceful degradation: overload_* scenarios

**What it shows:** frame drops (left Y, red) and tick p99 (right Y, blue) vs input fps,
for `overload_threshold` (120fps), `overload_moderate` (300fps), `overload_severe` (600fps).

**Setup:** all three use `LatencyDetector(5ms, Low, 1000fps)`. This runs on every eligible
tick (min_interval = 1ms), slowing effective tick rate to ~182/s. Camera fps > 182 causes
the bounded channel to saturate.

**What to look for:**
- `overload_threshold` (120fps): zero drops — below saturation point
- `overload_moderate` (300fps): substantial drops starting within the first second
- `overload_severe` (600fps): much heavier drops
- Tick p99 stays roughly constant across all three — drops absorb the excess, not latency

**Key claim supported:** "bounded queues degrade gracefully: excess frames are dropped
rather than queued, keeping tick latency predictable under overload."

**Red flags:**
- Frame drops = 0 for `overload_moderate` → slow detector not running every tick; harness
  validation will also `exit(1)` and print a diagnostic.
- Tick p99 grows sharply with fps → something is queuing latency instead of shedding frames.

**See also:** Figure 5 is the paired control — same fps range with DummyDetector (fast tick)
produces zero drops at all fps. This isolates the slow detector as the cause of overload, not
the fps itself.

---

### Figure 4 — Tick CDF

**What it shows:** approximate CDF of tick duration for four scenarios
(`baseline`, `inproc_low`, `blocking_3ms`, `blocking_10ms`). X-axis: latency in ms.
Y-axis: percentile (50th to 99.9th).

**Note on approximation:** the CDF is interpolated from three reported percentiles
(p50/p99/p99.9). It is an approximation — not a full empirical CDF — because the raw
histogram buckets are not exported to CSV. It is sufficient for a visual comparison but
should be noted as such in the report.

**What to look for:**
- The curves should separate cleanly: baseline lowest, then inproc_low (slight overhead),
  then blocking_3ms, then blocking_10ms.
- The steepness of each curve in the 99–99.9 range shows tail behaviour. A sudden jump
  indicates jitter or OS interference at the extreme tail.

---

## 9. Statistical rigour checklist

Before quoting any number in a report or paper:

- [ ] **≥ 5 runs per scenario** on the same machine, same governor setting. Report
      median + 95% confidence interval, not a single run.
- [ ] **Warm-up window excluded.** Default is 5s. For slow-converging scenarios (blocking_50ms),
      consider 10s warmup (`--warmup-secs 10`).
- [ ] **≥ 10,000 tick samples for p99.9.** Check `tick_count` in summary.csv. At 30s + 2kHz
      ceiling this is ~50,000 — fine. At lower rates, extend duration.
- [ ] **Baseline recorded in the same session.** Run `baseline` immediately before the
      comparison scenario so hardware state (caches, TLB) is consistent.
- [ ] **No other significant load on the machine.** Check with `htop` before starting.
      Stop anything using the network (the gRPC remote tests use port 50337).
- [ ] **Hardware spec documented.** Every quoted number must name the CPU, RAM, OS kernel,
      and Rust version.
- [ ] **Coordinated omission acknowledged.** The harness measures latency from when the
      tick fires, not from when the frame was due. This means it does not capture queuing
      delay at the camera channel. This is acceptable for a control-loop latency claim but
      must be noted as a limitation.

To run 5 times and compute median + CI manually:

```bash
for i in 1 2 3 4 5; do
  rm -f target/bench_results/summary.csv
  bash scripts/bench.sh --scenarios "baseline blocking_10ms load_shed" --duration 60
  cp target/bench_results/summary.csv target/bench_results/summary_run${i}.csv
done

# Then in Python:
import pandas as pd, glob
dfs = [pd.read_csv(f) for f in glob.glob("target/bench_results/summary_run*.csv")]
combined = pd.concat(dfs)
print(combined.groupby("scenario")[["tick_p99_ns","tick_p999_ns"]].agg(["median","std","count"]))
```

---

## 10. Adding a new scenario

1. Add the scenario name and detector list to `detectors_for()` in
   [load_harness.rs](../crates/rvo-bench/src/bin/load_harness.rs).
2. If it needs a different camera fps, add it to `camera_fps_for()`.
3. If it has a meaningful `detector_sleep_ms`, add it to the lookup near the end of `run()`.
4. Add the scenario to `ALL_SCENARIOS` in [bench.sh](../scripts/bench.sh).
5. If the scenario should validate that a mechanism fired, add a case to `validate_scenario()`.
6. Update [PLOT_GUIDE.md](PLOT_GUIDE.md) with the scenario's axes and expected output.

---

## 11. Common pitfalls

### `tick_p99` equals exactly `sleep_ms` for every blocking scenario

The load-shedding backoff is not activating. Likely cause: the `LatencyDetector` is set to
`cost_hint = Low` (Low-cost detectors are never backed off, by design). Check
[load_harness.rs:119](../crates/rvo-bench/src/bin/load_harness.rs#L119) — `LatencyDetector`
delegates to `DummyDetector`'s `cost_hint`, which is Low. To demonstrate backoff, use
`cost_hint = High` (requires a custom detector wrapper) or use `blocking_50ms` which exceeds
any budget and will trigger backoff via the OVERRUN_FACTOR gate.

### summary.csv keeps appending across runs

`bench.sh` deletes `summary.csv` at the start of a full run. If you run individual
scenarios manually without clearing first, rows accumulate. Clear manually before a clean
measurement session:

```bash
rm -f target/bench_results/summary.csv
```

### Plotting tool cannot find `summary.csv`

The harness must have completed at least one scenario with `--out-dir target/bench_results`.
Check that `target/bench_results/summary.csv` exists before attempting to plot.

### Criterion shows high variance

Usually governor not set, or turbo is still enabled. Verify:

```bash
cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor  # must print: performance
cat /sys/devices/system/cpu/intel_pstate/no_turbo           # must print: 1
```

### `cargo bench` reports 0 measurements

criterion requires `--release` implicitly for benchmarks (via the `[profile.bench]`
inheriting release in Cargo.toml). If benches are slow or produce 0 measurements, ensure
you are not forcing debug mode with `CARGO_PROFILE=debug`.

---

## 12. Using the numbers in the tech report

The Stage 3 tech report (LaTeX source under `docs/report/`) has one evaluation section.
Structure it as follows:

### 12.1 Experimental setup subsection

```
Hardware: <CPU, RAM>
OS: <kernel>
Toolchain: Rust 1.95.0, LTO=true, codegen-units=1
Governor: performance, turbo disabled
Warm-up: 5s excluded from all reported measurements
Sample count: ≥50,000 tick samples per scenario at default rate
```

### 12.2 Micro-benchmark table

Pull p50 and p99 from criterion HTML reports. Report as a table:

| Operation | p50 (ns) | p99 (ns) |
|---|---|---|
| SignalStore::upsert | — | — |
| SignalStore::get (hit) | — | — |
| FrameBuffer::push | — | — |
| EventEngine::update | — | — |
| Scheduler::tick (0 detectors) | — | — |
| Scheduler::tick (8 null detectors) | — | — |

This feeds a capacity sentence: "with 8 detectors, the scheduler overhead is Xµs/tick,
leaving a Y% budget at 1ms tick intervals."

### 12.3 Key results paragraph (fill in from your run)

A template, fill in the blanks from `summary.csv`:

> With no detectors, tick p50/p99/p99.9 are X/Y/Z µs, establishing the scheduler overhead
> floor. Adding a 10ms in-process LatencyDetector raises p99 to W ms — a [W/Y]× increase
> consistent with HOL blocking on the synchronous execution path. The `load_shed` scenario
> (DummyDetector + 50ms LatencyDetector together) shows p99 of V ms: the cost-hint backoff
> sheds the slow detector after its first overrun, keeping the fast detector's cadence
> intact, as visible in the skips rate (U skips/s for the 50ms detector vs near-zero for
> DummyDetector). Under camera overload at 300fps, frame drops climb to N/s while tick p99
> stays within T ms — bounded queues absorb the burst without latency explosion.

### 12.4 Figures

Embed all four PDFs from `docs/report/figures/` in the LaTeX source. Each figure should
have a caption that explicitly names what the X and Y axes measure and what the key takeaway
is. Do not embed raw CSV values in figure captions — state the claim the figure supports.

### 12.5 Limitations to state honestly

- Harness uses `thread::sleep(500µs)` between ticks, so maximum achieved rate is ~2 kHz,
  not the theoretical 1 kHz target. Real-world deployment would remove this ceiling.
- Coordinated omission: latency is measured from tick-start, not from frame arrival time.
  Queuing delay at the camera channel is not captured.
- Single-machine, single-process evaluation. Distributed scheduling, NUMA effects, and
  multiple camera streams are not tested.
- `LatencyDetector` uses `thread::sleep`, which is not the same cost distribution as a
  real model call (real inference has non-uniform latency, cache effects, and GPU
  synchronisation). The blocking scenarios are a controlled proxy, not a production model.

---

*This document is the living reference for Stage 2 (benchmarking) and Stage 3 (tech report)
of the RVO improvement plan. Update the "fill in from your run" placeholders once you have
real numbers from the bare-metal run.*
