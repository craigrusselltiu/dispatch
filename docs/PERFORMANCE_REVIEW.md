# Performance Review: Dispatch

**Date:** 2026-03-24
**Reviewer:** Alpha (automated analysis)
**Scope:** Console (Rust), Core library, Radio (Android/Kotlin), Dependencies

## Executive Summary

Full-system performance audit covering both the Rust console (~2000 LOC across 2 crates) and the Android radio app (~2500 LOC Kotlin). The console is well-structured with clean separation of concerns; the radio app is a lean single-activity design. Both components follow the project's simplicity philosophy.

**Console:** The highest-impact issues remain the unconditional 60fps redraw loop and allocation-heavy VT100 screen conversion. New findings since the previous review include per-frame file I/O for agent message polling, blocking `thread::sleep()` calls in Copilot input simulation, and O(n^2) callsign lookups in the core library.

**Radio:** Several resource lifecycle issues (SpeechRecognizer leaks, infinite animator battery drain) and main-thread blocking patterns (image I/O, JSON parsing) that affect battery life and UI responsiveness.

**Dependencies:** Feature flags and build profiles are well-optimized. Main opportunities are removing chrono in favor of the `time` crate already in the tree, and consolidating duplicate dependency versions (bitflags v1/v2, socket2, windows-sys).

## Priority Summary

### Console

| Priority | Issue | Location | Fix Effort |
|----------|-------|----------|------------|
| HIGH | Add dirty-tracking to skip idle redraws | main.rs | Medium |
| HIGH | Reduce allocations in screen_to_lines() | ui.rs | Low |
| HIGH | Per-frame file I/O in poll_agent_messages() | pty.rs | Low |
| MEDIUM | Reduce mutex hold time for VT100 screens | pty.rs, ui.rs | Medium |
| MEDIUM | Blocking thread::sleep() in type_to_copilot() | pty.rs | Low |
| MEDIUM | Fix resize_all_slots scrollback loss | pty.rs | Low |
| MEDIUM | Cache per-frame strings (clock, ticker) | ui.rs, app.rs | Low |
| MEDIUM | O(n^2) callsign lookup with repeated to_uppercase() | handler.rs | Low |
| MEDIUM | Unbounded loop in rpc_read_response() | orchestrator.rs | Low |
| LOW | Fix truncate() UTF-8 safety | util.rs | Trivial |
| LOW | Improve strip_action_blocks() to single-pass | util.rs | Low |
| LOW | Skip pane rendering under overlays | main.rs | Low |
| LOW | Avoid Vec allocation in key_to_pty_bytes() | pty.rs | Low |
| LOW | Avoid deep clone in render_orchestrator() | ui.rs | Low |
| LOW | Unnecessary string clones in tool execution | app.rs | Low |
| NEGLIGIBLE | Only return occupied slots from all_slot_infos() | handler.rs | Low |

### Radio (Android)

| Priority | Issue | Location | Fix Effort |
|----------|-------|----------|------------|
| HIGH | SpeechRecognizer listener leak on destroy | PushToTalkManager.kt, ContinuousListenManager.kt | Trivial |
| HIGH | Main-thread image I/O blocking | MainActivity.kt | Low |
| HIGH | WebSocket reconnect battery drain (no give-up) | RadioWebSocketClient.kt | Low |
| MEDIUM | Infinite status blink animator during background | MainActivity.kt | Trivial |
| MEDIUM | Main-thread JSON parsing for agent state | MainActivity.kt | Low |
| MEDIUM | AudioLevelView redraws at callback rate (no throttle) | AudioLevelView.kt | Trivial |
| LOW | AgentStatusOverlay context reference leak risk | AgentStatusOverlay.kt | Low |
| LOW | Notification object rebuilt on every status change | RadioService.kt | Low |
| LOW | Chat view removal causes LinearLayout invalidation | MainActivity.kt | Low |
| NEGLIGIBLE | SharedPreferences individual writes (not batched) | RadioSettings.kt | Trivial |

### Dependencies

| Priority | Issue | Fix Effort |
|----------|-------|------------|
| MEDIUM | chrono heavyweight; time crate already in tree | Medium |
| LOW | bitflags v1/v2 duplication (nix v0.25 uses v1) | Low |
| LOW | socket2 v0.5/v0.6 duplication (mdns-sd pins v0.5) | Low |
| NEGLIGIBLE | windows-sys 4 versions (ecosystem-wide) | N/A |

---

## Console: High Priority

### 1. Render Loop: Unconditional 60fps Full Redraw

**Files:** `console/src/main.rs`, `console/src/ui.rs`

