// CLI Phase command implementation.
//
// Extracted from `cli.rs` (Large File Detection: cli.rs exceeded the 2000-line
// ceiling; Cyclomatic Complexity Check: `cmd_phase_impl` exceeded CCN 15 under
// lizard 1.23 closure counting). Mirrors the existing `mcp_commands.rs`
// `#[path]` submodule pattern used by `cli.rs`.

use crate::phase::{DocsMode, FormatMode, PhaseOptions, PhaseSelection, run_phase_analysis};
use anyhow::Context;
use anyhow::Result as AnyhowResult;
use std::path::PathBuf;

/// Fields of the `phase` CLI command, passed as one value.
pub(crate) struct PhaseCommandArgs {
    pub(crate) phase: Option<u8>,
    pub(crate) all: bool,
    pub(crate) mode: String,
    pub(crate) path: Option<PathBuf>,
    pub(crate) project: Option<PathBuf>,
    pub(crate) max_files: usize,
    pub(crate) max_focus_files: usize,
    pub(crate) top_n: usize,
    pub(crate) max_output_chars: usize,
    pub(crate) include_docs: bool,
    pub(crate) docs_mode: String,
    pub(crate) no_incremental_refresh: bool,
}

/// Phase command implementation
pub(crate) async fn cmd_phase_impl(args: PhaseCommandArgs) -> AnyhowResult<()> {
    let PhaseCommandArgs {
        phase,
        all,
        mode,
        path,
        project,
        max_files,
        max_focus_files,
        top_n,
        max_output_chars,
        include_docs,
        docs_mode,
        no_incremental_refresh,
    } = args;
    resolve_phase_flags(all, phase)?;

    let target_path = resolve_phase_target(path, project)?;
    let (root, focus_files) = if target_path.is_file() {
        let parent = target_path
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("phase analysis file path has no parent directory"))?;
        (parent, vec![target_path.clone()])
    } else {
        (target_path, Vec::new())
    };

    let parsed_mode = FormatMode::parse(&mode)
        .ok_or_else(|| anyhow::anyhow!("Invalid mode '{}'. Use ultra|balanced|verbose", mode))?;

    let parsed_docs_mode = DocsMode::parse(&docs_mode).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid docs mode '{}'. Use off|markdown|text|all",
            docs_mode
        )
    })?;

    let selection = resolve_phase_selection(all, phase)?;

    let options = PhaseOptions {
        root,
        focus_files,
        mode: parsed_mode,
        max_files,
        max_focus_files,
        top_n,
        max_output_chars,
        use_incremental_refresh: !no_incremental_refresh,
        include_docs,
        docs_mode: parsed_docs_mode,
        hotspot_keywords: PhaseOptions::default().hotspot_keywords,
        universe: Default::default(),
    };

    let report = tokio::task::spawn_blocking(move || {
        let mut options = options;
        crate::cli::mcp::phase_handler::attach_index_universe(&mut options);
        run_phase_analysis(options, selection)
    })
    .await
    .context("Phase task failed")??;

    println!("{}", report.formatted_output);
    Ok(())
}

/// Validates the `--phase` / `--all` mutual-exclusion contract.
fn resolve_phase_flags(all: bool, phase: Option<u8>) -> AnyhowResult<()> {
    if !all && phase.is_none() {
        anyhow::bail!("Specify either --phase <1..5> or --all");
    }
    if all && phase.is_some() {
        anyhow::bail!("Use either --phase or --all, not both");
    }
    Ok(())
}

/// Resolves the analysis target (explicit path > project > cwd) and
/// canonicalizes it.
fn resolve_phase_target(path: Option<PathBuf>, project: Option<PathBuf>) -> AnyhowResult<PathBuf> {
    let target_path = match path.or(project) {
        Some(p) => p,
        None => std::env::current_dir().context("Failed to determine current directory")?,
    };
    target_path
        .canonicalize()
        .context("Failed to canonicalize phase analysis path")
}

/// Builds the [`PhaseSelection`] from the `--phase` / `--all` flags.
fn resolve_phase_selection(all: bool, phase: Option<u8>) -> AnyhowResult<PhaseSelection> {
    if all {
        Ok(PhaseSelection::All)
    } else {
        let p = phase.unwrap();
        PhaseSelection::from_number(p)
            .ok_or_else(|| anyhow::anyhow!("Invalid phase '{}'. Use 1..5", p))
    }
}
