// Codegen the gRPC client (and server, used by tests) from detector.proto.
//
// We point `tonic-build` at a vendored `protoc` binary so the build needs no
// system protobuf compiler — keeps local dev and CI hermetic.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    tonic_build::compile_protos("proto/detector.proto")?;
    Ok(())
}
