# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## v0.2.0

### Added

- Configurable agent names via `[agents].callsigns` in `config.toml`. The list drives the slot count, page layout, and agent callsigns -- replacing the hardcoded NATO alphabet and `max_agents` setting.

### Changed

- Slot count is now determined by the length of the `callsigns` list (default 8) instead of a fixed 26.
- Page count adapts automatically to fit the configured number of agents.
- The orchestrator system prompt now includes the configured callsign list so the LLM knows which agent names are valid.
- `terminal.max_agents` is deprecated; existing configs without `[agents]` fall back to NATO names using `max_agents` for backward compatibility.

### Removed

- Hardcoded 26-slot NATO alphabet as the sole agent naming scheme.
