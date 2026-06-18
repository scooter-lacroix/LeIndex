fn main() {
    // ONNX Runtime is loaded dynamically at runtime via ort::init_from() in the
    // ort_discovery module. No $ORIGIN rpath or build-time ORT linking is needed.
    #[cfg(feature = "onnx")]
    {
        println!("cargo:rerun-if-changed=build.rs");

        // Surface warnings when bundle-time model files are missing so the
        // developer knows they will need to be downloaded before first use.
        let model_dir = std::path::Path::new("models");
        let embed_model = model_dir.join("qwen3-embed-0.6b.onnx");
        let tokenizer = model_dir.join("tokenizer.json");

        println!("cargo:rerun-if-changed={}", embed_model.display());
        if !embed_model.exists() {
            println!("cargo:warning=ONNX model file not found: {} — model will need to be downloaded before use", embed_model.display());
        }

        println!("cargo:rerun-if-changed={}", tokenizer.display());
        if !tokenizer.exists() {
            println!("cargo:warning=Tokenizer file not found: {} — tokenizer will need to be downloaded before use", tokenizer.display());
        }
    }
}
