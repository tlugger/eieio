# CLAUDE.md

Guidance for agents working in this repository.

## What this is

eieio is a distributed stream-processing platform: WASM **blocks** wired into **services** running on **nodes** that form a **System**, targeting everything from servers down to MCUs. `README.md` is the human orientation; this file is how to build here.

**Current state: items 1–6 of [Implementation order](#implementation-order) are built, plus pub/sub.** `signal`, `expr` and `manifest` are the `no_std` foundation with their conformance vectors. `host-core` + `daemon` is a working node: lifecycle driver, executor, router, `eio:state` behind a real store, `eio:timer`, taps and log streaming over SSE, an OCI block manager that verifies digests and both cosign signature shapes, and the DAEMON §9 management API with OpenAPI at `/openapi.json` and a per-node bearer token. `service` is the service-file schema, parser and validator (SERVICE-SPEC). `block-sdk` is the guest runtime and `block-sdk-macros` the `#[block]` macro that generates the ABI exports, port enums, `Prop<T>` and the `eio:manifest` section; `cargo-eio` is `cargo eio new/build/test/publish`; `test-host` runs a block natively for the fast inner loop; `conformance` is the reference harness plus its scenario suite over wasmtime, wasm3 and WAMR; `cli` is the `eio` binary with management-API parity across every node in `~/.config/eieio/nodes.toml`, and `eio mcp` serves that same surface as MCP over stdio (SCOPE §4). Cross-node signals move over MQTT behind a swappable `Bridge`, with `publisher`/`subscriber` as host-native system blocks. `examples/blocks/` holds ABI §13.2's five golden blocks, written with the SDK, and `crates/conformance/scenarios/blocks/` the hand-written fixtures that test the harness itself.

**The Designer is built**: `designer/` is the Vite + Svelte 5 + Svelte Flow SPA and `crates/designer` its axum + rusqlite + rust-embed server — one catch-all proxy to a node, a SQLite registry of Systems and node addresses, session auth, canvas editing with live expression linting, a palette that browses a node's registry and pulls a block onto it — invalidating that reference's cached manifest in the same act, DESIGNER §3.3 — and a schema-parity check that holds its hand-written response types against the daemon's own OpenAPI schemas.

**`crates/leaf` exists as a host build**, not a firmware one: it links the five ★ crates unmodified, binds **both** of LEAF §3's engines — wasm3 and WAMR's interpreter — wires `eio:state`, `eio:core` and `eio:timer`, and passes 29 of 33 conformance scenarios on each, with the same four skipped by name. An image links one engine (LEAF §3.2); the host build links both so the suites can be run against each. It now has a `no_std` boundary drawn through it (LEAF §2.1): `--no-default-features` builds the shared half — `spawn`, generic over engine, clock, entropy and store; the timer scheduler; the budgets and router wiring — for both bare-metal targets under `just check-nostd`, while the engine binding, the file-backed state store, the host clock and entropy, and the fixtures stay behind `std`. That is the whole of what it proves — nothing has been cross-compiled into an image, linked against an allocator, or run on an MCU. **`crates/leaf-gen` is the build-host generator** (LEAF §6.4.5): it turns a service file, a node id and one artifact per block into the generated Rust source LEAF §6.4 specifies — one `static GRAPH` of `crates/leaf`'s hand-written types, with no `fn` and no control flow in it — and everything in that graph that could have been computed is `Descriptor::from_manifest`, `eio_host_core::resolve` and `eio_manifest::validate`'s own output, serialised.

**Not built yet:** the firmware build pipeline, an MCU target, and the platform half of a `no_std` leaf — an allocator, a panic handler, a hardware clock, flash-backed state (LEAF §11 lists what each needs). `cargo eio aot` is blocked on a `wamrc` toolchain — see eieio-7d8.21's notes before attempting it, and LEAF §4 for why the interpreter path needs none of it.

## The prime directive: specs are normative

The specifications in `docs/` are not design notes that code may drift from. They are the contract that the daemon, the leaf runtime, the SDK, the registry, and the conformance suites are all written against — the whole architecture depends on two independent host implementations agreeing byte for byte.

1. **Never implement past a spec.** If the spec is silent, ambiguous, or wrong for what you are building: stop, say so, amend the spec, then write the code.
2. **Spec change and code change land together**, in the same commit. A spec that describes something the code does not do is worse than no spec.
3. **Decisions are recorded in place** — as edits to `SCOPE.md` or the relevant spec. There is no ADR log; do not start one.
4. **`OPEN` items live in `SCOPE.md` only.** Specs reference them; they do not re-litigate them. Resolving an open question means editing `SCOPE.md` §3 and removing the marker — never quietly picking an answer in a spec or in code.
5. **`PROPOSED` markers** in a spec mean drafted-but-unratified. Implementing one is how it gets ratified: remove the marker in the same commit.
6. **`MUST`/`SHOULD`/`MAY`** in ABI-SPEC and EXPR-SPEC are RFC 2119 and mean exactly that. A `MUST` is not a strong suggestion.

If a spec turns out to be awkward to implement, that is a finding, not an obstacle to route around. ABI-SPEC §14 states the rule explicitly for the SDK: friction in the wrapper means the spec is wrong.

## Which document governs what

|Document|Authoritative for|
|---|---|
|`docs/SCOPE.md`|Every settled decision, all `OPEN` items, vocabulary, sequencing. Read first.|
|`docs/specs/ABI-SPEC.md`|Host↔guest binary contract: exports, imports, memory rules, lifecycle, status codes, manifest schema, versioning. **Everything else builds against this.**|
|`docs/specs/EXPR-SPEC.md`|The expression language: grammar, data model, evaluation, builtins, errors, budgets.|
|`docs/specs/SDK-SPEC.md`|The guest-side Rust crate. High-level; expects expansion.|
|`docs/specs/SERVICE-SPEC.md`|The service file: identity (a block instance is its **id**, never its name), block instances, properties, connections, the `[ui]` contract, validation classes.|
|`docs/specs/DAEMON-SPEC.md`|Daemon-class node internals: crates, on-disk layout, executor, router, API. High-level; expects per-subsystem expansion.|
|`docs/specs/DESIGNER-SPEC.md`|The visual management surface. High-level; expects expansion.|
|`docs/specs/LEAF-SPEC.md`|The MCU tier: what a leaf is, its engine and budgets, state on flash, what is baked at build time, and the deploy contract. High-level; expects expansion.|

Each spec has an **expansion list** as its final section — the in-depth work it knows it is missing. Consult it before deciding something is unspecified.

## Naming

The project is **eieio**; the identifier prefix is **`eio`** (SCOPE §5.1).

|Surface|Form|
|---|---|
|Guest exports|`eio_configure`, `eio_alloc`, `eio_free`, `eio_process_signals`, `eio_on_timer`, …|
|Import namespaces|`eio:core`, `eio:state`, `eio:timer`, `eio:gpio`, `eio:i2c`, `eio:http`|
|Custom section|`eio:manifest`|
|SDK|crate `eio-sdk`, import path `eio_sdk` (directory `crates/block-sdk`)|
|Tooling|`cargo eio`|
|Workspace crates|package `eio-<dir>`, import `eio_<dir>` (`crates/signal` → `eio-signal` → `eio_signal`); `cargo-eio` excepted (DAEMON §1)|
|Node data dir|`/etc/eieio/`|

`nio` is the defunct predecessor. It appears legitimately only in historic prose (SCOPE §1) and links to the original repos. **Any `nio_*` or `nio:*` identifier in code or a spec is a leftover — fix it.**

Vocabulary is settled and used precisely: **System** (group of nodes), **Node** (one device), **Service** (block graph on a node), **Block**, **Signal** (a *batch*, not a single record). Do not introduce "flow", "pipeline", "instance", "agent", or "job" as synonyms.

## Repository layout

```
Cargo.toml            workspace root
crates/
  abi/  host-core/  expr/  signal/  manifest/   ★ shared with the leaf runtime
  service/  daemon/  cli/  designer/  block-sdk/  block-sdk-macros/  test-host/  cargo-eio/
  leaf/     ★ its runtime half     the leaf runtime; `std` by default (LEAF §2.1)
  leaf-gen/ the build-host generator: a service file becomes LEAF §6.4's baked
            graph. `std`, never linked into an image (LEAF §6.4.5)
  wamr-host/ the WAMR interpreter binding of `eio_host_core::Engine`, written for
            neither of its two callers: `leaf`'s `wamr` feature and, as a
            dev-dependency, the conformance harness (LEAF §3)
  conformance/
expr-tests/           host-agnostic vectors: expressions, property types, canonical CBOR
schemas/              published JSON Schemas: manifest, service
designer/             the Vite + Svelte 5 SPA, own package.json; its server is
                      `crates/designer` (DESIGNER §1)
examples/
  services/           sample service TOMLs
  blocks/             ABI §13.2's golden blocks — their own cargo workspace
docs/
  SCOPE.md  specs/
```

★ crates **must stay `no_std`-compatible** (`alloc` permitted). They are compiled into the MCU leaf runtime and, in `expr`'s case, into the browser. A `std` dependency added to `abi`, `expr`, `signal`, `manifest`, or `host-core` breaks the embedded north star quietly — CI enforces it, and so should you. `daemon` and `cargo-eio` are `std` binaries, and so is `service` — nothing parses a service file on a leaf tier (SCOPE §3.7 deploys those by firmware build), and `toml` cannot compile without atomics anyway (DAEMON §1). `block-sdk` is `no_std` by necessity: it compiles into guests, and `just check-guest` is its gate.

`abi` holds only what both sides of the boundary must agree on — ABI §8's status codes, §3's sentinels, §9.6's alignment — and **has no dependencies**. Keep it that way: anything added there is added to every block that ships. `host-core` re-exports all of it, so host code imports it from there (DAEMON §1).

## Implementation order

Bottom-up, most-specified first. Do not start a later item because an earlier one is boring.

1. **`signal`** — CBOR value/signal/batch types (ABI §6.3), minicbor, `no_std`.
2. **`expr`** — parser → static analysis → interpreter → budgets, plus `expr-tests/` conformance vectors (EXPR §11). The most completely specified component in the repo; it should need no design decisions.
3. **`manifest`** — schema types, parsing, WASM import-section cross-check (ABI §4.3, §11), `manifest.schema.json`.
4. **`host-core` + `daemon` skeleton** — lifecycle driver, executor, router; load a block and route a signal.
5. **`block-sdk`** + first golden block, then the reference harness (ABI §13). ✅
6. Service file format + management API (DAEMON §2, §9). ✅
7. Pub/sub transport + cross-node signals (DAEMON §7). ✅
8. **CLI + agent tooling (MCP)** — `eio` and `eio mcp` are built. ✅
9. Designer UI — built (DESIGNER §1–§7 ratified); §10's expansion list is what remains. ✅
10. Leaf runtime + firmware build pipeline — `crates/leaf` runs on the host, passes conformance, and its runtime half builds `no_std` for both bare-metal targets (LEAF §2.1); the MCU target itself, the platform half, AOT and the firmware pipeline are LEAF §11's expansion items.

Items 1–4 are ✅ too. The authoritative list is SCOPE §7, with the epic-to-item mapping in §7.1; this is the same sequence, annotated.

## Invariants worth stating twice

These are the decisions most likely to be eroded by a reasonable-seeming local improvement:

- **Core WASM plus the measured six, nothing more.** No WASI, no component model, no threads. The accepted set is core WASM 1.0 plus exactly the six proposals ABI §4.3 lists — what the Rust toolchain emits by default *and* wasm3 demonstrably executes, pinned by the conformance suite on both engines. Widening it again means measuring again (SCOPE §3.2, ABI §1.1, §4.3). Reaching for a component-model convenience still deletes the embedded tier.
- **Copies, not shared references,** across the boundary. Host never retains a guest pointer past the call (ABI §9).
- **`emit` enqueues; it does not deliver.** Routing happens after the callback returns. This is what makes reentrancy unconstructible (ABI §6.2).
- **Traps are death; status codes are life.** A non-zero callback return is logged and counted, never fatal. Only traps, fuel exhaustion, and deadline violations kill an instance (ABI §8).
- **Expressions are pure and terminating.** No IO, no clock, no RNG, no recursion, no user-defined loops. Sorted map iteration, no NaN/inf escape, pinned canonical rendering. Determinism is the replay and debugging lever — do not trade it for a convenience builtin (EXPR §1, §9).
- **Missing data is an error, not null.** `$temp` on a signal without `temp` fails that signal. Silent nulls turn config typos into 2 a.m. mysteries (EXPR §6).
- **Every property is an expression.** There is no static/dynamic property split at the ABI level. Literal-looking fields in the Designer are a UI affordance only (ABI §11).
- **Block authors write safe Rust exclusively** (SDK §4). `unsafe` is confined to five places, and nowhere else may grow one without a reason written down beside it: `block-sdk`'s audited glue and the `#[block]` macro's generated exports (the ABI is a C boundary); `daemon`'s `Module::deserialize_file` (DAEMON §4.3's precompiled artifact, which is trusted by construction); **`crates/wamr-host`**, the one WAMR binding of `eio_host_core::Engine` (LEAF §3), whose engine is the one with no safe Rust binding that can express an ABI §7 host, because `wamrx::Linker`'s closures never see the calling instance; and `conformance/tests/wamr.rs`, whose remaining raw FFI is not a host binding but the harness's *own* ABI §4.3 engine measurement — the instruction table, the carved-out remainder and the nine refused proposals, which must drive `wamrx-sys` themselves to stay an independent measurement (eieio-7d8.34). `crates/leaf/src/wamr.rs` used to be a sixth, a copy of the fifth; it has no `unsafe` at all now and is LEAF §4.2's stack budget plus a `use`. Each of those module docs says exactly which published-crate gap forces it. Every `unsafe` block carries a `// SAFETY:` comment naming what makes it sound.
- **No async/await in guests.** No runtime exists there; the ABI is callback-shaped (SDK §3).
- **Nodes own their configuration as files.** The Designer's database holds Systems, node addresses, and caches — nothing a node could not be asked for. Persisting service state there is an architecture bug (SCOPE §3.8).
- **The Designer is a peer client.** Any capability it has that an agent cannot reach through the daemon API or MCP is a bug (SCOPE §4, DESIGNER §8).
- **System blocks are transport endpoints only.** `publisher`/`subscriber` are host-native because they need credentials and transport internals; that precedent does not extend (DAEMON §6).

