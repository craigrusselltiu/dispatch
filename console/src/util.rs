// Standalone utility functions for the dispatch console.

use if_addrs::{get_if_addrs, IfAddr};
use std::{net::Ipv4Addr, time::Duration};
use time::macros::format_description;

/// Format local time as "HH:MM:SS". Falls back to UTC if local offset unavailable.
pub fn local_time_hms() -> String {
    let fmt = format_description!("[hour]:[minute]:[second]");
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(fmt)
        .unwrap_or_default()
}

/// Format local time as "HH:MM". Falls back to UTC if local offset unavailable.
pub fn local_time_hm() -> String {
    let fmt = format_description!("[hour]:[minute]");
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(fmt)
        .unwrap_or_default()
}

/// Format local time as "YYYYMMDD_HHMMSS" for file names. Falls back to UTC.
pub fn local_timestamp_file() -> String {
    let fmt = format_description!("[year][month][day]_[hour][minute][second]");
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(fmt)
        .unwrap_or_default()
}

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
/// Uses char count (not byte length) for the comparison and char
/// boundaries for slicing to avoid panicking on multi-byte UTF-8.
pub fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
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

/// Detect the machine's local network IP.
/// Prefers a physical LAN IPv4 over VPN or virtual adapters when possible.
pub fn local_ip() -> Option<String> {
    preferred_lan_ipv4()
        .map(|ip| ip.to_string())
        .or_else(route_local_ip)
}

/// Pick the best LAN IPv4 to show in the UI and advertise over mDNS.
///
/// We prefer RFC1918 addresses on physical Ethernet/Wi-Fi interfaces and
/// penalize VPN and virtual adapters. This avoids resolving the console to
/// WARP, Hyper-V, Docker, or other non-LAN interfaces on Windows machines.
pub fn preferred_lan_ipv4() -> Option<Ipv4Addr> {
    let mut candidates = get_if_addrs()
        .ok()?
        .into_iter()
        .filter_map(|interface| match interface.addr {
            IfAddr::V4(addr) => {
                let score = score_ipv4_candidate(&interface.name, addr.ip)?;
                Some((score, interface.name, addr.ip))
            }
            IfAddr::V6(_) => None,
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|a, b| a.cmp(b));
    candidates.pop().map(|(_, _, ip)| ip)
}

fn route_local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip().to_string())
}

fn score_ipv4_candidate(interface_name: &str, ip: Ipv4Addr) -> Option<i32> {
    if ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
    {
        return None;
    }

    let mut score = if ip.is_private() {
        400
    } else if is_cgnat(ip) {
        -200
    } else {
        50
    };

    if looks_virtual_interface(interface_name) {
        score -= 300;
    } else if looks_physical_interface(interface_name) {
        score += 100;
    }

    Some(score)
}

fn looks_physical_interface(interface_name: &str) -> bool {
    let name = interface_name.to_ascii_lowercase();
    name.contains("ethernet")
        || name.contains("wi-fi")
        || name.contains("wifi")
        || name.starts_with("en")
        || name.starts_with("eth")
        || name.starts_with("wl")
}

fn looks_virtual_interface(interface_name: &str) -> bool {
    let name = interface_name.to_ascii_lowercase();
    [
        "warp",
        "cloudflare",
        "vethernet",
        "hyper-v",
        "virtual",
        "vmware",
        "virtualbox",
        "vbox",
        "docker",
        "podman",
        "tailscale",
        "zerotier",
        "wireguard",
        "loopback",
        "bluetooth",
        "bridge",
        "hamachi",
        "tun",
        "tap",
    ]
    .iter()
    .any(|keyword| name.contains(keyword))
}

