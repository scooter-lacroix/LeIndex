fn main() {
    // ONNX Runtime is loaded dynamically at runtime via ort::init_from() in the
    // ort_discovery module. No $ORIGIN rpath or build-time ORT linking is needed;
    // the worker dlopens libonnxruntime from the discovered path before creating
    // any Session.
    //
    // Intentionally empty: the `onnx`/`onnx-migraphx` features only change which
    // execution-provider bindings are compiled; neither requires ORT at build time.
}
