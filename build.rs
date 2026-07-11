fn main() {
    // ONNX Runtime is loaded dynamically at runtime via ort::init_from() in the
    // ort_discovery module. No $ORIGIN rpath or build-time ORT linking is needed.
    #[cfg(feature = "onnx")]
    {
        println!("cargo:rerun-if-changed=build.rs");
    }
}
