//! Terminal UI (`leindex tui`).
//!
//! An interactive window onto the same engine the CLI subcommands drive: the
//! project map, semantic search, and index health. Nothing here is a second
//! source of truth — every pane calls the same [`LeIndex`] entry points
//! `cmd_search_impl` / `cmd_diagnostics_impl` call, so the TUI cannot drift
//! from the CLI.
//!
//! Threading model: engine calls block for seconds (search) to minutes
//! (hydration), so they run on a background thread and post [`WorkerMsg`] back
//! over a channel. The UI thread only reads that channel and draws, which keeps
//! the terminal responsive while work is in flight.
//!
//! Terminal state is restored by a drop guard, so an error on the way out does
//! not strand the user in raw mode with no cursor.

use anyhow::{Context, Result as AnyhowResult};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use crate::cli::leindex::IndexPhase;
use crate::cli::{Diagnostics, LeIndex};
use crate::search::search::SearchResult;

mod activity;
mod render;

use activity::{IndexActivity, read_activity};
use render::draw;

/// How often the UI polls for input and worker messages.
pub(super) const TICK: Duration = Duration::from_millis(200);

/// How many hits a search request asks for. Bounded deliberately: the pane is a
/// window onto relevance ranking, not a paginated browser.
pub(super) const SEARCH_TOP_K: usize = 50;

/// The index pipeline in dependency order, for the staged bar on the Status
/// pane.
///
/// Not a second phase vocabulary — an *ordering* of the one that already
/// exists: [`IndexPhase::as_str`] supplies every label below, so a new phase
/// cannot be added without this table failing the exhaustiveness check in
/// `test_stage_bar_tracks_every_phase_in_pipeline_order`.
///
/// This bar is a plan, not a measurement: `index-state.json` carries no record
/// of a stage having finished, so the split is derived from the phase marker
/// (see [`stage_bar`]). That is sound only because `mark_index_phase` clears
/// the intra-phase counters on *every* transition, so the marker is a
/// single-step cursor. It is also why the bar is withheld conservatively
/// wherever the phase alone cannot prove what has run.
pub(super) const STAGES: [IndexPhase; 6] = [
    IndexPhase::Scan,
    IndexPhase::Parse,
    IndexPhase::Pdg,
    IndexPhase::Lexical,
    IndexPhase::Neural,
    IndexPhase::Persist,
];

/// Width, in cells, of the bar drawn for a stage that has finished.
pub(super) const STAGE_BAR_FULL: usize = 4;

/// The visible pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Pane {
    #[default]
    Map,
    Search,
    Status,
}

impl Pane {
    pub(super) fn title(self) -> &'static str {
        match self {
            Pane::Map => "Map",
            Pane::Search => "Search",
            Pane::Status => "Status",
        }
    }

    fn next(self) -> Self {
        match self {
            Pane::Map => Pane::Search,
            Pane::Search => Pane::Status,
            Pane::Status => Pane::Map,
        }
    }
}

/// Work handed to the background thread.
pub(super) enum Request {
    Refresh,
    Search(String),
    /// Re-read durable index state. Issued on a timer, never by a keystroke:
    /// it exists so the Status pane tracks an index run started by *another*
    /// process, which is the only way to watch `leindex index` from here.
    Tick,
}

/// Result posted back from the background thread.
pub(super) enum WorkerMsg {
    Diagnostics(Box<Diagnostics>),
    Search(SearchOutcome),
    Activity(Option<IndexActivity>),
    Failed(String),
}

/// Search hits plus the wall time they took, since "how long did that take" is
/// the question a search pane exists to answer.
pub(super) struct SearchOutcome {
    results: Vec<SearchResult>,
    elapsed: Duration,
}

/// Application state.
pub(super) struct App {
    pub(super) project_path: PathBuf,
    pub(super) pane: Pane,
    pub(super) query_input: String,
    pub(super) results: Vec<SearchResult>,
    pub(super) results_state: ListState,
    pub(super) last_search_elapsed: Option<Duration>,
    pub(super) diagnostics: Option<Diagnostics>,
    /// Last observed index run. `None` until the first tick lands, so a fresh
    /// pane shows "reading" rather than inventing an idle state.
    pub(super) activity: Option<Option<IndexActivity>>,
    /// True while a background job is in flight.
    pub(super) busy: bool,
    /// Last error or notice, shown in the status bar.
    pub(super) message: Option<String>,
    pub(super) should_quit: bool,
}