The main loop calls `terminal.draw()` every iteration (~16ms / 60fps) regardless of whether anything changed. Every frame computes layout splits, locks all visible VT100 screen mutexes, converts all screens cell-by-cell to ratatui Lines, and builds header/footer/ticker/overlay widgets.

When agents are idle and no input arrives, this is pure waste.

**Recommendation:** Add a dirty flag. Set it when PTY output arrives, user input occurs, ticker advances, resize happens, or overlay changes. Skip `terminal.draw()` when clean. This alone could cut CPU usage 50-90% during idle periods. A simpler alternative: increase the poll timeout to 100ms when idle (10fps) and drop to 16ms only when PTY output is flowing.

---

### 2. screen_to_lines() Allocation Storm

**File:** `console/src/ui.rs`

Called for each visible pane every frame. For a typical 80x40 pane, iterates 3200 cells, allocates a `String` per cell via `cell.contents()`, clones `current_text` on style changes, and builds `Vec<Span>` per row then `Vec<Line>` for the whole screen. With 4 active panes at 60fps, this is ~12,800 cell accesses and thousands of String/Span allocations per frame.

**Recommendations:**
1. Replace `current_text.clone()` with `std::mem::take(&mut current_text)` to avoid the clone.
2. Pre-allocate the `spans` vector with `Vec::with_capacity(cols)`.
3. Cache converted lines and only reconvert when the screen has new data (generation counter or dirty flag on the PTY reader side).

---

### 3. Per-Frame File I/O in poll_agent_messages() [NEW]

**File:** `console/src/pty.rs`

`poll_agent_messages()` is called from the main loop every frame. For each occupied slot, it calls `std::fs::metadata()` (stat syscall) and `std::fs::File::open()` (open syscall). With 8 active agents at 60fps, this is 480 stat calls and up to 480 open calls per second.

**Recommendation:** Reduce poll frequency to every 3rd frame (~20fps / 50ms intervals) which is still responsive for human-readable messages. Alternatively, move polling to a background thread with a configurable interval.

```rust
// In main loop: only poll every 3rd frame
if frame_counter % 3 == 0 {
    pty::poll_agent_messages(&mut app);
}
```

---

## Console: Medium Priority

### 4. Mutex Contention Between PTY Reader and Render Thread

**Files:** `console/src/pty.rs`, `console/src/ui.rs`

The PTY reader thread locks the screen mutex and calls `parser.process()` while holding it. The render thread locks the same mutex for the full cell-by-cell iteration in `screen_to_lines()`. The `set_scrollback()`/`set_scrollback(0)` sandwich in the render path extends the lock duration further.

**Recommendation:** Copy raw screen data under lock and render outside it, or use a double-buffer approach where the PTY reader swaps a snapshot under a brief lock.

---

### 5. Blocking thread::sleep() in type_to_copilot() [NEW]

**File:** `console/src/pty.rs`

The Copilot input simulation sleeps 2ms per character with `thread::sleep(Duration::from_millis(2))`, blocking the OS thread. For a 200-character prompt, this blocks for 400ms. Additional 50ms sleeps appear in message-sending paths.

**Recommendation:** For short delays (<5ms), consider yielding or spinning. For longer delays, move to an async context or accept the blocking since these run on dedicated threads, not the main thread. The main concern is thread pool exhaustion if many agents type simultaneously.

---

### 6. resize_all_slots Discards Scrollback

**File:** `console/src/pty.rs`

On terminal resize, VT100 parsers are replaced with `vt100::Parser::new(rows, cols, 0)` -- the `0` scrollback parameter discards all terminal history. The configured scrollback capacity is not preserved.

**Recommendation:** Store the scrollback capacity and use it when recreating parsers, or use `set_size()` if the vt100 crate supports in-place resize.

---

### 7. Per-Frame String Allocations in Header/Footer/Ticker

**Files:** `console/src/ui.rs`, `console/src/app.rs`

Every frame allocates: the clock string via `chrono::Local::now().format().to_string()`, multiple `format!()` calls for PSK/agent count/page numbers, a `Vec<char>` + buffer + new `String` in `ticker_display()`, and format strings for each pane info strip.

**Recommendations:**
1. Cache the clock string and update once per second.
2. Reuse a `String` buffer on `App` for `ticker_display()`.
3. Cache static portions of pane info strips (slot number, tool name, dispatch wall string).

---

### 8. O(n^2) Callsign Lookup with Repeated to_uppercase() [NEW]

**File:** `console/core/src/handler.rs`

`next_callsign()` rebuilds a `HashSet<String>` on every dispatch call, calling `.to_uppercase()` on every occupied slot's callsign, then calls `.to_uppercase()` again per candidate in the search loop. `find_slot_by_callsign()` similarly calls `.to_uppercase()` per slot during linear search.

With 26 slots this is small in absolute terms, but it is algorithmically wasteful and allocates on every dispatch/send/terminate path.

