# eieio — the single command surface.
#
# CI runs `just ci` and nothing else (SCOPE §7.1), so recipe names are a stable
# interface: rename one and you break CI as well as everyone's muscle memory.
# Add recipes freely; think hard before renaming or removing one.
#
# Note on comments: `just --list` shows only the LAST comment line above a
# recipe, so every recipe gets a one-line summary immediately above it, with any
# longer explanation in a block separated by a blank line.

set shell := ["bash", "-euo", "pipefail", "-c"]

# ── what check-nostd proves, and against what ────────────────────────────────
#
# The ★ crates (DAEMON-SPEC §1) compile into the MCU leaf runtime, so they MUST
# stay no_std (`alloc` is permitted). Neither target below ships a `std`, so an
# accidental `std` dependency simply fails to compile — that is the whole gate.
# Two targets, because they catch different mistakes:
#
#   Cortex-M4F   thumbv7em-none-eabihf        hard float, HAS atomics.
#                The common bare-metal Rust baseline.
#
#   ESP32-C3/C6  riscv32imc-unknown-none-elf  no FPU, and NO atomics — rv32imc
#                lacks the A extension, so this one also rejects any dependency
#                that assumes atomic compare-and-swap. ESP32 is the leaf-class
#                exemplar in SCOPE §3.7.
#
# Do not "simplify" this to the classic ESP32: that chip is Xtensa and needs the
# esp-rs toolchain fork, so it can never be a stock-rustup gate. Both targets
# here are declared in rust-toolchain.toml, so rustup installs them for you.
target_cortex_m4f := "thumbv7em-none-eabihf"
target_esp32c3 := "riscv32imc-unknown-none-elf"
nostd_targets := target_cortex_m4f + " " + target_esp32c3

# The ★ crates, by package name (DAEMON-SPEC §1 maps directory → package).
# Listed explicitly rather than globbed so a partially populated workspace still
# passes. Each epic appends its own crate as it lands: eio-sdk (eieio-7d8).
nostd_crates := "eio-signal eio-expr eio-manifest eio-host-core"

# List the available recipes.
default:
    @just --list --unsorted

# ── developer loop ───────────────────────────────────────────────────────────

# Format everything in place.
fmt:
    cargo fmt --all

# Upstream rustfmt defaults, so there is deliberately no rustfmt.toml.

# Formatting gate — fails instead of rewriting.
fmt-check:
    cargo fmt --all --check

# Warnings are denied here, not in any Cargo.toml, so a plain `cargo build`
# stays usable while this gate stays strict.

# Clippy with warnings denied, plus the lint opt-in check.
lint: check-lint-optin
    cargo clippy --all-targets --all-features -- -D warnings

# The shared [workspace.lints] baseline in the root Cargo.toml is opt-in per
# crate: a member missing `[lints] workspace = true` silently gets no lints at
# all, and clippy still passes. This catches that. Members come from cargo
# metadata, not a crates/* glob, because examples/blocks/* will hold crates that
# are deliberately NOT workspace members and must not carry the opt-in.

# Check that every workspace member opts into [workspace.lints].
check-lint-optin:
    #!/usr/bin/env bash
    set -euo pipefail
    missing=""
    for manifest in $(cargo metadata --no-deps --format-version 1 \
        | tr ',' '\n' | grep -o '"manifest_path":"[^"]*"' | sed 's/.*:"//;s/"$//'); do
        if ! awk '/^\[lints\]/{f=1;next} f&&/^\[/{f=0} f&&/^workspace[[:space:]]*=[[:space:]]*true/{ok=1} END{exit ok?0:1}' "$manifest"; then
            missing="$missing  $manifest"$'\n'
        fi
    done
    if [ -n "$missing" ]; then
        echo "error: workspace member(s) do not opt into [workspace.lints]:" >&2
        printf '%s' "$missing" >&2
        echo "add this to each manifest above:" >&2
        echo "" >&2
        echo "  [lints]" >&2
        echo "  workspace = true" >&2
        exit 1
    fi

# Build the workspace, including test and example targets.
build:
    cargo build --workspace --all-targets

# Run the workspace test suite.
test:
    cargo test --workspace

# See the comment block at the top of this file for the target rationale.

# Prove the ★ crates still build without std.
check-nostd:
    #!/usr/bin/env bash
    set -euo pipefail
    for target in {{ nostd_targets }}; do
        for crate in {{ nostd_crates }}; do
            echo "  $crate → $target"
            cargo build --quiet --package "$crate" --target "$target"
        done
    done

# ── the gate ─────────────────────────────────────────────────────────────────

# The one command CI runs. Dependencies run in order; the first failure aborts.
ci: fmt-check lint build test check-nostd
    @echo "ci: all gates passed"

# ── run recipes ──────────────────────────────────────────────────────────────

# Everything after the recipe name is passed through, so `just run-daemon dev
# run-block ./block.wasm --batch '[{"t":1}]'` works. Services and a listening
# daemon arrive with their own epics; today `dev run-block` is the whole surface.

# Run the daemon. Arguments are passed through.
run-daemon *args:
    cargo run --package eio-daemon -- {{ args }}

# Run the Designer (not built yet).
run-designer:
    #!/usr/bin/env bash
    echo "run-designer: the Designer does not exist yet — it lands with eieio-m9s.1." >&2
    exit 1

# ── release ──────────────────────────────────────────────────────────────────
#
# Release pipelines are per deployable component and tag-triggered (SCOPE §7.2),
# and the tag names the component: `daemon-v0.1.0`, never a bare `v0.1.0`. A
# shared tag would fire every pipeline and leave each one to filter itself back
# out, which is not the independence §7.2 asks for.
#
# The build lives here rather than in the workflow for the same reason `ci` does:
# a release step a contributor cannot run locally is a step nobody can debug when
# it fails at 2 a.m. `release-daemon` is exactly what CI runs, and it runs the
# same way on a laptop.

# Where `release-daemon` leaves its tarballs.
dist := "dist"

# Build and package the daemon for one target. Cross targets need their linker.
release-daemon target:
    #!/usr/bin/env bash
    set -euo pipefail
    rustup target add "{{ target }}"
    cargo build --release --package eio-daemon --target "{{ target }}"

    # Named for the target, because the artifacts of several targets land in one
    # GitHub Release and `eio-daemon` twice would be ambiguous.
    mkdir -p "{{ dist }}"
    staging="$(mktemp -d)"
    install -m 0755 "target/{{ target }}/release/eio-daemon" "$staging/eio-daemon"
    install -m 0644 LICENSE "$staging/LICENSE"
    tar --create --gzip \
        --file "{{ dist }}/eio-daemon-{{ target }}.tar.gz" \
        --directory "$staging" eio-daemon LICENSE
    rm -rf "$staging"

    # A checksum per artifact rather than one manifest for all of them: each is
    # downloaded on its own, and a file listing checksums for archives you did not
    # fetch is a file nobody checks.
    ( cd "{{ dist }}" && shasum --algorithm 256 "eio-daemon-{{ target }}.tar.gz" \
        > "eio-daemon-{{ target }}.tar.gz.sha256" )
    ls -l "{{ dist }}"
