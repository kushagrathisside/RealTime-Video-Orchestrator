# Low-Latency Systems — Concepts, Terms, and How RVO Applies Them

A self-contained conceptual guide to the domain RVO lives in: realtime, low-latency
systems. It defines the vocabulary, explains *why* each idea exists and what role it
plays, and grounds every term in a concrete RVO example so the abstraction is never
free-floating.

Read it top to bottom for a mental model, or jump to the [Glossary](#glossary) for a
quick lookup. RVO-specific framing of these same ideas, with file references, lives in
`DEVELOPER_GUIDE.md` (especially §5 design decisions and §6 caveats).

---

## 0. The one idea everything hangs off

> **In a low-latency system, the scarce resource is not CPU or memory — it is *time you can predict*.**

A request that usually takes 2 ms but occasionally takes 200 ms is often worse than one
that always takes 10 ms, because everything downstream must budget for the worst case.
Low-latency engineering is therefore mostly about **removing variance**, not lowering the
average. Almost every technique below — no GC, bounded queues, dropping instead of
blocking, off-loop slow work — exists to make the *distribution* of latency narrow and its
*tail* bounded.

RVO's invariant is the concrete form of this: **the live path must never wait for slow
work.**

---

## 1. Latency vs Throughput

- **Latency** — how long one operation takes, end to end. Measured in time (µs, ms).
- **Throughput** — how many operations complete per unit time (ops/s, fps, QPS).

They are different axes and often in tension. Batching raises throughput but adds latency
(you wait to fill the batch). Pipelining can raise both. A system can have high throughput
and terrible tail latency, or low latency and low throughput.

**Role:** decide which you are optimizing *before* you design. RVO optimizes **latency and
freshness**; throughput is capped deliberately (FPS gates) so the loop stays responsive.

> RVO: control loop ~972 Hz (throughput of the *scheduler*), but the design target is the
> per-tick latency staying ~1 ms so detectors run on schedule.

---

## 2. Tail latency (the heart of the field)

Averages lie. A mean of 5 ms can hide that 1% of requests take 500 ms. You quote
**percentiles**:

- **p50 (median)** — typical experience.
- **p95 / p99** — the slow minority; what most users hit *sometimes*.
- **p99.9 / p99.99** — the "tail." At scale, every user hits the tail regularly.

**Why the tail dominates at scale:** if a request fans out to *N* backends and waits for
all of them, its latency is the **max** of *N* samples. With N = 100 and each backend's
p99 = 10 ms, the probability that *at least one* is slow is ~63% — so the aggregate's
*median* is near the components' *p99*. This is **tail-at-scale amplification** (Dean &
Barroso, "The Tail at Scale").

**Coordinated omission** — a subtle measurement bug: if your load generator stops sending
while the system is stalled, it never records the latency of the requests that *would*
have arrived during the stall, so the measured p99 is far rosier than reality. Always
measure latency from the request's *intended* start time, not when the system got around
to it.

**Role:** percentiles + tail-aware design are the language of the field. "We improved
p99.9 from 40 ms to 8 ms" is a real claim; "we improved the average" usually isn't.

> RVO: the metric `avg exec µs` is an average — a known weakness. The honest upgrade is a
> latency **histogram** (e.g. HdrHistogram / Prometheus buckets) to report p50/p99 of
> detector execution. The gRPC fan-out to multiple model nodes is exactly a
> tail-at-scale situation — which is why each remote call is decoupled and TTL-bounded
> (§9) rather than awaited inline.

---

## 3. The fast-path / slow-path split

The single most important architectural pattern for low latency.

- **Fast path (hot path)** — the code that must be quick and predictable, run on every
  operation. Keep it free of allocation, locks held across I/O, blocking calls, and
  anything variable.
- **Slow path (cold path)** — anything slow, bursty, or best-effort: disk writes,
  encoding, network calls, downstream delivery. Push it *off* the hot path, behind a
  queue, onto another thread.

