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
#
# `eio-leaf` is here for the third distinct reason, and it is the one this gate was always
# aiming at: LEAF §2 calls a leaf "a `no_std` Rust firmware image", so the crate that will
# become one has a boundary drawn through it — `--no-default-features` builds its runtime
# half (`spawn`, the scheduler, the budgets, the router wiring) and leaves the wasm3
# binding, the file-backed state store, the host clock and entropy, and the demo binary
# behind. That is not the whole crate and is not meant to be; `crates/leaf/src/lib.rs`
# enumerates what does not cross and why. What this leg buys is that the line stays where
# it was put.
#
# `eio-leaf-mono` is here for the fourth reason, and it exists only for this gate: it is what
# makes the `eio-leaf` leg above emit machine code rather than only type-check (eieio-x7g.2.19).
# Almost everything that crosses the leaf's boundary is generic — `spawn<E, C, R, S>`,
# `timer::Scheduler<C>`, `timer::pump<E, C>` — and rustc monomorphises a generic function only
# where it is instantiated. `eio-leaf` instantiates none of its own: every call site is in
# `main.rs` or `tests/`, and both are `std`. Measured before this crate existed, the whole of
# `libeio_leaf.rlib` on rv32imc was `leaf_budgets`, `leaf_limits`, `timer::Scheduled` and
# `BakedGraph`'s non-generic methods — `spawn` and `pump` emitted nothing at all.
#
# `eio-leaf-mono` instantiates them at concrete types and nothing else; `crates/leaf-mono/src/lib.rs`
# is the long version, including why it is a crate of its own rather than a `#[cfg]` block inside
# `crates/leaf` (which is the crate that becomes firmware) and how it stays clear of what
# eieio-x7g.4 refused. Both legs are wanted: `eio-leaf` proves the *library* has no `std` in it,
# `eio-leaf-mono` proves those bodies also lower to instructions for a target with no atomics and
# no FPU. Deleting either weakens the gate; the second one goes away when eieio-x7g.2.11's real
# engine, clock and store make it redundant.
nostd_crates := "eio-abi eio-signal eio-expr eio-manifest eio-host-core eio-sdk eio-leaf eio-leaf-mono"

# The crates a `sdk-vX.Y.Z` tag publishes, in dependency order — `cargo publish` needs each
# crate's `eio-*` dependencies already on the registry, so this is the publish order and not
# an alphabetical list. It lives here because both `release-sdk.yml` and `publish-dry-run.yml`
# need it, and a set duplicated across two workflows is one that drifts. `eio-conformance` is
# in it because `cargo-eio` depends on it for `cargo eio test`, which SDK §5's prose does not
# make obvious.
#
# It is the closure of those roots over *normal* dependencies, and dev edges are deliberately
# not in it: a consumer links a published crate's library and never runs its tests. That is
# why `eio-wamr-host` is absent even though `eio-conformance` dev-depends on it — publishing
# an FFI crate that builds WAMR's C core, reachable from nothing on the registry, is a support
# commitment bought for nothing (eieio-7d8.39). The corresponding obligation is on the use
# site: such an edge must be path-only so cargo strips it when packaging, and
# `publish-dry-run` fails if one is not.
publish_set := "eio-abi eio-signal eio-expr eio-manifest eio-sdk-macros eio-host-core eio-sdk eio-conformance eio-test-host cargo-eio"

# The subset with no `eio-*` dependency of its own. Before the first publish these are the only
# crates `cargo publish --dry-run` can reach: packaging resolves a path dependency against the
# live registry, so a crate whose `eio-*` dependency is unpublished fails there, before any
# build, and no flag avoids it. Widen this to `publish_set` once the first `sdk-v` tag ships.
dry_runnable := "eio-abi eio-signal"

# The guest target (ABI §1). Blocks are core WASM modules and nothing else.
guest_target := "wasm32-unknown-unknown"

