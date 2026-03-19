# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## v0.2.0

### Added

- Persistent LLM orchestrator that replaces the keyword-based voice command parser. Voice transcripts from the radio are piped directly to a headless Claude process, which decides what to do via tool calls (dispatch, terminate, merge, message_agent, list_agents, plan, list_repos).
- Orchestrator view (`o` key) showing timestamped conversation log with the LLM.
- Orchestrator status in header bar (IDLE / THINKING / DEAD / OFF).
- System event notifications to orchestrator ([EVENT] TASK_COMPLETE, MERGE_CONFLICT, AGENT_EXITED).

### Changed

- Radio app now sends raw transcripts instead of parsing commands locally. The orchestrator handles all command interpretation.
- Console WebSocket server simplified to a single VoiceTranscript event type forwarded to the orchestrator.

### Removed

- Client-side voice command parser (CommandParser.kt) is no longer used for command routing. The radio sends raw transcripts.
- Deterministic command routing in ws_server.rs (AutoDispatch, QueueTask, PlanRequest variants).
