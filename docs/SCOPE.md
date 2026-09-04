# SCOPE

High-level scope and decision record for an open source rebuild of the nio platform. This document gives full background context for the system being built. It is the input to a more detailed SPEC. Decisions here are settled unless marked **OPEN**.

Name: **eieio** (settled; candidates floated and rejected: `thenio`, `zio`). Identifier prefix throughout the ABI, tooling, and crates: `eio` (§5).

---

## 1. Historic context: nio

nio (niolabs, ~2014–2019, now defunct) was a no-code/low-code platform for building distributed stream-processing systems, aimed at IoT and edge compute. Its core abstraction stack:

|Term|Meaning|
|---|---|
|System|A group of devices running nio, communicating via pub/sub|
|Instance|A single device running a nio configuration|
|Service|A collection of configured, connected blocks on an instance|
|Block|An executable unit: input signal → block logic → optional output signal|
|Signal|A batch of data (list of dicts) moving through the system|

Relationships were 1→many at each level. A block output connected to two receivers duplicated the signal to both.

### Surviving remnants

- Docs site capture: https://web.archive.org/web/20190716020124/https://docs.n.io/
- Workshops: https://web.archive.org/web/20181116060203/https://workshops.n.io/
- Block repos: https://github.com/nio-blocks
- Simulator blocks: https://github.com/tlugger/simulator
- Framework (block dev API): https://github.com/tlugger/nio

**Missing:** `nio-core` (the daemon/runtime) and the System Designer UI. Both must be rebuilt from scratch; runtime semantics must be reverse-engineered from the framework API, docs, and block source.

### Strengths worth preserving

- The framework abstraction (inputs, outputs, service config, block configs, signals) hid enormous complexity behind a composable model.
- Block extendability: install a block, restart, use it immediately.
- The Designer: visual, drag-and-drop construction of a running distributed system, with live signal inspection.
- Expression properties: per-signal dynamic evaluation inside block configuration, making blocks configurable without code.

### Weaknesses to fix

- **Python runtime.** Resource-heavy for long-running instances, reliability concerns, and — fatally — could only run where Python runs. No embedded/MCU story, ever.
- **No focus / poor business model.** Product did everything, aimed at everyone.
- **Cloud-hosted system-of-record.** Platform state lived in the vendor's product; when the company died, so did the platform.

---

## 2. What is being built

An open source monorepo producing two published artifacts:

1. **Daemon** — a compiled binary that establishes a node. Exposes a management API (service/block config, auth, pub/sub wiring), pulls and executes blocks, subscribes to topics, processes signals, publishes downstream.
2. **Designer UI** — optional management surface. Create Systems, attach nodes, design services on a drag-and-drop canvas, configure blocks, start/stop services, inspect logs and live signals.

Plus a **block registry** convention and per-block repos compiled and published independently.

### Differentiation (why not Node-RED / n8n / Home Assistant / Benthos)

- Multi-device mesh as a first-class concept: a System spans devices; cross-device signal flow is native, not bolted on.
- Compiled daemon with a real embedded/MCU deployment path.
- Agent-first: every design/build/deploy operation is executable by an LLM agent through text artifacts and APIs, with the visual Designer as an equal (not privileged) client.

---

## 3. Core architectural decisions

### 3.1 Runtime language: Rust

The daemon and leaf runtime are written in Rust. Rationale: strongest WASM hosting story (wasmtime), plausible `no_std` path for constrained targets, and predictable resource behavior for long-running instances — the direct fix for nio's Python problems.

### 3.2 Blocks are WASM modules

Blocks compile to **core WASM modules** (`wasm32-unknown-unknown`) with a **hand-specified ABI** — not the WASM component model. Rationale: core modules run everywhere from wasmtime down to WAMR/wasm3 on MCU-class hardware; component model support does not exist at the embedded tier. Ergonomics are traded for reach, consistent with the embedded north star.

