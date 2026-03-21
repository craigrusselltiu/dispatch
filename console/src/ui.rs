// UI rendering: header, footer, panes, overlays, orchestrator view.

use dispatch_core::orchestrator;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::types::*;
use crate::util::{format_runtime, local_ip, repo_name_from_path, truncate};

// ── VT100 screen conversion ────────────────────────────────────────────────

fn vt100_color_to_ratatui(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

pub fn screen_to_lines(screen: &vt100::Screen) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in 0..screen.size().0 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_text = String::new();
        let mut current_style = Style::default();

        for col in 0..screen.size().1 {
            let cell = screen.cell(row, col).unwrap();
            let mut style = Style::default();
            if let Some(fg) = vt100_color_to_ratatui(cell.fgcolor()) {
                style = style.fg(fg);
            }
            if let Some(bg) = vt100_color_to_ratatui(cell.bgcolor()) {
                style = style.bg(bg);
            }
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }

            let ch = cell.contents();
            let ch = if ch.is_empty() { " ".to_string() } else { ch };

            if style == current_style {
                current_text.push_str(&ch);
            } else {
                if !current_text.is_empty() {
                    spans.push(Span::styled(current_text.clone(), current_style));
                    current_text.clear();
                }
                current_text = ch;
                current_style = style;
            }
        }
        if !current_text.is_empty() {
            spans.push(Span::styled(current_text, current_style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

// ── rendering ─────────────────────────────────────────────────────────────────

/// Render the LED-style scrolling ticker line (dispatch-ami).
pub fn render_ticker(f: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let text = app.ticker_display(width);
    let style = Style::default().fg(Color::Yellow);
    f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
}

pub fn render_header(f: &mut Frame, area: Rect, app: &App) {
    // Status indicator pulses like a REC light: dot blinks on/off, text stays visible.
    let blink_on = app.status_blink_on();
    let (dot, dot_style, text, text_style) = match app.radio_state {
        RadioState::Connected => {
            let dot_char = if blink_on { "● " } else { "  " };
            (
                dot_char,
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                "CONNECTED",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )
        }
        RadioState::Disconnected => {
            let dot_char = if blink_on { "● " } else { "  " };
            (
                dot_char,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                "DISCONNECTED",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )
        }
    };

    let clock = chrono::Local::now().format("%H:%M").to_string();
    // dispatch-sa1: show repo count in multi-repo mode.
    let workspace_indicator = if app.is_multi_repo() {
        format!("  REPOS: {}  ", app.repo_list().len())
    } else {
        String::new()
    };
    // dispatch-h62: orchestrator status indicator.
    let orch_indicator = match &app.orchestrator {
        Some(o) if o.is_alive() => match o.state {
            orchestrator::OrchestratorState::Idle => "  ORCH: IDLE",
            orchestrator::OrchestratorState::Responding => "  ORCH: THINKING",
            orchestrator::OrchestratorState::Dead => "  ORCH: DEAD",
        },
        Some(_) => "  ORCH: DEAD",
        None => "  ORCH: READY",
    };
    let right = format!(
        "PSK: {}  AGENTS: {}/{}{}{}  PAGE {}/{}  {}",
        app.psk_display(),
        app.active_count(),
        app.slots.len(),
        workspace_indicator,
        orch_indicator,
        app.current_page + 1,
        app.total_pages(),
        clock,
    );

    // Build left and right portions, pad gap to right-align, and truncate to fit.
    let left_text = " RADIO: ";
    let radio_text = match app.radio_state {
        RadioState::Connected => "● CONNECTED",
        RadioState::Disconnected => "● DISCONNECTED",
    };
    let inner_width = area.width.saturating_sub(2) as usize; // minus border chars
    let left_len = left_text.len() + radio_text.len();
    // Truncate right side if it doesn't fit.
    let max_right = inner_width.saturating_sub(left_len + 1);
    let right_truncated: String = right.chars().take(max_right).collect();
    let used = left_len + right_truncated.len();
    let gap = if inner_width > used { inner_width - used } else { 1 };
    let right_padded = format!("{}{}", " ".repeat(gap), right_truncated);

    let status_line = Line::from(vec![
        Span::raw(left_text),
        Span::styled(dot, dot_style),
        Span::styled(text, text_style),
        Span::styled(right_padded, Style::default().fg(Color::White)),
    ]);

    let block = Block::default()
        .title(Span::styled(
            " DISPATCH ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(status_line), inner);
}

fn pane_info_strip(global_idx: usize, local_idx: usize, app: &App) -> Text<'static> {
    let slot_num = global_idx + 1;
    let is_target = app.target == local_idx;

    let marker_str = if is_target { "▸ " } else { "  " };
    let marker_style = if is_target {
        match app.mode {
            Mode::Command => Style::default().fg(Color::Cyan),
            Mode::Input => Style::default().fg(Color::Green),
        }
    } else {
        Style::default()
    };

    match &app.slots[global_idx] {
        None => {
            let line1 = Line::from(vec![
                Span::styled(marker_str.to_string(), marker_style),
                Span::styled(
                    format!("[{}] -- STANDBY --", slot_num),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            let sep = Line::from(Span::styled(
                "┄".repeat(40),
                Style::default().fg(Color::DarkGray),
            ));
            Text::from(vec![line1, Line::default(), sep])
        }
        Some(agent) => {
            let name_style = if is_target {
                Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            };
            // Activity indicator: shows WORK or IDLE based on PTY output.
            let (activity_label, activity_style) = if agent.task_id.is_some() && !agent.idle {
                ("WORK", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
            } else {
                ("IDLE", Style::default().fg(Color::DarkGray))
            };
            let runtime = format_runtime(agent.dispatch_time.elapsed());
            let line1 = Line::from(vec![
                Span::styled(marker_str.to_string(), marker_style),
                Span::styled(
                    format!("[{}] {}", slot_num, agent.display_name()),
                    name_style,
                ),
                Span::styled(" ", Style::default()),
                Span::styled(activity_label, activity_style),
                Span::styled(
                    format!("  @{} {}", agent.dispatch_wall_str, runtime),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            let task_span = match &agent.task_id {
                Some(id) => Span::styled(id.clone(), Style::default().fg(Color::Yellow)),
                None => Span::styled("idle", Style::default().fg(Color::DarkGray)),
            };
            let line2 = Line::from(vec![
                Span::styled(
                    format!("  {} | ", agent.tool.to_uppercase()),
                    Style::default().fg(Color::DarkGray),
                ),
                task_span,
            ]);
            let sep = Line::from(Span::styled(
                "┄".repeat(40),
                Style::default().fg(Color::DarkGray),
            ));
            Text::from(vec![line1, line2, sep])
        }
    }
}

fn standby_body(_global_idx: usize, _app: &App) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " STANDBY",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Console dispatches agents via voice.",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

fn render_pane(
    f: &mut Frame,
    area: Rect,
    local_idx: usize,
    global_idx: usize,
    app: &App,
    vt_lines: Option<Vec<Line<'static>>>,
    scrolled: bool,
) {
    let is_target = app.target == local_idx;
    let border_style = if is_target {
        match app.mode {
            Mode::Command => Style::default().fg(Color::Cyan),
            Mode::Input => Style::default().fg(Color::Green),
        }
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(inner);

    f.render_widget(Paragraph::new(pane_info_strip(global_idx, local_idx, app)), chunks[0]);

    if let Some(lines) = vt_lines {
        f.render_widget(Paragraph::new(Text::from(lines)), chunks[1]);
        // dispatch-ct2.4: show scroll indicator when not at bottom
        if scrolled {
            let indicator = Span::styled(
                " SCROLL ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            );
            let x = chunks[1].right().saturating_sub(9);
            let y = chunks[1].bottom().saturating_sub(1);
            if x >= chunks[1].x && y >= chunks[1].y {
                f.render_widget(Paragraph::new(Line::from(indicator)), Rect::new(x, y, 8, 1));
            }
        }
    } else {
        f.render_widget(Paragraph::new(standby_body(global_idx, app)), chunks[1]);
    }
}

/// Render the 2x2 quad pane for the current page (dispatch-bgz.1).
pub fn render_panes(f: &mut Frame, area: Rect, app: &App) {
    let page_start = app.current_page * SLOTS_PER_PAGE;

    // Pre-compute vt lines for each visible slot (hold locks briefly, then release).
    // dispatch-ct2.4: set scrollback offset before reading, then restore to 0.
    let mut page_lines: [Option<Vec<Line<'static>>>; SLOTS_PER_PAGE] =
        [None, None, None, None];
    let mut page_scrolled: [bool; SLOTS_PER_PAGE] = [false; SLOTS_PER_PAGE];
    for local in 0..SLOTS_PER_PAGE {
        let g = page_start + local;
        if g < app.slots.len() {
            if let Some(slot) = &app.slots[g] {
                let mut parser = slot.screen.lock().unwrap();
                parser.set_scrollback(slot.scroll_offset);
                page_lines[local] = Some(screen_to_lines(parser.screen()));
                page_scrolled[local] = slot.scroll_offset > 0;
                parser.set_scrollback(0);
            }
        }
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cols[0]);
    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cols[1]);

    // top-left=0, top-right=1, bottom-left=2, bottom-right=3
    let areas = [left_rows[0], right_rows[0], left_rows[1], right_rows[1]];
    for local in 0..SLOTS_PER_PAGE {
        let g = page_start + local;
        if g < app.slots.len() {
            render_pane(f, areas[local], local, g, app, page_lines[local].take(), page_scrolled[local]);
        }
    }
}

/// Render the orchestrator conversation log view (dispatch-6nm).
/// Replaces the panes area when ViewMode::Orchestrator is active.
pub fn render_orchestrator(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(Span::styled(
            " ORCHESTRATOR ",
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.orch_log.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            " No events yet. The orchestrator log will show voice transcripts, reasoning, and tool calls.",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(empty, inner);
        return;
    }

    // Build lines from events.
    let mut lines: Vec<Line<'static>> = Vec::new();
    for ev in &app.orch_log {
        let (icon, style, body) = match &ev.kind {
            OrchestratorEventKind::VoiceTranscript { text } => (
                "MIC",
                Style::default().fg(Color::Green),
                format!("\"{}\"", text),
            ),
            OrchestratorEventKind::TaskAssigned { id, agent, slot } => (
                "ASSIGN",
                Style::default().fg(Color::Yellow),
                format!("{} -> {} (slot {})", id, agent, slot),
            ),
            OrchestratorEventKind::TaskComplete { id, agent } => (
                "DONE",
                Style::default().fg(Color::Green),
                format!("{} completed by {}", id, agent),
            ),
            OrchestratorEventKind::Merged { id } => (
                "MERGE",
                Style::default().fg(Color::Green),
                format!("{} merged to main", id),
            ),
            OrchestratorEventKind::MergeConflict { id } => (
                "CONFLICT",
                Style::default().fg(Color::Red),
                format!("{} has merge conflicts", id),
            ),
            OrchestratorEventKind::Dispatched { agent, slot, tool } => (
                "DISPATCH",
                Style::default().fg(Color::Cyan),
                format!("{} in slot {} ({})", agent, slot, tool),
            ),
            OrchestratorEventKind::Terminated { agent, slot } => (
                "TERM",
                Style::default().fg(Color::Red),
                format!("{} (slot {})", agent, slot),
            ),
            OrchestratorEventKind::OrchestratorText { text } => (
                "LLM",
                Style::default().fg(Color::Magenta),
                truncate(text, 120).to_string(),
            ),
            OrchestratorEventKind::ToolCallIssued { name } => (
                "TOOL",
                Style::default().fg(Color::Yellow),
                format!("-> {}", name),
            ),
            OrchestratorEventKind::ToolResultSent { name, success } => (
                "RESULT",
                if *success { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Red) },
                format!("<- {} {}", name, if *success { "ok" } else { "error" }),
            ),
            OrchestratorEventKind::AgentMessage { agent, text } => (
                "AGENT",
                Style::default().fg(Color::Blue),
                format!("{}: {}", agent, text),
            ),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", ev.time),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(format!("{:<8} ", icon), style.add_modifier(Modifier::BOLD)),
            Span::styled(body, Style::default().fg(Color::White)),
        ]));
    }

    // Apply scroll from bottom.
    let visible = inner.height as usize;
    let total = lines.len();
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.orch_scroll.min(max_scroll);
    let start = total.saturating_sub(visible + scroll);
    let end = (start + visible).min(total);
    let visible_lines: Vec<Line<'static>> = lines[start..end].to_vec();

    let paragraph = Paragraph::new(Text::from(visible_lines));
    f.render_widget(paragraph, inner);

    // Scroll indicator on the right edge.
    if max_scroll > 0 {
        let pct = if scroll == 0 { 100 } else { ((max_scroll - scroll) * 100) / max_scroll };
        let indicator = format!(" {}% ", pct);
        let indicator_area = Rect {
            x: inner.x + inner.width.saturating_sub(indicator.len() as u16 + 1),
            y: area.y,
            width: indicator.len() as u16,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Span::styled(indicator, Style::default().fg(Color::DarkGray))),
            indicator_area,
        );
    }
}

pub fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let target_g = app.target_global();
    let target_name = app
        .slots
        .get(target_g)
        .and_then(|s| s.as_ref())
        .map(|a| a.display_name().to_string())
        .unwrap_or_else(|| format!("Slot {}", target_g + 1));

    let content = match app.mode {
        Mode::Command => {
            let view_indicator = match app.view_mode {
                ViewMode::Agents => "",
                ViewMode::Orchestrator => "ORCH ",
            };
            let hints = if app.view_mode == ViewMode::Orchestrator {
                " o:back  ?:help  q:quit"
            } else {
                " Enter:input  k:kill  o:orch  ?:help  q:quit"
            };
            Line::from(vec![
                Span::styled(
                    format!(" {} ▸ {} ", view_indicator, target_name),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled("│", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    hints,
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
        Mode::Input => Line::from(vec![
            Span::styled(
                format!(" INPUT [{}] ", target_name),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled("│", Style::default().fg(Color::DarkGray)),
            Span::styled(
                " ESC:exit  ESC ESC:send Esc to PTY",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    };

    f.render_widget(Paragraph::new(content), area);
}

// ── overlays ──────────────────────────────────────────────────────────────────

pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

pub fn render_help_overlay(f: &mut Frame, area: Rect) {
    let r = centered_rect(52, 21, area);
    f.render_widget(Clear, r);
    let lines = vec![
        Line::from(Span::styled(
            " COMMAND MODE KEYS ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(Span::raw("  Enter        Enter input mode")),
        Line::from(Span::raw("  1-4          Select slot on current page")),
        Line::from(Span::raw("  Tab          Next slot (all pages)")),
        Line::from(Span::raw("  Shift+Tab    Prev slot (all pages)")),
        Line::from(Span::raw("  → / ←        Next / prev page")),
        Line::from(Span::raw("  PgUp / PgDn  Scroll output")),
        Line::from(Span::raw("  ↑ / ↓        Scroll orchestrator view")),
        Line::from(Span::raw("  k            Kill target agent")),
        Line::from(Span::raw("  o            Toggle orchestrator view")),
        Line::from(Span::raw("  p            Toggle PSK visibility")),
        Line::from(Span::raw("  x            Show connection info")),
        Line::from(Span::raw("  q            Quit (confirms if agents running)")),
        Line::from(Span::raw("  ?            This help screen")),
        Line::default(),
        Line::from(Span::styled(
            "  INPUT MODE",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("  Esc          Return to command mode (immediate)")),
        Line::from(Span::raw("  Esc Esc      Send literal Escape to PTY (quick double-tap)")),
        Line::default(),
        Line::from(Span::styled(
            "  Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" HELP ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        ),
        r,
    );
}

/// Render a connection info overlay showing address, port, and PSK (dispatch-b54).
pub fn render_connection_info_overlay(f: &mut Frame, area: Rect, app: &App) {
    let host = local_ip().unwrap_or_else(|| "127.0.0.1".to_string());

    let lines = vec![
        Line::from(Span::styled(
            " CONNECTION INFO ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("  Address:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(&host),
        ]),
        Line::from(vec![
            Span::styled("  Port:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", app.port)),
        ]),
        Line::from(vec![
            Span::styled("  PSK:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(&app.psk),
        ]),
        Line::default(),
        Line::from(Span::styled(
            "  Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let content_height = lines.len() as u16 + 2;
    let r = centered_rect(46, content_height, area);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" CONNECTION ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        ),
        r,
    );
}

pub fn render_confirm_overlay(f: &mut Frame, area: Rect, title: &str, body: &str) {
    let r = centered_rect(50, 7, area);
    f.render_widget(Clear, r);
    let lines = vec![
        Line::default(),
        Line::from(Span::styled(format!("  {}", body), Style::default().fg(Color::White))),
        Line::default(),
        Line::from(vec![
            Span::styled("  y ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled("confirm    ", Style::default().fg(Color::DarkGray)),
            Span::styled("n / Esc ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ]),
        Line::default(),
    ];
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(format!(" {} ", title))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        r,
    );
}

/// Render the repo selection overlay for multi-repo mode.
pub fn render_repo_select_overlay(f: &mut Frame, area: Rect, app: &App) {
    let repos = app.repo_list();
    let height = (repos.len() as u16 + 5).min(area.height.saturating_sub(4));
    let r = centered_rect(60, height, area);
    f.render_widget(Clear, r);
    let mut lines = vec![Line::default()];
    for (i, repo) in repos.iter().enumerate() {
        let name = repo_name_from_path(repo);
        let marker = if i == app.repo_select_idx { ">" } else { " " };
        let style = if i == app.repo_select_idx {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!("  {} {}  {}", marker, i + 1, name),
            style,
        )));
    }
    if repos.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no git repos found in child directories)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  Enter select    j/k navigate    r rescan    Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::default());
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" SELECT REPO ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        r,
    );
}

