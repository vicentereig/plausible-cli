# Plausible CLI

A Rust-native command-line interface that surfaces the Plausible Analytics APIs
with a workflow tailored for humans and automation agents.

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
per-account key file.

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

Inspect the remaining API budget and next reset windows:

```bash
plausible status
plausible status --output json
```

Generate a sample custom event payload:

```bash
plausible events template
```

## Multi-Account Workflow

- `plausible accounts list` – show all configured aliases (with default marker).
- `plausible accounts export` – print non-secret metadata (table) or
  `--json` for machine consumption.
- Use `--account <alias>` on any command to override the default.

## Machine-Friendly Mode

Every command accepts `--output json` so that automation agents can parse a
stable structure. See `docs/llms-full.txt` for tips on driving the CLI from an
LLM.

## Project Docs

- `docs/architeture.md` – architecture overview and diagrams.
- `docs/000-v1-plan.md` – roadmap, milestones, and testing strategy.
- `docs/llms-full.txt` – LLM-oriented reference and prompt snippets.

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
and update the documentation when behavior changes.*** End Patch
