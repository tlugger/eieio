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
# passes. Each epic appends its own crate as it lands.
#
# `eio-sdk` is here for a different reason than the rest, and not to prove
# `no_std`: `check-guest` already does that, because the crate defines a
# `#[panic_handler]` and anything pulling `std` in makes it a duplicate lang
# item (verified by breaking it). What these two targets add is the only build
# with NO atomics anywhere — rv32imc has no `A` extension. That leg has already
# earned its place: it is what found that `log::set_logger` is unavailable
# without compare-and-swap, which is a real constraint on what the SDK may
# depend on and was invisible from the guest target.
nostd_crates := "eio-abi eio-signal eio-expr eio-manifest eio-host-core eio-sdk"

# The crates a `sdk-vX.Y.Z` tag publishes, in dependency order — `cargo publish` needs each
# crate's `eio-*` dependencies already on the registry, so this is the publish order and not
# an alphabetical list. It lives here because both `release-sdk.yml` and `publish-dry-run.yml`
# need it, and a set duplicated across two workflows is one that drifts. `eio-conformance` is
# in it because `cargo-eio` depends on it for `cargo eio test`, which SDK §5's prose does not
# make obvious.
publish_set := "eio-abi eio-signal eio-expr eio-manifest eio-sdk-macros eio-host-core eio-sdk eio-conformance eio-test-host cargo-eio"

# The subset with no `eio-*` dependency of its own. Before the first publish these are the only
# crates `cargo publish --dry-run` can reach: packaging resolves a path dependency against the
# live registry, so a crate whose `eio-*` dependency is unpublished fails there, before any
# build, and no flag avoids it. Widen this to `publish_set` once the first `sdk-v` tag ships.
dry_runnable := "eio-abi eio-signal"

# The guest target (ABI §1). Blocks are core WASM modules and nothing else.
guest_target := "wasm32-unknown-unknown"

# ── compiler cache ───────────────────────────────────────────────────────────
#
# Optional: when `sccache` is on PATH, every recipe below that shells out to `cargo` routes
# `rustc` through it, so a fresh git worktree does not repay wasmtime+cranelift's cold-build
# cost from scratch (eieio-p0k.7 measured ~14 parallel agents each doing exactly that in one
# session — the single largest waste of that day). Absent `sccache`, `sccache_present`
# evaluates to "no" and `RUSTC_WRAPPER` is exported as an empty string, which cargo treats
# identically to the variable being unset (verified: a build with `RUSTC_WRAPPER=""` behaves
# the same as one with it absent) — so nothing here requires `sccache` to be installed.
#
# Deliberately NOT a shared `CARGO_TARGET_DIR`: that would move the cost onto cargo's own
# build-lock file and serialize concurrent worktrees against each other instead of speeding
# them up, which is the opposite of the goal (eieio-p0k.7 measured that failure mode too).
# `sccache` caches compiler *output* keyed on its inputs, not cargo's own bookkeeping, so it
# gets the win without the lock contention — each worktree keeps its own `target/`.
sccache_present := `command -v sccache >/dev/null 2>&1 && echo yes || echo no`
export RUSTC_WRAPPER := if sccache_present == "yes" { "sccache" } else { "" }


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

# `cargo test` runs each test BINARY sequentially and threads only within one binary. This
# workspace has one ~200s binary (eio-daemon's unit tests, dominated by 25 real sleeps/waits
# under crates/daemon/src — eieio-p0k.7) and everything else is under 35s combined, so that
# serialization is most of this recipe's wall time. `cargo nextest` runs one process per
# test and schedules across binaries regardless of which binary a test came from, which is
# exactly this suite's shape, so it is the default path; `cargo test` remains as a fallback
# for an environment without `cargo-nextest` installed.
#
# nextest does NOT run doctests — they are compiled as their own throwaway binaries by
# rustdoc, a mechanism nextest deliberately does not hook into (https://nexte.st/docs/faq).
# `cargo test --doc` therefore always runs afterward, regardless of which branch above ran,
# or doctest coverage silently disappears from the gate.

