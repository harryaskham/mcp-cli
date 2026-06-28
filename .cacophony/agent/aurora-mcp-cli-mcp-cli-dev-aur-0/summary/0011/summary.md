# Session summary — add azure-ephemeral GitHub Actions CI for mcp-cli

## Goal

Operator directive (Harry, 2026-06-28): bring every project's x86/Linux CI onto the new azure-ephemeral self-hosted GitHub runners via `runs-on: [self-hosted, azure-ephemeral]`, one agent per project. mcp-cli had no CI at all.

## Bead(s)

- `bd-f023df` — Add GitHub Actions CI on azure-ephemeral self-hosted runner

## Before state

- mcp-cli had NO `.github/` and no CI workflows; validation was local-only (cargo fmt/clippy/test).

## After state

- Added `.github/workflows/ci.yml`: a single `check` job on `runs-on: [self-hosted, azure-ephemeral]` running `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features`, with the toolchain provisioned by `dtolnay/rust-toolchain@stable` (rustfmt, clippy) since mcp-cli has no flake (edition 2024 / MSRV 1.85).
- Triggers are `workflow_dispatch`-only for now (push/pull_request commented in-file) so the config lands WITHOUT queuing any job before the self-hosted runner pool is live, honoring the cross-project sequencing directive. Enabling the auto-triggers is a one-line follow-up once runners are up.
- Local validation green: fmt/clippy/test (27 unit + 1 doctest). The workflow file does not affect the cargo build.

## Diff summary

- Code/content commits: pending final squash SHA from reintegration receipt
- Summary artefact commit: intentionally omitted; must not self-reference its own mutable SHA
- Files touched: `.github/workflows/ci.yml` (new), `.cacophony/agent/aurora-mcp-cli-mcp-cli-dev-aur-0/summary/pending/summary.md`
- Tests: none added (CI config only); existing suite stays green.
- Behavioural delta: adds a manual-dispatch CI workflow targeting the azure-ephemeral runners; no auto-trigger yet.

## Operator-takeaway

mcp-cli now has CI wired to the azure-ephemeral self-hosted runner pool, staged as manual-dispatch so it queues nothing before the runners are live. Once the runners are up, uncomment the push/pull_request triggers in `.github/workflows/ci.yml` (or ask me to) and CI runs automatically on every push/PR to main. Toolchain is the dtolnay rust-toolchain action; can switch to a Nix flake if preferred since the runners have Nix.
