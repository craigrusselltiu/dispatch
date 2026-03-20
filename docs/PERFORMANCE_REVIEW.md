# Performance Review: Dispatch Console

**Date:** 2026-03-20
**Reviewer:** Bravo (automated analysis)

## Executive Summary

The dispatch console is a well-structured Rust TUI (~1500 LOC) with clean separation between PTY management, UI rendering, orchestrator logic, and WebSocket serving. The codebase follows the stated design philosophy of simplicity. However, several patterns cause unnecessary allocations, redundant work per frame, and mutex contention that affect responsiveness under load (multiple active agents producing output).

The most impactful issues are: unconditional full redraws at 60fps, allocation-heavy VT100 screen conversion every frame, O(n) removals from Vec heads, and missing dirty-tracking to skip idle frames.

---

## 1. Render Loop: Unconditional 60fps Full Redraw

**Files:** `console/src/main.rs` lines 334-373, `console/src/ui.rs` lines 310-353

**Issue:** The main loop calls `terminal.draw()` every iteration (~16ms / 60fps) regardless of whether anything changed. Every frame:
- Computes layout splits (4 `Layout::split()` calls)
- Locks all 4 visible VT100 screen mutexes
- Converts all 4 screens cell-by-cell to ratatui Lines
- Builds header, footer, ticker, and overlay widgets
- Ratatui diffs against the previous frame and flushes to stdout

When agents are idle and no input arrives, this is pure waste -- the output is identical frame to frame.

**Recommendation:** Add a dirty flag. Set it when: PTY output arrives, user input occurs, ticker advances, resize happens, or overlay changes. Skip `terminal.draw()` when clean. This alone could cut CPU usage 50-90% during idle periods. A simpler alternative: increase the poll timeout to 100ms when idle (10fps) and drop to 16ms only when PTY output is flowing.

**Impact:** HIGH -- affects baseline CPU usage at all times.

---

## 2. screen_to_lines() Allocation Storm

**File:** `console/src/ui.rs` lines 25-71

**Issue:** This function is called for each visible pane every frame. For a typical 80x40 pane, it:
- Iterates 3200 cells (80 cols x 40 rows)
- Calls `cell.contents()` which returns a `String` allocation per cell (line 51)
- Calls `.to_string()` on empty cell contents: `" ".to_string()` (line 52) -- 3200 String allocations per pane in the worst case
- Clones `current_text` when style changes (line 58): `Span::styled(current_text.clone(), ...)`
- Builds a `Vec<Span>` per row, then a `Vec<Line>` for the whole screen

With 4 active panes, this is ~12,800 cell accesses and thousands of String/Span allocations per frame, 60 times per second.

**Recommendations:**
1. Replace `current_text.clone()` on line 58 with `std::mem::take(&mut current_text)` to avoid the clone -- the old value is cleared immediately after anyway.
2. Pre-allocate the `spans` vector with `Vec::with_capacity(screen.size().1 as usize)` since worst case is one span per column.
3. Consider caching the converted lines and only reconverting when the screen mutex shows new data has arrived (via a generation counter or dirty flag on the PTY reader side).

**Impact:** HIGH -- this is the hottest path in the application.

---

## 3. Mutex Contention Between PTY Reader and Render Thread

**Files:** `console/src/pty.rs` line 114, `console/src/ui.rs` lines 322-326

**Issue:** The PTY reader thread (pty.rs:114) locks the screen mutex and calls `parser.process(&buf[..n])` while holding it. The render thread (ui.rs:322) locks the same mutex to read the screen. If a large chunk of PTY output arrives (e.g., a long `git log`), `process()` can take significant time, during which the render thread blocks, causing visible frame drops.

Conversely, `screen_to_lines()` holds the lock for the full cell-by-cell iteration (potentially milliseconds for large screens), during which incoming PTY data is buffered in the OS pipe but not processed.

**Recommendation:** Consider a double-buffer approach: the PTY reader processes into a "back" parser, then swaps a snapshot/flag under a brief lock. Alternatively, make the reader thread produce pre-rendered Lines and swap them atomically. The simplest mitigation is to reduce lock hold time in `screen_to_lines` by copying raw screen data under lock and rendering outside it.

Note: the current `set_scrollback()`/`set_scrollback(0)` sandwich in ui.rs lines 323-326 means the lock is held for even longer than just reading cells -- it also mutates scrollback state.

**Impact:** MEDIUM -- noticeable during burst output but acceptable for typical AI agent workloads.

---

## 4. O(n) Vec::remove(0) on orch_log and orchestrator pending queue

**Files:** `console/src/app.rs` line 68, `console/core/src/orchestrator.rs` line 218

