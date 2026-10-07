//! LeIndex evaluation harness for model and reranker bake-off (WS11 Tasks 1-5).
//!
//! This module provides the evaluation infrastructure for selecting the
//! production embedding model profile and reranker policy via LeIndex-specific
//! fused-retrieval evaluation (spec section 9).
//!
//! ## Components
//!
//! - [`gates`]: Predeclared acceptance gates and variance bands (section 9.4).
//! - [`corpus`]: Labeled evaluation corpus covering all section 9.2 categories.
//! - [`metrics`]: Recall@k, MRR@10, nDCG@10, per-category, confidence intervals.
//! - [`harness`]: Fused-retrieval harness with ablation support.
//! - [`report`]: Concise and machine-readable report generation.
//! - [`candidates`]: Embedding candidate registry and bake-off (Task 4).
//! - [`reranker_ablation`]: Reranker ablation and keep/replace/remove decision (Task 5).
//!
//! The gates are recorded BEFORE any candidate model is evaluated (section 9.4).
//! This prevents cherry-picking tolerances after seeing results.
//!
//! Anti-cheat (spec section 2.1 #4, #13): No precision reduction or model swap
//! ships without passing the gates below. Public MTEB/CodeSearchNet numbers
//! shortlist only; they do NOT select.

// Needs the TF-IDF embedder from the CLI index builder.
#[cfg(feature = "cli")]
pub mod agent_tasks;
pub mod budget_ledger;
pub mod candidates;
pub mod corpus;
#[cfg(feature = "cli")]
pub mod external_suite;
pub mod gates;
pub mod harness;
pub mod int8_parity;
pub mod metrics;
pub mod production_profile;
pub mod report;
pub mod reranker_ablation;