# ── disk headroom (eieio-7d8.40) ─────────────────────────────────────────────
#
# A full volume does not fail this gate honestly. Three failure shapes were observed in a
# single session of parallel agent work, and none of them said "disk":
#
#   - `rustc-LLVM ERROR: IO failure on output stream: No space left on device`, whose LAST
#     line — the one a summary shows — is `error: could not compile eio-manifest`. That reads
#     as a code failure and cost a debugging session, three times.
#   - A `crates/daemon` API test failing with `Io(Os { code: 22, kind: InvalidInput })` at
#     5.4G free, passing unchanged at 14G. That one does not even mention IO capacity.
#   - Everything downstream of either, which then looks like a cascade of real defects.
#
# So `ci` both looks before it starts and says so afterwards. The numbers below are measured,
# and each defends against something specific:
#
#   ci_write     10G    What ONE cold `just ci` writes, not what a `target/` can grow to.
#                       Measured across five independently built trees of this repository —
#                       `target/` plus `examples/blocks/target/` came to 8.7G, 8.2G, 8.2G,
#                       5.6G and 0.3G — so 10 is the top of that range rounded up. A
#                       long-lived worktree accretes far past it (~34G is on record) as
#                       profiles and feature combinations pile up, which is exactly why
#                       `check-disk` measures what is already on disk and subtracts it rather
#                       than assuming a fixed cost: a preflight that charged an incremental
#                       run for a cold one's writes would fire constantly, and a check that
#                       fires constantly is worse than no check at all.
#
#   floor         6G    The one REFUSAL, and the only number here that is not an estimate:
#                       at 5.4G free this workspace's own suite produced a WRONG ANSWER — a
#                       green test went red with an errno that names nothing about disk. A
#                       gate that lies is worse than a gate that did not run, so below this
#                       `ci` declines rather than reporting a result nobody should act on.
#
#   margin        8G    A WARNING on the projected end state, because each additional git
#                       worktree building concurrently was measured to add 8-9G — so a run
#                       projected to finish with less than that in hand is one agent away
#                       from the floor, through no fault of its own. A warning and not a
#                       refusal: the projection is an estimate, and an estimate must never
#                       fail a build. It names the risk and gets out of the way.
#
# All three are overridable, which is also how they are tested: `EIO_DISK_FLOOR_GB=999 just
# check-disk` demonstrates the refusal on any machine. An override states a number you are
# accepting; there is deliberately no flag that skips the check without naming one.
disk_ci_write_gb := env("EIO_DISK_CI_WRITE_GB", "10")
disk_floor_gb := env("EIO_DISK_FLOOR_GB", "6")
disk_margin_gb := env("EIO_DISK_MARGIN_GB", "8")

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
#
# `cd` rather than `--manifest-path`, and that is load-bearing rather than a style: cargo
# discovers `.cargo/config.toml` by walking up from the *working directory*, and
# `examples/blocks/.cargo/config.toml` is where SDK §5.2's shadow-stack default lives for
# these blocks. Pointed at the manifest from here it would be silently ignored, and the
# golden blocks would go back to declaring 17 pages of linear memory — which is also the
# working directory `eio_conformance::golden::build` and `eio_leaf::fixtures::build` use,
# so all three plain builds agree.
build-golden:
    cd examples/blocks && cargo build --release --target {{ guest_target }}

# Emit the daemon's and the Designer backend's own response shapes for the Designer's parity
# checks.
#
# This is the ONLY writer of designer/src/lib/api/__generated__/, and it is a prerequisite of
# every recipe that runs the SPA's suite — the same relationship designer-wasm has to
# crates/expr-wasm/pkg, and for the same reason: a gitignored artifact a fresh clone does not
# have is a prerequisite, not something a test can conjure for itself.
#
# It used to be neither. Both parity suites regenerated their own files from beforeAll, because
# ci's stages run in parallel and a check that trusted a stale file would be worse than the
# drift it catches. But that shells out to cargo from inside vitest while the test stage's own
# cargo holds the target-directory lock — so on a cold checkout the hook waits out the whole
# workspace build and times out (CI, 2026-09-03: "Hook timed out in 120000ms"), then passes on
# the second run against a now-warm cargo. Self-healing on a second run is the worst shape a CI
# failure can have (eieio-m9s.42). The suites now only READ, and fail loudly naming this recipe
# when the file is missing or older than the Rust sources it came from — see
# designer/src/lib/api/generated-shapes.ts.
#
# EIO_SHAPES_PREGENERATED makes this recipe a no-op rather than making a test skip a check. ci
# runs it once as a dependency, up front, then exports the variable — so the parallel
# "just test-designer" subprocess inherits it and does not put a second cargo on the
# target-directory lock the test stage is already holding.
#
# The two cargo calls run one after the other in this one recipe body, never in parallel with
# each other — eieio-m9s.33 added the second without turning this into two racing writers; see
# crates/designer/tests/response_shapes.rs's module doc for why that file emits to a sibling
# JSON file rather than sharing this one's.
shapes:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${EIO_SHAPES_PREGENERATED:-}" ]; then
        echo "shapes: already generated by this run (EIO_SHAPES_PREGENERATED) — skipping"
        exit 0
    fi
    cargo test -p eio-cli --test response_shapes
    cargo test -p eio-designer --test response_shapes

