#![allow(missing_docs)]

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

tokio::task_local! {
    static REQUEST_TIMINGS: Arc<Mutex<PhaseTimings>>;
}

pub static PROJECT_HYDRATIONS: AtomicU64 = AtomicU64::new(0);
pub static PDG_LOADS: AtomicU64 = AtomicU64::new(0);
pub static NEURAL_REQUESTS: AtomicU64 = AtomicU64::new(0);

pub fn reset_path_counters() {
    PROJECT_HYDRATIONS.store(0, Ordering::Relaxed);
    PDG_LOADS.store(0, Ordering::Relaxed);
    NEURAL_REQUESTS.store(0, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WorkBudget {
    pub max_latency_ms: u64,
    pub allow_partial: bool,
}

impl WorkBudget {
    pub fn elapsed(self, started: Instant) -> bool {
        self.allow_partial && started.elapsed().as_millis() >= self.max_latency_ms as u128
    }
}

pub fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct PhaseTimings {
    pub lock_wait_ms: u64,
    pub hydrate_ms: u64,
    pub scan_ms: u64,
    pub parse_ms: u64,
    pub git_ms: u64,
    pub catalog_ms: u64,
    pub pdg_ms: u64,
    pub lexical_ms: u64,
    pub neural_ms: u64,
    pub cache_read_ms: u64,
    pub cache_write_ms: u64,
    pub persist_ms: u64,
    pub handler_ms: u64,
    pub transport_queue_ms: u64,
    pub total_ms: u64,
}

pub async fn collect_request_timings<F>(future: F) -> (F::Output, PhaseTimings)
where
    F: Future,
{
    REQUEST_TIMINGS
        .scope(Arc::new(Mutex::new(PhaseTimings::default())), async move {
            let output = future.await;
            let timings = REQUEST_TIMINGS.with(|timings| {
                timings
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            });
            (output, timings)
        })
        .await
}

pub type RequestTimingSink = Arc<Mutex<PhaseTimings>>;

pub fn current_request_timing_sink() -> Option<RequestTimingSink> {
    REQUEST_TIMINGS.try_with(Arc::clone).ok()
}

pub fn record_hydrate_ms(elapsed_ms: u64) {
    let _ = REQUEST_TIMINGS.try_with(|timings| record_hydrate_ms_to(timings, elapsed_ms));
}

pub fn record_pdg_ms(elapsed_ms: u64) {
    let _ = REQUEST_TIMINGS.try_with(|timings| record_pdg_ms_to(timings, elapsed_ms));
}

pub fn record_git_ms(elapsed_ms: u64) {
    let _ = REQUEST_TIMINGS.try_with(|timings| {
        timings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .git_ms = elapsed_ms;
    });
}

pub fn record_neural_ms(elapsed_ms: u64) {
    let _ = REQUEST_TIMINGS.try_with(|timings| record_neural_ms_to(timings, elapsed_ms));
}

pub fn record_hydrate_ms_to(timings: &RequestTimingSink, elapsed_ms: u64) {
    timings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .hydrate_ms = elapsed_ms;
}

pub fn record_pdg_ms_to(timings: &RequestTimingSink, elapsed_ms: u64) {
    timings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .pdg_ms = elapsed_ms;
}

pub fn record_neural_ms_to(timings: &RequestTimingSink, elapsed_ms: u64) {
    timings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .neural_ms = elapsed_ms;
}
