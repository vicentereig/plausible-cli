# Plausible CLI

<div align="center">

[![Crates.io][crate-badge]][crate-url]
[![Repo][repo-badge]][repo-url]
[![Docs][docs-badge]][docs-url]
[![License][license-badge]][license-url]  
[![CI][ci-badge]][ci-url]
[![Dependencies][deps-badge]][deps-url]
[![Coverage][coverage-badge]][coverage-url]

</div>

A Rust-native command-line interface that surfaces the Plausible Analytics APIs
with a workflow tailored for humans and automation agents.

> :memo: **Primary documentation lives in [`docs/llms-full.txt`](docs/llms-full.txt).**
> It is authored in GitHub Flavored Markdown for LLMs and humans alike, and
> covers setup, command schemas, prompt snippets, and safety guidance.

## Requirements

- Rust 1.90 or newer (`rustup install stable` recommended).
- A Plausible Analytics API key with access to the desired sites.

## Installation

```bash
# Build and install from the current workspace
cargo install --path .
```

You can also run directly with `cargo run -- <command>` while developing.

## First-Run Setup

Add your Plausible API key and friendly metadata with the `accounts` command:

```bash
plausible accounts add \
  --alias personal \
  --api-key PLAUSIBLE_API_KEY_HERE \
  --label "Personal Dashboard" \
  --email you@example.com

# Optionally set a different default account later
plausible accounts use --alias personal
```

Accounts are stored under `~/.config/plausible-cli/` with secrets held in a
per-account key file when the OS keyring is unavailable. On macOS/Linux the CLI
attempts to store credentials in the system keychain by default; you can opt out
via `PLAUSIBLE_CLI_DISABLE_KEYRING=1` for ephemeral or CI environments.

## Usage Examples

List every site visible to the current account:

```bash
plausible sites list
```

Fetch aggregate visitors and pageviews for the last 7 days, returning JSON:

```bash
plausible stats aggregate \
  --site example.com \
  --metric visitors \
  --metric pageviews \
  --period 7d \
  --output json
```

Request a daily timeseries of visitors for the same site:

```bash
plausible stats timeseries \
  --site example.com \
  --metric visitors \
  --period 7d \
  --output json
```

Slice traffic by referrer with sorting and pagination:

```bash
plausible stats breakdown \
  --site example.com \
  --property visit:source \
  --metric visitors \
  --sort visitors:desc \
  --output json
```

Check realtime visitors currently online:

```bash
plausible stats realtime --site example.com --output json
```

Inspect the remaining API budget and next reset windows:

```bash
plausible status
plausible status --output json
```

Generate a sample custom event payload:

```bash
plausible events template
plausible events template --output json
```

Inspect the background queue (helpful when scripting bursts of API calls):

```bash
plausible queue inspect --output json
plausible queue drain   # block until all jobs finish
```

## Multi-Account Workflow

- `plausible accounts list` – show all configured aliases (with default marker).
- `plausible accounts export` – print non-secret metadata (table) or
  `--json` for machine consumption.
- `plausible accounts budget --alias <alias> [--daily <limit>|--clear]` – set or
  clear a per-account daily request ceiling (defaults to hourly quota).
- Use `--account <alias>` on any command to override the default.

## Machine-Friendly Mode

Every command accepts `--output json` so that automation agents can parse a
stable structure. See `docs/llms-full.txt` for tips on driving the CLI from an
LLM.

## Project Docs

- `docs/architeture.md` – architecture overview and diagrams.
- `docs/000-v1-plan.md` – roadmap, milestones, and testing strategy.
- `docs/llms-full.txt` – LLM-oriented reference and prompt snippets.

## Distribution

- **GitHub Releases** – Tags matching `v*` publish macOS and Linux archives in
  [`releases/`](https://github.com/vicentereig/plausible-cli/releases). Download, extract, and copy the `plausible` binary onto your `$PATH`.
- **Cargo install** – `cargo install plausible-cli --git https://github.com/vicentereig/plausible-cli.git --locked`
- **Homebrew (tap)** – After the first tagged release:
  ```bash
  brew tap vicentereig/plausible-cli
  brew install plausible
  ```
  The formula used by the tap lives in `homebrew/Formula/plausible.rb` and is updated automatically during the release workflow.

## Testing

Run the suite (includes unit, async integration, and HTTP mocks):

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Contributing

Issues and PRs are welcome. Please run the commands in the **Testing** section
before submitting changes. For new features, add coverage via unit/async tests
and update the documentation when behavior changes.

[crate-badge]: https://img.shields.io/crates/v/plausible-cli.svg?label=crates.io&logo=rust
[crate-url]: https://crates.io/crates/plausible-cli
[repo-badge]: https://img.shields.io/badge/github-vicentereig%2Fplausible--cli-181717?logo=github
[repo-url]: https://github.com/vicentereig/plausible-cli
[docs-badge]: https://img.shields.io/badge/docs-llms--full.txt-blue?logo=readthedocs
[docs-url]: https://github.com/vicentereig/plausible-cli/blob/main/docs/llms-full.txt
[license-badge]: https://img.shields.io/badge/license-MIT-blue.svg
[license-url]: https://github.com/vicentereig/plausible-cli/blob/main/LICENSE
[ci-badge]: https://img.shields.io/github/actions/workflow/status/vicentereig/plausible-cli/ci.yml?branch=main&label=CI&logo=github
[ci-url]: https://github.com/vicentereig/plausible-cli/actions/workflows/ci.yml
[deps-badge]: https://deps.rs/repo/github/vicentereig/plausible-cli/status.svg
[deps-url]: https://deps.rs/repo/github/vicentereig/plausible-cli
[coverage-badge]: https://codecov.io/gh/vicentereig/plausible-cli/branch/main/graph/badge.svg
[coverage-url]: https://codecov.io/gh/vicentereig/plausible-cli
