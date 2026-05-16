//! Output formatting utilities.

/// Format a duration in milliseconds to a human-readable string.
pub fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{}m {}s", mins, secs)
    }
}

/// Format a timestamp.
pub fn format_timestamp(secs: i64) -> String {
    // Simplified for fixture
    format!("2024-01-01T00:00:00+{}s", secs)
}

/// Indent a multi-line string.
pub fn indent(s: &str, level: usize) -> String {
    let prefix = " ".repeat(level * 2);
    s.lines()
        .map(|line| format!("{}{}", prefix, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wrap text to a given column width.
pub fn wrap_text(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut current_len = 0;
    for word in text.split_whitespace() {
        if current_len + word.len() + 1 > width && current_len > 0 {
            result.push('\n');
            current_len = 0;
        }
        if current_len > 0 {
            result.push(' ');
            current_len += 1;
        }
        result.push_str(word);
        current_len += word.len();
    }
    result
}
