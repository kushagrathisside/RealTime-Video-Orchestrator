# RVO Developer SDK

This directory contains the auto-generated gRPC SDK bindings for the RVO Detector service.

**Source of Truth:**
```text
crates/rvo-remote/proto/detector.proto
```

## How to Update the SDKs

The SDKs (Go, Python, TypeScript) are generated automatically from the `.proto` file. **You do not need to manually install `protoc` or run individual generation commands.**

### 1. The Automated CI Way (Recommended)
Simply edit `detector.proto` and open a Pull Request. 
The GitHub Actions CI pipeline will automatically compile the new SDK files and commit them directly to your PR branch to guarantee they are perfectly in sync.

### 2. The Local Script Way
If you prefer to generate them locally before pushing, use the unified generation script:

```bash
# From the repository root
./scripts/generate_sdks.sh
```

This script will use `npx` to fetch the exact `protoc` binaries required and compile all three languages deterministically.

## Directory Structure

* `sdk/go/` - Go module (`go.mod`) containing `detector.pb.go` and `detector_grpc.pb.go`.
* `sdk/node/` - Node package (`package.json`) containing the TypeScript bindings (`detector.ts`).
* `sdk/python/` - Python package (`requirements.txt`) containing `detector_pb2.py` and `detector_pb2_grpc.py`.

## Testing

To verify that the generated SDKs compile correctly in their native languages, run:
```bash
./sdk/test_sdks.sh
```