# See the comment block at the top of this file for the target rationale.

# `--no-default-features` is a no-op for every ★ crate — none of them declares a
# `[features]` table at all — and is load-bearing for exactly one member: `eio-leaf`
# defaults to `std` so that its host bring-up, its binary and its whole `tests/` tree
# keep working untouched, and this is the flag that asks it for the firmware half
# instead. Passed to the whole loop rather than special-casing one crate, so the list
# above stays a plain list.

# `cargo build` and not `cargo check`, which is load-bearing rather than habit: `check` stops
# at type-checking, and `eio-leaf-mono` is in the list above to be *codegen'd*. Swapping this
# for `cargo check` to make the gate faster would silently delete the eieio-x7g.2.19 leg while
# leaving every line of it in place.

# Prove the ★ crates and the leaf's runtime half still build — and codegen — without std.
check-nostd:
    #!/usr/bin/env bash
    set -euo pipefail
    for target in {{ nostd_targets }}; do
        for crate in {{ nostd_crates }}; do
            echo "  $crate → $target"
            cargo build --quiet --package "$crate" --no-default-features --target "$target"
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

# See the `disk headroom` block at the top of this file for where 10, 6 and 8 come from and
# what each one defends against. This recipe is a *precondition* check, not a gate stage: it
# says nothing about the code, only about whether this machine can give an answer about it.
# `ci` runs it first so it reports on the disk as it stands immediately before the first
# cargo invocation, which is when the estimate is at its most accurate.
#
# `du` over the two build trees rather than a flat threshold, because the question is not
# "how much is free" but "how much still has to be written". It costs well under a second on
# APFS (measured: 0.46s over an 8.2G tree), which is nothing against the build it precedes.

# Report free disk; refuse below the floor, warn when the run will finish tight.
check-disk:
    #!/usr/bin/env bash
    set -euo pipefail
    free_gb=$(df -Pk . | awk 'NR==2 { print int($4 / 1048576) }')
    have_kb=0
    for tree in target examples/blocks/target; do
        [ -d "$tree" ] || continue
        # `tail -1` because a partially unreadable tree still prints a usable total after its
        # complaints; the guard after it is what makes an unusable answer count as zero
        # rather than aborting a check whose whole job is to be advisory.
        size=$(du -sk "$tree" 2>/dev/null | tail -1 | awk '{ print $1 }') || true
        case "${size:-}" in ''|*[!0-9]*) size=0 ;; esac
        have_kb=$((have_kb + size))
    done
    have_gb=$((have_kb / 1048576))
    to_write_gb=$(( {{ disk_ci_write_gb }} - have_gb ))
    [ "$to_write_gb" -ge 0 ] || to_write_gb=0
    projected_gb=$((free_gb - to_write_gb))
    echo "check-disk: ${free_gb}G free · ${have_gb}G of build output already here · ~${to_write_gb}G still to write → ~${projected_gb}G left at the end"
    if [ "$free_gb" -lt {{ disk_floor_gb }} ]; then
        echo "error: ${free_gb}G free is below the {{ disk_floor_gb }}G floor — refusing to start." >&2
        echo "       This is not caution about speed. At 5.4G free this workspace's own suite" >&2
        echo "       failed a passing test with Io(Os { code: 22, kind: InvalidInput }), which" >&2
        echo "       names nothing about disk. A run from here would report something, and the" >&2
        echo "       something would not be trustworthy." >&2
        echo "       Free space, or state the number you are accepting: EIO_DISK_FLOOR_GB=<n> just ci" >&2
        exit 1
    fi
    if [ "$projected_gb" -lt {{ disk_margin_gb }} ]; then
        echo "warning: this run is projected to finish with ~${projected_gb}G free, under the {{ disk_margin_gb }}G margin." >&2
        echo "         Each concurrent worktree building adds 8-9G, so one more agent starting" >&2
        echo "         now takes this below the floor and failures stop meaning what they say." >&2
        echo "         Proceeding: the {{ disk_ci_write_gb }}G estimate is an estimate, and an estimate must not fail a build." >&2
    fi

