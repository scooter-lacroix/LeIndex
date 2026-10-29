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
    // Managed service: bring it up before the first remote call when the
    // section allows auto_start. Failure is logged and non-fatal (the
    // provider construction below fails on its own if the port stays down).
    if let Some(name) = embeddings_cfg.remote_service.as_deref() {
        let services = &crate::config::LeIndexConfig::load_cached().services;
        if let Some(svc) = services.get(name)
            && svc.auto_start
            && let Err(e) = crate::services::ensure_running(name, svc)
        {
            tracing::warn!(service = %name, %e, "could not start managed embedding service");
        }
    }
    let resolved = embeddings_cfg.resolved_remote()?;
    let api_key = match embeddings_cfg.resolved_api_key() {
        Ok(key) => key,
        Err(error) => {
            warn!("{error}; remote embeddings unavailable");
            return None;
        }
    };
    // One resolver owns the provider choice: hardcoding `Custom` here is what
    // made a configured `gemini` route silently run the OpenAI-compatible wire
    // format. `service_name` names the `[services.*]` entry behind this
    // endpoint, so a sustained outage can be fixed by restarting LeIndex's own
    // server instead of abandoning the neural layer (`None` for third parties).
    let remote_cfg = crate::search::onnx::RemoteEmbeddingConfig::from_resolved(
        &resolved,
        api_key,
        embeddings_cfg.remote_service.clone(),
    );
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