impl App {
    fn new(project_path: PathBuf) -> Self {
        Self {
            project_path,
            pane: Pane::default(),
            query_input: String::new(),
            results: Vec::new(),
            results_state: ListState::default(),
            last_search_elapsed: None,
            diagnostics: None,
            activity: None,
            busy: false,
            message: None,
            should_quit: false,
        }
    }

    /// Handle one key press. Returns `Some` when it starts background work.
    ///
    /// On the Search pane every printable key belongs to the query, so text
    /// input is checked before anything else and navigation is matched by key
    /// name only. A query containing "n" or "q" therefore still types them.
    fn handle_key(&mut self, key: KeyEvent) -> Option<Request> {
        // Only act on presses: Windows terminals also emit a release event, and
        // acting on both would double every keystroke.
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('d') => self.should_quit = true,
                KeyCode::Char('r') => return self.start_refresh(),
                _ => {}
            }
            return None;
        }

        match key.code {
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.pane = self.pane.next();
                self.message = None;
            }
            // The header renders `1 Map  2 Search  3 Status`, so those digits
            // have to actually switch panes — an advertised key that does
            // nothing is a lie the user pays for. On the Search pane a digit
            // belongs to the query (see above), so they are selectors only
            // elsewhere; Tab switches from anywhere.
            KeyCode::Char('1') if self.pane != Pane::Search => self.pane = Pane::Map,
            KeyCode::Char('2') if self.pane != Pane::Search => self.pane = Pane::Search,
            KeyCode::Char('3') if self.pane != Pane::Search => self.pane = Pane::Status,
            KeyCode::Enter if self.pane == Pane::Search => return self.start_search(),
            KeyCode::Down if self.pane == Pane::Search => self.move_selection(1),
            KeyCode::Up if self.pane == Pane::Search => self.move_selection(-1),
            KeyCode::Backspace if self.pane == Pane::Search => {
                self.query_input.pop();
            }
            KeyCode::Char(c) if self.pane == Pane::Search => self.query_input.push(c),
            _ => {}
        }
        None
    }

    /// Move the result selection, saturating at both ends.
    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let current = self.results_state.selected().unwrap_or(0) as isize;
        let last = self.results.len() as isize - 1;
        self.results_state
            .select(Some((current + delta).clamp(0, last) as usize));
    }

    fn start_search(&mut self) -> Option<Request> {
        let query = self.query_input.trim().to_string();
        if query.is_empty() {
            self.message = Some("Type a query, then press Enter.".to_string());
            return None;
        }
        if self.busy {
            self.message = Some("Still working; wait for the current job.".to_string());
            return None;
        }
        Some(Request::Search(query))
    }

    fn start_refresh(&mut self) -> Option<Request> {
        if self.busy {
            self.message = Some("Still working; wait for the current job.".to_string());
            return None;
        }
        Some(Request::Refresh)
    }

    fn apply(&mut self, msg: WorkerMsg) {
        // A timer-driven `Activity` must not clear `busy`: it arrives while a
        // search is still in flight and clearing the flag there would re-enable
        // input and let a second request race the first.
        if !matches!(msg, WorkerMsg::Activity(_)) {
            self.busy = false;
        }
        match msg {
            WorkerMsg::Diagnostics(diagnostics) => {
                self.message = None;
                self.diagnostics = Some(*diagnostics);
            }
            WorkerMsg::Search(outcome) => {
                self.message = None;
                self.last_search_elapsed = Some(outcome.elapsed);
                self.results = outcome.results;
                self.results_state.select(if self.results.is_empty() {
                    None
                } else {
                    Some(0)
                });
            }
            WorkerMsg::Activity(activity) => self.activity = Some(activity),
            WorkerMsg::Failed(error) => self.message = Some(error),
        }
    }
}

/// RAII terminal restoration.
struct TerminalGuard;

impl TerminalGuard {
    /// Enter raw mode and the alternate screen.
    ///
    /// Raw mode is enabled first and rolled back if the alternate screen fails,
    /// so a partial failure cannot leave the terminal half-configured.
    fn enter() -> AnyhowResult<Self> {
        enable_raw_mode().context("Failed to enable raw mode")?;
        if let Err(error) = execute!(std::io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error).context("Failed to enter the alternate screen");
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best effort: a panic inside a drop guard would abort instead of
        // unwinding, and the process is exiting either way.
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
        let _ = disable_raw_mode();
    }
}