## Testing

**Run `just ci`, not raw cargo.** It is the same gate CI runs — `fmt-check`, `lint` (warnings denied, plus the per-crate lint opt-in check), `build`, `test`, `check-nostd` — and `check-nostd` is the only thing that catches a `std` dependency creeping into a ★ crate. `just` bare lists the rest.

Conformance is the mechanism that keeps two host implementations honest, so it is not an afterthought:

- **`expr-tests/`** — host-agnostic vectors, three corpora and one format (`expr-tests/README.md` is normative): the expression language at the top level — every builtin, every special form, every error code, budget floors, signal-dependence classification (EXPR §11); `properties/` for ABI §11.1's property types; `cbor/` for ABI §6.3.1's canonical encoding, both RFC 8949 deviations included. Each has a runner that fails the build on an uncovered rule.
- **`conformance/`** — reference wasmtime harness plus golden blocks: pure transform, multi-port filter, timer emitter, stateful counter, GPIO echo, and hostile blocks (spinner, allocator-liar, reentrancy-prober, oversize-emitter) (ABI §13).
- Daemon and leaf runtime must pass the same suites. **Divergence between hosts is a conformance bug by definition** — when they disagree, the fix is not "make the leaf special".
- New ABI or language surface arrives with its vectors in the same commit.

