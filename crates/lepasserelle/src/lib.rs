// lepasserelle - Bridge & Integration
//
// *La Passerelle* (The Bridge) - PyO3 FFI bindings and unified MCP tool

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

pub mod bridge;
pub mod memory;
pub mod mcp;

pub use bridge::{RustAnalyzer, build_weighted_context};
pub use memory::{MemoryManager, MemoryConfig};
pub use mcp::{LeIndexDeepAnalyze, AnalysisResult};

/// Bridge library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}

// Python module (only when building as extension module)
#[cfg(feature = "python-bindings")]
use pyo3::prelude::*;

/// Python module definition
#[cfg(feature = "python-bindings")]
#[pymodule]
fn leindex_rust(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<RustAnalyzer>()?;
    m.add_function(wrap_pyfunction!(build_weighted_context, m)?)?;
    m.add_class::<MemoryManager>()?;
    Ok(())
}
