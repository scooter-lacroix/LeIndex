// LeIndex CLI Binary
//
// Main entry point for the leindex command-line tool.

use leindex::cli::cli;

// When the `memprof` feature is enabled, use jemalloc with heap profiling
// support as the global allocator. This allows engineers to generate detailed
// heap profiles via `MALLOC_CONF` environment variables without affecting
// default builds or CI.
//
// VAL-MEASURE-025: memprof is opt-in and build-time gated.
// VAL-MEASURE-026: Building with --features memprof succeeds.
#[cfg(feature = "memprof")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::main().await
}
