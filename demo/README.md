# RVO end-to-end demo — camera → RVO → gRPC model services

This demo shows RVO reading a camera and **fanning each frame out to external
model services over gRPC**, turning their replies into signals, firing events,
and writing evidence clips.

```
 webcam ──▶ RVO (rvo-bin) ──gRPC──▶ model_service.py :50051  (PersonDetected)
                  │         └─gRPC──▶ model_service.py :50052  (FacePresent)
                  ▼
           events.jsonl + clips/demo/   +   metrics on :9090
```

## The model services are intentionally trivial

`services/model_service.py` is **not** a real model. It implements the
`rvo.detect.v1.Detector` gRPC contract with a deterministic image pipeline
(decode JPEG → resize → mean brightness → emit the signal above a threshold).
This keeps the demo dependency-light and deterministic, and keeps the spotlight
on RVO's orchestration rather than model accuracy. Swapping in a real model
later is just a different `Detect` body behind the same proto.

## Run it

```bash
pip install -r demo/requirements.txt
bash demo/run_demo.sh          # codegens stubs, starts both services, runs RVO
```

Cover/uncover the webcam to cross the brightness threshold and drive the
signals. In other terminals:

```bash
curl http://127.0.0.1:9090/metrics   # detector_execs, events_emitted, detector_skips…
tail -f events.jsonl                 # events with confidence
ls clips/demo/                       # per-event JPEG frames + meta.json
```

## Things to demonstrate (the orchestration payoff)

- **Non-blocking inference.** The gRPC call runs on a worker thread; the
  scheduler tick never blocks on the network. See
  `crates/rvo-remote/src/lib.rs`.
- **Load-shedding.** Add latency to a service and watch `detector_skips` climb:
  ```bash
  python demo/services/model_service.py --port 50051 --signal PersonDetected --latency-ms 400
  ```
- **Resilience.** Kill a service mid-run; after a few failed RPCs the detector
  flips to `Failed`, the scheduler disables it, and the pipeline keeps running.
- **TTL staleness.** Stop sending frames and the last signal expires, so events
  stop firing — RVO never acts on stale detections.

## Browser dashboard (rvo-web)

For a point-and-click view — and to *add model nodes live from the browser* —
run the web POC instead of (or alongside) the CLI:

```bash
RVO_CONFIG=config/rvo-remote.yaml cargo run -p rvo-bin --bin rvo-web
# open http://127.0.0.1:8080
```

The page shows signals lighting up, metrics, nodes, and recent events. Type a
gRPC endpoint + the signal it produces and click **Add node** to plug a model
into the running pipeline with no restart. On a webcam-less host, set
`camera.source_uri` to a video file so frames flow.

## CLI alternative (no config file)

```bash
cargo run -p rvo-bin -- --list-cameras
cargo run -p rvo-bin -- \
  --camera-device 0 \
  --detector http://localhost:50051=PersonDetected \
  --detector http://localhost:50052=FacePresent \
  --clips-dir clips/demo
```

(The CLI augments the camera and adds detectors; events still come from the
config file.)
