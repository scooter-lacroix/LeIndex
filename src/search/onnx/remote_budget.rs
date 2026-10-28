//! Request shaping for remote embedding batches: token estimation, per-input
//! truncation, and request width.
//!
//! Moved out of `remote.rs` by code motion (Large File Detection gate). These
//! are pure functions over an endpoint string and text — none of them reads
//! provider state — which is why they lift out without a struct.

use super::{RemoteEmbeddingConfig, RemoteProvider};

/// In-flight requests per batch for a remote endpoint.
///
/// Measured against the live hosted endpoint with the configured model
/// `embeddinggemma-2` (768-dim, 32 x 882 B per request, n >= 6 requests per
/// arm, echoed model verified on every response):
///
/// | K | rows/s | speedup | median round |
/// |---|--------|---------|--------------|
/// | 1 |  17.41 |   1.00x |        1.84s |
/// | 2 |  19.76 |   1.13x |        3.24s |
/// | 4 |  47.65 |   2.74x |        2.69s |
///
/// K=2 is a plateau, not a smooth ramp: its median request rose to 2.17s and
/// one round took 3.70s for the same 64 rows, so a second request largely
/// queues. Width pays off between 2 and 4, where median request growth stays
/// sub-linear, so 4 is the smallest width that actually banks a speedup. Do not
/// read the K=2 row as "concurrency barely helps" — it is a shallow point on an
/// otherwise steep curve. K=8 was not measured here and is not claimed.
///
/// Local endpoints stay at 1: the managed llama-server is compute-bound rather
/// than round-trip-bound (it must actually run the model), and it carries the
/// documented stale-socket hazard that pool reuse is disabled for. Paying
/// queueing complexity for a small gain against a sub-millisecond RTT is not
/// worth it, and the serial arm keeps that path byte-identical.
///
/// `REMOTE_CHUNK` is a separate constant: the request WIDTH. With 32 rows per
/// request, a fixed round-trip of ~1.8s is amortised over fewer rows than a
/// larger request would manage, so this pair (width 32, width 4) is where the
/// 2.74x above was measured — not a proven optimum for either.
pub(crate) fn remote_max_in_flight(endpoint: &str) -> usize {
    if endpoint_is_local(endpoint) { 1 } else { 4 }
}

/// True when `endpoint` cannot leave this machine (see
/// [`RemoteEmbeddingConfig::is_local_endpoint`]).
pub(crate) fn endpoint_is_local(endpoint: &str) -> bool {
    let config = RemoteEmbeddingConfig {
        provider: RemoteProvider::Custom {
            endpoint: endpoint.to_string(),
        },
        ..RemoteEmbeddingConfig::default()
    };
    config.is_local_endpoint()
}

/// Rough token count for `text`, for BUDGETING only.
///
/// Deliberately a cheap byte-divisor, not a real tokenizer. This is used to
/// decide how many documents fit in one request, and it is compared against a
/// budget that is itself conservative: over-estimating costs a slightly
/// narrower request (cheap, more round trips), while under-estimating costs a
/// rejected request (the whole point of the exercise). Source and JSON are
/// token-dense — roughly 3 bytes per token — so dividing by 3 is the safe
/// direction. An exact tokenizer would need a
/// per-model vocabulary shipped and versioned alongside the config, which is
/// far more machinery than a width decision needs.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(3).max(1)
}

/// Truncate `input` so `estimate_tokens` fits `limit`, cutting at a char
/// boundary near the byte budget.
///
/// Keeps the HEAD of the document: for code, the signature, imports and
/// enclosing definition come before the body, so a head-truncated symbol still
/// carries most of its meaning, while a tail-truncated one keeps closing braces
/// and loses the thing being defined.
///
/// Returns the input unchanged when it already fits, so the common path
/// allocates nothing. The approximation is `3 * limit` bytes — the same
/// divisor `estimate_tokens` uses, so the two cannot disagree about whether a
/// text fits.
pub(crate) fn truncate_to_token_budget<'a>(
    input: std::borrow::Cow<'a, str>,
    limit: usize,
) -> std::borrow::Cow<'a, str> {
    if limit == 0 || estimate_tokens(&input) <= limit {
        return input;
    }
    let mut end = (limit.saturating_mul(3)).min(input.len());
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    match input {
        // Already owned: truncate in place instead of re-allocating a copy.
        std::borrow::Cow::Owned(mut owned) => {
            owned.truncate(end);
            std::borrow::Cow::Owned(owned)
        }
        std::borrow::Cow::Borrowed(borrowed) => std::borrow::Cow::Borrowed(&borrowed[..end]),
    }
}

/// Request width for one batch, packed against BOTH the document cap and the
/// request TOKEN budget.
///
/// `batch_limit` only counts DOCUMENTS, and a document cap alone cannot keep a
/// request inside a model's window: 32 documents of ~882 B is ~7k tokens, which
/// a 2,048-token endpoint rejects wholesale (`The prompt is too long ...
/// Shorten it and retry`). So after applying the document cap, the inputs are
/// accumulated against `token_budget` and the window ends at the first document
/// that would overflow it. One document always goes in, even if it alone
/// exceeds the budget — a request of one is the smallest legal unit, and
/// dropping it silently would lose a row instead of reporting the problem.
///
/// `pending` carries the ALREADY-TEMPLATED text, so the estimate reflects what
/// actually goes on the wire.
///
/// Local endpoints with no `batch_limit` and no `token_budget` keep today's
/// behaviour exactly: one request for the whole batch.
pub(crate) fn request_width_for(
    pending: &[std::borrow::Cow<'_, str>],
    batch_limit: Option<usize>,
    token_budget: Option<usize>,
    local: bool,
) -> usize {
    let total = pending.len();
    if total == 0 {
        return 1;
    }
    // The document-count ceiling for this request.
    let count_cap = match batch_limit.filter(|limit| *limit > 0) {
        Some(limit) => limit.min(total),
        None if local => total,
        None => REMOTE_CHUNK.min(total).max(1),
    };
    let Some(budget) = token_budget.filter(|budget| *budget > 0) else {
        return count_cap;
    };
    let mut used = 0usize;
    for (index, text) in pending.iter().take(count_cap).enumerate() {
        let cost = estimate_tokens(text);
        // At least one document per request, even when it alone overfills.
        if index > 0 && used.saturating_add(cost) > budget {
            return index;
        }
        used = used.saturating_add(cost);
    }
    count_cap
}

/// Concurrency-friendly request size for remote endpoints with no configured
/// hard cap. 32 x 882 B is far below any endpoint's token ceiling, and each of
/// the 4 in-flight requests stays small enough to overlap cleanly.
pub(crate) const REMOTE_CHUNK: usize = 32;
