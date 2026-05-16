//! Small repo fixture - main entry point.
//! This is a deterministic fixture for memory measurement.

mod models;
mod handlers;
mod utils;

use anyhow::Result;

fn main() -> Result<()> {
    println!("small-repo-fixture: hello");
    Ok(())
}