**Issue:** `orch_log` is a `Vec<OrchestratorEvent>` capped at 500 entries. When it overflows, `self.orch_log.remove(0)` shifts all ~500 elements left. This is O(n) per removal.

Similarly, in orchestrator.rs:218, `self.pending.remove(0)` removes the first pending message. While the pending queue is typically small, it's still an avoidable O(n).

**Recommendation:** Change `orch_log` from `Vec` to `VecDeque`. Use `pop_front()` instead of `remove(0)`. For the orchestrator pending queue, also use `VecDeque` or `drain(..1)`.

```rust
// app.rs - before:
if self.orch_log.len() > 500 {
    self.orch_log.remove(0);
}
// after:
if self.orch_log.len() > 500 {
    self.orch_log.pop_front();
}
```

**Impact:** LOW (500 elements is small) but trivial to fix.

---

## 5. Per-Frame String Allocations in Header/Footer/Ticker

**Files:** `console/src/ui.rs` lines 83-169 (header), 472-518 (footer), `console/src/app.rs` lines 206-222 (ticker_display)

**Issue:** Every frame allocates:
- `chrono::Local::now().format("%H:%M").to_string()` -- the clock (ui.rs:107)
- Multiple `format!()` calls for PSK display, agent count, page numbers, workspace indicator
- `ticker_display()` creates a `Vec<char>` from the current message, a `vec![' '; width]` buffer, and collects into a new `String` (app.rs:210-222)
- `pane_info_strip()` creates format strings for slot number, tool name, runtime, dispatch time every frame per pane (ui.rs:171-240)
- `format_runtime()` called per active pane per frame

**Recommendations:**
1. Cache the clock string and only update it once per second (compare `Instant::now()` to last update).
2. For `ticker_display()`, reuse a `String` buffer on `App` rather than allocating fresh each frame.
3. For pane info strips, cache the static portions (slot number, tool name, dispatch wall string) and only reformat the runtime each frame.

**Impact:** MEDIUM -- individually small but they compound across 4 panes at 60fps.

---

## 6. key_to_pty_bytes() Allocates a Vec Per Keystroke

**File:** `console/src/pty.rs` lines 183-229

**Issue:** Every key press in input mode allocates a `Vec<u8>`. Most escape sequences are 3-6 bytes (e.g., `b"\x1b[A".to_vec()`). Single characters allocate a 4-byte buffer, encode into it, then `.to_vec()` the slice.

**Recommendation:** Return a fixed-size array or `ArrayVec<u8, 8>` (from the `arrayvec` crate) to avoid heap allocation. Alternatively, return `Cow<'static, [u8]>` so the static sequences like `b"\x1b[A"` are zero-copy.

A simpler approach without adding dependencies: write directly to the PTY writer instead of returning bytes, avoiding the intermediate allocation entirely.

**Impact:** LOW -- keystroke frequency is human-limited.

---

## 7. strip_action_blocks() Quadratic String Mutation

**File:** `console/src/util.rs` lines 49-69

**Issue:** The function mutates a `String` in-place with `replace_range()` inside a `while` loop. Each `replace_range` is O(n) because it shifts the trailing bytes. With m action blocks in an n-length string, this is O(n*m). Additionally, `result.find("</tool_call>")` on line 62 searches from the beginning each time, not from the current position.

**Recommendation:** Build the output by copying non-block segments into a new String in a single pass, similar to how `parse_all_tool_calls` works. For typical orchestrator output (1-2 tool calls in a few KB of text), this is not a practical problem, but it's worth noting for correctness.

**Impact:** LOW -- orchestrator messages are small.

---

## 8. truncate() Uses Byte Indexing on Potentially Multi-Byte Strings

**File:** `console/src/util.rs` lines 37-45

**Issue:** `truncate()` compares `s.len()` (byte length) against `max` but then slices with `&s[..max - 3]` which is byte-indexed. If the string contains multi-byte UTF-8 characters, this could slice in the middle of a character and panic.

```rust
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {       // byte comparison
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])  // byte slice -- could panic on UTF-8!
    }
```

**Recommendation:** Use `.chars().take(max - 3)` or find the nearest char boundary with `s.floor_char_boundary(max - 3)` (nightly) or manual boundary detection.

**Impact:** LOW for typical ASCII callsigns/paths, but a latent correctness bug.

---

## 9. resize_all_slots Discards Scrollback

**File:** `console/src/pty.rs` lines 175-181

**Issue:** On terminal resize, all VT100 parsers are replaced with new ones:
```rust
*parser = vt100::Parser::new(new_size.rows, new_size.cols, 0);
```
The `0` scrollback parameter means all terminal history is lost on resize. The original scrollback size (`scrollback_lines`) is not passed through.

