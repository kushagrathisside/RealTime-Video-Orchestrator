#!/usr/bin/env bash
set -euo pipefail

SDK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== Testing Go SDK ==="
cd "${SDK_DIR}/go"
go build ./...
echo "Go SDK compiled successfully."

echo "=== Testing Node SDK ==="
cd "${SDK_DIR}/node"
# Install dependencies if not present (helpful for local runs)
npm install
# Typecheck the generated TypeScript code
npx tsc --noEmit detector.ts
echo "Node SDK typechecked successfully."

echo "=== Testing Python SDK ==="
cd "${SDK_DIR}/python"
# Install dependencies to ensure correct protobuf runtime
pip install -r requirements.txt --upgrade --quiet
# Quick syntax and import check
python3 -c "import detector_pb2, detector_pb2_grpc; print('Python SDK imported successfully.')"


echo "=== All SDK checks passed! ==="
