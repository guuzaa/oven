# Changelog

## [0.0.3] - 2026-08-20

### Changed
- Adopt oven-llm Router (0.4.1) for model/provider routing
- Replace provider `kind` with `protocol` (custom vendors only); rewrite aliases `grok`/`kimi`/`glm` to `xai`/`moonshot`/`zhipu`
- Stop sending a default `max_tokens` of 4096
- File tools use async I/O
- Restructure TUI widgets under `components/`

### Fixed
- Accumulate token usage of all provider responses in a turn

## [0.0.2] - 2026-08-18

### Added
- Tool system architecture and UI overhaul

### Changed
- Replaced XDG directories with a single ~/.oven home directory
- Split transcript.rs into modular components

### Fixed
- UI full paint when scrolling
- Enlarged truncate length for tool results


## [0.0.1] - 2026-08-16
- Initial Release