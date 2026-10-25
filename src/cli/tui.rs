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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use crate::cli::leindex::IndexPhase;
use crate::cli::{Diagnostics, LeIndex};
use crate::search::search::SearchResult;

/// How often the UI polls for input and worker messages.
const TICK: Duration = Duration::from_millis(200);

/// How many hits a search request asks for. Bounded deliberately: the pane is a
/// window onto relevance ranking, not a paginated browser.
const SEARCH_TOP_K: usize = 50;

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
const STAGES: [IndexPhase; 6] = [
    IndexPhase::Scan,
    IndexPhase::Parse,
    IndexPhase::Pdg,
    IndexPhase::Lexical,
    IndexPhase::Neural,
    IndexPhase::Persist,
];

/// Width, in cells, of the bar drawn for a stage that has finished.
const STAGE_BAR_FULL: usize = 4;

/// The visible pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Pane {
    #[default]
    Map,
    Search,
    Status,
}

impl Pane {
    fn title(self) -> &'static str {
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
enum Request {
    Refresh,
    Search(String),
    /// Re-read durable index state. Issued on a timer, never by a keystroke:
    /// it exists so the Status pane tracks an index run started by *another*
    /// process, which is the only way to watch `leindex index` from here.
    Tick,
}

/// Result posted back from the background thread.
enum WorkerMsg {
    Diagnostics(Box<Diagnostics>),
    Search(SearchOutcome),
    Activity(Option<IndexActivity>),
    Failed(String),
}

/// One observation of an index run, read from state that outlives the process
/// that wrote it.
///
/// Deliberately not an `IndexJobSnapshot`: those live in the serving
/// `ProjectRegistry`'s in-memory map, so a second process sees none of them.
/// What *is* cross-process is the pair the registry's own progress updater
/// polls — `index-state.json` for the phase and the `index.lock` flock for
/// liveness.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexActivity {
    /// Phase vocabulary shared with `--memory-report` (`IndexPhase::as_str`).
    phase: &'static str,
    generation: u64,
    indexed_file_count: usize,
    /// Items the running phase has finished, and how many it must do. `Some`
    /// only while a phase is reporting — every phase transition and every
    /// completed publish clears it — so its presence is itself evidence that a
    /// phase is live now, not a number left over from a finished one.
    progress_done: usize,
    progress_total: Option<usize>,
    /// A live writer holds the project write lock. This is a fact read from
    /// the kernel, not an inference from how recent the phase looks: a
    /// crashed indexer leaves a stale phase behind but drops its flock, so
    /// phase alone cannot tell "running" apart from "died mid-run".
    writer_active: bool,
    /// Set when the lock could not be probed at all, so "not active" is never
    /// silently reported for "could not tell".
    probe_error: Option<String>,
    last_failure_phase: Option<&'static str>,
    last_failure: Option<String>,
}

/// Search hits plus the wall time they took, since "how long did that take" is
/// the question a search pane exists to answer.
struct SearchOutcome {
    results: Vec<SearchResult>,
    elapsed: Duration,
}