# Run the workspace test suite.
test:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v cargo-nextest >/dev/null 2>&1; then
        echo "test: cargo nextest run --workspace"
        cargo nextest run --workspace
    else
        echo "test: cargo-nextest not found on PATH, falling back to cargo test --workspace"
        # This branch runs doctests too, which is why `ci` only schedules `test-doc`
        # separately when nextest is present — otherwise they would run twice.
        cargo test --workspace
    fi

# Doctests, which `cargo nextest` cannot run (measured: 974 tests in 42s under nextest, while
# the doctest pass costs minutes — each doctest is its own compile). Its own recipe so `ci`
# can run it beside the other stages instead of after them, which is where the time was going.
test-doc:
    cargo test --doc --workspace

# The golden blocks (ABI §13.2) are their own cargo workspace under `examples/blocks/`, so
# `cargo test --workspace` above never sees them. Their harness half is checked by the
# conformance suite, which builds and drives them, and `cargo eio build` is run over all five
# by `cargo-eio`'s own tests — but their `tests/native.rs` files are SDK §6.1's other layer,
# and this is the only thing that runs them.

# Run the golden blocks' native tests (SDK §6.1).
test-golden:
    cargo test --manifest-path examples/blocks/Cargo.toml

# Build the golden blocks' wasm once, up front, so no test process has to.
#
# `ci` depends on this and it is not an optimisation. Two crates build these on demand —
# `eio_conformance::golden::build()` (reached from `suite::run_own`) and
# `eio_leaf::fixtures::build()` — and both shell out to this same `cargo build` against this
# same target directory. Under nextest every test binary is its own *process*, so neither
# crate's in-process memoisation helps: half a dozen of them race, cargo's lock serialises the
# builds but not a build against another process's *read*, and a reader that arrives while the
# artifact is being re-linked sees `No such file or directory` for a file that is there before
# and after. That is exactly how `ci` went red on `wamr_passes_the_conformance_suite` looking
# for `transform.wasm`, which passed on its own immediately afterwards.
#
# Building first makes every later invocation a no-op that touches no artifact, which is the
# fix: the race needs a *writer*, and after this there is none.
build-golden:
    cargo build --release --manifest-path examples/blocks/Cargo.toml --target {{ guest_target }}

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

# `check-nostd` proves the SDK has no `std`; this proves it is a usable guest.
# They are not the same claim and neither implies the other: the bare-metal
# targets never compile the allocator or the panic handler (both are gated to
# wasm32, because `dlmalloc` has no backend for them), so this is the only gate
# that builds the glue a block actually ships with.
#
# `panic=abort` because SDK §4 requires it — panics become traps, and a guest
# has no unwinder. Passed here as a flag rather than set in a profile so that it
# is checked the same way `cargo eio build` will set it (SDK §5, eieio-7d8.5).

# Prove the SDK builds as a guest: wasm32, panic=abort.
check-guest:
    RUSTFLAGS="-C panic=abort" cargo build --quiet --package eio-sdk --target {{ guest_target }}

# ── the gate ─────────────────────────────────────────────────────────────────

# The one command CI runs. Dependencies run in order; the first failure aborts.
# Prove the publish set is publishable, as far as that can be proven before the first publish.
#
# Two checks, because neither covers the other. `cargo publish --dry-run` is the real thing but
# reaches only `dry_runnable`; `cargo metadata` needs no network and so covers every crate,
# confirming the three fields crates.io requires are present. Publishability then rots loudly
# here rather than at release time, which is the point (SCOPE §7.2).
publish-dry-run:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo metadata --no-deps --format-version 1 > /tmp/eio-publish-meta.json
    python3 -c 'import json,sys; m=json.load(open("/tmp/eio-publish-meta.json")); want=sys.argv[1].split(); pk={p["name"]:p for p in m["packages"]}; bad=[(n,f) for n in want for f in ("description","license","repository") if not pk.get(n,{}).get(f)]; missing=[n for n in want if n not in pk]; [sys.stderr.write("not in this workspace: %s\n" % n) for n in missing]; [sys.stderr.write("%s has no %s\n" % (n,f)) for n,f in bad]; sys.exit(1 if (bad or missing) else 0)' "{{ publish_set }}"
    echo "  registry metadata present for every crate in the publish set"
    for crate in {{ dry_runnable }}; do
        echo "  $crate → cargo publish --dry-run"
        cargo publish --dry-run -p "$crate"
    done