# ── the gate ─────────────────────────────────────────────────────────────────

# Three checks, because none covers the others. `cargo publish --dry-run` is the real thing but
# reaches only `dry_runnable`; the two static checks below need no network and so cover every
# crate in the set. Publishability then rots loudly here rather than at release time, which is
# the point (SCOPE §7.2).
#
# The static half is one Python program with two jobs:
#
#   1. The three fields crates.io's publish endpoint requires — `description`, `license`,
#      `repository` — are present on every crate in the set.
#
#   2. **The set is closed over the edges that survive packaging, and ordered.** This is the
#      leg eieio-7d8.39 added, and it exists because the first one did not catch that bug:
#      `eio-conformance` is in the set and grew a *versioned* dev-dependency on
#      `eio-wamr-host`, which is not, so a `sdk-v*` tag would have failed on an unpublished
#      dependency months after anyone remembered adding the edge. The rule is read off cargo's
#      own packaging behaviour rather than a convention: a workspace dependency whose `req` is
#      `*` (path-only) is *stripped* when cargo packages, and one that carries a version is
#      *kept* — so every kept edge into another workspace member must land on a crate that is
#      in the set and published before it, and every stripped one must be a dev-dependency,
#      because a path-only normal or build dependency is rejected outright. That covers the
#      ordering too, which nothing checked before: `publish_set` is a hand-written topological
#      sort and `release-sdk.yml` publishes in exactly that order.
#
#      Dependencies on crates that are not workspace members are not this check's business —
#      they are on the registry already, which is what makes them buildable at all.

# Prove the publish set is publishable, as far as that can be proven before the first publish.
publish-dry-run:
    #!/usr/bin/env bash
    set -euo pipefail
    meta="$(mktemp)"
    trap 'rm -f "$meta"' EXIT
    cargo metadata --no-deps --format-version 1 > "$meta"
    python3 - "$meta" "{{ publish_set }}" <<'PUBLISH_SET_CHECK'
    import json, sys

    meta_path, publish_set = sys.argv[1], sys.argv[2].split()
    meta = json.load(open(meta_path))
    pkgs = dict((p["name"], p) for p in meta["packages"])
    members = set(pkgs)
    errors = []

    for name in publish_set:
        if name not in pkgs:
            errors.append("%s is in publish_set but is not a member of this workspace" % name)

    for i, name in enumerate(publish_set):
        p = pkgs.get(name)
        if p is None:
            continue
        for field in ("description", "license", "repository"):
            if not p.get(field):
                errors.append("%s has no %s, which crates.io requires" % (name, field))
        for dep in p["dependencies"]:
            dname = dep["name"]
            if dname not in members:
                continue
            kind = dep.get("kind")
            label = "dev-dependency" if kind == "dev" else ("%s dependency" % (kind or "normal"))
            if dep["req"] == "*":
                if kind != "dev":
                    errors.append(
                        "%s has a path-only %s on workspace crate %s. cargo strips a "
                        "version-less edge only for dev-dependencies; a normal or build one "
                        "is rejected at publish. Give it the workspace version."
                        % (name, label, dname))
                continue
            if dname not in publish_set:
                errors.append(
                    "%s is in publish_set and has a %s on workspace crate %s that carries a "
                    "version, so the edge survives packaging - but %s is not in publish_set. "
                    "cargo publish -p %s would fail on an unpublished dependency. Either add "
                    "%s to publish_set ahead of %s, or drop the version key at the use site "
                    "so cargo strips the edge, which is legal for a dev-dependency only."
                    % (name, label, dname, dname, name, dname, name))
            elif publish_set.index(dname) > i:
                errors.append(
                    "publish_set is out of dependency order: %s is published before %s, but "
                    "%s is a %s of it" % (name, dname, dname, label))

    for e in errors:
        sys.stderr.write("error: %s\n" % e)
    sys.exit(1 if errors else 0)
    PUBLISH_SET_CHECK
    echo "  registry metadata present, and the publish set is closed and ordered"
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
#
# `build`, `build-golden` and `shapes` used to be `just` dependencies of this recipe and are
# now its first three stages instead (eieio-7d8.40). Nothing about their order changed — they
# still run sequentially, before anything else, and the first failure still aborts. What
# changed is that a dependency's output goes nowhere this recipe can read, and a run that dies
# in `build` therefore reached no summary at all: the last line on the terminal was cargo's
# `error: could not compile eio-manifest`, four lines under an LLVM `No space left on device`
# that nobody scrolls back to. `tee` keeps them live AND greppable, so `disk_verdict` below
# sees every stage of the run rather than only the parallel ones.

