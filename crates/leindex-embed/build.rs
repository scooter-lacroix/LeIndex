fn main() {
    // Add rpath $ORIGIN so leindex-embed finds libonnxruntime.so next to itself
    #[cfg(feature = "onnx")]
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}