fn is_cgnat(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
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

#[cfg(test)]
mod tests {
    use super::*;

    // -- truncate --

    #[test]
    fn truncate_ascii_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_ascii_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_ascii_over() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_emoji_no_panic() {
        // 4 emoji = 4 chars, max=10 should not truncate
        let s = "\u{1F600}\u{1F601}\u{1F602}\u{1F603}";
        assert_eq!(truncate(s, 10), s);
    }

    #[test]
    fn truncate_emoji_truncates() {
        // 6 emoji = 6 chars, max=5 should truncate to 2 emoji + "..."
        let s = "\u{1F600}\u{1F601}\u{1F602}\u{1F603}\u{1F604}\u{1F605}";
        let result = truncate(s, 5);
        assert_eq!(result, "\u{1F600}\u{1F601}...");
        assert_eq!(result.chars().count(), 5);
    }

    #[test]
    fn truncate_cjk_no_panic() {
        let s = "\u{4F60}\u{597D}\u{4E16}\u{754C}"; // 4 CJK chars
        assert_eq!(truncate(s, 4), s);
    }

    #[test]
    fn truncate_cjk_truncates() {
        let s = "\u{4F60}\u{597D}\u{4E16}\u{754C}\u{FF01}"; // 5 CJK chars
        let result = truncate(s, 4);
        assert_eq!(result.chars().count(), 4); // 1 char + "..."
    }

    #[test]
    fn truncate_multibyte_not_falsely_truncated() {
        // "cafe\u{0301}" = 5 chars, 6 bytes. max=5 should NOT truncate.
        let s = "caf\u{00E9}x"; // 5 chars, 6 bytes (e-acute is 2 bytes)
        assert_eq!(truncate(s, 5), s);
    }

    #[test]
    fn truncate_max_zero() {
        assert_eq!(truncate("hello", 0), "");
    }

    #[test]
    fn truncate_max_three() {
        assert_eq!(truncate("hello", 3), "hel");
    }

    // -- strip_action_blocks --

    #[test]
    fn strip_action_blocks_no_blocks() {
        assert_eq!(strip_action_blocks("hello world"), "hello world");
    }

    #[test]
    fn strip_action_blocks_action_block() {
        let input = "before```action\ndo stuff\n```after";
        assert_eq!(strip_action_blocks(input), "beforeafter");
    }

    #[test]
    fn strip_action_blocks_tool_call() {
        let input = "start<tool_call>payload</tool_call>end";
        assert_eq!(strip_action_blocks(input), "startend");
    }

    #[test]
    fn strip_action_blocks_tool_result() {
        let input = "a<tool_result>data</tool_result>b";
        assert_eq!(strip_action_blocks(input), "ab");
    }

    #[test]
    fn strip_action_blocks_multiple() {
        let input = "x<tool_call>a</tool_call>y<tool_result>b</tool_result>z";
        assert_eq!(strip_action_blocks(input), "xyz");
    }

    #[test]
    fn strip_action_blocks_preserves_ascii() {
        let plain = "The quick brown fox jumps over the lazy dog.";
        assert_eq!(strip_action_blocks(plain), plain);
    }

    #[test]
    fn prefers_physical_private_ip_over_vpn_ip() {
        let ethernet = score_ipv4_candidate("Ethernet 5", Ipv4Addr::new(10, 61, 5, 37)).unwrap();
        let warp = score_ipv4_candidate("CloudflareWARP", Ipv4Addr::new(100, 96, 2, 212)).unwrap();
        assert!(ethernet > warp);
    }

    #[test]
    fn penalizes_virtual_private_interfaces() {
        let ethernet = score_ipv4_candidate("Ethernet 5", Ipv4Addr::new(10, 61, 5, 37)).unwrap();
        let hyper_v =
            score_ipv4_candidate("vEthernet (Default Switch)", Ipv4Addr::new(172, 24, 160, 1))
                .unwrap();
        assert!(ethernet > hyper_v);
    }

    #[test]
    fn rejects_loopback_and_link_local_ips() {
        assert!(score_ipv4_candidate("Ethernet 5", Ipv4Addr::new(127, 0, 0, 1)).is_none());
        assert!(score_ipv4_candidate("Ethernet 5", Ipv4Addr::new(169, 254, 1, 5)).is_none());
    }
}