# `fmt-check`, `lint`, `check-nostd` and `check-guest` depend on neither `test` nor on each
# other (eieio-p0k.7), so they run concurrently with `test` below rather than strictly
# before it — `test`'s ~200s daemon binary was most of this recipe's wall time (see the
# comment on `test` above), and the other four were never waiting on it for anything.
# `build` still runs first as a fast-failing baseline compile, and `test-golden` still runs
# last since it is examples/blocks' own cargo workspace.
#
# `just` has no built-in concurrent-dependency execution (there is no `--jobs` for recipe
# prerequisites), so the fan-out is hand-rolled below. Each stage's stdout/stderr is
# captured to its own file rather than left to interleave live on one terminal — five
# `cargo` invocations printing at once is not a faster gate, it is a coin flip on whether
# you can find the one that failed — and printed in full, labeled by stage name, once that
# stage finishes. All five are allowed to run to completion even if one fails early, so a
# second broken gate cannot hide behind "never got to run"; first-failure-aborts is restored
# at the end by exiting non-zero, with every failed stage named on the final line, if any
# stage did.

# The one command CI runs: builds, then fmt/lint/test/nostd/guest concurrently, then the golden blocks.
ci: build build-golden
    #!/usr/bin/env bash
    set -euo pipefail
    logdir="$(mktemp -d)"
    trap 'rm -rf "$logdir"' EXIT

    # `test-doc` only when nextest is present: without it, `test` already ran doctests
    # via `cargo test --workspace`, and scheduling both would run them twice.
    stages=(fmt-check lint test check-nostd check-guest)
    if command -v cargo-nextest >/dev/null 2>&1; then
        stages+=(test-doc)
    fi
    # The SPA's own suite, only where a JS toolchain exists. The Rust half of the Designer
    # builds and tests without one — `crates/designer` embeds `designer/dist`, and an empty
    # `dist` is a valid build that serves no UI — so a machine with no npm is degraded, not
    # broken, and says so rather than failing a gate it cannot run.
    if command -v npm >/dev/null 2>&1; then
        stages+=(test-designer)
    else
        echo "ci: npm not found — skipping test-designer (the Designer SPA's suite)" >&2
    fi
    pids=()
    for stage in "${stages[@]}"; do
        just "$stage" > "$logdir/$stage.log" 2>&1 &
        pids+=("$!")
    done

    failed=()
    for i in "${!stages[@]}"; do
        stage="${stages[$i]}"
        pid="${pids[$i]}"
        status=0
        wait "$pid" || status=$?
        echo "═══ ${stage} ═══"
        cat "$logdir/$stage.log"
        if [ "$status" -ne 0 ]; then
            echo "═══ ${stage}: FAILED (exit $status) ═══"
            failed+=("$stage")
        else
            echo "═══ ${stage}: passed ═══"
        fi
    done

    if [ "${#failed[@]}" -gt 0 ]; then
        echo "ci: failed stage(s): ${failed[*]}" >&2
        exit 1
    fi

    just test-golden
    echo "ci: all gates passed"

# ── designer ─────────────────────────────────────────────────────────────────
#
# Two halves, and only one of them is cargo's. `crates/designer` embeds
# `designer/dist` at compile time, and that directory is gitignored — so a fresh
# clone compiles the server against an EMPTY `dist` and serves a 404 for `/`
# until the SPA is built. That is deliberate: it keeps `just ci` runnable with no
# JS toolchain installed, and the binary also checks a runtime `--assets-dir`
# first, so a rebuild is not needed to pick up freshly built assets.

