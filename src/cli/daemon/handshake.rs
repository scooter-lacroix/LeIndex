//! Protocol-version handshake (spec §12.1).
//!
//! Spec §12.1: the handshake carries LeIndex version, daemon protocol version,
//! artifact format version, worker protocol version, and capabilities. Needed
//! before shim↔daemon comms so a client built for one protocol version can
//! detect incompatibility and emit an actionable error (spec §4.1: "reject
//! incompatible versions with actionable instructions") instead of silently
//! failing.

use serde::{Deserialize, Serialize};

/// Daemon protocol version. Bumped on every breaking wire-protocol change
/// (e.g., new framing, new required handshake field, new request shape). When
/// the client and daemon values differ, the shim must refuse to forward
/// (spec §12.1).
pub const DAEMON_PROTOCOL_VERSION: u32 = 1;

/// Artifact format version. Bumped when the on-disk `.leindex/` layout changes
/// in a backward-incompatible way (e.g., generation manifest magic, layer
/// encoding, CAS header version). A mismatch means the daemon cannot safely
/// read artifacts produced by the client's build and vice versa.
pub const ARTIFACT_FORMAT_VERSION: u32 = 1;

/// Wire handshake exchange between a client (shim) and a daemon (or vice
/// versa). All fields are checked by [`Handshake::validate_against`] to decide
/// compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Handshake {
    /// SemVer string of the LeIndex build sending the handshake
    /// (`CARGO_PKG_VERSION` at compile time). Informational: the numeric
    /// versions below are authoritative for compatibility.
    pub leindex_version: String,
    /// Must equal [`DAEMON_PROTOCOL_VERSION`]. A mismatch fails with
    /// [`HandshakeError::ProtocolMismatch`].
    pub daemon_protocol_version: u32,
    /// Must equal [`ARTIFACT_FORMAT_VERSION`]. A mismatch fails with
    /// [`HandshakeError::ArtifactMismatch`].
    pub artifact_format_version: u32,
    /// Worker protocol version (embed worker IPC). Checked for informational
    /// logging only — the embed worker has its own compatibility negotiation —
    /// but carried in the handshake so the client can refuse to talk to a
    /// daemon whose worker protocol it does not understand at all.
    pub worker_protocol_version: u32,
    /// Capability strings (e.g. `"streaming-index"`, `"generation-readers"`).
    /// Not validated by [`Handshake::validate_against`]: callers inspect the
    /// list themselves when they need an optional capability.
    pub capabilities: Vec<String>,
}

/// Error returned by [`Handshake::validate_against`] when the daemon and client
/// are incompatible. Both the client's and daemon's version numbers are carried
/// so the caller can print an actionable error message (spec §4.1).
#[derive(Debug, PartialEq)]
pub enum HandshakeError {
    /// `daemon_protocol_version` differs between client and daemon. The shim
    /// must refuse to forward and instruct the user to restart the daemon.
    ProtocolMismatch {
        /// Client's `daemon_protocol_version`.
        client: u32,
        /// Daemon's `daemon_protocol_version`.
        daemon: u32,
    },
    /// `artifact_format_version` differs: artifacts produced by one side cannot
    /// be read by the other.
    ArtifactMismatch {
        /// Client's `artifact_format_version`.
        client: u32,
        /// Daemon's `artifact_format_version`.
        daemon: u32,
    },
}

impl Handshake {
    /// Build a `Handshake` for the current process. Reads `CARGO_PKG_VERSION`
    /// at compile time and the two numeric constants defined in this module.
    /// `worker_protocol_version` defaults to `1` matching the embed worker's
    /// current protocol; `capabilities` starts empty — callers push feature
    /// strings before sending.
    pub fn current() -> Self {
        Handshake {
            leindex_version: env!("CARGO_PKG_VERSION").to_string(),
            daemon_protocol_version: DAEMON_PROTOCOL_VERSION,
            artifact_format_version: ARTIFACT_FORMAT_VERSION,
            worker_protocol_version: 1,
            capabilities: Vec::new(),
        }
    }

