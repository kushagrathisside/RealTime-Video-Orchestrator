#!/usr/bin/env bash
# Generate the Python gRPC stubs from the shared proto into demo/services/.
set -euo pipefail

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

python -m grpc_tools.protoc \
  -I "${DEMO_DIR}/proto" \
  --python_out="${DEMO_DIR}/services" \
  --grpc_python_out="${DEMO_DIR}/services" \
  "${DEMO_DIR}/proto/detector.proto"

echo "[gen] wrote detector_pb2.py and detector_pb2_grpc.py to ${DEMO_DIR}/services"