/// Application state.
struct App {
    project_path: PathBuf,
    pane: Pane,
    query_input: String,
    results: Vec<SearchResult>,
    results_state: ListState,
    last_search_elapsed: Option<Duration>,
    diagnostics: Option<Diagnostics>,
    /// Last observed index run. `None` until the first tick lands, so a fresh
    /// pane shows "reading" rather than inventing an idle state.
    activity: Option<Option<IndexActivity>>,
    /// True while a background job is in flight.
    busy: bool,
    /// Last error or notice, shown in the status bar.
    message: Option<String>,
    should_quit: bool,
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

/// Observe the current index run without an engine or a registry.
///
/// `None` means there is no usable durable state: the project has never
/// completed an index, or its state file is unreadable. `Some` reports whatever
/// the last run left behind, which may be a completed generation, an
/// interrupted one, or a run still in progress in another process.
fn read_activity(root: &Path) -> Option<IndexActivity> {
    // `resolve_existing_storage_path` only *finds* storage; it never creates a
    // directory or opens SQLite, so a TUI started on an unindexed project stays
    // read-only.
    let storage = crate::cli::leindex::resolve_existing_storage_path(root)?;
    let health = crate::cli::index_freshness::load_health(&storage)?;

    let (writer_active, probe_error) = probe_writer(&storage);
    Some(IndexActivity {
        phase: health.phase.as_str(),
        generation: health.generation,
        indexed_file_count: health.indexed_file_count,
        progress_done: health.progress_done,
        progress_total: health.progress_total,
        writer_active,
        probe_error,
        // `IndexPhase::as_str` is `&'static str`, so the failure phase needs no
        // allocation either.
        last_failure_phase: health.last_failure_phase.map(|phase| phase.as_str()),
        last_failure: health.last_failure,
    })
}

/// Ask the kernel whether a live writer holds the project lock.
///
/// This is the only honest liveness signal available across processes: the
/// phase in `index-state.json` is rewritten at phase boundaries, so a crashed
/// indexer leaves a phase that looks identical to a slow one. The flock is
/// released when the holder dies, so it answers the question the phase cannot.
///
/// Returns `(active, error)`. `try_acquire` reports `Ok(None)` when another
/// process holds the lock and `Ok(Some(guard))` when it is free (the guard is
/// dropped immediately). An error means "could not tell", which is reported
/// rather than folded into a misleading "not running".
fn probe_writer(storage: &Path) -> (bool, Option<String>) {
    // `ProjectWriteLock::try_acquire` opens the lock file with `.create(true)`,
    // so probing an absent `index.lock` would CREATE it — a read-only status
    // pane must not mutate the project. No file means no writer: the indexer
    // creates it before it can hold anything.
    if !storage.join("index.lock").exists() {
        return (false, None);
    }
    match crate::cli::leindex::ProjectWriteLock::try_acquire(storage) {
        Ok(Some(_)) => (false, None),
        Ok(None) => (true, None),
        Err(error) => (false, Some(error.to_string())),
    }
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

fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(frame.area());

    draw_header(frame, app, areas[0]);
    match app.pane {
        Pane::Map => draw_map(frame, app, areas[1]),
        Pane::Search => draw_search(frame, app, areas[1]),
        Pane::Status => draw_status(frame, app, areas[1]),
    }
    draw_status_bar(frame, app, areas[2]);
}

/// The tab label shown in the header.
///
/// Digits are pane selectors everywhere except the Search pane, where they
/// belong to the query (typing `2` while searching must insert a `2`, not jump
/// panes). The header therefore withholds the digit there rather than
/// advertising a key that types instead of selecting.
fn pane_tab_label(position: usize, pane: Pane, current: Pane) -> String {
    if current == Pane::Search {
        format!(" {} ", pane.title())
    } else {
        format!(" {} {} ", position, pane.title())
    }
}

fn draw_header(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let mut tabs: Vec<Span<'_>> = Vec::with_capacity(6);
    for (index, pane) in [Pane::Map, Pane::Search, Pane::Status]
        .into_iter()
        .enumerate()
    {
        let style = if pane == app.pane {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        tabs.push(Span::styled(
            pane_tab_label(index + 1, pane, app.pane),
            style,
        ));
        tabs.push(Span::raw(" "));
    }

    let project = Line::from(vec![
        Span::styled("Project: ", Style::default().fg(Color::DarkGray)),
        Span::raw(app.project_path.display().to_string()),
        // A running index is visible from every pane, so switching to Search
        // does not hide the fact that the tree underneath is being rewritten.
        match app.activity {
            Some(Some(IndexActivity {
                writer_active: true,
                ..
            })) => Span::styled(
                "  ● indexing",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            _ => Span::raw(""),
        },
    ]);

    frame.render_widget(
        Paragraph::new(vec![Line::from(tabs), project]).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(" LeIndex ")),
        ),
        area,
    );
}

fn draw_map(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let body = match &app.diagnostics {
        Some(diagnostics) => {
            let stats = &diagnostics.stats;
            vec![
                count_line("Files indexed", stats.files_parsed),
                count_line("Successful parses", stats.successful_parses),
                count_line("Failed parses", stats.failed_parses),
                count_line("Symbols (search)", stats.indexed_nodes),
                count_line("PDG nodes (indexed)", stats.pdg_nodes),
                count_line("PDG edges (indexed)", stats.pdg_edges),
                text_line("Index duration", format!("{} ms", stats.indexing_time_ms)),
                Line::from(vec![
                    Span::styled(
                        format!("{:<20}", "Health"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        diagnostics.index_health.clone(),
                        Style::default().fg(Color::Yellow),
                    ),
                ]),
            ]
        }
        None => vec![waiting_line(app.busy)],
    };

    frame.render_widget(
        Paragraph::new(body)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Line::from(" Project ")),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_search(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area);

    let mut input = app.query_input.clone();
    if app.pane == Pane::Search {
        input.push('▏');
    }
    frame.render_widget(
        Paragraph::new(input)
            .style(if app.pane == Pane::Search {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Line::from(" Query ")),
            ),
        chunks[0],
    );

    let title = match (app.results.len(), app.last_search_elapsed) {
        (0, _) => " Results ".to_string(),
        (count, Some(elapsed)) => {
            format!(
                " Results: {count} in {:.0}ms ",
                elapsed.as_secs_f64() * 1000.0
            )
        }
        (count, None) => format!(" Results: {count} "),
    };

    if app.results.is_empty() {
        let hint = if app.busy {
            "Searching…"
        } else if app.query_input.trim().is_empty() {
            "Press Tab to this pane, type a query, press Enter."
        } else {
            "No matches."
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Line::from(title)),
            ),
            chunks[1],
        );
        return;
    }

