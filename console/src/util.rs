// Standalone utility functions for the dispatch console.

use std::time::Duration;

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

/// Truncate a string to `max` chars, appending "..." if trimmed.
/// Uses char boundaries to avoid panicking on multi-byte UTF-8.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    if max > 3 {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{}...", truncated)
    } else {
        s.chars().take(max).collect()
    }
}

/// Strip ```action ... ```, <tool_call>...</tool_call>, and
/// <tool_result>...</tool_result> blocks from text, returning only the
/// prose/reasoning portion for chat display (dispatch-chat).
/// Single-pass: copies non-block segments into a new String, avoiding
/// the quadratic cost of repeated replace_range() calls.
pub fn strip_action_blocks(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;

    while pos < text.len() {
        let remaining = &text[pos..];
        // Try each block pattern at current position.
        if let Some(end) = try_skip_block(remaining, "```action", "```")
            .or_else(|| try_skip_block(remaining, "<tool_call>", "</tool_call>"))
            .or_else(|| try_skip_block(remaining, "<tool_result>", "</tool_result>"))
        {
            pos += end;
        } else {
            // Advance one char (handles multi-byte UTF-8 correctly).
            let ch = remaining.chars().next().unwrap();
            result.push(ch);
            pos += ch.len_utf8();
        }
    }
    result
}

/// If `text` starts with `open_tag`, find the matching `close_tag`
/// and return the byte offset just past it (relative to `text`).
fn try_skip_block(text: &str, open_tag: &str, close_tag: &str) -> Option<usize> {
    if !text.starts_with(open_tag) {
        return None;
    }
    let after_open = open_tag.len();
    text[after_open..].find(close_tag).map(|offset| after_open + offset + close_tag.len())
}

/// Strip system context tags that leak from the LLM framework into
/// orchestrator output. Removes `<reminder>`, `<current_datetime>`,
/// `<system_notification>`, and `<sql_tables>` blocks.
/// Single-pass approach to avoid quadratic replace_range() cost.
pub fn strip_system_tags(text: &str) -> String {
    const TAGS: &[(&str, &str)] = &[
        ("<reminder>", "</reminder>"),
        ("<current_datetime>", "</current_datetime>"),
        ("<system_notification>", "</system_notification>"),
        ("<sql_tables>", "</sql_tables>"),
    ];

    let mut result = String::with_capacity(text.len());
    let mut pos = 0;

    while pos < text.len() {
        let remaining = &text[pos..];
        let mut matched = false;
        for &(open, close) in TAGS {
            if remaining.starts_with(open) {
                let after_open = open.len();
                if let Some(close_offset) = remaining[after_open..].find(close) {
                    pos += after_open + close_offset + close.len();
                } else {
                    // No closing tag: skip to end of line.
                    let eol = remaining.find('\n').unwrap_or(remaining.len());
                    pos += eol;
                }
                matched = true;
                break;
            }
        }
        if !matched {
            let ch = remaining.chars().next().unwrap();
            result.push(ch);
            pos += ch.len_utf8();
        }
    }
    result
}

/// Remove lines with internal message prefixes that the orchestrator receives
/// for coordination but should never be forwarded to user-facing chat on the
/// radio. Strips `[EVENT]`, `[AGENT_MSG]`, `[MIC]`, and `Human:` prefixed
/// lines so the LLM cannot echo or fabricate them into the chat stream.
/// `Human:` is particularly dangerous because it creates fake conversation
/// turns that look like real user input.
pub fn strip_event_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("[EVENT]")
                && !trimmed.starts_with("[AGENT_MSG]")
                && !trimmed.starts_with("[MIC]")
                && !trimmed.starts_with("Human:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Clear stale `.dispatch/messages/` and `.dispatch/images/` contents for a repo.
/// Called at startup to manage disk space. Removes files only (not subdirectories).
pub fn clean_dispatch_dirs(repo_root: &str) {
    for subdir in &["messages", "images"] {
        let dir = format!("{}/.dispatch/{}", repo_root, subdir);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
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
