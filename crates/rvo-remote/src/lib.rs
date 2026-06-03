//! Remote gRPC detector.
//!
//! Wraps an external model service (any gRPC server implementing the
//! `rvo.detect.v1.Detector` contract) as an in-process [`DetectorNode`]. This
//! is how RVO fans camera frames out to external inference services (e.g.
//! Python YOLO / image-pipeline stubs) without coupling the orchestrator to a
//! specific model.
//!
//! # Why a background worker
//!
//! The scheduler times every `execute()` call and backs off detectors that
//! overrun their frame budget. Blocking the 1 ms tick loop on a network
//! round-trip would be the cardinal sin of a low-latency pipeline. So the gRPC
//! call lives on a dedicated worker thread with its own Tokio runtime and a
//! persistent HTTP/2 channel:
//!
//! ```text
//! scheduler tick (hot)          worker thread (its own runtime + channel)
//! ────────────────────          ─────────────────────────────────────────
//! execute(ctx):                 loop:
//!   publish newest frame   →      take newest frame, JPEG-encode
//!   read cached result     ←      Detect() over the persistent channel
//!   return signals (w/ ttl)       store (value, produced_at)
//! ```
//!
//! `execute()` never blocks: it hands the latest frame to the worker
//! (overwrite-newest, like the camera→buffer path) and returns the most recent
//! cached result. Inference lag is absorbed by **Signal TTL** — the result is
//! stamped with `ts_ns = now_ns - result_age`, so a stale answer simply expires
//! in the [`SignalStore`](rvo_signals::store::SignalStore) and events never fire
//! on old data.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use opencv::core::{Mat, Vector};
use opencv::imgcodecs;

use rvo_buffer::Frame;
use rvo_detector::detector::{
    DetectorContext, DetectorCostHint, DetectorHealth, DetectorMeta, DetectorNode, DetectorResult,
};
use rvo_signals::store::{Signal, SignalType};

/// Generated protobuf + gRPC bindings for `rvo.detect.v1`.
pub mod pb {
    tonic::include_proto!("rvo.detect.v1");
}

use pb::detector_client::DetectorClient;
use pb::DetectRequest;

/// Whether the worker may write diagnostics to stderr.
///
/// The TUI sets `RVO_REMOTE_SILENT` so a down service doesn't spam the
/// alternate screen; the plain CLI leaves it unset and keeps the logs.
fn stderr_enabled() -> bool {
    std::env::var_os("RVO_REMOTE_SILENT").is_none()
}

/// Consecutive RPC failures before the detector reports `Failed` health.
///
/// The scheduler permanently disables a `Failed` detector, so we tolerate
/// transient blips and only give up once a service is durably unreachable —
/// which is exactly the "kill the service, pipeline survives" resilience story.
const FAILURE_THRESHOLD: u32 = 5;

/// State shared between the scheduler-thread `execute()` and the worker thread.
struct Shared {
    /// Newest frame awaiting dispatch (overwrite-newest; `None` once consumed).
    inbox: Mutex<Option<Frame>>,
    /// Most recent detection result, or `None` until the first reply arrives.
    result: Mutex<Option<ResultCell>>,
    /// Consecutive failed/timed-out RPCs; reset to 0 on any success.
    consecutive_failures: AtomicU32,
    /// Set on drop so the worker loop can exit instead of leaking.
    stop: AtomicBool,
}

struct ResultCell {
    /// `Some(value)` if the service reported the target signal this round,
    /// `None` if it was absent (lets the prior signal expire via TTL).
    value: Option<u64>,
    /// Wall-clock instant the reply was received, used to age the signal.
    produced_at: Instant,
}

/// A [`DetectorNode`] backed by a remote gRPC model service.
pub struct RemoteGrpcDetector {
    meta: DetectorMeta,
    output_signal: SignalType,
    ttl_ns: u64,
    shared: Arc<Shared>,
    _worker: JoinHandle<()>,
}

impl RemoteGrpcDetector {
    /// Build a remote detector and start its worker.
    ///
    /// - `id`: stable detector id (leaked to `'static`, matching the codebase's
    ///   existing dynamic-detector pattern).
    /// - `endpoint`: gRPC target, e.g. `http://localhost:50051`.
    /// - `output_signal`: which [`SignalType`] this service produces.
    /// - `max_fps`: cap on dispatch rate (the worker honours it; the scheduler
    ///   also gates on it).
    /// - `timeout_ms`: per-RPC timeout.
    /// - `ttl_ns`: freshness window applied to emitted signals.
    pub fn new(
        id: impl Into<String>,
        endpoint: impl Into<String>,
        output_signal: SignalType,
        max_fps: f64,
        timeout_ms: u64,
        ttl_ns: u64,
    ) -> Self {
        let id: &'static str = Box::leak(id.into().into_boxed_str());
        let output_signals: &'static [SignalType] =
            Box::leak(vec![output_signal].into_boxed_slice());

        let meta = DetectorMeta {
            id,
            max_fps,
            dependencies: &[],
            output_signals,
            // Remote inference is the expensive path; let the scheduler shed it
            // under load.
            cost_hint: DetectorCostHint::High,
            requires_frame: true,
        };

        let shared = Arc::new(Shared {
            inbox: Mutex::new(None),
            result: Mutex::new(None),
            consecutive_failures: AtomicU32::new(0),
            stop: AtomicBool::new(false),
        });

        let endpoint = endpoint.into();
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name(format!("rvo-remote/{}", id))
            .spawn(move || run_worker(endpoint, output_signal, max_fps, timeout_ms, worker_shared))
            .expect("spawn remote detector worker");

        Self {
            meta,
            output_signal,
            ttl_ns,
            shared,
            _worker: worker,
        }
    }
}

