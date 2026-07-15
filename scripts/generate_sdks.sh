#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROTO_DIR="${ROOT_DIR}/crates/rvo-remote/proto"
SDK_DIR="${ROOT_DIR}/sdk"

echo "[sdk] Generating Python stubs..."
python3 -m grpc_tools.protoc \
  -I "${PROTO_DIR}" \
  --python_out="${SDK_DIR}/python" \
  --grpc_python_out="${SDK_DIR}/python" \
  "${PROTO_DIR}/detector.proto"

echo "[sdk] Generating Go stubs..."
# Requires protoc-gen-go and protoc-gen-go-grpc
protoc \
  -I "${PROTO_DIR}" \
  --go_out="${SDK_DIR}/go" --go_opt=paths=source_relative \
  --go-grpc_out="${SDK_DIR}/go" --go-grpc_opt=paths=source_relative \
  "${PROTO_DIR}/detector.proto"

echo "[sdk] Generating TypeScript/Node stubs..."
# We assume the user has ts-proto installed, or we just rely on standard generation if available.
# Actually, the PR author used ts-proto based on the header of detector.ts.
# We'll use npx ts-proto in the node directory if available.
cd "${SDK_DIR}/node"
npm i -D ts-proto || true
npx protoc \
  --plugin=protoc-gen-ts_proto=./node_modules/.bin/protoc-gen-ts_proto \
  --ts_proto_out="${SDK_DIR}/node" \
  --ts_proto_opt=outputServices=grpc-js,env=node,esModuleInterop=true \
  -I "${PROTO_DIR}" \
  "${PROTO_DIR}/detector.proto"

echo "[sdk] Done."