**How much past core WASM 1.0 is admitted is a measurement, not a preference** (ABI §1.1, §4.3). This decision originally read "MVP and nothing more", justified by the claim that the leaf interpreter admits nothing else. That claim was never tested, and is false: wasm3 executes bulk memory, sign extension, reference types, multi-value and non-trapping float-to-int, and runs a stock-built Rust block through the whole lifecycle. Meanwhile the restriction made a Rust block impossible to produce at all — `alloc::string::String::clone` in the precompiled `rust-std` contains a `memory.copy`, and no compiler flag rebuilds that.

So the accepted set is what the guest toolchain emits *and* the leaf engine runs, and it is pinned by `crates/conformance/tests/wasm3.rs` rather than by prose here: the suite runs each instruction on wasm3 and checks the value it produces. Anything further — SIMD, tail calls, threads, exceptions, GC, the component model — stays refused, because the toolchain does not emit it. Widening the set again means measuring again.

**Refused is not the same as unimplemented, and the difference matters.** The same measurement found three proposals wasm3 *does* run and refuses nowhere: tail call, memory64 and a shared memory (ABI §4.3). For the two memory flags it appears to read the encoding and drop it, which is a silent misinterpretation and worse than a refusal. Nothing about the accepted set changes — the toolchain emits none of the three — but a gap in an engine's refusals is not a gap in the platform, so those three are refused in the shared loader as well, and both hosts then say the same thing about the same bytes.

**And the measurement is per instruction, not per proposal.** Measured again in full, wasm3 runs sign extension, non-trapping float-to-int and multi-value whole, but only `memory.copy`/`memory.fill` of bulk memory and only the `call_indirect` encoding of reference types — `memory.init`, `table.copy`, `table.get`, `ref.func` and the rest are refused. The remainder of those two proposals is therefore carved out of the accepted set (ABI §4.3), and because no engine's feature configuration can express a partial proposal, that carve-out is the one piece of feature conformance a host enforces in its loader rather than its engine. It costs a block author nothing: stock-built Rust blocks contain none of the carved-out instructions.

Consequences:

