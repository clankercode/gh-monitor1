set shell := ["bash", "-uc"]
set dotenv-load := true

# gh-monitor — task runner

# Display this help
default:
    @just --list

# === Build ===

# Debug build (all crates)
build:
    cargo build --workspace

# Release build (all crates, all targets)
build-release:
    cargo build --workspace --release

# Build the app binary only
build-app:
    cargo build -p gh-monitor-app

# === Test ===

# Run all tests
test:
    cargo test --workspace --all-features

# Run tests with output captured
test-nocapture:
    cargo test --workspace --all-features -- --nocapture

# Run tests for one crate
test-one crate:
    cargo test -p {{crate}} --all-features

# Review insta snapshots
test-review:
    cargo insta review

# === Lint / Format ===

# Check formatting
fmt:
    cargo fmt --all -- --check

# Fix formatting
fmt-fix:
    cargo fmt --all

# Lint with clippy (CI-equivalent)
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# === Run ===

# Run the app in debug
run: build-app
    cargo run -p gh-monitor-app

# Run the app in release
run-release: build-release
    cargo run -p gh-monitor-app --release

# === Clean ===

# Remove build artifacts
clean:
    cargo clean

# Remove build artifacts AND target/
clean-all: clean
    rm -rf target

# === CI helpers ===

# Run everything CI runs
ci: fmt lint test build-release
    @echo "CI checks passed locally."

# === Documentation ===

# Build docs
docs:
    cargo doc --workspace --no-deps

# === Install / Release ===

# Install locally
install: build-release
    cargo install --path crates/app --locked

# Build release artifacts for all targets
dist:
    @echo "See .github/workflows/release.yml — release artifacts are produced on tag push."

# === Misc ===

# Audit dependencies
audit:
    cargo audit

# Show outdated deps
outdated:
    cargo outdated

# Show tree
tree:
    cargo tree

# Show bloat in release
bloat:
    cargo bloat --release -p gh-monitor-app

# Show a summary of the project
info:
    @echo "Project: gh-monitor"
    @echo "Crates:"
    @ls -1 crates/