/// Entry point for `leindex tui`.
pub async fn run(project_path: Option<PathBuf>) -> AnyhowResult<()> {
    // Check for a terminal before touching terminal state. Raw mode on a pipe
    // has no meaning, and a downstream cursor round-trip fails minutes later
    // with a message that says nothing about the actual cause.
    anyhow::ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "`leindex tui` needs an interactive terminal: stdin and stdout are not a TTY"
    );

    let root = match project_path {
        Some(path) => path,
        None => std::env::current_dir().context("Failed to read the current directory")?,
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("Failed to resolve {}", root.display()))?;

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    // No explicit `clear()`: it makes the backend ask the terminal for a cursor
    // position report and wait for the reply, which hangs on any terminal that
    // does not answer. The first `draw` repaints the whole viewport anyway, so
    // clearing up front buys nothing.
    let mut terminal = Terminal::new(backend).context("Failed to initialize the terminal")?;

    let (request_tx, request_rx) = channel::<Request>();
    let (result_tx, result_rx) = channel::<WorkerMsg>();
    spawn_worker(root.clone(), request_rx, result_tx)?;

    let mut app = App::new(root);
    // Open with real data instead of an empty pane the user has to poke.
    app.busy = true;
    if request_tx.send(Request::Refresh).is_err() {
        app.busy = false;
        app.message = Some("The worker stopped unexpectedly.".to_string());
    }

    // Restore the terminal before reporting: locals drop in reverse declaration
    // order, so `terminal` is released before `_guard` re-enables the cursor and
    // leaves the alternate screen — and both happen before the returned error
    // reaches the caller's printer.
    event_loop(&mut terminal, &mut app, &request_tx, &result_rx)
}

fn spawn_worker(
    root: PathBuf,
    requests: Receiver<Request>,
    results: Sender<WorkerMsg>,
) -> AnyhowResult<()> {
    std::thread::Builder::new()
        .name("leindex-tui-worker".to_string())
        .spawn(move || {
            // One engine instance is reused so model load and PDG hydration are
            // paid once per TUI session, not once per keystroke.
            let mut engine: Option<LeIndex> = None;

            for request in requests {
                let message = match request {
                    Request::Refresh => match read_diagnostics(&root, &mut engine) {
                        Ok(diagnostics) => WorkerMsg::Diagnostics(Box::new(diagnostics)),
                        Err(error) => WorkerMsg::Failed(format!("Status failed: {error:#}")),
                    },
                    Request::Search(query) => match run_search(&root, &mut engine, &query) {
                        Ok(outcome) => WorkerMsg::Search(outcome),
                        Err(error) => WorkerMsg::Failed(format!("Search failed: {error:#}")),
                    },
                    // A tick is a two-file read: no engine, no hydration, no
                    // registry. It must stay that cheap or it would compete
                    // with the very index run it is watching.
                    Request::Tick => WorkerMsg::Activity(read_activity(&root)),
                };
                // A closed receiver means the UI is gone; stop quietly.
                if results.send(message).is_err() {
                    return;
                }
            }
        })
        .context("Failed to start the TUI worker thread")?;
    Ok(())
}

/// Read index state without hydrating the PDG.
///
/// `cmd_diagnostics_impl` documents why this must stay a snapshot operation:
/// the resident PDG is multi-gigabyte, and loading it here would make every
/// status refresh cost tens of seconds.
fn read_diagnostics(root: &Path, engine: &mut Option<LeIndex>) -> AnyhowResult<Diagnostics> {
    let engine = engine_for(root, engine)?;
    // Persisted stats are the source for the index-time counts. A missing or
    // unreadable stats blob is not fatal — diagnostics still report live health
    // — but it must not pass silently either.
    if let Err(error) = engine.load_stats_from_storage() {
        tracing::warn!("TUI could not load persisted stats: {error:#}");
    }
    engine.get_diagnostics()
}

fn run_search(
    root: &Path,
    engine: &mut Option<LeIndex>,
    query: &str,
) -> AnyhowResult<SearchOutcome> {
    let engine = engine_for(root, engine)?;
    let started = Instant::now();
    let results = engine.search(query, SEARCH_TOP_K, None)?;
    Ok(SearchOutcome {
        results,
        elapsed: started.elapsed(),
    })
}

