# RVO Benchmark Validity Audit

**Auditor:** engineering review of `crates/rvo-bench`, `scripts/`, and `docs/BENCHMARKING.md`
**Repository state:** post Stage-1/Stage-2 build, pre-remediation
**Scope:** does each documented benchmark claim trace to a code path that actually exercises it?

---

## 1. Claim–Implementation Traceability Table

| # | Documented Claim | Source | Implementation Responsible | Tested? | Evidence |
|---|---|---|---|---|---|
| C1 | Baseline tick p50/p99 ≈ µs (scheduler overhead floor) | BENCHMARKING §8 | `baseline` scenario, no detectors | **Yes** | Hardware: p50≈4.4µs, p99≈8.5µs ✓ |
| C2 | HOL blocking: tick p99 tracks detector sleep | BENCHMARKING §8, Fig 1 | `blocking_*` scenarios, `LatencyDetector` | **Yes** | 1ms→1ms, 3ms→3ms, 10ms→10ms ✓ |
| C3 | Load-shedding: slow detector shed, tick p99 stays near-baseline | BENCHMARKING §8, Fig 2 | `load_shed` scenario, scheduler `apply_backoff()` | **No** | `load_shed` p99≈50ms = same as `blocking_50ms` |
| C4 | Bounded queues: overload raises drops, not latency | BENCHMARKING §8, Fig 3 | `fps_*` scenarios, `try_send` in camera thread | **No** | fps_30 through fps_300: zero drops |
| C5 | Panic isolation: panicking detector disables, pipeline survives | scheduler.rs test | `catch_unwind` + `panicking_detector_does_not_kill_scheduler` | **Yes** | Unit test passes ✓ |
| C6 | Bounded clip threads: burst does not spawn unbounded threads | clips/manager.rs test | `ClipManager` bounded channel + `event_burst_is_bounded` test | **Yes** | Unit test passes ✓ |
| C7 | gRPC remote path: non-blocking inference over real transport | grpc_pipeline.rs | In-process tonic server + `RemoteGrpcDetector` | **Yes** | Integration test passes ✓ |
| C8 | Signal TTL staleness: expired signals do not trigger events | signals/store.rs tests | `SignalStore::get` TTL check | **Yes** | Unit tests pass ✓ |

**Summary:** C1 and C2 are experimentally supported. C3 and C4 are not — the scenarios produce the wrong conditions. C5–C8 are supported by unit/integration tests.

---

## 2. Finding 1: `LatencyDetector` inherits incorrect cost classification

**File:** `crates/rvo-testkit/src/detectors.rs:179`

```rust
impl DetectorNode for LatencyDetector {
    fn meta(&self) -> DetectorMeta {
        self.inner.meta()   // ← delegates entirely to inner
    }
}
```

`LatencyDetector` wraps `DummyDetector`. `DummyDetector.meta()` returns:

```rust
DetectorMeta {
    max_fps: 30.0,
    cost_hint: DetectorCostHint::Low,
    ...
}
```

The scheduler's backoff guard (`scheduler.rs:41`):

```rust
fn apply_backoff(&mut self, cost: DetectorCostHint, now: Instant) {
    let duration = match cost {
        DetectorCostHint::Low => return,   // ← early return, no backoff ever
        ...
    };
}
```

**Result:** A 50ms `LatencyDetector` can never be shed regardless of its actual runtime, because the backoff path is statically unreachable for `Low`-cost detectors. The `load_shed` scenario is structurally identical to `blocking_50ms`.

---

## 3. Finding 2: Overrun budget not exceeded for `blocking_50ms`

**File:** `crates/rvo-scheduler/src/scheduler.rs:226`

```rust
const OVERRUN_FACTOR: f64 = 2.0;
let budget_ns = (min_interval.as_nanos() as f64 * OVERRUN_FACTOR) as u64;
if elapsed_ns > budget_ns {
    self.runtime[i].apply_backoff(detector.cost_hint(), now);
}
```

`LatencyDetector` inherits `max_fps = 30.0` from `DummyDetector`:

```
min_interval = 1/30 ≈ 33 ms
budget       = 33 ms × 2.0 = 66 ms
actual sleep = 50 ms
50 ms < 66 ms  →  overrun condition is FALSE
```

**Result:** Even if `cost_hint` were `High`, the overrun budget would not be breached by a 50ms sleep at 30fps. Load-shedding requires that the detector both (a) be classified `Medium` or `High`, and (b) exceed its FPS-derived time budget. Finding 1 and Finding 2 are independent fences; both must be cleared for backoff to fire.

**Fixed by:** setting `cost_hint = High` AND `max_fps = 60.0` on the shed detector, giving a 33ms budget that 50ms genuinely exceeds.

---

## 4. Finding 3: fps overload scenarios never saturate the scheduler

**File:** `crates/rvo-bench/src/bin/load_harness.rs:224`

```rust
thread::sleep(Duration::from_micros(500)); // ~2 kHz ceiling
```

The inter-tick sleep caps the scheduler at ~2000 ticks/second. The synthetic camera in the overload scenarios produces at most 300fps:

```
drain rate  ≈ 2000 frames/s (scheduler ticks)
feed  rate  =  300 frames/s (fps_300 camera)
drain >> feed  →  frame channel never fills  →  zero drops
```

Frame channel capacity (`bounded(64)`) is irrelevant because the drain rate is always faster than the feed rate for all `fps_*` scenarios.

**Fixed by:** adding `overload_*` scenarios with a slow in-process detector (`LatencyDetector(5ms, Low, 1000fps)`) that caps the effective tick rate to ~182/s — below the 300fps camera rate — so the channel genuinely saturates.

Derivation of effective tick rate under overload:

```
tick_cost    = 5ms detector + 0.5ms inter-tick sleep = 5.5ms/tick
tick rate    ≈ 1 / 5.5ms ≈ 182 ticks/s
fps_300 feed = 300 frames/s > 182 → overflow rate = 118 frames/s
channel_cap  = 64 frames → saturation in 64/118 ≈ 0.54s → drops guaranteed
```

---

## 5. Finding 4: Benchmarks do not self-validate

The harness exits 0 regardless of whether the intended mechanism fired. A run where load-shedding silently failed (as in the current `load_shed` scenario) is indistinguishable from a successful run at the terminal level. This creates a category of false-positive benchmarks: the numbers look plausible (they match `blocking_50ms`) but prove the opposite of the claim.

**Fixed by:** adding explicit post-run validation checks in the harness that `exit(1)` with a diagnostic message when:
- `load_shed` does not produce scheduler skips
- `overload_*` produces zero frame drops

---

## 6. What is NOT broken

- The **scheduler implementation** is correct. Load-shedding does work for detectors classified `Medium` or `High` that genuinely exceed their budget. `LoadDetector` (`cost=High`, `max_fps=10`) in the production config is correctly classified.
- The **metrics infrastructure** is correct. All counters and histograms record accurately.
- The **HOL blocking** demonstrations (C2) are valid and show real behaviour.
- The **unit and integration tests** (C5–C8) exercise the correct code paths.

The bugs are entirely in benchmark scenario design, not in the runtime.