impl Drop for RemoteGrpcDetector {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl DetectorNode for RemoteGrpcDetector {
    fn meta(&self) -> DetectorMeta {
        self.meta
    }

    fn execute(&mut self, ctx: &DetectorContext<'_>) -> DetectorResult {
        // Publish the newest frame to the worker (non-blocking, overwrite-newest).
        if let Some(frame) = ctx.frame {
            *self.shared.inbox.lock().unwrap() = Some(frame.clone());
        }

        // Durable unreachability -> Failed (scheduler disables us).
        if self.shared.consecutive_failures.load(Ordering::Relaxed) >= FAILURE_THRESHOLD {
            return DetectorResult {
                signals: Vec::new(),
                health: DetectorHealth::Failed,
            };
        }

        // Return the most recent cached result, aged so the store's TTL check
        // expires it naturally if the worker has fallen behind.
        let signals = match self.shared.result.lock().unwrap().as_ref() {
            Some(cell) => match cell.value {
                Some(value) => {
                    let age_ns = cell.produced_at.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    vec![Signal {
                        signal_type: self.output_signal,
                        value,
                        ts_ns: ctx.now_ns.saturating_sub(age_ns),
                        ttl_ns: self.ttl_ns,
                    }]
                }
                None => Vec::new(),
            },
            None => Vec::new(),
        };

        DetectorResult {
            signals,
            health: DetectorHealth::Ok,
        }
    }
}

/// Worker loop: owns the Tokio runtime and the persistent gRPC channel.
fn run_worker(
    endpoint: String,
    output_signal: SignalType,
    max_fps: f64,
    timeout_ms: u64,
    shared: Arc<Shared>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("[REMOTE] failed to build runtime for {endpoint}: {err}");
            return;
        }
    };

    let log = stderr_enabled();

    rt.block_on(async move {
        // `connect_lazy` never blocks construction; tonic dials on first use and
        // reconnects transparently, so a service that is down at startup or
        // bounces mid-run is handled without tearing down the detector.
        let channel = match tonic::transport::Channel::from_shared(endpoint.clone()) {
            Ok(ep) => ep.connect_lazy(),
            Err(err) => {
                if log {
                    eprintln!("[REMOTE] invalid endpoint {endpoint}: {err}");
                }
                return;
            }
        };
        let mut client = DetectorClient::new(channel);

        let want = signal_type_name(output_signal);
        let min_interval = Duration::from_secs_f64(1.0 / max_fps.max(0.1));
        let timeout = Duration::from_millis(timeout_ms);

        while !shared.stop.load(Ordering::Relaxed) {
            let frame = shared.inbox.lock().unwrap().take();

            if let Some(frame) = frame {
                // Capture the source frame's timestamp to measure true
                // end-to-end remote latency (capture → reply).
                let captured_ts = frame.ts;
                match encode_jpeg(&frame.image) {
                    Ok(bytes) => {
                        let req = tonic::Request::new(DetectRequest {
                            frame_jpeg: bytes,
                            frame_id: frame.id,
                            ts_ns: 0,
                        });

                        match tokio::time::timeout(timeout, client.detect(req)).await {
                            Ok(Ok(resp)) => {
                                shared.consecutive_failures.store(0, Ordering::Relaxed);
                                let latency_ns =
                                    captured_ts.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                                rvo_metrics::METRICS.remote_latency_ns.record_ns(latency_ns);
                                let value = resp
                                    .into_inner()
                                    .signals
                                    .into_iter()
                                    .find(|s| s.signal_type == want)
                                    .map(|s| s.value);
                                *shared.result.lock().unwrap() = Some(ResultCell {
                                    value,
                                    produced_at: Instant::now(),
                                });
                            }
                            Ok(Err(status)) => {
                                shared.consecutive_failures.fetch_add(1, Ordering::Relaxed);
                                if log {
                                    eprintln!("[REMOTE] {endpoint} rpc error: {}", status.code());
                                }
                            }
                            Err(_) => {
                                shared.consecutive_failures.fetch_add(1, Ordering::Relaxed);
                                if log {
                                    eprintln!("[REMOTE] {endpoint} timed out after {timeout_ms}ms");
                                }
                            }
                        }
                    }
                    Err(err) => {
                        if log {
                            eprintln!("[REMOTE] jpeg encode failed: {err}");
                        }
                    }
                }
            }

            tokio::time::sleep(min_interval).await;
        }
    });
}

/// Encode an OpenCV `Mat` to JPEG bytes (default quality).
fn encode_jpeg(img: &Mat) -> opencv::Result<Vec<u8>> {
    let mut buf = Vector::<u8>::new();
    let params = Vector::<i32>::new();
    imgcodecs::imencode(".jpg", img, &mut buf, &params)?;
    Ok(buf.to_vec())
}

/// Map a [`SignalType`] to the wire string used in the proto contract.
///
/// Must stay in sync with the names accepted by `rvo-config` / `rvo-bin`.
fn signal_type_name(s: SignalType) -> &'static str {
    match s {
        SignalType::Dummy => "Dummy",
        SignalType::MotionLevel => "MotionLevel",
        SignalType::FacePresent => "FacePresent",
        SignalType::PersonDetected => "PersonDetected",
    }
}