# Install the SPA's dependencies. `npm ci` when there is a lockfile to honour.
designer-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    cd designer
    if [ -f package-lock.json ]; then npm ci; else npm install; fi

# Compile `expr` to WASM for the browser (DESIGNER §1, §5).
#
# `crates/expr-wasm/pkg/` is generated and gitignored (wasm-pack writes its own
# `.gitignore` containing `*`), so a fresh clone has no `pkg/` at all and the SPA's
# `import … from '…/expr-wasm/pkg/eio_expr_wasm.js'` cannot resolve. That is not a
# missing-file inconvenience: keystroke linting IS the real interpreter (§5), so
# without this the SPA does not type-check, let alone run. Every recipe that touches
# the SPA depends on it for that reason.
designer-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v wasm-pack >/dev/null 2>&1; then
        echo "designer-wasm: wasm-pack is not installed (cargo install wasm-pack)" >&2
        exit 1
    fi
    cd crates/expr-wasm && wasm-pack build --target web --release

# Build the SPA into `designer/dist`, which is what the server embeds and serves.
designer-build: designer-wasm designer-deps
    cd designer && npm run build

# The SPA's own suite: the derived-value rules, the manifest-reference match, the
# operation builders, and the linter against the real interpreter.
test-designer: designer-wasm designer-deps
    #!/usr/bin/env bash
    set -euo pipefail
    cd designer
    npm run check
    npm run test -- --run

# ── run recipes ──────────────────────────────────────────────────────────────

# Everything after the recipe name is passed through, so `just run-daemon dev
# run-block ./block.wasm --batch '[{"t":1}]'` works. Services and a listening
# daemon arrive with their own epics; today `dev run-block` is the whole surface.

# Run the daemon. Arguments are passed through.
run-daemon *args:
    cargo run --package eio-daemon -- {{ args }}

# Run the CLI. Arguments are passed through, so `just eio service show kitchen.toml` works.
eio *args:
    cargo run --quiet --package eio-cli -- {{ args }}

# Run the Designer (not built yet).
# Run the Designer: build the SPA, then serve it from the Rust binary.
run-designer *args: designer-build
    cargo run --package eio-designer -- --assets-dir designer/dist {{ args }}

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

# The Designer's bare binary (DESIGNER-SPEC §1's other half, beside the container image
# below). Same two Linux targets as `release-daemon` — the server and the Pi-class node of
# SCOPE §3.7 — dynamically linked against glibc, same as the daemon's. This is deliberately
# NOT the musl build: that one exists only inside `Dockerfile`, for the container image
# alone, and the reasoning for why the two builds differ lives in that file's header
# comment, not here.
#
# The SPA must exist first (`designer-build`) or the binary embeds nothing to serve — see
# `Dockerfile`'s header comment for the failure this ordering exists to prevent.

# Build and package the Designer bare binary for one target.
release-designer target: designer-build
    #!/usr/bin/env bash
    set -euo pipefail
    rustup target add "{{ target }}"
    cargo build --release --package eio-designer --target "{{ target }}"

    mkdir -p "{{ dist }}"
    staging="$(mktemp -d)"
    install -m 0755 "target/{{ target }}/release/eio-designer" "$staging/eio-designer"
    install -m 0644 LICENSE "$staging/LICENSE"
    tar --create --gzip \
        --file "{{ dist }}/eio-designer-{{ target }}.tar.gz" \
        --directory "$staging" eio-designer LICENSE
    rm -rf "$staging"

    ( cd "{{ dist }}" && shasum --algorithm 256 "eio-designer-{{ target }}.tar.gz" \
        > "eio-designer-{{ target }}.tar.gz.sha256" )
    ls -l "{{ dist }}"

