fn main() {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/bridge_test.proto"], &["proto"])
        .expect("failed to compile test protos");
}
