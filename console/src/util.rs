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

/// Clean a dispatch message: strip ANSI escapes and control chars,
/// handle cursor-movement sequences (which may indicate terminal noise
/// like status bars rendered after the message), and trim shell artifacts
/// like trailing `")` from echo output.
pub fn clean_dispatch_msg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    // Track the first cursor-movement-after-punctuation position as a
    // candidate truncation point. We defer the decision until the end so
    // multi-sentence messages ("Task received. Working on it now.") are
    // preserved while terminal noise ("Done.\x1b[10Cthinking...") is stripped.
    let mut truncate_candidate: Option<usize> = None;
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    // CSI sequence: collect params, then check final byte.
                    let mut _param = String::new();
                    loop {
                        match chars.next() {
                            Some(fb) if ('\x40'..='\x7e').contains(&fb) => {
                                if matches!(fb, 'A' | 'B' | 'C' | 'D' | 'H' | 'f')
                                    && !out.trim().is_empty()
                                {
                                    // Record first cursor-movement after sentence
                                    // punctuation as a potential truncation point.
                                    if truncate_candidate.is_none()
                                        && out.trim_end().ends_with(|c: char| c == '.' || c == '!' || c == '?')
                                    {
                                        truncate_candidate = Some(out.len());
                                    }
                                    // Insert space to preserve word boundaries —
                                    // ConPTY uses cursor positioning instead of
                                    // spaces within a line.
                                    if !out.is_empty() && !out.ends_with(' ') {
                                        out.push(' ');
                                    }
                                }
                                break;
                            }
                            Some(c2) => _param.push(c2),
                            None => break,
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence: consume until ST (ESC \ or BEL)
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
            // Collapse runs of whitespace to a single space.
            if c == ' ' && out.ends_with(' ') {
                continue;
            }
            out.push(c);
        }
    }
    // If we recorded a candidate truncation point, only truncate if the
    // text after it contains no sentence-ending punctuation (terminal noise).
    // Multi-sentence messages will have punctuation in both halves.
    if let Some(trunc_pos) = truncate_candidate {
        let tail = &out[trunc_pos..];
        if !tail.contains('.') && !tail.contains('!') && !tail.contains('?') {
            out.truncate(trunc_pos);
        }
    }
    // Truncate at closing ") from echo command — everything after is terminal noise.
    let out = out.trim();
    let out = match out.find("\")") {
        Some(pos) => &out[..pos],
        None => out,
    };
    // Strip trailing quote characters left over from shell command echo
    // (e.g. `echo "@@DISPATCH_MSG:msg"` output may include trailing `"`).
    let out = out.trim_end_matches('"').trim_end_matches('\'');
    // Strip any trailing non-punctuation characters that follow the last
    // sentence-ending punctuation — these are terminal noise (e.g. "now.U").
    let out = out.trim();
    if let Some(end) = out.rfind(|c| c == '.' || c == '!' || c == '?') {
        // Only truncate if the trailing chars are short (noise), not a whole word.
        let tail = &out[end + 1..];
        if !tail.is_empty() && tail.len() <= 3 && tail.chars().all(|c| c.is_ascii_alphanumeric()) {
            return out[..=end].trim().to_string();
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
    fn clean_dispatch_msg_plain() {
        assert_eq!(
            clean_dispatch_msg("Task received. Working on it now."),
            "Task received. Working on it now."
        );
    }

    #[test]
    fn clean_dispatch_msg_strips_trailing_double_quote() {
        // From command echo: echo "@@DISPATCH_MSG:msg" leaves trailing "
        assert_eq!(
            clean_dispatch_msg("Task received.\""),
            "Task received."
        );
    }

    #[test]
    fn clean_dispatch_msg_strips_trailing_single_quote() {
        assert_eq!(
            clean_dispatch_msg("Task received.'"),
            "Task received."
        );
    }

    #[test]
    fn clean_dispatch_msg_strips_ansi() {
        assert_eq!(
            clean_dispatch_msg("\x1b[0mTask received.\x1b[0m"),
            "Task received."
        );
    }

    #[test]
    fn clean_dispatch_msg_truncates_at_close_paren_quote() {
        assert_eq!(
            clean_dispatch_msg("Task received.\")extra noise"),
            "Task received."
        );
    }

    #[test]
    fn clean_dispatch_msg_empty_input() {
        assert_eq!(clean_dispatch_msg(""), "");
    }

    #[test]
    fn clean_dispatch_msg_preserves_internal_quotes() {
        // Quotes in the middle of the message should be preserved.
        assert_eq!(
            clean_dispatch_msg("Fixed the \"login\" bug."),
            "Fixed the \"login\" bug."
        );
    }

    #[test]
    fn clean_dispatch_msg_truncates_at_cursor_forward() {
        // Cursor-forward (\x1b[10C) followed by status bar text is terminal noise.
        assert_eq!(
            clean_dispatch_msg("Done. Added timestamps.\x1b[10Cthinking with high effort"),
            "Done. Added timestamps."
        );
    }

    #[test]
    fn clean_dispatch_msg_truncates_at_cursor_position() {
        // Cursor-position (\x1b[1;40H) followed by noise.
        assert_eq!(
            clean_dispatch_msg("Task complete.\x1b[1;40Hstatus text"),
            "Task complete."
        );
    }

    #[test]
    fn clean_dispatch_msg_ignores_cursor_move_before_content() {
        // Cursor movement before any message content should not truncate.
        assert_eq!(
            clean_dispatch_msg("\x1b[CTask received."),
            "Task received."
        );
    }

    #[test]
    fn clean_dispatch_msg_skips_cursor_move_mid_message() {
        // TUI redraws can insert cursor positioning mid-message (e.g. Ink
        // layout repositioning). Cursor movement after non-punctuation
        // content should NOT truncate — only after a complete sentence.
        assert_eq!(
            clean_dispatch_msg("Task\x1b[15;20H received. Working on it now."),
            "Task received. Working on it now."
        );
    }

    #[test]
    fn clean_dispatch_msg_skips_cursor_move_after_partial_word() {
        // Cursor movement after a partial word (no punctuation) should not truncate.
        assert_eq!(
            clean_dispatch_msg("Working\x1b[10C on the fix."),
            "Working on the fix."
        );
    }

    #[test]
    fn clean_dispatch_msg_preserves_multi_sentence() {
        // Cursor movement between sentences should not truncate when both
        // halves contain sentence-ending punctuation.
        assert_eq!(
            clean_dispatch_msg("Task received.\x1b[1;30H Working on it now."),
            "Task received. Working on it now."
        );
    }

    #[test]
    fn clean_dispatch_msg_preserves_long_multi_sentence() {
        // Multi-sentence messages with cursor repositioning throughout.
        assert_eq!(
            clean_dispatch_msg("Done.\x1b[1;20H Fixed the bug.\x1b[1;40H Pushed to remote."),
            "Done. Fixed the bug. Pushed to remote."
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