# The tag `docker build` without arguments below produces, for local use only — the release
# workflow tags its own build for ghcr.io instead (`docker/metadata-action`), so nothing here
# needs to match that scheme.
designer_image_tag := "eio-designer:local"

# See `Dockerfile`'s header comment for what the image contains and why it is musl where the
# bare binary above is not. `--platform linux/amd64` is passed here too, not left to each
# `FROM`'s own pin: without it, a build on an arm64 host still produces a working image
# (BuildKit still resolves every `FROM` to the pinned platform, and the emulated binary
# runs), but the image's own declared platform metadata defaults to the host's — arm64
# metadata wrapped around an amd64 binary. Passing it here makes the image tell the truth
# about itself on every host, including this one.

# Build the Designer's container image. Requires Docker.
designer-image:
    docker build --platform linux/amd64 --file Dockerfile --tag {{ designer_image_tag }} .

# ── housekeeping ─────────────────────────────────────────────────────────────

# Reclaim disk from build artifacts no build needs any more.
#
# `target/` grows without bound: cargo never garbage-collects the artifacts of
# commits you have moved past, so every rebuild leaves the last one behind. This
# workspace reached **66G**, of which 41G was reclaimable — enough to fill a disk
# and wedge the machine, which it has done here once.
#
# Two kinds of waste, and only one of them is safe to delete by hand:
#
#   - `tmp/` is cargo's scratch. Deleting it costs nothing.
#   - `debug/deps/` is where the weight is, and it CANNOT be pruned by mtime with
#     `find`: cargo pairs every artifact with a fingerprint, and deleting a
#     `.rlib` while its `.fingerprint` survives leaves cargo believing a crate is
#     fresh when its output is gone — a confusing link error, not a clean rebuild.
#     `cargo-sweep` understands that pairing, which is why this shells out to it
#     rather than hand-rolling the walk.
#
# Deliberately NOT wired into `ci`: cleaning before a gate forces a full rebuild,
# which on macOS also re-pays first-exec scanning of every test binary
# (eieio-p0k.9). Run it when the disk is tight, not on a schedule.
#
# Deliberately does NOT touch sibling worktrees (`../eieio-wt-*`). Each has its
# own `target/`, and deleting one out from under a build in progress is how you
# get a failure nobody can reproduce. Clean those by removing the worktree.
clean-stale days="14":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -d target ]; then echo "clean-stale: no target/ here, nothing to do"; exit 0; fi
    before="$(du -sk target | cut -f1)"

    rm -rf target/tmp

    if command -v cargo-sweep >/dev/null 2>&1; then
        # `--time` keeps anything touched in the last N days, so a tree you are
        # actively working in survives; `--installed` additionally drops output
        # from toolchains rustup no longer has, which a toolchain bump orphans.
        cargo sweep --time {{ days }} .
        cargo sweep --installed .
    else
        echo "clean-stale: cargo-sweep is not installed, so only cargo's scratch was" >&2
        echo "  removed — the artifacts in target/debug/deps are the bulk of the weight" >&2
        echo "  and pruning them safely needs it. Install: cargo install cargo-sweep" >&2
    fi

    after="$(du -sk target | cut -f1)"
    # awk rather than `numfmt`, which is GNU coreutils and absent on macOS —
    # the platform this is most likely to be run on.
    human() { awk -v k="$1" 'BEGIN {
        split("K M G T", u, " "); i = 1
        while (k >= 1024 && i < 4) { k /= 1024; i++ }
        printf "%.1f%s", k, u[i]
    }'; }
    printf 'clean-stale: %s -> %s (freed %s)\n' \
        "$(human "$before")" "$(human "$after")" "$(human "$((before - after))")"

# Show what `clean-stale` would remove, without removing it.
clean-stale-dry days="14":
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v cargo-sweep >/dev/null 2>&1; then
        echo "clean-stale-dry: needs cargo-sweep (cargo install cargo-sweep)" >&2
        exit 1
    fi
    du -sh target 2>/dev/null || true
    cargo sweep --dry-run --time {{ days }} .
