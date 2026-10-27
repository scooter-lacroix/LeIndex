//! Rendering for `leindex tui`: the pane layout, every draw function, and the
//! pipeline-stage bar.
//!
//! Split out of `tui.rs`, which now holds app state, the worker thread, and the
//! event loop. Everything here turns an [`App`] or an [`IndexActivity`] into
//! cells; none of it mutates state, so a render bug is testable without a
//! terminal.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use super::activity::IndexActivity;
use super::{App, Pane, STAGE_BAR_FULL, STAGES};
use crate::cli::leindex::IndexPhase;

pub(super) fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
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
pub(super) fn pane_tab_label(position: usize, pane: Pane, current: Pane) -> String {
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

/// How one pipeline stage stands in a single observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StageState {
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
pub(super) fn active_stage(phase: &str) -> Option<usize> {
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
pub(super) fn stage_bar(activity: &IndexActivity) -> Option<Vec<StageState>> {
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
pub(super) fn stage_bar_text(activity: &IndexActivity) -> String {
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
pub(super) fn stages_line(activity: &IndexActivity) -> Line<'static> {
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
pub(super) fn activity_lines(activity: &IndexActivity) -> Vec<Line<'static>> {
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
pub(super) fn truncate(value: &str, limit: usize) -> String {
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
