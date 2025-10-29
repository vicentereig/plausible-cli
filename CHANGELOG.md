# Changelog

All notable changes to this project will be documented in this file.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/) and
this project adheres to [Semantic Versioning](https://semver.org/).

## [1.0.0] - 2025-10-29

### Added
- **Multi-account management** with secure keyring-backed credential storage, optional file fallback, and per-account daily budget controls.
- **Rate-limit aware runtime** combining hourly/daily ledgers, status reporting, and queue-aware telemetry (retry counts, next retry timestamps, error summaries).
- **Plausible API coverage** for sites (list/create/update/reset/delete), stats (aggregate/timeseries/breakdown/realtime), and events (template/send/import).
- **Queued worker architecture** with exponential backoff, configurable retries, and human/JSON-friendly queue inspection/drain commands.
- **Integration snapshot tests** using `assert_cmd` + `insta` for deterministic CLI output across accounts, events, and queue flows.
- **GitHub Actions pipelines** for CI and tagged releases (macOS/Linux archives) plus a Homebrew formula template.
- Comprehensive documentation for people and LLM agents (`docs/llms-full.txt`), architecture diagrams, and an installation-focused README.

### Changed
- Binary renamed to `plausible`, aligning CLI invocation with the project name.

### Fixed
- Ensured queue telemetry reflects retry outcomes and that status/inspect outputs remain stable for automation.

[1.0.0]: https://github.com/vicentereig/plausible-cli/releases/tag/v1.0.0
