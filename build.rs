fn main() {
    // Only check for model files if ONNX feature is enabled
    #[cfg(feature = "onnx")]
    {
        println!("cargo:rerun-if-changed=build.rs");

        let model_dir = std::path::Path::new("models");
        let embed_model = model_dir.join("qwen3-embed-0.6b.onnx");
        let tokenizer = model_dir.join("tokenizer.json");

        if !embed_model.exists() {
            panic!(
                "ONNX model file not found: {}. \
                 Download it from https://huggingface.co/n24q02m/Qwen3-Embedding-0.6B-ONNX \
                 and place it in the models/ directory. \
                 See docs/R15_MODEL_DISTRIBUTION.md for details.",
                embed_model.display()
            );
        }

        if !tokenizer.exists() {
            panic!(
                "Tokenizer file not found: {}. \
                 Download it from https://huggingface.co/Qwen/Qwen3-Embedding-0.6B \
                 and place it in the models/ directory. \
                 See docs/R15_MODEL_DISTRIBUTION.md for details.",
                tokenizer.display()
            );
        }

        println!("cargo:rerun-if-changed={}", embed_model.display());
        println!("cargo:rerun-if-changed={}", tokenizer.display());
    }
}
