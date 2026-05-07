//! Beautiful output formatting for LeIndex tools
//!
//! This module provides visually appealing, human-readable output formatting
//! for various LeIndex tools including search results, diffs, project maps, and diagnostics.

use serde_json::Value;

/// Resets all ANSI formatting attributes
pub const RESET: &str = "\x1b[0m";
/// Bold text formatting
pub const BOLD: &str = "\x1b[1m";
/// Dimmed text formatting
pub const DIM: &str = "\x1b[2m";

/// Black foreground color
pub const BLACK: &str = "\x1b[30m";
/// Red foreground color
pub const RED: &str = "\x1b[31m";
/// Green foreground color
pub const GREEN: &str = "\x1b[32m";
/// Yellow foreground color
pub const YELLOW: &str = "\x1b[33m";
/// Blue foreground color
pub const BLUE: &str = "\x1b[34m";
/// Magenta foreground color
pub const MAGENTA: &str = "\x1b[35m";
/// Cyan foreground color
pub const CYAN: &str = "\x1b[36m";
/// White foreground color
pub const WHITE: &str = "\x1b[37m";

/// Light grey foreground color
pub const LIGHT_GREY: &str = "\x1b[90m";
/// Light red foreground color
pub const LIGHT_RED: &str = "\x1b[91m";
/// Light green foreground color
pub const LIGHT_GREEN: &str = "\x1b[92m";
/// Light yellow foreground color
pub const LIGHT_YELLOW: &str = "\x1b[93m";
/// Light blue foreground color
pub const LIGHT_BLUE: &str = "\x1b[94m";
/// Light magenta foreground color
pub const LIGHT_MAGENTA: &str = "\x1b[95m";
/// Light cyan foreground color
pub const LIGHT_CYAN: &str = "\x1b[96m";

/// Formatter for diff output with color-coded additions/removals
pub struct DiffFormatter {
    color: bool,
}

impl DiffFormatter {
    /// Create a new DiffFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format a diff between original and modified content
    pub fn format(&self, original: &str, modified: &str, file_path: &str) -> String {
        let mut output = String::new();

        output.push_str(&self.header(file_path));
        output.push('\n');

        let old_lines: Vec<&str> = original.lines().collect();
        let new_lines: Vec<&str> = modified.lines().collect();

        let mut i = 0;
        let mut j = 0;
        let mut in_change = false;

        while i < old_lines.len() || j < new_lines.len() {
            let old_line = old_lines.get(i);
            let new_line = new_lines.get(j);

            match (old_line, new_line) {
                (Some(ol), Some(nl)) if ol == nl => {
                    output.push_str(&self.context_line(i + 1, ol));
                    i += 1;
                    j += 1;
                    in_change = false;
                }
                (Some(_), Some(_)) => {
                    if !in_change {
                        output.push_str("\n  Changes:\n");
                        in_change = true;
                    }
                    
                    if let Some(ol) = old_line {
                        output.push_str(&self.removed_line(i + 1, ol));
                        i += 1;
                    }
                    if let Some(nl) = new_line {
                        output.push_str(&self.added_line(j + 1, nl));
                        j += 1;
                    }
                }
                (Some(ol), None) => {
                    if !in_change {
                        output.push_str("\n  Changes:\n");
                        in_change = true;
                    }
                    output.push_str(&self.removed_line(i + 1, ol));
                    i += 1;
                }
                (None, Some(nl)) => {
                    if !in_change {
                        output.push_str("\n  Changes:\n");
                        in_change = true;
                    }
                    output.push_str(&self.added_line(j + 1, nl));
                    j += 1;
                }
                (None, None) => break,
            }
        }

        output
    }

    fn header(&self, file_path: &str) -> String {
        let title = format!("Diff: {}", file_path);
        self.box_title(&title)
    }