**Recommendation:** Store callsigns in a canonical (lowercase or uppercase) form at creation time. Use a simple linear scan without case conversion -- with max 26 slots, a HashSet is unnecessary overhead.

---

### 9. Unbounded Loop in rpc_read_response() [NEW]

**File:** `console/core/src/orchestrator.rs`

The RPC response reader loops indefinitely waiting for a matching response ID. There is no iteration limit or timeout. If the orchestrator subprocess misbehaves or produces unexpected output, this blocks the reader thread forever.

**Recommendation:** Add an iteration limit (e.g., 10,000 lines) and/or a timeout to prevent indefinite blocking.

---

## Console: Low Priority

### 10. truncate() UTF-8 Safety Bug

**File:** `console/src/util.rs`

`truncate()` compares byte length against `max` but slices with byte indexing (`&s[..max - 3]`). Multi-byte UTF-8 characters could cause a panic if the slice falls mid-character.

**Recommendation:** Use `.chars().take(max - 3)` or find the nearest char boundary.

---

### 11. strip_action_blocks() Quadratic String Mutation

**File:** `console/src/util.rs`

Three separate `while` loops each do linear searches and in-place `replace_range()` mutations. Each `replace_range()` is O(n) because it shifts trailing bytes. With m action blocks in an n-length string, total work is O(n*m).

**Recommendation:** Build output by copying non-block segments into a new String in a single pass.

---

### 12. Redundant Work When Overlays Are Visible

**File:** `console/src/main.rs`

When a full-screen overlay is displayed, all 4 VT100 screens are still locked, converted to Lines, and rendered underneath before the overlay is drawn on top.

**Recommendation:** Skip pane rendering when a full-screen overlay is active.

---

### 13. key_to_pty_bytes() Allocates a Vec Per Keystroke

**File:** `console/src/pty.rs`

Every key press in input mode allocates a `Vec<u8>` for escape sequences (3-6 bytes).

**Recommendation:** Return `Cow<'static, [u8]>` for static sequences or write directly to the PTY writer.

---

### 14. render_orchestrator() Clones Visible Lines

**File:** `console/src/ui.rs`

`lines[start..end].to_vec()` deep-clones all visible Lines for the orchestrator view every frame. `Line` contains `Vec<Span>` which contains `String`.

**Recommendation:** Use a slice reference directly or drain the subrange.

---

### 15. Unnecessary String Clones in Tool Execution [NEW]

**File:** `console/src/app.rs`

Multiple `.clone()` calls on callsign strings and `user_callsign` in the tool execution path (e.g., `&app.user_callsign.clone()`). These are unnecessary when a reference would suffice.

**Recommendation:** Use `&app.user_callsign` directly instead of cloning.

---

## Console: Negligible

### 16. all_slot_infos() Always Returns Full Slot Array

**File:** `console/core/src/handler.rs`

Creates `SlotInfo` structs for all configured slots (up to 26) on every `list_agents` call, including empty ones. This is a WebSocket response path, not the render loop.

---

## Radio: High Priority

### 17. SpeechRecognizer Listener Leak on Destroy [NEW]

**Files:** `PushToTalkManager.kt`, `ContinuousListenManager.kt`

Both managers set a recognition listener on the `SpeechRecognizer` but never clear it before calling `destroy()`. If the recognizer is destroyed without clearing the listener, the listener holds references to callbacks, preventing garbage collection. This accumulates with repeated PTT activations and continuous-listen toggles.

**Recommendation:** Call `recognizer?.setRecognitionListener(null)` and `recognizer?.stopListening()` before `recognizer?.destroy()` in both managers' `destroy()` methods.

---

### 18. Main-Thread Image I/O Blocking [NEW]

**File:** `MainActivity.kt`

`contentResolver.openInputStream(uri)?.use { it.readBytes() }` reads an entire image file into memory on the main thread. A 5MB image blocks the main thread for 50-500ms, causing dropped frames and ANR risk.

**Recommendation:** Move image reading to a coroutine on `Dispatchers.IO`:
```kotlin
lifecycleScope.launch(Dispatchers.IO) {
    val bytes = contentResolver.openInputStream(uri)?.use { it.readBytes() }
    withContext(Dispatchers.Main) { /* send via WebSocket */ }
}
```

---

### 19. WebSocket Reconnect Has No Give-Up Threshold [NEW]

**File:** `RadioWebSocketClient.kt`

When the console is unreachable, the client retries indefinitely with exponential backoff capped at 30 seconds. Combined with 15-second ping intervals when connected, this causes continuous network radio activation and CPU wake-ups, draining battery.