**Role:** isolate variance. The slow path can stall, retry, or fall behind without ever
touching the hot path's latency distribution.

> RVO: hot path = camera ingest → frame buffer → detector dispatch → signal write →
> event check. Cold path = clip encoding, file I/O, and — crucially — **remote model
> inference**, which runs on a worker thread so a slow model never blocks the ~1 ms tick.

---

## 4. Queueing theory you actually need

You don't need heavy math, but two results explain most capacity decisions.

### Little's Law

> **L = λ × W** — average number of items in a system = arrival rate × average time each
> spends in it.

Rearranged, `W = L / λ`. Use it to size queues and reason about latency: if 1000 req/s
arrive and each spends 50 ms in the system, there are ~50 in flight on average. If your
queue is smaller than that, you'll drop; if much larger, you're hiding latency in a
backlog.

### Utilization and the hockey stick

For a simple queue (M/M/1), mean response time ≈ **S / (1 − ρ)**, where `S` is service
time and `ρ` is **utilization** (fraction of capacity used). The curve is a hockey stick:

- ρ = 0.5 → latency ≈ 2× service time.
- ρ = 0.9 → latency ≈ 10× service time.
- ρ = 0.99 → latency ≈ 100× service time.

**Role:** this is *why* low-latency systems run at moderate utilization (often < 70%) and
shed load before saturation. Chasing 100% utilization trades a little efficiency for a
catastrophic latency tail.

> RVO: the tick budget is ~1 ms. A detector needing 3 ms can't fit, so the scheduler
> FPS-caps it and backs it off on overrun — keeping effective utilization of the loop low
> enough that detectors run on time. Capacity ≈ Σ(detector_fps × service_time) must stay
> under one core's budget.

---

## 5. Backpressure

**Backpressure** is how a system tells (or forces) an upstream producer to slow down when
a downstream consumer can't keep up. Without it, a fast producer + slow consumer = an
unbounded queue = growing latency and eventual OOM.

Three responses when a buffer fills:

1. **Block** — make the producer wait. Propagates slowness upstream; correct when you
   must not lose data, dangerous when the producer is realtime (a camera can't "wait").
2. **Drop (shed)** — discard and count. Correct when **freshness > completeness**.
3. **Buffer** — store for later. Only safe if *bounded*; an unbounded buffer just defers
   the failure and makes it worse.

**Role:** decide, per queue, which of the three is correct — and make every queue bounded.

> RVO: every handoff is a **bounded** `crossbeam` channel with `try_send` and **drop-and-
> count** (camera→scheduler cap 5, →events cap 64, →clips cap 8). A full queue increments
> a `*_drops_total` metric. "Drop-don't-block: a 1-second-old frame is worth less than a
> fresh one."

---

## 6. Load shedding & admission control

- **Admission control** — deciding *whether* to do a unit of work before starting it.
- **Load shedding** — deliberately *not* doing some work when overloaded, to protect the
  rest.
- **Graceful degradation** — under overload, the system does *less*, not *worse*; quality
  drops smoothly instead of the whole thing collapsing.

**Role:** an overloaded system that keeps accepting everything melts down (latency hockey
stick, then OOM). Shedding keeps the surviving work fast.

> RVO: detectors declare a **cost hint** (Low/Medium/High). When one overruns its
> FPS-budget interval (2× threshold), the scheduler applies **backoff** (Medium 100 ms /
> High 500 ms), skipping it rather than queuing missed runs. The observable is
> `detector_skips`. The identity `skips + execs = ticks × detectors` shows admission
> control is exhaustive — every slot is explicitly run or shed, never silently lost.

---

## 7. Freshness, staleness, and TTL

In realtime systems, **data has an expiry**. A detection that a person is present is only
meaningful for a short window; acting on a 5-second-old detection is a correctness bug,
not just a performance one.

- **TTL (time-to-live)** — how long a value stays valid after it's produced.
- **Staleness** — how old the data you're acting on is.

**Role:** bound staleness explicitly so the system never acts on outdated state. This also
lets you decouple producers and consumers safely — a lagging producer's data simply
expires instead of triggering wrong actions.

> RVO: every `Signal` carries `ts_ns + ttl_ns`; the `SignalStore` returns "absent" once
> expired. A remote detector stamps its cached result with `ts_ns = now − result_age`, so
> if the model worker falls behind, the signal expires naturally and events don't fire on
> stale inferences.

---

## 8. Determinism and the runtime (why language/runtime choice matters)

Latency variance often comes from *below* your code:

- **Garbage collection (GC)** — managed runtimes (Java, Go, Python, JS) periodically
  reclaim memory, sometimes "stop-the-world." A GC pause is unbounded variance injected at
  random — fatal for a 1 ms loop. This is the #1 reason low-latency systems use
  non-GC languages (C, C++, Rust) or heavily tuned/region-based allocators.
- **Allocation on the hot path** — even without GC, `malloc` can take a lock or hit the
  OS. Low-latency code pre-allocates and reuses (object pools, ring buffers, arenas) so
  the hot path does **zero allocation**.
- **Jitter sources** — OS scheduler preemption, page faults, interrupts, CPU frequency
  scaling, NUMA effects, cache misses. Mitigations: CPU pinning, huge pages, `mlock`,
  isolated cores, busy-polling instead of sleeping.

**Role:** predictable latency requires a predictable substrate. You can't p99-tune on top
of a runtime that randomly pauses.

> RVO: written in **Rust** — no GC, deterministic destruction at end of scope, ownership
> that enforces lock discipline at compile time. The hot path avoids per-tick allocation;
> trait objects (`Box<dyn DetectorNode>`) are allocated once at startup, not per call.

---

## 9. Concurrency models

How work is split across threads/cores, and how they coordinate.

- **Shared-memory + locks** — threads share state, guarded by `Mutex`/`RwLock`. Simple but
  lock contention and *holding a lock across slow work* are classic latency killers.
- **Message passing / shared-nothing** — threads own their state and communicate via
  channels (Actor model, CSP, Go channels). Avoids shared mutable state and most data
  races. Scales cleanly.
- **Lock-free / wait-free** — algorithms using atomics + CAS so no thread blocks another
  (ring buffers, `seqlock`). Lowest latency, highest difficulty.
- **Thread-per-core (share-nothing)** — pin one thread per core, partition data so cores
  never synchronize (Seastar, ScyllaDB, Redis-ish). Eliminates cross-core contention.
- **Async / event loop** — one thread multiplexes many tasks via non-blocking I/O
  (Tokio, Node). Powerful, but **blocking the event loop** (a sync call inside async)
  stalls *everything* — a top async footgun.

Key sub-terms:

- **Lock discipline** — never hold a lock across I/O, sleep, or an `await`. Keep critical
  sections tiny.
- **Atomics & memory ordering** — `Relaxed`, `Acquire`, `Release`, `AcqRel`, `SeqCst`
  control how memory operations are observed across threads. Stronger ordering = more
  correctness guarantees = more cost.
- **seqlock** — a reader/writer pattern: the writer bumps a version counter before
  (odd = write in progress) and after (even = done) updating; the reader reads version,
  data, version again, and retries if it changed or was odd. Cheap reads, no reader locks.
- **False sharing** — two unrelated atomics on the same 64-byte cache line cause cores to
  fight over it; fixed by padding/aligning to a cache line.
- **Head-of-line (HOL) blocking** — one slow item stuck at the front delays everything
  behind it (in a queue, a TCP connection, or a single-threaded loop).

**Role:** pick the model that matches your latency target and complexity budget.

> RVO: mostly **message passing** (bounded channels between threads) + a single
> `Arc<Mutex<FrameBuffer>>` with strict lock discipline (lock held only for a push/slice,
> never across the post-roll sleep). The `SignalStore` uses a **seqlock** version counter.
> The remote detector runs gRPC on its own thread so the tick loop never blocks. A known
> limitation: detectors run **sequentially** within a tick, so a slow one is HOL blocking
> within that tick (mitigated next tick by backoff).

---

## 10. Memory discipline

- **Bounded memory** — usage is a function of a *window* or fixed capacity, not of uptime
  or input volume. `O(window)`, not `O(uptime)`.
- **Ring / circular buffer** — fixed-size array that overwrites the oldest entry; O(1)
  push, no allocation, natural for "keep the last N."
- **Cache locality** — sequential, compact data is faster than pointer-chasing; a cache
  miss can cost ~100× an L1 hit.

**Role:** unbounded memory growth is a latency *and* availability bug (GC pressure, swap,
OOM). Bound it structurally.

> RVO: the `FrameBuffer` is a 300-slot ring (~10 s @ 30 fps), overwrite-oldest. Memory is
> a function of the window, never of how long the process has run.

---

## 11. The control-loop pattern

Many realtime systems are a **loop running at a fixed cadence** (a "tick"), rather than
purely event-driven. Each tick does a bounded amount of work and the loop targets a fixed
frequency.

- **Soft real-time** — missing a deadline degrades quality but isn't catastrophic (video,
  trading dashboards). 