    fn box_title(&self, title: &str) -> String {
        let _border = "─".repeat(50);
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn added_line(&self, line_num: usize, content: &str) -> String {
        if self.color {
            format!(" {} │ {}{}{}\n", 
                format!("{:>4}", line_num),
                LIGHT_GREEN,
                content,
                RESET)
        } else {
            format!(" {:>4} │ {}\n", line_num, content)
        }
    }

    fn removed_line(&self, line_num: usize, content: &str) -> String {
        if self.color {
            format!(" {} │ {}{}{}\n",
                format!("{:>4}", line_num),
                LIGHT_RED,
                content,
                RESET)
        } else {
            format!(" {:>4} │ {}\n", line_num, content)
        }
    }

    fn context_line(&self, line_num: usize, content: &str) -> String {
        if self.color {
            format!(" {} │ {}{}{}\n",
                format!("{:>4}", line_num),
                DIM,
                content,
                RESET)
        } else {
            format!(" {:>4} │ {}\n", line_num, content)
        }
    }
}

impl Default for DiffFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for search results with ranked listings and scores
pub struct SearchFormatter {
    color: bool,
}

impl SearchFormatter {
    /// Create a new SearchFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format search results with ranking and scoring
    pub fn format(&self, results: &Value, query: &str) -> String {
        let results_array = match results {
            Value::Array(arr) => arr,
            _ => return "Invalid search results format".to_string(),
        };

        if results_array.is_empty() {
            return self.format_empty(query);
        }

        let mut output = String::new();
        output.push_str(&self.header(query, results_array.len()));

        for (idx, result) in results_array.iter().enumerate() {
            output.push_str(&self.format_result(result, idx + 1));
        }

        output
    }

    fn header(&self, query: &str, count: usize) -> String {
        let title = format!("Search: \"{}\" ({} results)", query, count);
        self.box_title(&title)
    }

    fn format_empty(&self, query: &str) -> String {
        let msg = format!("No results for: {}", query);
        self.box_title(&msg)
    }

    fn box_title(&self, title: &str) -> String {
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn format_result(&self, result: &Value, rank: usize) -> String {
        let file_path = result.get("file_path").and_then(|v| v.as_str()).unwrap_or("?");
        let symbol = result.get("symbol").and_then(|v| v.as_str());
        let score = result.get("score").and_then(|v| v.as_f64());
        let line = result.get("line").and_then(|v| v.as_u64());
        let snippet = result.get("snippet").and_then(|v| v.as_str())
            .or_else(|| result.get("content").and_then(|v| v.as_str()));

        let mut s = String::new();
        
        if self.color {
            s.push_str(&format!("{} {}. {}{}{}", 
                BOLD, rank, LIGHT_YELLOW, file_path, RESET));
        } else {
            s.push_str(&format!("{}. {}", rank, file_path));
        }

        if let Some(sym) = symbol {
            if self.color {
                s.push_str(&format!("{}::{}{}", DIM, sym, RESET));
            } else {
                s.push_str(&format!("::{}", sym));
            }
        }

        if let Some(l) = line {
            s.push_str(&format!(" {}", l));
        }

        if let Some(sc) = score {
            let pct = (sc * 100.0) as usize;
            let bar = "█".repeat(pct / 10);
            if self.color {
                s.push_str(&format!(" {}{}{}%{}", LIGHT_GREEN, bar, RESET, pct));
            } else {
                s.push_str(&format!(" [{}] {}%", bar, pct));
            }
        }
        
        s.push('\n');

        if let Some(snip) = snippet {
            let truncated = if snip.len() > 80 { format!("{}...", &snip[..77]) } else { snip.to_string() };
            if self.color {
                s.push_str(&format!("    {}{}{}\n", DIM, truncated, RESET));
            } else {
                s.push_str(&format!("    {}\n", truncated));
            }
        }

        s
    }
}

impl Default for SearchFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for project structure/dependency tree visualization
pub struct ProjectMapFormatter {
    color: bool,
}

impl ProjectMapFormatter {
    /// Create a new ProjectMapFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format project structure data as a tree view
    pub fn format(&self, data: &Value) -> String {
        let mut output = String::new();
        output.push_str(&self.header());

        if let Some(tree) = data.get("tree") {
            output.push_str(&self.format_tree(tree, "", true));
        } else if let Some(files) = data.get("files").and_then(|v| v.as_array()) {
            for (i, f) in files.iter().enumerate() {
                output.push_str(&self.format_file(f, i == 0, files.len()));
            }
        }

        if let Some(stats) = data.get("stats") {
            output.push('\n');
            output.push_str(&self.format_stats(stats));
        }

        output
    }