    let items: Vec<ListItem<'_>> = app
        .results
        .iter()
        .map(|result| {
            let score = result.score.overall;
            // Line numbers are only known when a PDG was loaded; show the path
            // alone rather than a misleading ":0".
            let location = match result.line_number {
                Some(line) => format!("{}:{line}", result.file_path),
                None => result.file_path.clone(),
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{score:>5.3} "),
                    Style::default().fg(score_color(score)),
                ),
                Span::styled(
                    result.symbol_name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {location}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let mut state = app.results_state;
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Line::from(title)),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
        chunks[1],
        &mut state,
    );
}

/// Colour a relevance score so it reads at a glance. The cutoffs are a
/// legibility aid, not a calibrated threshold — the exact number is always on
/// screen next to the colour.
fn score_color(score: f32) -> Color {
    if score >= 0.7 {
        Color::Green
    } else if score >= 0.4 {
        Color::Yellow
    } else {
        Color::Gray
    }
}

fn draw_status(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let mut body: Vec<Line<'static>> = Vec::new();

    // Index activity leads: it is the only line that changes without the user
    // doing anything, which is the whole point of the pane.
    match &app.activity {
        // No tick has landed yet.
        None => body.push(waiting_line(app.busy)),
        // Ticked, but the project has no durable index state at all.
        Some(None) => body.push(text_line(
            "Index",
            "no index state — run `leindex index`".to_string(),
        )),
        Some(Some(activity)) => body.extend(activity_lines(activity)),
    }
    body.push(Line::from(""));

    match &app.diagnostics {
        Some(diagnostics) => body.extend([
            text_line("Project", diagnostics.display_name.clone()),
            text_line("Embedding", diagnostics.embedding_model.clone()),
            text_line("Preset", diagnostics.embedding_preset.clone()),
            text_line("Health", diagnostics.index_health.clone()),
            text_line("PDG loaded", diagnostics.pdg_loaded.to_string()),
            // The two PDG counters below are read live from the loaded graph,
            // so they are 0 whenever no PDG is resident — unlike the snapshot
            // counts on the Map pane, which always reflect the last index run.
            count_line("PDG nodes (live)", diagnostics.pdg_nodes),
            count_line("PDG edges (live)", diagnostics.pdg_edges),
            count_line("Search nodes", diagnostics.search_index_nodes),
            text_line(
                "Process memory",
                format!("{:.1} MiB", mib(diagnostics.memory_usage_bytes)),
            ),
            text_line("Cache temperature", diagnostics.cache_temperature.clone()),
        ]),
        None => body.push(text_line("Diagnostics", "press Ctrl-R".to_string())),
    }

    frame.render_widget(
        Paragraph::new(body)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Line::from(" Index state ")),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// How one pipeline stage stands in a single observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageState {
    /// Behind the phase marker. The marker is a single-step cursor (see
    /// [`STAGES`]), so every stage before it has necessarily run.
    Done,
    /// The stage the marker sits on, drawn with the intra-phase count.
    Active,
    /// Not reached yet.
    Pending,
}

/// Which stage the phase marker stands on, or `None` when it names none of
/// them (`complete`, or a phase someone renamed without updating [`STAGES`]).
fn active_stage(phase: &str) -> Option<usize> {
    STAGES.iter().position(|stage| stage.as_str() == phase)
}

/// The staged bar for one observation, or `None` when the evidence is not
/// there to draw one.
///
/// `None` is the interesting case. A bar is a claim about how much work is
/// done, and this pane can only support that claim while a writer holds the
/// lock: `mark_index_phase` carries the previous snapshot forward, so a run
/// that died keeps whatever phase it died in. Drawing that as live progress
/// would dress a wedge up as work in flight.
///
/// `persist` is the second refusal. The pipeline marks it *twice* — once at
/// the end of `run_lexical`, before the core generation is published, and
/// again in `run_and_publish_neural` before the enhanced one. So standing on
/// `persist` proves lexical ran and says nothing at all about neural, and
/// either answer would be a fabrication: `Pending` dims a stage that may be
/// running, `Done` claims ~98% of a cold run's wall time finished. There is no
/// third box to tick, so no bar is drawn.
fn stage_bar(activity: &IndexActivity) -> Option<Vec<StageState>> {
    if !activity.writer_active {
        return None;
    }
    let active = active_stage(activity.phase)?;
    if STAGES[active] == IndexPhase::Persist {
        return None;
    }
    let mut states = Vec::with_capacity(STAGES.len());
    for (index, _) in STAGES.iter().enumerate() {
        states.push(match index.cmp(&active) {
            std::cmp::Ordering::Less => StageState::Done,
            std::cmp::Ordering::Greater => StageState::Pending,
            std::cmp::Ordering::Equal => StageState::Active,
        });
    }
    Some(states)
}

/// Cells filled for the active stage, from the count that stage publishes.
///
/// Floors rather than rounds: a bar must never claim more progress than the
/// counter reports. A phase that publishes nothing gets nothing.
fn active_cells(activity: &IndexActivity) -> usize {
    let Some(total) = activity.progress_total.filter(|total| *total > 0) else {
        return 0;
    };
    activity.progress_done.min(total) * STAGE_BAR_FULL / total
}

/// Fraction of the whole pipeline the active stage has covered.
fn active_fraction(activity: &IndexActivity) -> f32 {
    match activity.progress_total.filter(|total| *total > 0) {
        Some(total) => activity.progress_done.min(total) as f32 / total as f32,
        None => 0.0,
    }
}

/// Render the bar as one fixed-width line.
///
/// Every stage is always present, so the width never jumps under a watching
/// eye and the stage you are tracking keeps its position. The percentage is
/// computed from the same cells — finished stages plus the fraction of the
/// active one — so the number and the blocks can never disagree.
fn stage_bar_text(activity: &IndexActivity) -> String {
    let Some(states) = stage_bar(activity) else {
        return "-".to_string();
    };
    let mut cells = String::with_capacity(states.len() * (STAGE_BAR_FULL + 1));
    let mut done = 0usize;
    for state in &states {
        if !cells.is_empty() {
            cells.push(' ');
        }
        match state {
            StageState::Done => {
                done += 1;
                cells.push_str(&"━".repeat(STAGE_BAR_FULL));
            }
            StageState::Active => {
                let filled = active_cells(activity);
                cells.push_str(&"━".repeat(filled));
                cells.push_str(&"─".repeat(STAGE_BAR_FULL - filled));
            }
            StageState::Pending => cells.push_str(&"─".repeat(STAGE_BAR_FULL)),
        }
    }
    let fraction = (done as f32 + active_fraction(activity)) / STAGES.len() as f32;
    format!("{cells} {:>3.0}%", fraction * 100.0)
}

/// The staged bar, labelled with where in the pipeline we are.
///
/// The label is derived from the same facts as the bar, never guessed
/// separately, and it never asserts liveness the pane cannot prove: a run with
/// no writer is labelled "not running" even though `Phase` above still reports
/// where it stopped.
fn stages_line(activity: &IndexActivity) -> Line<'static> {
    let label = if !activity.writer_active {
        "not running".to_string()
    } else if activity.phase == IndexPhase::Complete.as_str() {
        "run complete".to_string()
    } else if let Some(index) = active_stage(activity.phase) {
        // Reached while the bar is withheld for `persist`: the position is
        // known, how much of the pipeline has run is not.
        format!("{}/{} {}", index + 1, STAGES.len(), activity.phase)
    } else {
        // A phase this build does not know. Say so instead of pointing at a
        // stage that may not be the one running.
        format!("unrecognised phase '{}'", activity.phase)
    };
    text_line("Stages", format!("{}  {}", stage_bar_text(activity), label))
}

