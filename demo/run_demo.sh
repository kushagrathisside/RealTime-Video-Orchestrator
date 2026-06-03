#!/usr/bin/env bash
# End-to-end demo: two stub gRPC model services + RVO reading a camera and
# fanning frames out to them over gRPC.
#
#   pip install -r demo/requirements.txt
#   bash demo/run_demo.sh
#
# Then cover/uncover the webcam to drive the signals. Observe:
#   curl http://127.0.0.1:9090/metrics
#   tail -f events.jsonl
#   ls clips/demo/
set -euo pipefail

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${DEMO_DIR}/.." && pwd)"

# 1. Codegen the Python stubs.
bash "${DEMO_DIR}/gen_protos.sh"

# 2. Start the two stub model services.
python "${DEMO_DIR}/services/model_service.py" --port 50051 --signal PersonDetected --threshold 50 &
PID_A=$!
python "${DEMO_DIR}/services/model_service.py" --port 50052 --signal FacePresent --threshold 60 &
PID_B=$!

cleanup() { kill "${PID_A}" "${PID_B}" 2>/dev/null || true; }
trap cleanup EXIT

# Give the services a moment to bind their ports.
sleep 1

# 3. Run RVO against the demo config (device 0). Override the camera here if
#    needed, e.g. --camera-uri rtsp://… or --camera-device 1.
cd "${ROOT_DIR}"
RVO_CONFIG="${ROOT_DIR}/config/rvo-remote.yaml" cargo run -p rvo-bin -- \
  --config "${ROOT_DIR}/config/rvo-remote.yaml"
