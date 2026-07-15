# RVO Developer SDK

This directory contains generated gRPC SDK bindings for the RVO Detector service.

Proto source:

```text
crates/rvo-remote/proto/detector.proto
Generation Environment
Generated using:
protoc: 35.1
Python: 3.13.7
Node.js: v22.20.0
Go: go1.26.5

Python SDK

Install dependencies:
python3 -m pip install grpcio grpcio-tools

Generate:

python3 -m grpc_tools.protoc \
-I crates/rvo-remote/proto \
--python_out=sdk/python \
--grpc_python_out=sdk/python \
crates/rvo-remote/proto/detector.proto

Generated files:

sdk/python/
├── detector_pb2.py
└── detector_pb2_grpc.py

TypeScript SDK

Install generator:
npm install --save-dev ts-proto

Generate:

protoc \
-I crates/rvo-remote/proto \
--plugin=./node_modules/.bin/protoc-gen-ts_proto \
--ts_proto_out=sdk/node \
--ts_proto_opt=outputServices=grpc-js \
crates/rvo-remote/proto/detector.proto
Generated files:
sdk/node/
└── detector.ts

Go SDK
Install generators:

go install google.golang.org/protobuf/cmd/protoc-gen-go@latest
go install google.golang.org/grpc/cmd/protoc-gen-go-grpc@latest

Generate:

protoc \
-I crates/rvo-remote/proto \
--go_out=sdk/go \
--go-grpc_out=sdk/go \
--go_opt=paths=source_relative \
--go-grpc_opt=paths=source_relative \
crates/rvo-remote/proto/detector.proto
Generated files:
sdk/go/
├── detector.pb.go
└── detector_grpc.pb.go

---

