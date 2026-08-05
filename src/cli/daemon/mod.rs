//! User-scoped daemon infrastructure: endpoint discovery, hard single-winner
//! startup lock, and protocol-version handshake.
//!
//! This module provides the building blocks for the `leindexd` daemon and the
//! thin stdio shim that forwards MCP/JSON-RPC frames to it (spec §4.1, §4.2).
//! The daemon module itself is NOT feature-gated: the types it defines
//! (`Handshake`, `DaemonEndpoint`, `StartupOutcome`) are used by both the inline
//! server path and the daemon-client path. The actual shim forwarder and spawn
//! helpers that pull in tokio/UnixStream dependencies are feature-flagged under
//! `daemon-client` in later tasks.

/// Daemon endpoint discovery + liveness checking.
pub mod endpoint;
/// Protocol-version handshake (spec §12.1).
pub mod handshake;
/// Stdio shim forwarder: connects to `leindexd` and byte-faithfully proxies
/// MCP/JSON-RPC frames between stdin/stdout and the daemon Unix socket
/// (spec §4.1).
#[cfg(feature = "daemon-client")]
pub mod shim;
/// Daemon spawn helper (creates `leindexd` when the shim wins the startup race).
#[cfg(feature = "daemon-client")]
pub mod spawn;
