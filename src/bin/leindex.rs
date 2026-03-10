// LeIndex CLI Binary
//
// Main entry point for the leindex command-line tool.

use leindex::cli::cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::main().await
}