## Commits

- Emoji-prefixed subject lines. **No trailers, no `Co-Authored-By`, no `Generated with` footers.**
- Commit directly to `main`. No feature branches unless asked.
- Group changes by area, one commit per area, each with its own best-fit emoji — not one omnibus commit.

|Emoji|For|
|---|---|
|✨|New feature or capability|
|🐛|Bug fix|
|♻️|Refactor|
|📝|Docs, specs, README|
|💡|Scope, design decisions, planning artifacts|
|🤖|Agent-facing config: this file, skills, MCP setup|
|✅|Tests, conformance vectors|
|🔧|Build config, Cargo/workspace, CI|
|🙈|`.gitignore`|
|📄|License, legal|
|⚡|Performance|
|🔒|Security, auth|
|🚚|Renames and moves|
|🔥|Removing code|
|⬆️|Dependency bumps|

Pick a better-fitting emoji over a listed one when the list does not cover it.


<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:970c3bf2 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   bd dolt push
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

### wdpkr

This repo has a semantic codebase index via `wdpkr`. Use it to **locate feature areas by concept** — "where does commission logic live," "how is rate limiting implemented," "what does the PDF pipeline look like." Parse the JSON output; `path` and `summary` fields tell you where to look, then read the actual files.

#### Options

| Flag | Description |
|------|-------------|
| `--scope <path>` | Limit to subtree (repeatable: `--scope src/finance --scope src/annuity`) |
| `--filter <glob>` | Glob on result paths (repeatable, OR logic: `--filter "*.go" --filter "*schedule*"`) |
| `--terse` | Paths + one-sentence summaries, no symbols — minimal context cost |
| `--no-symbols` | File-level results only, omit symbol nesting |
| `-k, --top-k <N>` | Max file results (default 5). Use `-k 2` for precise hits |
| `--symbols-per-file <N>` | Max symbols per file (default 3) |
| `--pretty` | Human-readable colored output instead of JSON |