**Recommendation:** Store the scrollback capacity in `SlotState` and use it when recreating parsers. Or better, check if vt100 supports in-place resize (it likely does via `set_size()`).

**Impact:** MEDIUM -- resize events lose all terminal output, which is disorienting for users.

---

## 10. Redundant Work When Overlays Are Visible

**File:** `console/src/main.rs` lines 334-372

**Issue:** When an overlay is displayed (Help, ConfirmQuit, etc.), the full pane grid is still rendered underneath before the overlay is drawn on top. All 4 VT100 screens are locked, converted to Lines, and rendered into the buffer, only to be partially obscured by the overlay.

**Recommendation:** Skip pane rendering when a full-screen overlay is active. For partial overlays (centered dialogs), this is less important since ratatui's diff algorithm minimizes actual terminal writes, but the allocation work in `screen_to_lines` still happens.

**Impact:** LOW -- overlays are infrequent.

---

## 11. Orchestrator render_orchestrator() Clones Visible Lines

**File:** `console/src/ui.rs` lines 450

**Issue:** `lines[start..end].to_vec()` clones all the visible Lines for the orchestrator view every frame. Since `Line` contains `Vec<Span>` which contains `String`, this is a deep clone.

**Recommendation:** Use a slice reference directly. `Paragraph::new()` accepts `Text::from(lines)` but the lifetime of the slice must outlive the frame. Since `lines` is a local, the simplest fix is to drain/take the subrange rather than clone, or restructure to avoid the intermediate full `lines` vec.

**Impact:** LOW -- only when orchestrator view is active.

---

## 12. all_slot_infos() Always Returns 26 Items

**File:** `console/core/src/handler.rs` lines 90-92

**Issue:** `all_slot_infos()` creates 26 `SlotInfo` structs every time `list_agents` is called, including 22+ empty ones in typical usage. Each `SlotInfo` contains `Option<String>` fields that allocate for occupied slots.

**Recommendation:** Only return occupied slots, or use a fixed-size array to avoid heap allocation. This is a WebSocket response path, not the render loop, so impact is minimal.

**Impact:** NEGLIGIBLE.

---

## 13. Startup Latency Profile

**File:** `console/src/main.rs` lines 72-176

The startup sequence is well-optimized:
- Config loaded synchronously (fast, file I/O)
- TLS cert loaded/generated (fast, cached on disk)
- WebSocket server spawned on a background thread (non-blocking)
- mDNS advertisement is fire-and-forget
- Orchestrator spawned eagerly on a background thread (good -- eliminates first-message lag)
- `git rev-parse --show-toplevel` is the only subprocess in the critical path

**Observation:** The orchestrator waits up to 10 seconds for a session_id (orchestrator.rs:160). If Claude is slow to start, this blocks the background thread for 10s. The main TUI is responsive during this time, but voice messages are queued. This is acceptable.

No issues found with startup time.

---

## 14. Memory Usage Patterns

**Observation:** Memory usage is well-bounded:
- `orch_log` capped at 500 entries (app.rs:67)
- `ticker_queue` is unbounded `VecDeque<String>` -- could grow large if messages arrive faster than they scroll, but this is unlikely in practice
- VT100 parsers with configurable scrollback (default 1000 lines, capped at 10,000 in pty.rs:81)
- 26 slot array is fixed-size, not heap-allocated
- PTY reader uses a 4KB read buffer and 512-byte line buffer (pty.rs:89-90) -- efficient

The main memory consumers are the 4-26 vt100 parsers (each holding scrollback) and the orchestrator log. Both are bounded. No memory leak patterns detected.

---

## Summary of Recommendations by Priority

| Priority | Issue | File | Fix Effort |
|----------|-------|------|------------|
| HIGH | Add dirty-tracking to skip idle redraws | main.rs | Medium |
| HIGH | Reduce allocations in screen_to_lines() | ui.rs | Low |
| MEDIUM | Reduce mutex hold time for VT100 screens | pty.rs, ui.rs | Medium |
| MEDIUM | Use VecDeque for orch_log | app.rs, types.rs | Trivial |
| MEDIUM | Fix resize_all_slots scrollback loss | pty.rs | Low |
| MEDIUM | Cache per-frame strings (clock, ticker) | ui.rs, app.rs | Low |
| LOW | Fix truncate() UTF-8 safety | util.rs | Trivial |
| LOW | Improve strip_action_blocks() to single-pass | util.rs | Low |
| LOW | Use VecDeque for orchestrator pending | orchestrator.rs | Trivial |
| LOW | Skip pane rendering under overlays | main.rs | Low |
| LOW | Avoid Vec allocation in key_to_pty_bytes() | pty.rs | Low |
