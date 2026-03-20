// Standalone utility functions for the dispatch console.

use std::time::Duration;

use crate::types::RESERVED_CALLSIGNS;

/// Extract the short directory name from a repo root path (dispatch-2dc).
pub fn repo_name_from_path(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// Scan immediate children of `parent` for git repos. Returns sorted list of
/// absolute paths to directories that contain a `.git` entry (dispatch-sa1).
pub fn scan_child_repos(parent: &str) -> Vec<String> {
    let mut repos = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join(".git").exists() {
                if let Some(s) = path.to_str() {
                    repos.push(s.to_string());
                }
            }
        }
    }
    repos.sort();
    repos
}

pub fn format_runtime(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    format!("{}m{:02}s", s / 60, s % 60)
}

/// Validate a custom callsign (dispatch-bgz.3).
pub fn is_valid_callsign(name: &str) -> bool {
    if name.is_empty() || name.len() > 20 {
        return false;
    }
    if name.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let upper = name.to_uppercase();
    !RESERVED_CALLSIGNS.contains(&upper.as_str())
}

/// Truncate a string to `max` chars, appending "..." if trimmed.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])
    } else {
        s[..max].to_string()
    }
}

/// Strip ```action ... ``` and <tool_call>...</tool_call> blocks from text,
/// returning only the prose/reasoning portion for chat display (dispatch-chat).
pub fn strip_action_blocks(text: &str) -> String {
    let mut result = text.to_string();
    // Remove ```action ... ``` blocks
    while let Some(start) = result.find("```action") {
        if let Some(end_fence) = result[start + 9..].find("```") {
            let end = start + 9 + end_fence + 3;
            result.replace_range(start..end, "");
        } else {
            break;
        }
    }
    // Remove <tool_call>...</tool_call> blocks
    while let Some(start) = result.find("<tool_call>") {
        if let Some(end) = result.find("</tool_call>") {
            result.replace_range(start..end + "</tool_call>".len(), "");
        } else {
            break;
        }
    }
    result
}

/// Detect the machine's local network IP by connecting a UDP socket.
/// No data is sent; this just determines the outgoing interface address.
pub fn local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip().to_string())
}

/// Compute PTY dimensions from terminal size.
pub fn compute_pane_size(term_rows: u16, term_cols: u16) -> (u16, u16) {
    // Cap to sane values (guards against bogus size reports on some terminals).
    let term_rows = term_rows.min(500);
    let term_cols = term_cols.min(1000);
    // 3-row header + 1-row ticker + 1-row footer = 5 fixed rows; remaining split 2 ways vertically.
    // Each pane: 2 border rows + 4 info strip rows = 6 overhead.
    let pane_h = term_rows.saturating_sub(5) / 2;
    let rows = pane_h.saturating_sub(6).max(10);
    // Each pane is half the terminal width minus 2 for borders.
    let cols = (term_cols / 2).saturating_sub(2).max(20);
    (rows, cols)
}

/// Extract the text content of a single screen row, trimming trailing spaces.
pub fn screen_row_text(screen: &vt100::Screen, row: u16) -> String {
    let mut s = String::new();
    for col in 0..screen.size().1 {
        if let Some(cell) = screen.cell(row, col) {
            let ch = cell.contents();
            s.push_str(if ch.is_empty() { " " } else { &ch });
        }
    }
    s.trim_end().to_string()
}

/// Hash all screen content to detect changes without storing the full buffer.
pub fn compute_screen_hash(screen: &vt100::Screen) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    for row in 0..screen.size().0 {
        screen_row_text(screen, row).hash(&mut hasher);
    }
    hasher.finish()
}

/// Return true if the last non-blank row of the screen matches the idle prompt
/// pattern for the given tool.
///
/// claude-code idle: last non-blank row is exactly ">" or "> "
pub fn is_idle_prompt(screen: &vt100::Screen, tool: &str) -> bool {
    if tool != "claude-code" {
        return false;
    }
    let (rows, _) = screen.size();
    for r in (0..rows).rev() {
        let text = screen_row_text(screen, r);
        if !text.is_empty() {
            return text == ">" || text == "> ";
        }
    }
    false
}
