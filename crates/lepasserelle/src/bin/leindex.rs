// LeIndex CLI Binary
//
// Main entry point for the leindex command-line tool.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    lepasserelle::cli::main().await
}
