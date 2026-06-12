# AGENTS.md — repo norms for AI coding agents

> Load this before working in this repo. It captures the conventions you
> must follow when writing or reviewing code here.

## Project: gh-monitor

A small, light, native (Linux, macOS, Windows) desktop app that displays a
**transparent, always-on-top overlay timeline** of GitHub activity for
user-selected repos and orgs. See `PLAN.md` for the full plan.

## Tech stack (locked)

- **Language:** Rust (edition 2021, MSRV 1.81+)
- **GUI:** Iced 0.14
- **Async:** tokio
- **HTTP:** reqwest with rustls
- **Serialization:** serde + serde_json
- **Time:** chrono
- **Config:** toml
- **Logging:** tracing
- **Tests:** insta (snapshot), wiremock (HTTP), proptest (property)

## Repo layout

```
crates/
  gh/         # GitHub API client (pure logic, no GUI)
  timeline/   # grouping/compression model (pure logic)
  config/     # persistence
  app/        # binary — Iced GUI
```

`crates/gh` and `crates/timeline` are pure logic — no GUI, no Iced. Test them
heavily with snapshots and HTTP fixtures. `crates/app` is the Iced
`Application` that glues them together.

## Conventions

### Style
- `cargo fmt` is the source of truth for formatting. CI fails if `cargo fmt --check` fails.
- `cargo clippy --all-targets -- -D warnings` must pass. No warnings.
- No `unwrap()` in production code. Use `?` / `anyhow::Result` / `thiserror`.
- No `expect()` in production code; if a panic is acceptable, it should be in
  startup code and have a clear message.
- Module-level documentation: every `lib.rs` and every `mod foo;` should have
  a 1-line `//!` comment explaining its purpose.
- Functions: prefer `pub(crate)` over `pub` until the symbol is part of a
  stable crate API.
- Naming: `snake_case` for fns/vars, `PascalCase` for types, `SCREAMING_SNAKE`
  for consts, `kebab-case` for file names (Rust convention).

### Error handling
- Library crates (`gh`, `timeline`, `config`) use `thiserror` and expose a
  per-crate `Error` enum.
- The binary (`app`) uses `anyhow` for ergonomic top-level handling.
- No `unwrap`, no `panic!`, no `unimplemented!()` in production code.

### Logging
- `tracing` everywhere. No `println!` in production code. Use `info!`,
  `debug!`, `trace!` as appropriate.
- Spans for the polling loop, the event handler, and the render loop.

### Testing
- Unit tests live in `#[cfg(test)] mod tests` at the bottom of the same file
  as the code they test.
- Integration tests live in `tests/`.
- Snapshot tests use `insta`. Review snapshots with `cargo insta review`.
- HTTP fixtures use `wiremock`. Real-API tests are gated on `GH_TOKEN`.
- Property tests use `proptest` for any pure function over time/strings.

### Commits
- Conventional-ish, present tense, no scope prefix noise. "Add hover passthrough
  state machine", not "feat(overlay): add hover passthrough state machine".
- No "Co-authored-by: AI" footers unless asked.
- No `--no-verify`. If pre-commit hooks fail, fix the issue.
- One logical change per commit. Squash trivial commits locally before
  pushing.

### Branches / worktrees
- New features and plan execution go in a worktree under `.worktrees/`.
- Branch name: `feat/<short-slug>` or `fix/<short-slug>`.
- Merge back to `main` with `--no-ff` after the worktree is verified.

### Tasks
- **Always** use `just` to run things. Never `cargo run` directly.
- The `justfile` is the canonical reference for build / test / lint / run.

## Anti-patterns to avoid

- ❌ `tokio::main` with `iced::Application` — use `iced::daemon` (or run
  the GUI event loop on the main thread and tokio on a worker).
- ❌ Storing `String` in the render hot path — use `Cow<'static, str>` or
  interned strings.
- ❌ Building a fresh `reqwest::Client` per request — build it once in
  `crates/app` and pass it down.
- ❌ Calling `tokio::time::sleep` in a tight loop — use the `time::repeat`
  subscription from Iced.
- ❌ "I'll add this in a follow-up" — finish the slice or don't start it.

## Definition of done for a change

- [ ] Code compiles with no warnings
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] New code has tests; changed code has updated tests
- [ ] Manual smoke test on Linux (the dev machine) at minimum
- [ ] If the change is user-facing: update `PLAN.md` "Open questions" or
      "Definition of done"

## Where to find things

- `PLAN.md` — what we're building and why
- `docs/architecture.md` — module/crate responsibilities and data flow
- `justfile` — all the commands you need
- `crates/*/src/lib.rs` — crate-level docs explain the module structure

## How to get help

- `cf-igc` skill for any "what should we pick" decision
- `cf-breakpoints` for operationalising vague degree-words
- `ultra-tui-iteration` if you find yourself writing display logic without
  a test loop
- `review-and-fix` after any non-trivial implementation
- `ccc-review-cx` for an external review pass