/// Get the shared engine, creating and hydrating it on first use.
fn engine_for<'a>(root: &Path, slot: &'a mut Option<LeIndex>) -> AnyhowResult<&'a mut LeIndex> {
    if let Some(engine) = slot {
        return Ok(engine);
    }
    let mut engine =
        LeIndex::new(root).with_context(|| format!("Failed to open {}", root.display()))?;
    // An unhydrated engine returns no hits, which reads as "no matches" instead
    // of "not loaded yet" — hydrate once, up front.
    if let Err(error) = engine.load_from_storage() {
        tracing::warn!("TUI could not load the index from storage: {error:#}");
    }
    Ok(slot.insert(engine))
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    requests: &Sender<Request>,
    results: &Receiver<WorkerMsg>,
) -> AnyhowResult<()> {
    // Prime the activity pane immediately; the first draw should never show an
    // empty "reading" line when the answer costs two file reads.
    if requests.send(Request::Tick).is_err() {
        app.message = Some("The worker stopped unexpectedly.".to_string());
    }
    let mut last_tick = Instant::now();

    loop {
        terminal
            .draw(|frame| draw(frame, app))
            .context("Failed to draw the interface")?;

        if app.should_quit {
            return Ok(());
        }

        if crossterm::event::poll(TICK).context("Failed to poll for input")? {
            match crossterm::event::read().context("Failed to read input")? {
                Event::Key(key) => {
                    if let Some(request) = app.handle_key(key) {
                        app.busy = true;
                        app.message = None;
                        if requests.send(request).is_err() {
                            app.busy = false;
                            app.message = Some("The worker stopped unexpectedly.".to_string());
                        }
                    }
                }
                // A resize invalidates the layout; redraw immediately.
                Event::Resize(_, _) => continue,
                _ => {}
            }
        }

        // Refresh observed index activity on a timer rather than per draw: an
        // index run publishes a new phase at phase boundaries, so polling four
        // times a second is already finer than the signal it watches.
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            if requests.send(Request::Tick).is_err() {
                app.message = Some("The worker stopped unexpectedly.".to_string());
            }
        }

        loop {
            match results.try_recv() {
                Ok(message) => app.apply(message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    app.busy = false;
                    app.message = Some("The worker stopped unexpectedly.".to_string());
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::activity::probe_writer;
    use super::render::{
        StageState, active_stage, activity_lines, pane_tab_label, stage_bar, stage_bar_text,
        stages_line, truncate,
    };
    use super::*;
    use ratatui::text::Line;

    /// Write a minimal `index-state.json` so the reader has real input to parse.
    #[cfg(unix)]
    fn storage_with_phase(dir: &Path, phase: crate::cli::leindex::IndexPhase) -> PathBuf {
        let storage = dir.join(".leindex");
        std::fs::create_dir_all(&storage).expect("create storage");
        let health = crate::cli::leindex::IndexHealth {
            generation: 7,
            phase,
            indexed_file_count: 12,
            ..Default::default()
        };
        crate::cli::index_freshness::save_health(&storage, &health).expect("write health");
        storage
    }

    /// The pane must render a live "done of total" and must NOT invent one when
    /// the phase is not reporting.
    ///
    /// This is the check for the whole progress channel: the counters are
    /// written by an indexing process into `index-state.json`, read back into
    /// `IndexActivity`, and rendered here. Before this existed the counters
    /// were written and never read — a bar nobody could see. Fails if a field is
    /// dropped anywhere along that chain, including if `read_activity` stops
    /// copying them.
    #[cfg(unix)]
    #[test]
    fn test_activity_lines_render_live_progress_only_when_a_phase_reports_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).expect("create storage");
        let health = crate::cli::leindex::IndexHealth {
            generation: 7,
            phase: crate::cli::leindex::IndexPhase::Parse,
            indexed_file_count: 12,
            progress_total: Some(400),
            progress_done: 137,
            ..Default::default()
        };
        crate::cli::index_freshness::save_health(&storage, &health).expect("write health");

        let activity = read_activity(dir.path()).expect("activity");
        let rendered = activity_lines(&activity)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("137 of 400"),
            "a reporting phase must render its live count, got:\n{rendered}"
        );

        // Clearing the total must clear the bar. A finished run that still
        // showed "137 of 400" is the exact frozen-bar bug this guards.
        let health = crate::cli::leindex::IndexHealth {
            progress_total: None,
            progress_done: 0,
            ..health
        };
        crate::cli::index_freshness::save_health(&storage, &health).expect("rewrite health");
        let activity = read_activity(dir.path()).expect("activity");
        let rendered = activity_lines(&activity)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("of 400"),
            "a phase with no total must render no count at all, got:\n{rendered}"
        );
    }

    /// `STAGES` must stay the pipeline, in order, or the bar places the marker
    /// in a pipeline that does not exist. `IndexPhase::as_str` still owns the
    /// vocabulary, so a renamed phase fails here rather than silently dropping
    /// out of the bar and rendering as "stage unknown" forever.
    #[test]
    fn test_stage_bar_tracks_every_phase_in_pipeline_order() {
        assert_eq!(
            STAGES.map(IndexPhase::as_str),
            ["scan", "parse", "pdg", "lexical", "neural", "persist"],
            "STAGES must mirror the order the indexer marks phases in"
        );
    }

    fn running(phase: IndexPhase, done: usize, total: Option<usize>) -> IndexActivity {
        IndexActivity {
            phase: phase.as_str(),
            generation: 7,
            indexed_file_count: 12,
            progress_done: done,
            progress_total: total,
            writer_active: true,
            probe_error: None,
            last_failure_phase: None,
            last_failure: None,
        }
    }

    /// The bar's split is only sound because a phase transition clears the
    /// counters: an active stage drawn with a *previous* phase's numbers would
    /// be the frozen-bar bug in a new costume.
    #[test]
    fn test_stage_bar_dims_what_runs_after_the_marker_and_fills_what_ran_before() {
        let activity = running(IndexPhase::Pdg, 0, None);
        assert_eq!(
            stage_bar(&activity),
            Some(vec![
                StageState::Done,
                StageState::Done,
                StageState::Active,
                StageState::Pending,
                StageState::Pending,
                StageState::Pending,
            ]),
            "pdg is the third stage, so the two before it have run"
        );
        assert_eq!(active_stage(activity.phase), Some(2));

        let text = stage_bar_text(&activity);
        assert!(
            text.starts_with("━━━━ ━━━━ ──── ──── ──── ────"),
            "a stage with no published count must not draw as partially done, got {text}"
        );
        assert!(text.ends_with("33%"), "two of six stages are done: {text}");
    }

    /// The published counter must drive the active stage's cells, and the
    /// percentage with it — otherwise the bar is decoration.
    #[test]
    fn test_stage_bar_fills_the_active_stage_from_its_published_count() {
        let half = running(IndexPhase::Parse, 50, Some(100));
        let text = stage_bar_text(&half);
        assert!(
            text.starts_with("━━━━ ━━── ──── ──── ──── ────"),
            "parse half done is two of four cells, got {text}"
        );
        assert!(text.ends_with("25%"), "one done plus half: {text}");

        let complete = stage_bar_text(&running(IndexPhase::Parse, 100, Some(100)));
        assert!(
            complete.starts_with("━━━━ ━━━━ ──── ──── ──── ────"),
            "a full count fills the stage, got {complete}"
        );
        assert!(complete.ends_with("33%"), "two of six: {complete}");
    }

    /// Cells floor and the count is clamped, so a writer that reports `done >
    /// total` (or a stale total from a resumed phase) cannot paint past its
    /// stage or overstate the pipeline.
    #[test]
    fn test_stage_bar_never_claims_more_progress_than_the_counter_reports() {
        let over = running(IndexPhase::Neural, 999, Some(100));
        let text = stage_bar_text(&over);
        assert!(
            text.starts_with("━━━━ ━━━━ ━━━━ ━━━━ ━━━━ ────"),
            "an over-reporting phase must fill only its own cells, got {text}"
        );
        assert!(text.ends_with("83%"), "four of six stages done: {text}");

        // A zero total carries no fraction; it must not divide by zero.
        let zero = stage_bar_text(&running(IndexPhase::Parse, 5, Some(0)));
        assert!(
            zero.starts_with("━━━━ ──── ──── ──── ──── ────"),
            "a zero total is not a measurement, got {zero}"
        );
    }

    /// Two refusals, both of which would otherwise turn a guess into a claim:
    /// no writer (a dead run keeps its phase on disk), and `persist` (marked
    /// twice, so it proves nothing about neural, which owns almost all of a
    /// cold run's wall time).
    #[test]
    fn test_stage_bar_is_withheld_when_the_phase_cannot_prove_what_ran() {
        let mut interrupted = running(IndexPhase::Parse, 50, Some(100));
        interrupted.writer_active = false;
        assert_eq!(
            stage_bar(&interrupted),
            None,
            "a stale phase with no writer must not render as live progress"
        );

        for phase in [IndexPhase::Persist, IndexPhase::Complete] {
            assert_eq!(
                stage_bar(&running(phase, 0, None)),
                None,
                "{} cannot be placed in the pipeline, so no bar",
                phase.as_str()
            );
        }

        // The line still says something true in that case. `persist` is a known
        // position with unknowable progress; a dead run is not running at all
        // and must not be labelled as if it were.
        let rendered = render(&stages_line(&running(IndexPhase::Persist, 0, None)));
        assert!(rendered.contains("-  6/6 persist"), "got {rendered}");

        let dead = render(&stages_line(&interrupted));
        assert!(
            dead.contains("-  not running"),
            "a run with no writer must not be labelled in progress: {dead}"
        );

        // Completed, and a phase this build has never heard of.
        assert!(
            render(&stages_line(&running(IndexPhase::Complete, 0, None)))
                .contains("-  run complete")
        );
        let unknown = IndexActivity {
            phase: "teleport",
            ..running(IndexPhase::Parse, 0, None)
        };
        assert!(
            render(&stages_line(&unknown)).contains("unrecognised phase 'teleport'"),
            "an unknown phase must not be silently placed in the pipeline"
        );
    }

    fn render(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// The bar is fixed width in cells, so a stage keeps its position while a
    /// user watches and the pane does not reflow on every transition.
    #[test]
    fn test_stage_bar_width_is_stable_across_phases() {
        let mut widths = Vec::new();
        for phase in [
            IndexPhase::Scan,
            IndexPhase::Parse,
            IndexPhase::Pdg,
            IndexPhase::Lexical,
            IndexPhase::Neural,
        ] {
            let text = stage_bar_text(&running(phase, 7, Some(11)));
            let cells = text.split(' ').next().expect("bar cells");
            widths.push(cells.chars().count());
        }
        assert!(
            widths.iter().all(|width| *width == widths[0]),
            "every phase must render the same bar width, got {widths:?}"
        );
    }

    #[test]
    fn test_read_activity_returns_none_without_any_index_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            read_activity(dir.path()).is_none(),
            "a project that has never been indexed must report no activity, \
             not an invented idle state"
        );
    }

    /// The liveness probe is the whole reason this pane can tell a running
    /// index from a crashed one: a crashed indexer leaves a stale phase in
    /// `index-state.json` but drops its flock, so the two are indistinguishable
    /// from the phase alone.
    #[cfg(unix)]
    #[test]
    fn test_read_activity_uses_the_write_lock_to_distinguish_running_from_crashed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = storage_with_phase(dir.path(), crate::cli::leindex::IndexPhase::Neural);

        // Nobody holds the lock: a non-terminal phase with no writer is an
        // interrupted run.
        let idle = read_activity(dir.path()).expect("activity");
        assert!(
            !idle.writer_active,
            "uncontended lock must read as inactive"
        );
        assert_eq!(idle.phase, "neural");
        assert_eq!(idle.generation, 7);
        assert_eq!(idle.indexed_file_count, 12);
        assert!(idle.probe_error.is_none(), "no probe error expected");

        // Same phase, live writer: now it is a run in progress.
        let holder = crate::cli::leindex::ProjectWriteLock::try_acquire(&storage)
            .expect("probe lock")
            .expect("lock should be free before we take it");
        let running = read_activity(dir.path()).expect("activity");
        assert!(
            running.writer_active,
            "a held write lock must read as an active writer even though the \
             phase on disk is unchanged"
        );
        assert_eq!(
            running.phase, idle.phase,
            "the probe must not change the reported phase"
        );
        drop(holder);

        // Released: back to interrupted, proving the signal tracks the lock
        // rather than latching.
        let after = read_activity(dir.path()).expect("activity");
        assert!(
            !after.writer_active,
            "dropping the guard must clear activity"
        );
    }

    /// A completed generation with no writer is idle, and must never render as
    /// the "stopped mid-run" failure state.
    #[cfg(unix)]
    #[test]
    fn test_read_activity_reports_a_completed_generation_as_complete() {
        let dir = tempfile::tempdir().expect("tempdir");
        storage_with_phase(dir.path(), crate::cli::leindex::IndexPhase::Complete);
        let activity = read_activity(dir.path()).expect("activity");
        assert_eq!(activity.phase, "complete");
        assert!(!activity.writer_active);
        assert!(
            activity_lines(&activity)[0]
                .spans
                .iter()
                .any(|span| span.content.contains("idle")),
            "a completed generation must not be described as an interrupted run"
        );
    }

    /// Probing must not create the lock file: a read-only status pane cannot
    /// mutate the project it is inspecting.
    #[cfg(unix)]
    #[test]
    fn test_probe_writer_does_not_create_a_lock_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = storage_with_phase(dir.path(), crate::cli::leindex::IndexPhase::Complete);
        let lock = storage.join("index.lock");
        assert!(!lock.exists(), "precondition: no lock file");

        let (active, error) = probe_writer(&storage);
        assert!(!active);
        assert!(error.is_none());
        assert!(
            !lock.exists(),
            "probing a free project must not create an index.lock"
        );
    }

    #[test]
    fn test_truncate_keeps_short_messages_and_clips_long_ones() {
        assert_eq!(truncate("short", 200), "short");
        let clipped = truncate(&"x".repeat(500), 10);
        assert_eq!(clipped.chars().count(), 10);
        assert!(clipped.ends_with('…'));
    }

    /// A timer-driven activity update must not clear `busy`, or a keystroke
    /// queued behind an in-flight search would fire a second concurrent job.
    #[test]
    fn test_activity_updates_do_not_clear_the_busy_flag() {
        let mut app = App::new(PathBuf::from("/tmp"));
        app.busy = true;
        app.apply(WorkerMsg::Activity(None));
        assert!(
            app.busy,
            "a timer tick must leave in-flight work marked busy"
        );
        assert_eq!(app.activity, Some(None));

        app.apply(WorkerMsg::Failed("boom".to_string()));
        assert!(!app.busy, "a real worker result must clear busy");
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Request> {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// The header advertises `1 Map  2 Search  3 Status`; those digits have to
    /// act from the panes that advertise them.
    #[test]
    fn test_digit_keys_select_the_pane_the_header_advertises() {
        let mut app = App::new(PathBuf::from("/tmp"));
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.pane, Pane::Search);

        press(&mut app, KeyCode::Char('3'));
        assert_eq!(
            app.pane,
            Pane::Search,
            "on Search a digit types, not selects"
        );

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.pane, Pane::Status);
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.pane, Pane::Map);
        press(&mut app, KeyCode::Char('3'));
        assert_eq!(app.pane, Pane::Status);
    }

    /// On the Search pane a digit is part of the query, so `3` must type rather
    /// than jump away from under the user's fingers.
    #[test]
    fn test_digits_type_into_the_search_query_instead_of_switching_panes() {
        let mut app = App::new(PathBuf::from("/tmp"));
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.pane, Pane::Search);

        press(&mut app, KeyCode::Char('3'));
        assert_eq!(app.pane, Pane::Search, "must stay on the Search pane");
        assert_eq!(app.query_input, "3", "the digit belongs to the query");

        // Tab still cycles out of Search, where digits become selectors again.
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.pane, Pane::Status);
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.pane, Pane::Map);
    }

    /// The header must never advertise a selector digit the active pane steals
    /// for text entry: on Search, `2` types, so the tab must not read `2 Search`.
    #[test]
    fn test_header_withholds_selector_digits_on_the_search_pane() {
        assert_eq!(pane_tab_label(1, Pane::Map, Pane::Status), " 1 Map ");
        assert_eq!(pane_tab_label(3, Pane::Status, Pane::Map), " 3 Status ");
        assert_eq!(
            pane_tab_label(2, Pane::Search, Pane::Search),
            " Search ",
            "a digit here would type into the query instead of selecting"
        );
    }
}
