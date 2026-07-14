// Generate the test-only gRPC server from greeter.proto WITHOUT protoc:
// protox (pure-Rust) compiles the .proto to a FileDescriptorSet, and tonic-build
// turns that into server code. Only the integration test uses the output.
fn main() {
    let proto = "tests/greeter.proto";
    println!("cargo:rerun-if-changed={proto}");
    let fds = match protox::compile([proto], ["tests"]) {
        Ok(fds) => fds,
        Err(e) => {
            // Don't fail non-test builds if the proto is absent for any reason.
            println!("cargo:warning=protox: {e}");
            return;
        }
    };
    tonic_build::configure()
        .build_client(false)
        .build_server(true)
        .compile_fds(fds)
        .expect("tonic-build codegen");
}