- **Hard real-time** — missing a deadline is a failure (pacemakers, avionics); requires an
  RTOS and provable worst-case execution time.

**Role:** a fixed-cadence loop makes timing predictable and admission control natural (you
know how much budget each tick has).

> RVO: a ~1 kHz `tick()` loop. It's **soft** real-time — a late tick drops freshness, not
> safety. Hard-real-time guarantees would need an RTOS and WCET analysis, which RVO does
> not claim.

---

## 12. Reliability under load & failure isolation

Low latency is worthless if one slow dependency takes down everything.

- **Bulkhead** — isolate resources (thread pools, queues) per dependency so one failure
  can't exhaust the whole system (named after ship compartments).
- **Circuit breaker** — after N failures, stop calling a dead dependency for a while
  (fail fast) instead of piling up timeouts.
- **Timeout** — never wait unbounded on a dependency; bound every external call.
- **Retry** — re-attempt failed calls — but with **exponential backoff + jitter**, or you
  cause a **retry storm / thundering herd** that finishes off a struggling service.
- **Failure isolation / graceful degradation** — a failed component is removed; the system
  continues with reduced function.

**Role:** convert dependency failure into bounded, local degradation instead of global
collapse.

> RVO: each remote detector has a **per-RPC timeout**, tolerates transient failures, and
> flips to `Failed` only after a threshold — at which point the scheduler **disables** it
> and the rest of the pipeline keeps running (`detector_failures` counts it). That's a
> bulkhead + circuit-breaker-ish pattern: "kill a model service, the pipeline survives."

---

## 13. Observability

You cannot tune what you cannot measure.

- **SLI (Indicator)** — a measured quantity: latency p99, error rate, drop rate.
- **SLO (Objective)** — the target for an SLI: "p99 < 10 ms, 99.9% of the time."
- **SLA (Agreement)** — a contractual SLO with consequences.
- **Error budget** — `1 − SLO`. If your SLO is 99.9%, you have 0.1% to "spend" on failures
  before you must stop shipping risk.

Metric types:

- **Counter** — monotonic total (requests, drops). You rate it: `rate(x)`.
- **Gauge** — a current value (queue depth, in-flight count, temperature).
- **Histogram** — a distribution; **required** for latency, because you cannot average
  percentiles. (p99 of two machines ≠ average of their p99s.)

Two standard checklists:

- **RED** (request-driven services): **R**ate, **E**rrors, **D**uration.
- **USE** (resources): **U**tilization, **S**aturation, **E**rrors.

**Role:** define SLIs/SLOs, expose counters + histograms, and watch saturation (queue
depth, drops) as your early-warning signal.