**Recommendation:** Add a maximum reconnect attempt count (e.g., 20 attempts = ~30 minutes). After exhausting attempts, stop retrying and notify the user. Reset the counter on successful connection.

---

## Radio: Medium Priority

### 20. Infinite Status Blink Animator [NEW]

**File:** `MainActivity.kt`

The connection status dot animator uses `repeatCount = ValueAnimator.INFINITE` and runs at ~60fps. It continues even when the app is paused or the screen is off.

**Recommendation:** Cancel the animator in `onPause()` and restart it in `onResume()`.

---

### 21. Main-Thread JSON Parsing [NEW]

**File:** `MainActivity.kt`

`gson.fromJson()` is called on the main thread for every WebSocket message. The entire agents list is rebuilt on every `"agents"` message. During active communication, this causes measurable main-thread stalls.

**Recommendation:** Parse JSON on a background thread and post only the parsed results to the main thread for UI updates.

---

### 22. AudioLevelView Redraws at Full Callback Rate [NEW]

**File:** `AudioLevelView.kt`

The audio level bar calls `invalidate()` on every RMS callback during PTT or continuous listening, potentially 60+ times per second. Drawing 20 segments per frame at this rate impacts battery.

**Recommendation:** Throttle `invalidate()` to ~15fps by checking elapsed time since last update:
```kotlin
var level: Float = 0f
    set(value) {
        val now = System.currentTimeMillis()
        if (now - lastUpdateMs >= 66) {  // ~15fps
            field = value.coerceIn(0f, 1f)
            lastUpdateMs = now
            invalidate()
        }
    }
```

---

## Radio: Low Priority

### 23. AgentStatusOverlay Context Reference

**File:** `AgentStatusOverlay.kt`

The `context` parameter is held as a member field. If the overlay instance outlives the activity (e.g., due to an exception before cleanup in `VolumeUpHandler.onKeyUp()`), the activity context leaks.

**Recommendation:** Pass context only when creating the dialog, or use a `WeakReference`.

---

### 24. Notification Rebuilt on Every Status Change

**File:** `RadioService.kt`

`updateNotification()` creates a new `Notification.Builder` with all properties on every connection status change.

**Recommendation:** Cache the builder and only update the content text.

---

### 25. Chat View Removal Triggers Layout Invalidation

**File:** `MainActivity.kt`

When the 100-message cap is reached, `llChat.removeViewAt(0)` invalidates the entire LinearLayout. Under rapid message arrival, this compounds.

**Recommendation:** Replace the LinearLayout+ScrollView with a RecyclerView and ListAdapter for efficient view recycling.

---

## Dependencies

### 26. chrono Is Heavyweight for Simple Timestamp Needs [NEW]

chrono pulls in `iana-time-zone`, platform-specific complexity, and WASM bindings not needed for a TUI app. The `time` crate is already in the dependency tree (pulled by other crates) and would serve the same purpose with less bloat.

**Recommendation:** Replace chrono with `time` crate or `std::time` for the ~2-3 call sites that format timestamps. Estimated savings: ~8-10 seconds compile time, ~5MB binary size.

---

### 27. Duplicate Dependency Versions [NEW]

- **bitflags:** v1.3.2 (via nix v0.25, polling, portable-pty) and v2.11.0 (via crossterm, ratatui). Updating nix to v0.27+ would consolidate to v2 only.
- **socket2:** v0.5.10 (via mdns-sd) and v0.6.3 (via tokio). Check if mdns-sd has a newer release using socket2 v0.6.
- **windows-sys:** Four versions (0.48, 0.52, 0.59, 0.61) due to ecosystem fragmentation. Acceptable; will resolve as upstream crates update.

---

## Positive Patterns

These aspects of the codebase are well-optimized:

- **Async WebSocket server** with proper `tokio::select!` multiplexing and graceful slow-client handling
- **PTY readers on dedicated threads** with atomic exit flags -- no UI blocking
- **Orchestrator spawned eagerly** in background -- warm by first message
- **VecDeque for orch_log and pending queue** -- O(1) push/pop (previously fixed from Vec)
- **Bounded memory usage** -- orch_log capped at 500, scrollback capped at 10,000, PTY reader uses 4KB buffer
- **File-based agent messaging** instead of fragile PTY output parsing
- **Well-tuned Cargo feature flags** -- tokio, rustls, rcgen all use minimal feature sets
- **Good build profiles** -- dev uses incremental + line-tables-only debug; release uses thin LTO + strip
- **Core crate isolation** -- dispatch-core has only serde dependencies, keeping logic testable and lightweight

---

## Resolved (from prior review)

### O(n) Vec::remove(0) on orch_log and orchestrator pending queue

Both were changed from `Vec` with `remove(0)` (O(n) shift) to `VecDeque` with `pop_front()` (O(1)).