# The one command CI runs: builds, then fmt/lint/test/nostd/guest concurrently, then the golden blocks.
ci: check-disk
    #!/usr/bin/env bash
    set -euo pipefail
    logdir="$(mktemp -d)"
    trap 'rm -rf "$logdir"' EXIT

    # Say "disk" when it was the disk. Two independent signals, because a full volume shows up
    # in two quite different shapes (eieio-7d8.40):
    #
    #   - The explicit one. rustc, ld and cargo all say `No space left on device` somewhere in
    #     the middle of their output and then exit with a message that names a CRATE. Grepping
    #     every stage log for the phrase turns "could not compile eio-manifest" back into what
    #     actually happened.
    #   - The silent one, and the reason this does not stop at a grep. A `crates/daemon` API
    #     test failed with `Io(Os { code: 22, kind: InvalidInput })` at 5.4G free and passed
    #     unchanged at 14G; the phrase appears nowhere in that. All this can do is read the
    #     volume as the run ends and say that the failures are suspect — which is exactly the
    #     sentence that was missing when this cost a debugging session.
    disk_verdict() {
        if grep -qs -e 'No space left on device' -e 'IO failure on output stream' "$logdir"/*.log; then
            echo "ci: THE DISK FILLED. A stage hit 'No space left on device' — whatever crate or" >&2
            echo "ci: test is named above did not fail on its own merits. Free space and rerun." >&2
            grep -hs -e 'No space left on device' -e 'IO failure on output stream' "$logdir"/*.log \
                | sort -u | head -3 | sed 's/^/ci:   /' >&2
            return 0
        fi
        local free_gb
        free_gb=$(df -Pk . | awk 'NR==2 { print int($4 / 1048576) }')
        if [ "$free_gb" -lt {{ disk_margin_gb }} ]; then
            echo "ci: ${free_gb}G free on this volume as the run ended, under the {{ disk_margin_gb }}G margin." >&2
            echo "ci: A tight disk makes this suite fail in ways that name no disk at all — an" >&2
            echo "ci: EINVAL from a test that passes at 14G, for one. Treat the failures above as" >&2
            echo "ci: unproven until a rerun with room reproduces them." >&2
        fi
        return 0
    }

    # Sequential prelude. `shapes` must run before EIO_SHAPES_PREGENERATED is exported below —
    # that variable is how it makes itself a no-op for the `test-designer` stage.
    for stage in build build-golden shapes; do
        echo "═══ ${stage} ═══"
        if ! just "$stage" 2>&1 | tee "$logdir/$stage.log"; then
            echo "═══ ${stage}: FAILED ═══" >&2
            disk_verdict
            echo "ci: failed stage(s): ${stage}" >&2
            exit 1
        fi
    done

    # `shapes` (a stage above) has already written both generated files, so the
    # `test-designer` stage below must not run it a second time — it would put a second cargo
    # on the target-directory lock the `test` stage is holding. See the `shapes` recipe.
    export EIO_SHAPES_PREGENERATED=1
    # `build-golden` (a stage above) has already built the golden blocks, so no test
    # process may invoke cargo on that target directory — see `golden::build`'s comment for
    # why even a no-op invocation is a writer, and how that turned CI red twice.
    export EIO_GOLDEN_PREBUILT=1

    # `test-doc` only when nextest is present: without it, `test` already ran doctests
    # via `cargo test --workspace`, and scheduling both would run them twice.
    # `check-designer-hermetic` is unconditional and the `test-designer` below is not:
    # it is a grep over `designer/src`, needs no JS toolchain, and the invariant it holds
    # (eieio-m9s.42's, guarded by eieio-m9s.44) is a repository rule rather than something
    # the SPA's own suite could check about itself.
    stages=(fmt-check lint test check-nostd check-guest check-designer-hermetic)
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
        disk_verdict
        echo "ci: failed stage(s): ${failed[*]}" >&2
        exit 1
    fi

    echo "═══ test-golden ═══"
    if ! just test-golden 2>&1 | tee "$logdir/test-golden.log"; then
        echo "═══ test-golden: FAILED ═══" >&2
        disk_verdict
        echo "ci: failed stage(s): test-golden" >&2
        exit 1
    fi
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

# A `just` recipe and not an eslint rule, and that is the repo's shape rather than a
# shortcut: `designer/` has no eslint and no eslint config — its static checking is
# `npm run check` (svelte-check plus `tsc`), and neither of those can express "this
# import is forbidden here". Adding eslint, a config, a plugin and a CI wiring to hold
# one line would be a toolchain bought for a grep. The precedent is `check-lint-optin`
# above: a repository invariant that no linter models, enforced as a shell check in the
# one command surface CI runs (eieio-m9s.44).
#
# It is its OWN stage rather than a line inside `test-designer` for two reasons. It
# needs no npm, so it runs on a machine where `ci` skips the SPA suite entirely; and it
# needs no `designer-wasm`, no `npm ci` and no `shapes`, so it answers in milliseconds
# instead of after three prerequisites that a test about to be rejected has no business
# paying for.
#
# Deliberately `child_process` and nothing more. Writing into the repo from a test
# (`node:fs`) and spawning through a wrapper are both imaginable and neither is what
# happened: the measured failure was a `cargo` build spawned from `beforeAll`. A guard
# that fires on things nobody has done yet is a guard people learn to route around.

# Fail if a designer test spawns a process — the eieio-m9s.42 regression, guarded.
check-designer-hermetic:
    #!/usr/bin/env bash
    set -euo pipefail
    # The quoted form rather than the bare word, so that a comment naming the module —
    # this one, quoted in a test file, for instance — is not itself an offence. Both
    # spellings, because `node:child_process` and `child_process` are the same import.
    offenders="$(find designer/src -name '*.test.ts' -exec \
        grep -l -E "['\"](node:)?child_process['\"]" {} + || true)"
    if [ -n "$offenders" ]; then
        echo "error: designer test(s) import child_process:" >&2
        printf '%s\n' "$offenders" | sed 's/^/  /' >&2
        echo "" >&2
        echo "  A designer test MUST NOT spawn a process, and the reason is a measured CI" >&2
        echo "  failure rather than a style rule. schema-parity.test.ts used to regenerate" >&2
        echo "  designer/src/lib/api/__generated__/ by running cargo from beforeAll. On a" >&2
        echo "  cold checkout that cargo took 143s against a 120s vitest hook, so CI died" >&2
        echo "  with 'Hook timed out in 120000ms' and then PASSED on the next run against a" >&2
        echo "  now-warm cargo. Self-healing on a rerun is the worst shape a CI failure can" >&2
        echo "  have. There was no lock contention: a cold cargo alone outlasts the hook." >&2
        echo "" >&2
        echo "  A generated input to the SPA's suite is a PREREQUISITE, not something a test" >&2
        echo "  conjures for itself. 'just shapes' is the only writer of __generated__/ and" >&2
        echo "  'just designer-wasm' the only writer of crates/expr-wasm/pkg/; both are" >&2
        echo "  prerequisites of 'just test-designer'. A test READS them and fails naming the" >&2
        echo "  recipe when one is missing or stale — designer/src/lib/api/generated-shapes.ts" >&2
        echo "  is how (eieio-m9s.42)." >&2
        echo "" >&2
        echo "  If your test needs a new generated input: add a 'just' recipe that writes it" >&2
        echo "  and make it a prerequisite of test-designer. Do not spawn it from the test." >&2
        exit 1
    fi
    echo "  no designer test spawns a process"

# The SPA's own suite: the derived-value rules, the manifest-reference match, the
# operation builders, the linter against the real interpreter, and the schema-parity
# checks — which is why `shapes` is a dependency here alongside `designer-wasm`: both
# generate a gitignored artifact the suite reads and no longer generates for itself
# (eieio-m9s.42). `check-designer-hermetic` is what keeps that relationship from being
# quietly undone by the next test that decides to generate its own inputs; `ci` also
# runs it as a stage of its own, so it holds on a machine with no npm.
test-designer: check-designer-hermetic designer-wasm designer-deps shapes
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