/// The intra-phase count, kept separate from the bar so the bar's *plan*
/// (which stages have run) is never confused with its *measurement* (how far
/// through the current stage we are). Two stages report a count; the rest do
/// not, and say so instead of showing a frozen zero.
fn progress_line(activity: &IndexActivity) -> Line<'static> {
    let value = match activity.progress_total {
        Some(total) => format!("{} of {}", activity.progress_done, total),
        None => "-".to_string(),
    };
    text_line("Progress", value)
}

/// Render one observed index run.
///
/// The state word is chosen so the four cases stay distinguishable: a run in
/// progress, a completed generation, a run that stopped without publishing,
/// and a probe that could not answer. Folding "unknown" into "idle" would make
/// a wedged indexer look finished.
fn activity_lines(activity: &IndexActivity) -> Vec<Line<'static>> {
    let (state, style) = if activity.writer_active {
        (
            "indexing".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if let Some(error) = &activity.probe_error {
        (
            format!("liveness unknown ({error})"),
            Style::default().fg(Color::Magenta),
        )
    } else if activity.phase == "complete" {
        (
            "idle — last run complete".to_string(),
            Style::default().fg(Color::Green),
        )
    } else {
        (
            // A non-terminal phase with nobody holding the lock means a run was
            // interrupted (or is still publishing the last phase of a run whose
            // lock already dropped). Say what is observable, not more.
            format!("stopped mid-run at '{}'", activity.phase),
            Style::default().fg(Color::Red),
        )
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{:<20}", "Index activity"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(state, style),
        ]),
        text_line("Phase", activity.phase.to_string()),
        stages_line(activity),
        progress_line(activity),
        text_line("Generation", activity.generation.to_string()),
        count_line("Files indexed", activity.indexed_file_count),
    ];

    if let Some(phase) = activity.last_failure_phase {
        // A failure from an earlier run survives a later success, so label it
        // as last rather than implying the current state is broken.
        lines.push(text_line("Last failure phase", phase.to_string()));
    }
    if let Some(failure) = &activity.last_failure {
        lines.push(text_line("Last failure", truncate(failure, 200)));
    }
    lines
}

/// Clip a bounded failure message to the pane. `last_failure` is already
/// bounded by the writer, but a terminal can be 40 columns wide.
fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let clipped: String = value.chars().take(limit.saturating_sub(1)).collect();
    format!("{clipped}…")
}

fn count_line(label: &str, value: usize) -> Line<'static> {
    text_line(label, value.to_string())
}

fn text_line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<20}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value),
    ])
}

fn waiting_line(busy: bool) -> Line<'static> {
    Line::from(Span::styled(
        if busy {
            "Reading index state…"
        } else {
            "No status yet — press Ctrl-R to refresh."
        },
        Style::default().fg(Color::DarkGray),
    ))
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn draw_status_bar(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let message = match (&app.message, app.busy) {
        (Some(message), _) => Span::styled(message.clone(), Style::default().fg(Color::Red)),
        (None, true) => Span::styled("Working…", Style::default().fg(Color::Yellow)),
        (None, false) => Span::styled(
            "Tab next pane · type to search · Enter run · Ctrl-R refresh · Esc quit",
            Style::default().fg(Color::DarkGray),
        ),
    };
    frame.render_widget(Paragraph::new(Line::from(message)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

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