    /// Validate this handshake (typically the client's) against a peer's
    /// (typically the daemon's). Returns `Ok(())` when the daemon protocol and
    /// artifact format versions both match; otherwise returns the specific
    /// mismatch. `leindex_version`, `worker_protocol_version`, and
    /// `capabilities` are not checked by this method — they are informational
    /// or caller-negotiated.
    pub fn validate_against(&self, other: &Handshake) -> Result<(), HandshakeError> {
        if self.daemon_protocol_version != other.daemon_protocol_version {
            return Err(HandshakeError::ProtocolMismatch {
                client: self.daemon_protocol_version,
                daemon: other.daemon_protocol_version,
            });
        }
        if self.artifact_format_version != other.artifact_format_version {
            return Err(HandshakeError::ArtifactMismatch {
                client: self.artifact_format_version,
                daemon: other.artifact_format_version,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// Identical handshakes validate successfully. This is the happy path:
    /// a shim and daemon built from the same LeIndex version agree.
    #[test]
    fn test_matching_handshake_ok() {
        let h = Handshake::current();
        assert!(h.validate_against(&h).is_ok());
    }

    /// A daemon-protocol-version mismatch is rejected with both version
    /// numbers captured so the shim can print them in its actionable error.
    #[test]
    fn test_protocol_version_mismatch_rejected() {
        let a = Handshake::current();
        let mut b = a.clone();
        b.daemon_protocol_version = a.daemon_protocol_version + 1;
        assert_eq!(
            a.validate_against(&b),
            Err(HandshakeError::ProtocolMismatch {
                client: a.daemon_protocol_version,
                daemon: b.daemon_protocol_version,
            }),
        );
    }

    /// An artifact-format-version mismatch is rejected with an
    /// `ArtifactMismatch` (distinct from a protocol mismatch so the caller can
    /// suggest the correct remediation).
    #[test]
    fn test_artifact_version_mismatch_rejected() {
        let a = Handshake::current();
        let mut b = a.clone();
        b.artifact_format_version = a.artifact_format_version + 1;
        assert_eq!(
            a.validate_against(&b),
            Err(HandshakeError::ArtifactMismatch {
                client: a.artifact_format_version,
                daemon: b.artifact_format_version,
            }),
        );
    }

    /// `current()` reads `CARGO_PKG_VERSION` and the numeric constants, so the
    /// handshake a client sends matches what a daemon built from the same
    /// source rejects as incompatible when the constants diverge.
    #[test]
    fn test_current_reads_constants() {
        let h = Handshake::current();
        assert_eq!(h.daemon_protocol_version, DAEMON_PROTOCOL_VERSION);
        assert_eq!(h.artifact_format_version, ARTIFACT_FORMAT_VERSION);
        assert!(!h.leindex_version.is_empty());
    }

    /// `validate_against` is symmetric for matching handshakes but the error's
    /// `client`/`daemon` labels are from the perspective of `self`, so swapping
    /// the arguments in a mismatch swaps the labels.
    #[test]
    fn test_validate_labels_correctly() {
        let client = Handshake {
            leindex_version: "1.0.0".into(),
            daemon_protocol_version: 1,
            artifact_format_version: 1,
            worker_protocol_version: 1,
            capabilities: vec![],
        };
        let daemon = Handshake {
            leindex_version: "1.0.0".into(),
            daemon_protocol_version: 2,
            artifact_format_version: 1,
            worker_protocol_version: 1,
            capabilities: vec![],
        };
        // self=client, other=daemon → labels match the perspective.
        assert_eq!(
            client.validate_against(&daemon),
            Err(HandshakeError::ProtocolMismatch {
                client: 1,
                daemon: 2,
            }),
        );
        // self=daemon, other=client → labels swapped.
        assert_eq!(
            daemon.validate_against(&client),
            Err(HandshakeError::ProtocolMismatch {
                client: 2,
                daemon: 1,
            }),
        );
    }
}