> RVO: exposes Prometheus **counters** (`scheduler_ticks`, `detector_execs`,
> `detector_skips`, `detector_failures`, `events_emitted`, `*_drops_total`) on `/metrics`.
> SLI mapping: loop frequency (≥ target Hz), drop rate (< X%), detector failure budget.
> The honest gap: latency is currently a mean, not a histogram (see §2).

---

## 14. Capacity planning & scaling

- **Vertical scaling** — bigger machine (more cores/RAM). Simple, finite, diminishing.
- **Horizontal scaling** — more machines/instances. Needs the work to partition.
- **Shared-nothing** — instances share no mutable state, so they scale linearly with no
  coordination cost. The gold standard for horizontal scale.
- **Back-of-envelope capacity** — estimate before you load-test: `capacity ≈ resource /
  per-unit cost`. State assumptions; don't invent a QPS you didn't measure.
- **Fan-out bandwidth** — for a service that pushes data out: `rate × payload_size`.

**Role:** know your ceiling and your scaling axis before you're asked.

> RVO: **shared-nothing per camera** — one process per stream, no cross-instance state, so
> it scales linearly across cameras. Model services scale independently behind their gRPC
> endpoint (replicas + load balancer). Adding a model node is **O(1) on the hot path** (a
> worker + a channel), so it doesn't degrade loop latency. Per-node fan-out ≈
> `max_fps × frame_size` (e.g. 15 fps × ~30 KB ≈ 450 KB/s).

---

## 15. Networking for low latency

Relevant because RVO fans frames out over gRPC.

- **Connection setup cost** — TCP handshake + TLS adds round-trips. **Reuse persistent
  connections** (connection pooling) instead of dialing per request.
- **HTTP/1.1** — one in-flight request per connection (or fragile pipelining) → HOL
  blocking; clients open many connections.
- **HTTP/2** — **multiplexes** many streams over one connection (what gRPC uses). Removes
  application-level HOL, but a single TCP connection still has **TCP-level HOL** (a lost
  packet stalls all streams). **HTTP/3 / QUIC** (over UDP) fixes that.
- **gRPC + Protobuf vs REST + JSON** — binary framing + a compact schema → smaller
  payloads, cheaper (de)serialization, streaming, and codegen'd contracts. Lower and more
  predictable latency than text JSON over HTTP/1. The tradeoff: not human-readable, needs
  tooling.
- **Serialization cost** — encoding/decoding is real CPU on the hot path; binary formats
  and zero-copy parsing matter at scale.
- **Sync vs async I/O** — blocking a thread per connection limits concurrency; non-blocking
  I/O + an event loop scales to many connections, but you must never block that loop.

**Role:** the wire format and connection model are first-order latency decisions, not
afterthoughts.

> RVO: chose **gRPC over HTTP/2** with a **persistent, lazily-dialed channel** per model
> node (reused across calls, auto-reconnecting). The frame is JPEG-encoded once and sent
> as bytes. The gRPC client runs on a worker thread (async runtime) so the synchronous
> tick loop is never blocked on the network.

---

## 16. Common anti-patterns (what *not* to do)

- **Unbounded queues** — defer failure, then OOM. Always bound + decide drop/block.
- **Blocking the hot path / event loop** — one sync call (I/O, lock, sleep) inside the
  fast path or async executor stalls everything.
- **Holding a lock across slow work** — turns a brief critical section into a global stall.
- **Optimizing the average, ignoring the tail** — the tail is what users and downstreams
  actually feel.
- **Running at ~100% utilization** — the latency hockey stick; leave headroom, shed early.
- **Retry without backoff/jitter** — turns a blip into a self-inflicted DDoS.
- **Acting on stale data** — no TTL → correctness bugs masquerading as features.
- **Coordinated-omission measurement** — your benchmark says p99 = 3 ms; production says
  300 ms.

---

## 17. How RVO embodies the domain (one-screen map)

