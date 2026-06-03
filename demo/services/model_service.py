#!/usr/bin/env python3
"""Trivial gRPC "model" service for the RVO demo.

This is intentionally NOT a real model. It implements the rvo.detect.v1.Detector
contract with a deterministic image pipeline so the demo runs anywhere with no
ML dependencies, and so RVO's orchestration (not model accuracy) is what's on
display. Swapping in a real model later is just a different `Detect` body behind
the same proto.

Pipeline: decode JPEG -> resize -> mean brightness -> emit the configured signal
when brightness crosses a threshold. Cover/uncover the camera to drive it.

Run two instances to stand in for two model microservices:
    python model_service.py --port 50051 --signal PersonDetected --threshold 50
    python model_service.py --port 50052 --signal FacePresent   --threshold 60
"""

import argparse
import os
import sys
import time
from concurrent import futures

import cv2
import grpc
import numpy as np

# Generated stubs live next to this file (see gen_protos.sh / run_demo.sh).
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import detector_pb2  # noqa: E402
import detector_pb2_grpc  # noqa: E402


class DetectorServicer(detector_pb2_grpc.DetectorServicer):
    def __init__(self, signal: str, threshold: float, ttl_ms: int,
                 latency_ms: int, always: bool):
        self.signal = signal
        self.threshold = threshold
        self.ttl_ns = ttl_ms * 1_000_000
        self.latency_ms = latency_ms
        self.always = always

    def Detect(self, request, context):
        # Optional artificial latency to demo RVO's load-shedding / backoff.
        if self.latency_ms:
            time.sleep(self.latency_ms / 1000.0)

        present = self.always
        if not present:
            arr = np.frombuffer(request.frame_jpeg, dtype=np.uint8)
            img = cv2.imdecode(arr, cv2.IMREAD_COLOR)
            if img is not None:
                resized = cv2.resize(img, (320, 240))
                gray = cv2.cvtColor(resized, cv2.COLOR_BGR2GRAY)
                brightness = float(gray.mean())
                present = brightness >= self.threshold

        resp = detector_pb2.DetectResponse()
        if present:
            resp.signals.append(detector_pb2.SignalOut(
                signal_type=self.signal, value=1, ttl_ns=self.ttl_ns))
        return resp


def main():
    p = argparse.ArgumentParser(description="RVO demo stub model service")
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--signal", required=True,
                   choices=["Dummy", "MotionLevel", "FacePresent", "PersonDetected"])
    p.add_argument("--threshold", type=float, default=50.0,
                   help="mean-brightness threshold (0..255) to emit the signal")
    p.add_argument("--ttl-ms", type=int, default=1000)
    p.add_argument("--latency-ms", type=int, default=0,
                   help="artificial per-request delay to demo load-shedding")
    p.add_argument("--always", action="store_true",
                   help="always emit the signal (ignore the image)")
    args = p.parse_args()

    server = grpc.server(futures.ThreadPoolExecutor(max_workers=4))
    detector_pb2_grpc.add_DetectorServicer_to_server(
        DetectorServicer(args.signal, args.threshold, args.ttl_ms,
                         args.latency_ms, args.always),
        server,
    )
    server.add_insecure_port(f"[::]:{args.port}")
    server.start()
    print(f"[stub:{args.signal}] listening on :{args.port} "
          f"(threshold={args.threshold}, latency={args.latency_ms}ms, "
          f"always={args.always})", flush=True)
    try:
        server.wait_for_termination()
    except KeyboardInterrupt:
        server.stop(0)


if __name__ == "__main__":
    main()
