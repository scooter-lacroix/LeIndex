//! Remote embedding construction helpers for the index builder.

use super::{HybridEmbedder, TfIdfEmbedder};
use tracing::warn;

/// Attempt the remote arm of the neural ladder for one TF-IDF base embedder.
///
/// Returns `Some(HybridRemote)` when `[indexing.embeddings]` configures a
/// remote endpoint and construction succeeds; `None` otherwise (endpoint
/// unset, or construction failed — already logged). Shared by the lexical
/// route (`build_neural_embedder`) and the enrichment phase
/// (`configured_neural_embedder`) so every neural construction site follows
/// the same remote → hybrid_local → tfidf_only ladder and never hard-fails.
#[cfg(feature = "remote-embeddings")]
pub(crate) fn try_remote_neural_embedder(
    tfidf_embedder: TfIdfEmbedder,
    embeddings_cfg: &crate::config::EmbeddingConfig,
) -> Option<HybridEmbedder> {
    // Runtime kill switch (LEINDEX_FEATURE_REMOTE_EMBEDDINGS=0): config alone
    // must not select remote; callers fall through to local/TF-IDF.
    if !crate::feature_flags::FeatureFlag::RemoteEmbeddings.is_enabled() {
        return None;
    }
    let resolved = embeddings_cfg.resolved_remote()?;
    let remote_cfg = crate::search::onnx::RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: resolved.endpoint,
        },
        api_key: embeddings_cfg.remote_api_key.clone(),
        model: resolved.model,
        remote_dimension: resolved.remote_dimension,
        doc_template: resolved.doc_template,
        query_template: resolved.query_template,
        base_url: None,
        ..crate::search::onnx::RemoteEmbeddingConfig::default()
    };
    match HybridEmbedder::hybrid_remote(
        tfidf_embedder,
        remote_cfg,
        Some(crate::config::LeIndexConfig::load_cached().neural_weight_f32()),
    ) {
        Ok(hybrid) => Some(hybrid),
        Err(e) => {
            warn!(
                "Failed to create hybrid_remote embedder ({}), falling back to local neural path",
                e
            );
            None
        }
    }
}