- Hot-install without recompiling the daemon (restores nio's "add a block, restart, go" magic).
- Sandboxed block execution.
- Block authorship is language-agnostic in principle; Rust-first in practice. Legacy Python blocks have a migration path via componentization tooling.
- Dependencies compile into the module — nio's runtime dependency-install problem is deleted.

### 3.3 Block ABI (to be fully specified in SPEC)

The ABI has two halves:

1. **Guest exports** — lifecycle (`configure`, `start`, `stop`) and `process_signals`, plus support for self-scheduling blocks (timers/generators, e.g. simulators) and stateful blocks with persistence hooks.
2. **Host functions** — capabilities the runtime exposes _into_ the block. This includes hardware access: a sensor block cannot touch I2C/GPIO from inside WASM; it calls host functions. The host maps these per platform (Pi: `/dev/i2c-*`; ESP32: esp-idf).

Every block manifest declares required capabilities (`gpio`, `i2c`, `http`, none, ...). Deployment validation = "does this node provide what these blocks require." This capability negotiation is the mechanism that makes designer-to-embedded deployment honest.

Memory/ownership conventions for buffer passing across the boundary are part of the ABI spec.

### 3.4 Signals

- Signals remain **batches** (nio semantics preserved).
- Single serialization everywhere — on the wire and across the WASM boundary: **CBOR** (`minicbor` for `no_std` compatibility). A leaf node's block and a Pi daemon's block are byte-identical consumers.
- Schemaless by default (dict-shaped), schemas optional/later.
- **Cross-device delivery is at-most-once.** A batch crossing a node boundary MAY be lost and is never delivered twice, so a block needs no idempotence and authoring a cross-node block is the same job as authoring an in-node one. Every loss is logged and counted at the bridge, like every other discard (DAEMON §6.2). Two things follow and are stated rather than left inferable: **ordering** holds per publisher per topic and nowhere wider — two publishers into one topic are not ordered against each other — and **cross-device backpressure does not exist**, because a publisher that cannot send drops, which is what at-most-once means. A stronger guarantee is purely additive and may be added later behind DAEMON §7's bridge; taking one away would not be, which is why the weaker promise is the one made first.
- **OPEN:** normative floors and reference defaults for `max_payload`, `max_batch` and `max_emission_bytes` (ABI §5.2, §9.7). All three are host configuration and discoverable from the instance descriptor, which is settled; what is not settled is whether a *floor* exists — the guarantee that lets a block author assume a batch of at least some size will be delivered, as EXPR §9's budget floors do for expressions. Deferred deliberately until there is a real workload to size them against: a floor is a promise to every block ever written, and the embedded tier pays for it twice, since host and guest each hold a copy during delivery. Until then hosts supply every value explicitly and blocks may assume nothing. `max_emission_bytes` is the third of them and the newest (ABI §9.7 rule 9): a leaf bounds one callback's emissions at 4 096 and a daemon bounds them not at all, which is also why the *backpressure* half of this item stays open — a daemon that picked a number there would be answering it in passing (DAEMON §6.2).

### 3.5 Expression language: a purpose-built micro-Lisp

Block configuration properties support per-signal expressions (successor to nio's `{{ $attr }}` Python-eval properties). Decisions:

- Lisp syntax: trivially parseable, tiny interpreter, agent-friendly.
- **Host-side evaluation**: the interpreter lives in the daemon/leaf runtime, not in blocks. Every block gets expressions for free; blocks stay dumb; MCUs need exactly one interpreter.
- Purpose-built, not borrowed: pure (no IO), bounded evaluation steps, `no_std`, deterministic. Existing Rust Lisps assume std and carry weight we don't want on a leaf.
- Evaluation is per-signal within a batch (nio semantics preserved).

### 3.6 Block registry: OCI artifacts

Blocks live in separate source repos. Each repo's CI compiles the block to WASM (plus AOT variants per embedded target) and publishes it as an **OCI artifact** (e.g. ghcr.io): content-addressed, versioned, signable (cosign), zero registry infrastructure to run. Daemons pull by reference.

The **block manifest** is a core design artifact: properties schema (JSON Schema — drives Designer config panels _and_ agent tooling), input/output ports, required host capabilities, block version, ABI version.

- **OPEN:** what `cargo eio publish` does with a SemVer build-metadata suffix. ABI §11.1 requires `version` to be SemVer, which admits `+build`; the OCI distribution spec does not admit `+` in a tag, so a legal block version can be an illegal reference. `publish` refuses such a version today, naming both specifications rather than picking a rewrite — dropping the suffix silently changes what was published, and escaping it invents a convention no other registry client would read back. Deferred because no block has needed build metadata yet, and the refusal is the safe reading until one does.

### 3.7 Node tiers (embedded north star)

Two node classes, one wire protocol:

- **Daemon-class** (Pi and up): full daemon, hot-loads WASM blocks, exposes management API.
- **Leaf-class** (MCU: ESP32 etc.): minimal runtime; services are deployed via a build step — AOT-compiled WASM baked into firmware and flashed. No hot-install, no management API beyond what the wire protocol carries.

The Designer flow is identical for both: design a service (subscriber/sensor blocks → logic → publisher/actuator blocks), deploy. For leaf targets, "deploy" runs a firmware build+flash pipeline instead of a config push. Extra flashing steps are acceptable; a different design flow is not.

**A client that can name a node records its class, and refuses a leaf by name rather than dialling it.** A leaf serves no HTTP at all (LEAF §7), so an HTTP client pointed at one gets a connection error indistinguishable from a node that is down — reporting a fault against a device working exactly as designed, which is the most expensive kind of wrong answer to debug. The Designer already refuses one (DESIGNER §3.1); the `eio` CLI's `~/.config/eieio/nodes.toml` records no class, so it does not, and §4's peer-client rule makes that asymmetry a bug rather than a difference. So **a node entry carries an optional `class`, `"daemon"` or `"leaf"`, and an absent one means `"daemon"`** — every file that exists today keeps working and keeps meaning what it meant. `eio` refuses a `leaf` entry by naming the class and the reason, not by reporting a failed request.

Optional-with-a-default rather than required, because the alternative is worse in both directions: making it required breaks every existing file to describe the case nobody has yet, and inferring it from a refused connection cannot tell a leaf from a daemon that is genuinely down — which is the whole problem, restated as a guess.

Specified in `docs/specs/LEAF-SPEC.md`.

- **OPEN:** the **flash wear budget policy** — how much writing is too much, over what window, and what a leaf does when a block ignores repeated refusals. ABI §7.2 already gives a leaf host the vocabulary (`ERR_THROTTLED`, refused rather than silently dropped) and LEAF §5 already requires it be used; what is unsettled is the *policy behind* the refusal. Deferred deliberately: the answer depends on the flash part and the write pattern, and neither exists to measure yet. Until it does, a leaf refuses on whatever budget it was built with and blocks may assume only that a refusal is possible.

### 3.8 Configuration source of truth: files on the node

Each daemon owns its configuration as **declarative text files on disk**. The management API reads/applies them. The Designer's backend DB is a registry (Systems, node connection info) — never the system of record. Rationale: survivability (nio's cloud-state failure mode), GitOps-compatibility, and the agentic goal — agents and CLIs operate on text artifacts, not DB rows behind an API.

### 3.9 Pub/sub

Cross-device signal flow is native pub/sub: publish/subscribe blocks are ordinary drag-and-drop blocks bound to topics.

- **Transport is MQTT**, via `rumqttc` behind DAEMON §7's bridge trait. Chosen for the embedded north star: MQTT is the one protocol a leaf tier plausibly speaks, and brokers are everywhere. The bridge is a **normative boundary, not a convenience** — no MQTT concept may appear above it. QoS belongs to the bridge and not to the router, topics do not appear in `host-core` or the ABI, and no service semantics may assume a retained message. Guarantees are stated in this project's own vocabulary (§3.4) and *mapped* onto QoS at the bridge; if a second transport could not satisfy them without changing anything above the trait, the boundary is in the wrong place.
- **The broker is an elected node, and only a daemon-class one is eligible.** A leaf tier cannot host a broker, so it is never a candidate. Electing from within the System rather than depending on an external broker keeps a System self-contained — the smallest useful deployment needs nothing installed beside it. The Designer MAY offer a UI to **pin** a specific node as broker; that is an override for an operator who wants one, not the mechanism, and it does not make the Designer infrastructure: §4's peer-client rule still holds and the Designer never carries data-plane traffic. The election *mechanism* — candidate set, lease, handover, and the case where no daemon-class node is reachable — is a DAEMON §7 expansion item, not settled here.
- **Node discovery is static configuration.** A node's own config names the broker candidates, and the Designer's database holds node addresses, which §3.8 already permits it to. mDNS is an OPTIONAL convenience a daemon-class node MAY offer and nothing in the protocol may depend on it: it is LAN-only, routinely firewalled, and would put "which System am I in" within reach of anything on the subnet while §3.11's transport security is still open.

### 3.10 Management API: REST + OpenAPI

- Management plane: REST/JSON with a published OpenAPI spec. Curl-able, browser-friendly, and the spec doubles as agent/MCP tooling surface for free.
- Streaming (logs, signal taps): WebSocket or SSE.
- gRPC and GraphQL rejected (embedded and browser friction; problems we don't have).

### 3.11 Auth

- UI/agent ↔ daemon: per-node token generated by the daemon, included in requests. Deliberately simple; JWT rejected as overcomplicated for this.
- **The pub/sub bus takes a pre-shared key per bus.** One shared secret in `pubsub.toml`, presented as the MQTT credential and required by a broker candidate that has one. Chosen over mTLS with a System CA because the MCU leaf tier has to be able to do it: a CA lifecycle, cert distribution and a TLS stack on every tier is the class of weight that keeps deleting the embedded north star, and it would reverse the `default-features = false` that keeps rustls out of the daemon. It raises the floor from *anyone on the LAN* to *anyone with the key* and no further — no per-node identity, no revocation, and re-keying every node is the only recovery from a leak. Not a claim of security on a hostile network.
- **OPEN:** transport security on LAN more generally (likely mTLS with a System-level CA provisioned via the Designer, for the management API), **per-node identity and revocation on the pub/sub bus**, token rotation. The key above is a floor, not an answer to these.

### 3.12 Observability

- Logger blocks, log inspection via Designer.
- **Live signal tapping** on connections is in scope: per-connection taps published over the same pub/sub, sampled and ring-buffered.
- **Correction to this document's own history.** Earlier drafts called tapping "nio's killer debugging UX". Reconstructed from the archives, it was not nio's at all. nio's answer to "what is flowing here" was a **Logger block wired into the graph** plus a logger panel that printed every signal as one-line JSON, complete rather than sampled. That works, and the panel was good — but observing a connection meant *editing the service to add a block to it*, which changes the thing being observed and costs a restart to put in and another to take out. eieio's tap is therefore not a restoration; it is the fix for the gap that shape leaves. Keep the logger panel's virtues (complete, per-block, levelled, historical-then-streaming) and drop the requirement to modify a running design in order to look at it.
- **OPEN:** metrics surface.

### 3.13 Supervision and lifecycle

- **OPEN:** block failure policy (restart block vs kill service vs error routing / dead-letter). Erlang-style restart strategies are the reference model.
- Hot reload of a running service: acceptable v1 answer is "restart the service" — but stated explicitly, including what happens to in-flight signals and block state.

---

## 4. Agent-first builder interface

LLM agents are a first-class client for designing, building, deploying, and introspecting Systems — peer to the Designer UI, sharing the same APIs and artifacts. Design pressure this creates (and why several decisions above landed where they did):

- **Text artifacts before UI.** Service definitions are declarative files (3.8). Block manifests are machine-readable schemas (3.6). An agent can author a complete service without a canvas.
- **Discoverable block specs.** JSON Schema manifests let an agent enumerate available blocks, their properties, ports, and capability requirements — the same data that renders Designer config panels.
- **OpenAPI as tool surface.** The management API spec (3.10) is directly consumable as agent tooling.
- **CLI parity.** A CLI with full API parity, connectable to all nodes in a System.
- **MCP is a mode of the CLI, not an endpoint of the node.** `eio mcp` speaks MCP over stdio and reaches every node in `~/.config/eieio/nodes.toml`. Three packagings were available and this one was chosen deliberately:

  |Packaging|Why not|
  |---|---|
  |An MCP server inside each daemon|One server per node, so an agent holding a System juggles N connections and has no way to ask a question that spans them. It also widens what a node serves from one protocol to two — and the leaf tier (3.7) could never serve the second, so the agent surface would stop existing exactly where the platform is most interesting.|
  |A sidecar deriving tools from `/openapi.json`|A third component to install, configure and version, whose entire content is a document plus a node address. The CLI already holds both.|
  |A mode of the CLI|Chosen. The multi-node context, the token handling and the API client already exist there (5.1), and an agent gets the same reach an operator has, through the same credentials file, with nothing added to any node.|

  What this buys is the target capability below at System scope rather than node scope: the agent addresses nodes by the names the operator gave them, and a tool that needs two nodes is an ordinary tool call. What it costs is that the agent must run the CLI locally — acceptable, because an agent that cannot run a local binary also cannot author the service files (3.8) it is being asked to deploy.

  **This is the only MCP surface.** The Designer serves none of its own (DESIGNER §8): it drives the same daemon API, so a second implementation would be the same tools twice with drift as the only possible difference. The peer-client rule below is unaffected and is in fact what makes one surface sufficient — a Designer capability an agent cannot reach means the *daemon* is missing an operation, not that the Designer is missing a tool.
- **Agent-legible expressions.** The micro-Lisp (3.5) is trivially generated and validated by an LLM.

Target capability: a whole service can be built, configured, deployed, started, and introspected from a single agent prompt — in the Designer or via CLI.

---

## 5. Nomenclature

Settled. nio's vocabulary is kept except `Instance` → `Node`; the terms earned their keep and renaming them bought nothing.

|Term|Meaning|
|---|---|
|System|A group of nodes communicating via pub/sub|
|Node|A single device running eieio (daemon-class or leaf-class, §3.7)|
|Service|A graph of configured, connected blocks on one node; the unit of deployment|
|Block|An executable unit: input signal → block logic → optional output signal|
|Signal|A batch of data (list of dicts) moving through the system|

### 5.1 Identifier prefix

The project is `eieio`; the identifier prefix is `eio` — short enough for hot-path signatures, unambiguous under grep. Applies uniformly:

|Surface|Form|
|---|---|
|WASM guest exports|`eio_configure`, `eio_alloc`, `eio_process_signals`, …|
|WASM import namespaces|`eio:core`, `eio:state`, `eio:gpio`, …|
|Module custom section|`eio:manifest`|
|SDK crate / import path|`eio-sdk` / `eio_sdk`|
|Cargo subcommand|`cargo eio`|
|CLI binary|`eio`|
|Node data directory|`/etc/eieio/`|

`nio` survives only in historic references (§1) and links to the original project's repos. Any `nio_*` or `nio:*` identifier in code or docs is a leftover and a bug.

**Still open:** the expression language's own name (EXPR-SPEC §13).

---

## 6. Out of scope (for now)

- Cloud-hosted anything. The platform is self-hosted end to end.
- Multi-tenancy, orgs, RBAC.
- Signal schemas/typing (schemaless first).
- Block authorship in languages other than Rust (ABI permits it; tooling later).
- Rebuilding nio's full block library (port on demand).

---

## 7. Sequencing

**Specification phase — done (Draft 1).** ABI-SPEC (the contract everything builds against), EXPR-SPEC, SDK-SPEC, DAEMON-SPEC, DESIGNER-SPEC.

**Implementation phase.** Bottom-up: the leaf-shareable `no_std` crates first, because they are the most completely specified, carry conformance suites, and unblock everything above them.

1. `signal` — CBOR value, signal, and batch types (ABI §6.3). Small; unblocks `expr`.
2. `expr` — parser, static analysis, interpreter, budgets, plus the `expr-tests/` vector suite (EXPR §11). Proves the `no_std` discipline and gives the Designer its in-browser linter early.
3. `manifest` — schema types, parsing, import-section cross-check (ABI §4.3, §11).
4. `host-core` + `daemon` skeleton — lifecycle driver, executor, router; load and run a block, route signals locally.
5. `block-sdk` + first golden block, then the conformance harness (ABI §13).
6. Service file format + management API (DAEMON-SPEC §2, §9).
7. Pub/sub transport + cross-node signals (§3.9, settled: MQTT behind DAEMON §7's bridge).
8. CLI + agent tooling (MCP).
9. Designer UI.
10. Leaf runtime + firmware build pipeline.

### 7.1 Work tracking (beads)

The implementation phase is tracked in the beads issue tracker (`bd`, see CLAUDE.md), planned 2026-08. One epic per sequencing stage plus cross-cutting epics for tooling and CI/CD; dependencies encode the ordering above, so `bd ready` always surfaces the correct next work. Issue descriptions carry the governing spec sections and repo standards; acceptance criteria are the conformance bar.

|Epic|Covers|Sequencing item|
|---|---|---|
|`eieio-u2m`|Workspace scaffold + `just` recipe suite|precedes 1|
|`eieio-e6s`|`signal` crate|1|
|`eieio-s85`|`expr` crate + `expr-tests/` vectors|2|
|`eieio-m7o`|`manifest` crate + `manifest.schema.json`|3|
|`eieio-35h`|`host-core` + daemon skeleton (executor, router, end-to-end milestone)|4|
|`eieio-7d8`|`block-sdk`, `cargo-eio`, TestHost, conformance harness, golden + hostile blocks|5|
|`eieio-8yq`|Service files, `node.toml`, block manager, management API, state store, taps/logs|6|
|`eieio-p0k`|CI/CD (see gates below)|cross-cutting|
|`eieio-2vm`|Pub/sub transport (§3.4, §3.9) + bridge + system blocks|7|
|`eieio-yck`|CLI + daemon MCP surface|8|
|`eieio-m9s`|Designer UI|9|
|`eieio-x7g`|LEAF-SPEC draft, then leaf runtime + firmware pipeline|10|

Tooling and CI/CD placement (settled):

- **`just` is the single command surface.** A `justfile` lands with the workspace scaffold. The canonical recipes are `fmt`, `fmt-check`, `lint`, `build`, `test`, `test-doc`, `test-golden`, `check-nostd`, `check-guest`, `ci`, plus `run-*` recipes per runnable component and `release-*`/`publish-dry-run` per release pipeline (§7.2). `ci` runs the gate stages and is the only thing CI invokes; the rest are addressable individually because a contributor fixing one class of failure should not have to run the others. Recipe names are a stable interface; CI invokes `just ci` and nothing else, so local and CI runs are identical and the entrypoint never changes. Warnings are denied in `lint`, never in a manifest, so a plain `cargo build` stays usable. `ci` also runs `check-disk` before anything else, and that one is a *precondition* rather than a gate stage: it asserts nothing about the code, only that this machine has room to answer honestly about it. A full volume was measured to fail this suite in two shapes that name no disk — an LLVM `No space left on device` whose closing line reads `error: could not compile <crate>`, and a daemon API test failing with a bare `EINVAL` at 5.4G free that passed unchanged at 14G — so `ci` refuses below a floor where its own answers have been observed to be wrong, warns (never refuses) when the projected end state is merely tight, and names the disk in its failure summary instead of naming a crate.
- **`check-nostd` compiles the leaf-shareable `no_std` crates** (DAEMON-SPEC §1) **for two bare-metal targets**, `thumbv7em-none-eabihf` (Cortex-M4F) and `riscv32imc-unknown-none-elf` (ESP32-C3/C6 class). Neither ships a `std`, so a `std` dependency fails to compile — that is the enforcement. The pair is deliberate: rv32imc lacks the A extension, so it also rejects dependencies assuming atomic compare-and-swap, which the Cortex-M target would accept. Classic ESP32 is Xtensa and requires the esp-rs toolchain fork, so it cannot serve as a stock-rustup gate.
- **CI waits for the test baseline.** The GitHub Actions workflow lands only once `signal` tests and the `expr-tests/` vector suite exist — CI on an untested codebase asserts nothing.
- **Deploy/release pipelines land per deployable component**, each tag-triggered and independent within the monorepo: daemon binaries (multi-arch), `eio-sdk` + `cargo-eio` to crates.io, Designer container image. Each is blocked on its component existing; all require CI as a prerequisite job.
- **The tag names the component**: `daemon-v0.1.0`, `sdk-v0.1.0`, `designer-v0.1.0` — never a bare `v0.1.0`, which would fire every pipeline in the repository and leave each one to filter itself back out. Independent triggering is the point, so components version independently and the daemon carries its own `version` rather than the workspace's. The ★ crates and `eio-sdk` keep a shared one, because crates.io publishes them together anyway. A release *reuses* the CI workflow rather than restating its checks: a release that ran a different gate from the one `main` runs would be free to drift from it.
- **Daemon binaries are `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`**, covering the server and the Pi-class node of §3.7, each a `.tar.gz` with a `.sha256` beside it. Statically linked (musl) builds are not published: they would be the natural base for a container image, and that is the Designer pipeline's question rather than the daemon's. Adding an architecture is adding a matrix entry, which is the shape this is kept in.
- **Publishing `eio-sdk` publishes its shared dependencies.** crates.io rejects path-only dependencies, so `eio-signal` and `eio-expr` go up with it. Every workspace crate therefore carries a publishable `eio`-prefixed package name from its first commit (DAEMON-SPEC §1) — the bare names are taken upstream, and renaming later would break every import in the tree. **The published set is the closure over *normal* dependencies, and dev edges are not in it**: a consumer links a published crate's library and never runs its tests, so a workspace crate reachable only through a dev-dependency is not published, and the dev-dependency is written path-only so cargo strips it when packaging. That is why `eio-wamr-host` is absent even though `eio-conformance` dev-depends on it — publishing an FFI crate that builds WAMR's C core, reachable from nothing on the registry, is a permanent support commitment bought for nothing. `just publish-dry-run` enforces both halves: every versioned edge from a crate in the set into another workspace member must land on a crate that is also in the set and published before it, and every version-less one must be a dev edge.
