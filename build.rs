fn main() {
    #[cfg(feature = "onnx")]
    {
        println!("cargo:rerun-if-changed=build.rs");

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
