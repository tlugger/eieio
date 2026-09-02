# The Designer's container image (DESIGNER-SPEC §1: "ships as a container image and a bare
# binary; localhost-first"). Two builds happen in here that never happen together anywhere
# else in this repository, which is exactly the risk eieio-p0k.4 exists to guard against:
# `crates/designer` embeds `designer/dist` at compile time
# (`crates/designer/src/assets.rs`'s `#[derive(RustEmbed)]`), and that directory is
# gitignored — so the SPA MUST be built in an earlier stage than the Rust binary. Get the
# `COPY --from=spa-builder` below wrong (or drop it) and this file still builds without
# complaint — cargo has no idea `designer/dist` was ever supposed to hold anything — and
# ships a binary that answers `404` for `/` (`assets.rs`'s own
# `an_empty_embed_answers_not_found_rather_than_panicking` test is the same failure, at the
# unit level). The release workflow's smoke job re-proves this at the image level, on
# purpose: a Dockerfile that merely parses is not a Dockerfile that serves anything.
#
# ── the musl decision (eieio-p0k.4, and SCOPE §7.2 which foresaw it) ────────────────────────
#
# SCOPE §7.2 left musl out of the daemon's release matrix and said exactly why: "they would
# be the natural base for a container image, and that is the Designer pipeline's question
# rather than the daemon's." This is that pipeline, and the answer is yes — for this binary,
# for reasons the daemon does not share:
#
#   - `eio-designer` has no `wasmtime` dependency. That crate (and its C toolchain
#     assumptions — cranelift, its own C shims) belongs to `eio-daemon` and `eio-conformance`
#     alone; it is the thing that would have made a musl cross-build painful, and it is not
#     in this binary's dependency graph.
#   - Its only C code is `rusqlite`'s `bundled` feature (SQLite's own amalgamation, compiled
#     from source), which `musl-gcc` builds without complaint.
#   - `reqwest` here carries no TLS backend at all — see the root `Cargo.toml`'s comment on
#     the dependency: every proxied call is plain HTTP to a node on the operator's own
#     network (DESIGNER §3.1). There is no OpenSSL to reconcile with a static musl libc,
#     which is the usual reason a musl build turns painful.
#
# So `x86_64-unknown-linux-musl` produces a genuinely static binary, and the final stage is
# `scratch`: no libc to match, no package manager, no shell for a stray script to hide in.
# This is a musl leg in *this* Dockerfile's own build only — it does not add an entry to
# `release-designer`'s bare-binary matrix in the justfile, and it does not touch SCOPE §7.2's
# table, which stays about the daemon. The bare Designer binary attached to a GitHub Release
# is glibc, on the same two targets the daemon ships (see the justfile's `release-designer`).
#
# Only linux/amd64 today: an aarch64 musl leg is possible (the reasoning above applies
# equally) but would add a second cross-linker to this file for no user this repository has
# yet. Widening the platform list is a matrix entry away when one shows up — the same framing
# SCOPE §7.2 already uses for the daemon's own matrix.

# Every stage below is pinned to `linux/amd64` explicitly, not left to the builder's default
# platform: the final binary targets `x86_64-unknown-linux-musl`, and on an arm64 build host
# (Apple Silicon, an arm64 GitHub-hosted runner) an unpinned `FROM` would pull arm64 base
# images instead — `musl-gcc` then wraps an aarch64 `cc1`, which does not understand the
# `-m64` flag `cc-rs` passes for this target and fails opaquely. Pinning here makes the
# platform this file targets an explicit, single fact rather than "whatever the host is
# today", and is what makes the emulated build reproducible on both kinds of host.

# ---- stage 1: the SPA (designer/dist, embedded by crates/designer at compile time) --------
FROM --platform=linux/amd64 node:22-bookworm-slim AS spa-builder
WORKDIR /designer
# Dependencies before source: Docker's layer cache reuses this `npm ci` layer across builds
# that only touch designer/src.
COPY designer/package.json designer/package-lock.json ./
RUN npm ci
COPY designer/ ./
RUN npm run build

# ---- stage 2: the Rust binary, statically linked against musl -----------------------------
FROM --platform=linux/amd64 rust:1.97.1-slim-bookworm AS rust-builder
# `musl-tools` provides `musl-gcc`, the only C compiler `rusqlite`'s bundled SQLite needs for
# this target (see the header comment above — there is nothing else in this binary's
# dependency graph that shells out to a C toolchain).
RUN apt-get update && apt-get install --yes --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl
WORKDIR /workspace
# The whole workspace, not just crates/designer: `eio-designer` carries no `eio-*` path
# dependency today (see the header comment), but it is still a Cargo workspace member, and
# cargo reads every member's manifest to resolve the workspace even when building one package
# with `-p`. `.dockerignore` keeps this to source only — no `target/`, no `node_modules/`.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
# The SPA half, built in stage 1, landing at the exact path
# `crates/designer/src/assets.rs` embeds: `$CARGO_MANIFEST_DIR/../../designer/dist`, i.e.
# this workspace's `designer/dist`. This COPY is the line that proves the build order:
# comment it out and the image still builds — only a runtime `curl` of `/` (the release
# workflow's smoke job) says it shipped nothing.
COPY --from=spa-builder /designer/dist designer/dist
ENV CC_x86_64_unknown_linux_musl=musl-gcc
# Explicit rather than relied-upon: this target links statically by default, but saying so
# is cheaper than a future default change silently producing a dynamically linked "static"
# build that only fails on `FROM scratch` below.
ENV RUSTFLAGS="-C target-feature=+crt-static"
RUN cargo build --release --package eio-designer --target x86_64-unknown-linux-musl
# No `[profile.release] strip` in the workspace `Cargo.toml` (that file is shared with every
# other crate this repository ships, and this pipeline does not touch it) — stripped here
# instead, where it affects only this image.
RUN strip target/x86_64-unknown-linux-musl/release/eio-designer

# ---- stage 3: the image itself -------------------------------------------------------------
# `scratch`: the binary above is fully static, so there is no libc to provide and nothing
# else this process touches at runtime — no TLS, no timezone database, no shell. The
# smallest image that is not lying about what is in it.
FROM scratch
COPY --from=rust-builder \
    /workspace/target/x86_64-unknown-linux-musl/release/eio-designer /eio-designer
COPY LICENSE /LICENSE

# Loopback-only is the binary's own default (`crates/designer/src/main.rs`'s `listen` doc:
# "a default reaching every interface would make installing the package the exposing act
# rather than a deliberate one"). That reasoning is about installing a package on a shared
# host; publishing a container image already is the deliberate act, and a loopback bind
# inside the container's own network namespace is unreachable from `docker run --publish`
# outside it regardless. So the image overrides it explicitly, once, here — rather than
# leaving every `docker run` invocation to remember to.
EXPOSE 7474
ENTRYPOINT ["/eio-designer"]
CMD ["--data-dir", "/data", "--listen", "0.0.0.0:7474"]
