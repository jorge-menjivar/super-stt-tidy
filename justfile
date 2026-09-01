# SPDX-License-Identifier: GPL-3.0-only
# Task runner for Tidy, the standalone transcript-cleanup WASM backend. Mirrors the
# recipe names used by the main super-stt repo (`just check`, `just ci`, etc.).
#
# `just --list` shows only the last comment line before a recipe, so any recipe
# whose comment runs to more than one line carries a [doc("…")] summary for the
# listing and keeps the prose above it for whoever opens this file.

# Default: build the component
default: build-component

# Remove everything this repo's recipes generate: the cargo target dir, the
# component staged into the repo root by `just stage`, and the coverage
# artifacts. All are gitignored and all are one recipe away from coming back
# (`just stage`, `just coverage-lcov`).
[doc("Remove the target dir, the staged component, and coverage output")]
clean:
    cargo clean
    rm -f tidy.wasm lcov.info

# Build the wasi:http component (wasm32-wasip2). Requires the target:
#   rustup target add wasm32-wasip2
[doc("Build the wasi:http component for wasm32-wasip2")]
build-component *args:
    cargo build --release --locked --target wasm32-wasip2 {{ args }}

# Build, then copy the component to the entrypoint name `backend.toml` declares,
# so this directory can be installed with the daemon's Import-from-dir path.
# Cargo names the artifact after the crate; the release workflow renames it the
# same way when attaching it to a GitHub release, so a local install and a
# published one stage the same bytes under the same name.
[doc("Build and stage tidy.wasm for Import-from-dir")]
stage: build-component
    cp target/wasm32-wasip2/release/super_stt_backend_tidy.wasm tidy.wasm
    @echo "staged tidy.wasm — this directory is now installable with Import from dir"

# Lint the shipped component on its real target (wasm32-wasip2). The host build
# only sees the pure rules (the wasi handler is cfg'd out), so the component's
# own code is linted here. Gating: `-D warnings`.
[doc("Lint the shipped component (wasm32-wasip2)")]
check *args:
    cargo clippy --target wasm32-wasip2 --lib --release {{ args }} -- -W clippy::pedantic -D warnings -D unused_must_use

# Lint the host-side unit tests (default/host target).
check-host *args:
    cargo clippy --all-targets {{ args }} -- -W clippy::pedantic -D warnings -D unused_must_use

# Apply rustfmt to the whole crate
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Run the unit tests (host target — the rules are pure and host-testable).
test *args:
    cargo test --locked {{ args }}

# Measure code coverage (requires cargo-llvm-cov). The component runs in
# wasmtime (wasm32) and isn't host-instrumentable, so this covers the rules in
# src/ as the host build sees them — which is all of them, since the rules are
# pure and only the wasi handler is cfg'd out. --remap-path-prefix keeps report
# paths relative (src/clean.rs, not the absolute build path).
# Usage: just coverage [--html]
[doc("Measure code coverage. Usage: just coverage [--html]")]
coverage *args:
    cargo llvm-cov --locked --remap-path-prefix --ignore-filename-regex 'tests/' {{ args }}

# Coverage for CI: write lcov.info and print a summary.
coverage-lcov:
    cargo llvm-cov --locked --remap-path-prefix --ignore-filename-regex 'tests/' --lcov --output-path lcov.info
    cargo llvm-cov report --summary-only --ignore-filename-regex 'tests/'

# Render the browsable HTML report to target/llvm-cov/html/, which the Pages
# job uploads verbatim. cargo-llvm-cov's index is a single flat file list;
# per-file pages show line-level coverage.
[doc("Render the browsable HTML report to target/llvm-cov/html/")]
coverage-html *args:
    cargo llvm-cov --locked --remap-path-prefix --ignore-filename-regex 'tests/' --html {{ args }}

# Drop a shields.io "endpoint" JSON beside the rendered HTML so the README badge
# reads the latest line-% straight from Pages — no Gist or PAT. `report` reuses
# the profile data coverage-html just collected, so run it after that recipe.
[doc("Write the shields.io badge JSON beside the rendered HTML")]
coverage-badge:
    #!/usr/bin/env bash
    set -euo pipefail
    pct=$(cargo llvm-cov report --json --summary-only --ignore-filename-regex 'tests/' \
      | jq -r '.data[0].totals.lines.percent')
    if   awk "BEGIN{exit !($pct>=90)}"; then color=brightgreen
    elif awk "BEGIN{exit !($pct>=75)}"; then color=green
    elif awk "BEGIN{exit !($pct>=60)}"; then color=yellow
    elif awk "BEGIN{exit !($pct>=40)}"; then color=orange
    else color=red
    fi
    printf '{"schemaVersion":1,"label":"coverage","message":"%.1f%%","color":"%s"}\n' \
      "$pct" "$color" > target/llvm-cov/html/coverage.json
    cat target/llvm-cov/html/coverage.json

# Full local CI gate: format, lint (component + host), build, test
ci: fmt-check check check-host build-component test
