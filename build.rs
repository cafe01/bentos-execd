fn main() {
    prost_build::Config::new()
        .compile_protos(&["exec_wire.proto"], &["."])
        .expect("Failed to compile exec_wire.proto");
}