| Concept | RVO mechanism |
|---|---|
| Fast/slow path split | 1 kHz tick loop; encoding, I/O, inference off-loop |
| Bounded queues + backpressure | `crossbeam` bounded channels, `try_send`, drop-and-count |
| Load shedding | cost hints + overrun backoff; `detector_skips` |
| Freshness/TTL | `Signal.ts_ns + ttl_ns`; expiry at read time |
| No-GC determinism | Rust; once-at-startup allocation; ownership-enforced locks |
| Concurrency | message passing + one disciplined `Mutex`; seqlock store |
| Bounded memory | 300-slot ring `FrameBuffer` (~10 s) |
| Failure isolation | per-RPC timeout, threshold → `Failed` → detector disabled |
| Decoupled fan-out | remote detector = worker thread + persistent gRPC channel |
| Observability | Prometheus counters on `/metrics` (+ planned histograms) |
| Shared-nothing scale | one process per camera; O(1) hot-path node add |

---

## Glossary

- **Admission control** — deciding whether to start a unit of work before doing it.
- **Backpressure** — mechanism forcing/asking a producer to slow when the consumer lags.
- **Backoff (exponential)** — increasing wait between retries/attempts to avoid storms.
- **Bulkhead** — resource isolation so one failure can't sink the whole system.
- **Circuit breaker** — stop calling a failing dependency for a cooldown; fail fast.
- **Control loop / tick** — fixed-cadence execution of bounded work per cycle.
- **Coordinated omission** — under-measuring latency by not counting requests missed during a stall.
- **Cold/slow path** — non-time-critical work moved off the hot path.
- **Error budget** — `1 − SLO`; the allowable amount of failure.
- **False sharing** — cache-line contention between unrelated data.
- **Fan-out** — one request triggering many downstream calls.
- **Freshness** — how recent the data is; opposite of staleness.
- **Graceful degradation** — doing less under load, not failing entirely.
- **Hard/soft real-time** — deadline miss is catastrophic vs merely quality-reducing.
- **Head-of-line (HOL) blocking** — a stuck front item delaying everything behind it.
- **Histogram** — distribution metric; required to compute latency percentiles.
- **Hot/fast path** — the latency-critical code run on every operation.
- **Jitter** — variance in latency/timing.
- **Little's Law** — `L = λW`; items-in-system = arrival rate × time-in-system.
- **Load shedding** — intentionally dropping work under overload.
- **Lock discipline** — never hold a lock across slow work; keep critical sections tiny.
- **Lock-free / wait-free** — coordination without blocking, via atomics/CAS.
- **Memory ordering** — `Relaxed/Acquire/Release/AcqRel/SeqCst` visibility guarantees.
- **Percentile (p50/p99/p99.9)** — latency at a rank of the distribution.
- **RED / USE** — Rate-Errors-Duration (services) / Utilization-Saturation-Errors (resources).
- **Ring buffer** — fixed-size, overwrite-oldest circular buffer.
- **seqlock** — versioned read/write pattern for cheap, lock-free reads.
- **Shared-nothing** — instances share no mutable state; scale linearly.
- **SLI / SLO / SLA** — measured indicator / internal target / contractual target.
- **Stale data** — data older than its useful window.
- **Tail latency** — the high percentiles; what dominates at scale.
- **Tail-at-scale amplification** — fan-out makes aggregate latency track component tails.
- **Throughput** — operations completed per unit time.
- **TTL** — time-to-live; validity window of a value.
- **Thread-per-core** — pin one thread per core, partition data, avoid cross-core sync.
- **Utilization (ρ)** — fraction of capacity in use; latency explodes as ρ → 1.
- **Backpressure strategies** — block / drop / (bounded) buffer.

---

*Companion docs:* `DEVELOPER_GUIDE.md` (RVO's concrete design decisions and tradeoffs),
`ARCHITECTURE.md` (data flow). This file is the conceptual layer beneath both.
