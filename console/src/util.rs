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
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])
    } else {
        s[..max].to_string()
    }
}

/// Clean a dispatch message: strip ANSI escapes and extract content from
/// triple-backtick fences, ignoring any terminal noise outside them.
///
/// ConPTY on Windows often replaces runs of spaces with cursor-forward
/// CSI sequences (`\x1b[nC`).  We convert those back to spaces so the
/// message text retains its original spacing.
pub fn clean_dispatch_msg(s: &str) -> String {
    // Strip ANSI escapes first to get plain text for fence detection.
    // Cursor-forward (CSI n C) is converted to n spaces instead of being
    // stripped, because ConPTY uses it in place of literal space characters.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    // Collect parameter bytes, then the final byte.
                    let mut params = String::new();
                    let final_byte;
                    loop {
                        match chars.next() {
                            Some(fb) if ('\x40'..='\x7e').contains(&fb) => {
                                final_byte = fb;
                                break;
                            }
                            Some(pb) => params.push(pb),
                            None => { final_byte = '\0'; break; }
                        }
                    }
                    // CSI n C = cursor forward n columns → emit n spaces.
                    // An empty parameter means 1 (the default for CUF).
                    if final_byte == 'C' {
                        let n: usize = params.parse().unwrap_or(1);
                        for _ in 0..n {
                            out.push(' ');
                        }
                    }
                    // All other CSI sequences (colors, cursor positioning, etc.)
                    // are silently dropped.
                }
                Some(']') => {
                    let mut prev = '\0';
                    for c2 in chars.by_ref() {
                        if c2 == '\x07' || (prev == '\x1b' && c2 == '\\') {
                            break;
                        }
                        prev = c2;
                    }
                }
                Some(c2) if ('\x40'..='\x5f').contains(&c2) => {}
                _ => {}
            }
        } else if !c.is_control() {
            out.push(c);
        }
    }

    // Extract content between ``` fences.
    if let Some(start) = out.find("```") {
        let after_open = start + 3;
        if let Some(end) = out[after_open..].find("```") {
            return out[after_open..after_open + end].trim().to_string();
        }
    }

    out.trim().to_string()
}

/// Strip ```action ... ```, <tool_call>...</tool_call>, and
/// <tool_result>...</tool_result> blocks from text, returning only the
/// prose/reasoning portion for chat display (dispatch-chat).
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
    // Remove <tool_result>...</tool_result> blocks
    while let Some(start) = result.find("<tool_result>") {
        if let Some(end) = result.find("</tool_result>") {
            result.replace_range(start..end + "</tool_result>".len(), "");
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_dispatch_msg_fenced() {
        assert_eq!(
            clean_dispatch_msg("```Task received. Working on it now.```"),
            "Task received. Working on it now."
        );
    }

    #[test]
    fn clean_dispatch_msg_fenced_with_trailing_noise() {
        assert_eq!(
            clean_dispatch_msg("```Task received.``` Ruminating... + a Q n"),
            "Task received."
        );
    }

    #[test]
    fn clean_dispatch_msg_fenced_with_ansi() {
        assert_eq!(
            clean_dispatch_msg("\x1b[0m```Done. Fixed the bug.```\x1b[10CRuminating..."),
            "Done. Fixed the bug."
        );
    }

    #[test]
    fn clean_dispatch_msg_fenced_with_thinking_noise() {
        assert_eq!(
            clean_dispatch_msg("```Task complete.``` (thinking with high effort)"),
            "Task complete."
        );
    }

    #[test]
    fn clean_dispatch_msg_empty_input() {
        assert_eq!(clean_dispatch_msg(""), "");
    }

    #[test]
    fn clean_dispatch_msg_strips_ansi() {
        assert_eq!(
            clean_dispatch_msg("\x1b[0m```Task received.```\x1b[0m"),
            "Task received."
        );
    }

    #[test]
    fn clean_dispatch_msg_unfenced_passthrough() {
        // Unfenced messages pass through with ANSI stripped.
        assert_eq!(
            clean_dispatch_msg("Task received."),
            "Task received."
        );
    }

    #[test]
    fn clean_dispatch_msg_conpty_cursor_forward_to_spaces() {
        // ConPTY replaces spaces with CSI C (cursor forward). Verify they
        // become spaces so agent messages retain their original spacing.
        assert_eq!(
            clean_dispatch_msg("Task\x1b[1Creceived.\x1b[1CWorking\x1b[1Con\x1b[1Cit\x1b[1Cnow."),
            "Task received. Working on it now."
        );
    }

    #[test]
    fn clean_dispatch_msg_conpty_cursor_forward_default() {
        // CSI C with no parameter defaults to 1 space.
        assert_eq!(
            clean_dispatch_msg("Hello\x1b[Cworld"),
            "Hello world"
        );
    }

    #[test]
    fn clean_dispatch_msg_conpty_cursor_forward_multi() {
        // CSI 3 C = 3 spaces.
        assert_eq!(
            clean_dispatch_msg("A\x1b[3CB"),
            "A   B"
        );
    }

    #[test]
    fn clean_dispatch_msg_conpty_fenced_with_cursor_forward() {
        // Fenced message where ConPTY replaced spaces with cursor-forward.
        assert_eq!(
            clean_dispatch_msg("```Task\x1b[Creceived.```"),
            "Task received."
        );
    }
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

