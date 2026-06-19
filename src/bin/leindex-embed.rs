// leindex-embed worker binary — root-crate mirror for cargo install.
//
// VAL-CARGO-005: `cargo install leindex --features onnx` must place BOTH
// the `leindex` and `leindex-embed` binaries in the install directory.
// Because `cargo install <pkg>` only installs `[[bin]]` targets declared
// in `<pkg>`'s own `Cargo.toml` (it does NOT install binaries from path
// dependencies or workspace members), the root leindex crate mirrors the
// worker binary here as a thin wrapper around `leindex_embed::worker_main::run()`.
//
// The actual worker logic lives in the `leindex-embed` library crate
// (`crates/leindex-embed/src/worker_main.rs`) so there is a single source
// of truth shared by both this wrapper and the subcrate's own binary at
// `crates/leindex-embed/src/bin/leindex-embed.rs`.
//
// This target is gated by `required-features = ["onnx"]` so that default
// installs (`cargo install leindex` without neural support) are unaffected;
// the worker is only installed when the user opts into neural embeddings.

fn main() {
    leindex_embed::worker_main::run()
}