    fn header(&self) -> String {
        self.box_title("Project Structure")
    }

    fn box_title(&self, title: &str) -> String {
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn format_tree(&self, node: &Value, prefix: &str, is_last: bool) -> String {
        let mut s = String::new();
        
        let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let node_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("file");
        let symbol_count = node.get("symbol_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let children = node.get("children").and_then(|v| v.as_array());

        let icon = match node_type {
            "directory" | "dir" => "📂",
            "module" => "📦",
            _ => "📄",
        };

        let connector = if is_last { "└── " } else { "├── " };

        if self.color {
            let type_color = match node_type {
                "directory" | "dir" => LIGHT_BLUE,
                "module" => LIGHT_MAGENTA,
                _ => WHITE,
            };
            s.push_str(&format!("{}{}{}{} {}{}[{}]{}",
                prefix, connector, icon, type_color, name, DIM, symbol_count, RESET));
        } else {
            s.push_str(&format!("{}{}{} {} [{}]", prefix, connector, icon, name, symbol_count));
        }
        s.push('\n');

        if let Some(kids) = children {
            let child_prefix = if is_last { "    " } else { "│   " };
            for (i, child) in kids.iter().enumerate() {
                s.push_str(&self.format_tree(child, &format!("{}{}", prefix, child_prefix), i == kids.len() - 1));
            }
        }

        s
    }

    fn format_file(&self, file: &Value, _first: bool, _total: usize) -> String {
        let path = file.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let symbols = file.get("symbols").and_then(|v| v.as_u64()).unwrap_or(0);

        if self.color {
            format!("  📄 {}{}{} | {} symbols\n", LIGHT_YELLOW, path, RESET, symbols)
        } else {
            format!("  📄 {} | {} symbols\n", path, symbols)
        }
    }

    fn format_stats(&self, stats: &Value) -> String {
        let mut s = String::new();
        s.push_str("Statistics:\n");

        if let Some(total) = stats.get("total_files").and_then(|v| v.as_u64()) {
            s.push_str(&format!("  Files: {}\n", total));
        }
        if let Some(syms) = stats.get("total_symbols").and_then(|v| v.as_u64()) {
            s.push_str(&format!("  Symbols: {}\n", syms));
        }
        if let Some(avg) = stats.get("avg_complexity").and_then(|v| v.as_f64()) {
            s.push_str(&format!("  Avg complexity: {:.1}\n", avg));
        }

        s
    }
}

impl Default for ProjectMapFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for project diagnostics and index status
pub struct DiagnosticsFormatter {
    color: bool,
}

impl DiagnosticsFormatter {
    /// Create a new DiagnosticsFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format diagnostics data including index stats and issues
    pub fn format(&self, data: &Value) -> String {
        let mut output = String::new();
        output.push_str(&self.header());

        if let Some(project) = data.get("project_path").and_then(|v| v.as_str()) {
            output.push_str(&self.field("Project", project));
        }
        if let Some(indexed) = data.get("indexed_files").and_then(|v| v.as_u64()) {
            output.push_str(&self.field("Indexed", &indexed.to_string()));
        }
        if let Some(size) = data.get("index_size_mb").and_then(|v| v.as_f64()) {
            output.push_str(&self.field("Size", &format!("{:.2} MB", size)));
        }
        if let Some(syms) = data.get("symbol_count").and_then(|v| v.as_u64()) {
            output.push_str(&self.field("Symbols", &syms.to_string()));
        }

        if let Some(issues) = data.get("issues").and_then(|v| v.as_array()) {
            if !issues.is_empty() {
                output.push('\n');
                output.push_str("Issues:\n");
                for issue in issues.iter().take(10) {
                    let sev = issue.get("severity").and_then(|v| v.as_str()).unwrap_or("info");
                    let msg = issue.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                    
                    let icon = match sev {
                        "error" => "❌",
                        "warning" => "⚠️",
                        _ => "ℹ️",
                    };
                    let sev_color = match sev {
                        "error" => LIGHT_RED,
                        "warning" => LIGHT_YELLOW,
                        _ => LIGHT_BLUE,
                    };

                    if self.color {
                        output.push_str(&format!("  {} {}{}{} {}\n", icon, sev_color, sev, RESET, msg));
                    } else {
                        output.push_str(&format!("  {} {}: {}\n", icon, sev, msg));
                    }
                }
            }
        }

        output
    }

    fn header(&self) -> String {
        self.box_title("Diagnostics")
    }

    fn box_title(&self, title: &str) -> String {
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn field(&self, name: &str, value: &str) -> String {
        if self.color {
            format!("  {}{}:{} {}{}{}\n", BOLD, name, RESET, LIGHT_CYAN, value, RESET)
        } else {
            format!("  {}: {}\n", name, value)
        }
    }
}

impl Default for DiagnosticsFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for symbol impact analysis showing forward/backward dependencies
pub struct ImpactFormatter {
    color: bool,
}

impl ImpactFormatter {
    /// Create a new ImpactFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format impact analysis data with risk levels and affected symbols
    pub fn format(&self, data: &Value) -> String {
        let mut output = String::new();
        output.push_str(&self.header());

        if let Some(symbol) = data.get("symbol").and_then(|v| v.as_str()) {
            output.push_str(&self.field("Symbol", symbol));
        }
        if let Some(risk) = data.get("risk_level").and_then(|v| v.as_str()) {
            let (_icon, color) = match risk.to_lowercase().as_str() {
                "high" => ("🔴", LIGHT_RED),
                "medium" => ("🟡", LIGHT_YELLOW),
                "low" => ("🟢", LIGHT_GREEN),
                _ => ("⚪", WHITE),
            };
            if self.color {
                output.push_str(&format!("  {}{}:{} {}{}{}\n", BOLD, "Risk", RESET, color, risk, RESET));
            } else {
                output.push_str(&format!("  Risk: {}\n", risk));
            }
        }

        if let Some(forward) = data.get("forward_impact").and_then(|v| v.as_array()) {
            if !forward.is_empty() {
                output.push('\n');
                output.push_str("Forward Impact (affected):\n");
                for item in forward.iter().take(15) {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let file = item.get("file").and_then(|v| v.as_str()).unwrap_or("");
                    if self.color {
                        output.push_str(&format!("  → {}{}{} {}{}{}\n", LIGHT_CYAN, name, RESET, DIM, file, RESET));
                    } else {
                        output.push_str(&format!("  → {} {}\n", name, file));
                    }
                }
            }
        }

        if let Some(backward) = data.get("backward_impact").and_then(|v| v.as_array()) {
            if !backward.is_empty() {
                output.push('\n');
                output.push_str("Backward Impact (may break):\n");
                for item in backward.iter().take(15) {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    if self.color {
                        output.push_str(&format!("  ← {}{}{}\n", LIGHT_RED, name, RESET));
                    } else {
                        output.push_str(&format!("  ← {}\n", name));
                    }
                }
            }
        }

        output
    }

    fn header(&self) -> String {
        self.box_title("Impact Analysis")
    }

    fn box_title(&self, title: &str) -> String {
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn field(&self, name: &str, value: &str) -> String {
        if self.color {
            format!("  {}{}:{} {}{}{}\n", BOLD, name, RESET, LIGHT_CYAN, value, RESET)
        } else {
            format!("  {}: {}\n", name, value)
        }
    }
}

impl Default for ImpactFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for symbol lookup results with callers and callees
pub struct SymbolLookupFormatter {
    color: bool,
}

impl SymbolLookupFormatter {
    /// Create a new SymbolLookupFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format symbol lookup data showing definition, callers, and callees
    pub fn format(&self, data: &Value) -> String {
        let mut output = String::new();
        output.push_str(&self.header());

        if let Some(symbol) = data.get("symbol").and_then(|v| v.as_str()) {
            output.push_str(&self.field("Symbol", symbol));
        }
        if let Some(file) = data.get("file_path").and_then(|v| v.as_str()) {
            output.push_str(&self.field("File", file));
        }
        if let Some(line) = data.get("line").and_then(|v| v.as_u64()) {
            output.push_str(&self.field("Line", &line.to_string()));
        }
        if let Some(typ) = data.get("symbol_type").and_then(|v| v.as_str()) {
            let icon = match typ {
                "function" | "fn" => "ƒ",
                "method" => "m",
                "struct" => "S",
                "enum" => "E",
                "trait" => "T",
                "impl" => "I",
                "field" => "f",
                "module" => "M",
                _ => "•",
            };
            let color = match typ {
                "function" | "fn" => LIGHT_GREEN,
                "method" => LIGHT_CYAN,
                "struct" => LIGHT_MAGENTA,
                "enum" => LIGHT_YELLOW,
                "trait" => LIGHT_BLUE,
                _ => WHITE,
            };
            if self.color {
                output.push_str(&format!("  {}{}:{} {}{} {}{}\n", BOLD, "Type", RESET, color, icon, typ, RESET));
            } else {
                output.push_str(&format!("  Type: {} {}\n", icon, typ));
            }
        }

        if let Some(callers) = data.get("callers").and_then(|v| v.as_array()) {
            if !callers.is_empty() {
                output.push('\n');
                output.push_str("Callers:\n");
                for c in callers.iter().take(10) {
                    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let file = c.get("file").and_then(|v| v.as_str()).unwrap_or("");
                    if self.color {
                        output.push_str(&format!("  → {}{}{} {}{}{}\n", LIGHT_CYAN, name, RESET, DIM, file, RESET));
                    } else {
                        output.push_str(&format!("  → {} {}\n", name, file));
                    }
                }
            }
        }

        if let Some(callees) = data.get("callees").and_then(|v| v.as_array()) {
            if !callees.is_empty() {
                output.push('\n');
                output.push_str("Callees:\n");
                for c in callees.iter().take(10) {
                    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    if self.color {
                        output.push_str(&format!("  → {}{}{}\n", LIGHT_CYAN, name, RESET));
                    } else {
                        output.push_str(&format!("  → {}\n", name));
                    }
                }
            }
        }

        output
    }

    fn header(&self) -> String {
        self.box_title("Symbol Lookup")
    }

    fn box_title(&self, title: &str) -> String {
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn field(&self, name: &str, value: &str) -> String {
        if self.color {
            format!("  {}{}:{} {}{}{}\n", BOLD, name, RESET, LIGHT_CYAN, value, RESET)
        } else {
            format!("  {}: {}\n", name, value)
        }
    }
}

impl Default for SymbolLookupFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for phase analysis results
pub struct PhaseFormatter {
    color: bool,
}

impl PhaseFormatter {
    /// Create a new PhaseFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format phase analysis data with phase status and summaries
    pub fn format(&self, data: &Value) -> String {
        let mut output = String::new();
        output.push_str(&self.header());

        if let Some(mode) = data.get("mode").and_then(|v| v.as_str()) {
            output.push_str(&self.field("Mode", mode));
        }

        if let Some(phases) = data.get("phases").and_then(|v| v.as_array()) {
            output.push('\n');
            for phase in phases {
                let num = phase.get("phase").and_then(|v| v.as_u64()).unwrap_or(0);
                let name = phase.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let status = phase.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let findings = phase.get("findings").and_then(|v| v.as_u64()).unwrap_or(0);

let (icon, color) = match status {
                    "completed" | "success" => ("✓", LIGHT_GREEN),
                    "failed" | "error" => ("✗", LIGHT_RED),
                    "skipped" => ("○", DIM),
                    _ => ("•", WHITE),
                };

                if self.color {
                    output.push_str(&format!("  {}{} {}{} Phase {}: {}{} ({})\n",
                        color, icon, RESET, BOLD, num, RESET, name, findings));
                } else {
                    output.push_str(&format!("  {} Phase {}: {} ({})\n", icon, num, name, findings));
                }
            }
        }

        if let Some(summary) = data.get("summary").and_then(|v| v.as_str()) {
            output.push('\n');
            let truncated = if summary.len() > 150 { format!("{}...", &summary[..147]) } else { summary.to_string() };
            if self.color {
                output.push_str(&format!("  {}{}:{} {}{}{}\n", BOLD, "Summary", RESET, DIM, truncated, RESET));
            } else {
                output.push_str(&format!("  Summary: {}\n", truncated));
            }
        }

        output
    }

    fn header(&self) -> String {
        self.box_title("Phase Analysis")
    }

    fn box_title(&self, title: &str) -> String {
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn field(&self, name: &str, value: &str) -> String {
        if self.color {
            format!("  {}{}:{} {}{}{}\n", BOLD, name, RESET, LIGHT_CYAN, value, RESET)
        } else {
            format!("  {}: {}\n", name, value)
        }
    }
}

impl Default for PhaseFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for git status with staged, modified, and untracked files
pub struct GitStatusFormatter {
    color: bool,
}

impl GitStatusFormatter {
    /// Create a new GitStatusFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format git status data showing branch and file changes
    pub fn format(&self, data: &Value) -> String {
        let mut output = String::new();
        output.push_str(&self.header());

        if let Some(branch) = data.get("branch").and_then(|v| v.as_str()) {
            output.push_str(&self.field("Branch", branch));
        }
        if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
            let (_icon, color) = match status {
                "clean" => ("✓", LIGHT_GREEN),
                "dirty" => ("⚠", LIGHT_YELLOW),
                _ => ("?", WHITE),
            };
            if self.color {
                output.push_str(&format!("  {}{}:{} {}{}{}\n", BOLD, "Status", RESET, color, status, RESET));
            } else {
                output.push_str(&format!("  Status: {}\n", status));
            }
        }

        if let Some(staged) = data.get("staged").and_then(|v| v.as_array()) {
            if !staged.is_empty() {
                output.push('\n');
                output.push_str("Staged:\n");
                for f in staged.iter().take(15) {
                    let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                    if self.color {
                        output.push_str(&format!("  + {}{}{}\n", LIGHT_GREEN, path, RESET));
                    } else {
                        output.push_str(&format!("  + {}\n", path));
                    }
                }
            }
        }

        if let Some(modified) = data.get("modified").and_then(|v| v.as_array()) {
            if !modified.is_empty() {
                output.push('\n');
                output.push_str("Modified:\n");
                for f in modified.iter().take(15) {
                    let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                    if self.color {
                        output.push_str(&format!("  ~ {}{}{}\n", LIGHT_YELLOW, path, RESET));
                    } else {
                        output.push_str(&format!("  ~ {}\n", path));
                    }
                }
            }
        }

        if let Some(untracked) = data.get("untracked").and_then(|v| v.as_array()) {
            if !untracked.is_empty() {
                output.push('\n');
                output.push_str("Untracked:\n");
                for f in untracked.iter().take(15) {
                    let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                    if self.color {
                        output.push_str(&format!("  ? {}{}{}\n", LIGHT_GREY, path, RESET));
                    } else {
                        output.push_str(&format!("  ? {}\n", path));
                    }
                }
            }
        }

        output
    }

    fn header(&self) -> String {
        self.box_title("Git Status")
    }

    fn box_title(&self, title: &str) -> String {
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn field(&self, name: &str, value: &str) -> String {
        if self.color {
            format!("  {}{}:{} {}{}{}\n", BOLD, name, RESET, LIGHT_GREEN, value, RESET)
        } else {
            format!("  {}: {}\n", name, value)
        }
    }
}

impl Default for GitStatusFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for file summary with symbols and complexity metrics
pub struct FileSummaryFormatter {
    color: bool,
}

impl FileSummaryFormatter {
    /// Create a new FileSummaryFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format file summary data with file info and symbol list
    pub fn format(&self, data: &Value) -> String {
        let mut output = String::new();
        output.push_str(&self.header());

        if let Some(file) = data.get("file_path").and_then(|v| v.as_str()) {
            output.push_str(&self.field("File", file));
        }
        if let Some(lang) = data.get("language").and_then(|v| v.as_str()) {
            output.push_str(&self.field("Language", lang));
        }
        if let Some(size) = data.get("size").and_then(|v| v.as_u64()) {
            output.push_str(&self.field("Size", &format!("{} bytes", size)));
        }
        if let Some(complexity) = data.get("complexity").and_then(|v| v.as_u64()) {
            let (label, color) = match complexity {
                0..=5 => ("Low", LIGHT_GREEN),
                6..=15 => ("Medium", LIGHT_YELLOW),
                _ => ("High", LIGHT_RED),
            };
            if self.color {
                output.push_str(&format!("  {}{}:{} {}{} ({}){}\n", 
                    BOLD, "Complexity", RESET, color, complexity, label, RESET));
            } else {
                output.push_str(&format!("  Complexity: {} ({})\n", complexity, label));
            }
        }

        if let Some(symbols) = data.get("symbols").and_then(|v| v.as_array()) {
            if !symbols.is_empty() {
                output.push('\n');
                output.push_str("Symbols:\n");
                for sym in symbols.iter().take(20) {
                    let name = sym.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let typ = sym.get("type").and_then(|v| v.as_str()).unwrap_or("symbol");
                    
                    let icon = match typ {
                        "function" | "fn" => "ƒ",
                        "method" => "m",
                        "struct" => "S",
                        "enum" => "E",
                        "trait" => "T",
                        "impl" => "I",
                        "const" => "C",
                        "static" => "s",
                        "field" => "f",
                        "module" => "M",
                        "use" => "u",
                        _ => "•",
                    };

                    if self.color {
                        output.push_str(&format!("  {}{}{} {}{}{}\n", LIGHT_GREEN, icon, RESET, LIGHT_CYAN, name, RESET));
                    } else {
                        output.push_str(&format!("  {} {}\n", icon, name));
                    }
                }
            }
        }

        output
    }

    fn header(&self) -> String {
        self.box_title("File Summary")
    }

    fn box_title(&self, title: &str) -> String {
        if self.color {
            format!("{}┌─ {} ─┐{}", LIGHT_CYAN, title, RESET)
        } else {
            format!("┌─ {} ─┐", title)
        }
    }

    fn field(&self, name: &str, value: &str) -> String {
        if self.color {
            format!("  {}{}:{} {}{}{}\n", BOLD, name, RESET, LIGHT_CYAN, value, RESET)
        } else {
            format!("  {}: {}\n", name, value)
        }
    }
}

impl Default for FileSummaryFormatter {
    fn default() -> Self {
        Self::new()
    }
}