#### Call graph data

Symbol-level results include `calls` and `called_by` fields when the index has been built with call-graph support. Use these to assess blast radius before making changes:

- `"calls": ["src/finance/rates.rs:lookup_rate_table"]` — this symbol calls `lookup_rate_table` in `src/finance/rates.rs`
- `"called_by": ["src/api/handler.rs:process_request"]` — `process_request` depends on this symbol

A `null` value means the symbol hasn't been indexed with call-graph data yet (run `wdpkr index --skip-summaries` to rebuild). An empty array `[]` means the symbol genuinely has no callers or callees.

When changing a symbol, check its `called_by` to find all dependents — read those files to verify your change doesn't break callers. When exploring unfamiliar code, check `calls` to understand what a function depends on before diving into its implementation.

#### When to use

- **Conceptual questions** where you don't know what to grep for: "where does X live," "how is Y implemented"
- **Orientation** before touching an unfamiliar area — get the lay of the land first
- Combine `--scope` with `--filter` and `--terse` for fast, precise lookups:
  `wdpkr search "rate table" --scope src/finance --filter "*.go" --terse -k 3`

#### When NOT to use

- You have a concrete symbol or string to find — use `rg`/grep instead
- You already know which file to read — read it directly
- You need exact text matches or regex — wdpkr is semantic, not lexical

#### Best practices

- **Scope aggressively.** If you know the layer, `--scope` is more valuable than refining the query. Unscoped searches return results across all layers (UI, backend, infra), wasting result slots on irrelevant files.
- **Use `--terse` by default** for simple lookups. Full summaries and symbol trees are useful for deep exploration but waste context tokens when you just need to find the right file.
- **Combine `--scope` with `--filter`** to narrow both the search space and the result set. `--scope` limits the vector query (efficient); `--filter` prunes results by filename pattern (flexible).
- **Switch to `rg` after wdpkr points you somewhere.** Don't chain wdpkr queries to refine — once you have a file or symbol name, grep is faster.
- **Run scoped queries in parallel** when a question spans layers — e.g., one `--scope src/graphql` and one `--scope src/finance`.
