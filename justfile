default := "help"

set export

help:
    @just --list

fmt:
    cargo fmt

clippy:
    cargo clippy --all-targets --all-features -- -D warnings

check:
    cargo check --all-targets --all-features

test:
    cargo test --all-features

nextest:
    cargo nextest run --all-features

deny:
    cargo deny check bans sources vulnerabilities

lint: fmt clippy deny

ci: fmt clippy check test
