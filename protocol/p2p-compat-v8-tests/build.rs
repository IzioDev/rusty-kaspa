use std::path::PathBuf;

fn main() {
    let mut config = prost_build::Config::new();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo");
    let descriptor_path = PathBuf::from(manifest_dir).join("src/p2p_v8.desc");
    config.file_descriptor_set_path(&descriptor_path);
    config.compile_protos(&["proto/p2p_v8.proto"], &["proto"]).unwrap();
}
