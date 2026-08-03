# SCOPE

High-level scope and decision record for an open source rebuild of the nio platform. This document gives full background context for the system being built. It is the input to a more detailed SPEC. Decisions here are settled unless marked **OPEN**.

Working name: `eieio` (candidates floated: `thenio`, `zio`). Placeholder used throughout: **the platform**.

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
- **OPEN:** delivery guarantees (at-most-once vs at-least-once), ordering, and backpressure policy for cross-device flow.

### 3.5 Expression language: a purpose-built micro-Lisp

Block configuration properties support per-signal expressions (successor to nio's `{{ $attr }}` Python-eval properties). Decisions:

- Lisp syntax: trivially parseable, tiny interpreter, agent-friendly.
- **Host-side evaluation**: the interpreter lives in the daemon/leaf runtime, not in blocks. Every block gets expressions for free; blocks stay dumb; MCUs need exactly one interpreter.
- Purpose-built, not borrowed: pure (no IO), bounded evaluation steps, `no_std`, deterministic. Existing Rust Lisps assume std and carry weight we don't want on a leaf.
- Evaluation is per-signal within a batch (nio semantics preserved).

### 3.6 Block registry: OCI artifacts

Blocks live in separate source repos. Each repo's CI compiles the block to WASM (plus AOT variants per embedded target) and publishes it as an **OCI artifact** (e.g. ghcr.io): content-addressed, versioned, signable (cosign), zero registry infrastructure to run. Daemons pull by reference.

The **block manifest** is a core design artifact: properties schema (JSON Schema — drives Designer config panels _and_ agent tooling), input/output ports, required host capabilities, block version, ABI version.

### 3.7 Node tiers (embedded north star)

Two node classes, one wire protocol:

- **Daemon-class** (Pi and up): full daemon, hot-loads WASM blocks, exposes management API.
- **Leaf-class** (MCU: ESP32 etc.): minimal runtime; services are deployed via a build step — AOT-compiled WASM baked into firmware and flashed. No hot-install, no management API beyond what the wire protocol carries.

The Designer flow is identical for both: design a service (subscriber/sensor blocks → logic → publisher/actuator blocks), deploy. For leaf targets, "deploy" runs a firmware build+flash pipeline instead of a config push. Extra flashing steps are acceptable; a different design flow is not.

### 3.8 Configuration source of truth: files on the node

Each daemon owns its configuration as **declarative text files on disk**. The management API reads/applies them. The Designer's backend DB is a registry (Systems, node connection info) — never the system of record. Rationale: survivability (nio's cloud-state failure mode), GitOps-compatibility, and the agentic goal — agents and CLIs operate on text artifacts, not DB rows behind an API.

### 3.9 Pub/sub

Cross-device signal flow is native pub/sub: publish/subscribe blocks are ordinary drag-and-drop blocks bound to topics.

- **OPEN:** transport build-vs-buy (MQTT — best IoT/MCU ecosystem compat — vs embedded NATS vs custom).
- **OPEN:** broker topology (per-instance, elected instance, or Designer backend) and node discovery (mDNS?).

### 3.10 Management API: REST + OpenAPI

- Management plane: REST/JSON with a published OpenAPI spec. Curl-able, browser-friendly, and the spec doubles as agent/MCP tooling surface for free.
- Streaming (logs, signal taps): WebSocket or SSE.
- gRPC and GraphQL rejected (embedded and browser friction; problems we don't have).

### 3.11 Auth

- UI/agent ↔ daemon: per-node token generated by the daemon, included in requests. Deliberately simple; JWT rejected as overcomplicated for this.
- **OPEN:** transport security on LAN (likely mTLS with a System-level CA provisioned via the Designer), node↔node auth on the pub/sub bus, token rotation.

### 3.12 Observability

- Logger blocks, log inspection via Designer.
- **Live signal tapping** on connections — nio's killer debugging UX — is in scope: per-connection taps published over the same pub/sub, sampled and ring-buffered.
- **OPEN:** metrics surface.

### 3.13 Supervision and lifecycle

- **OPEN:** block failure policy (restart block vs kill service vs error routing / dead-letter). Erlang-style restart strategies are the reference model.
- Hot reload of a running service: acceptable v1 answer is "restart the service" — but stated explicitly, including what happens to in-flight signals and block state.

---

## 4. Agent-first builder interface

LLM agents are a first-class client for designing, building, deploying, and introspecting Systems — peer to the Designer UI, sharing the same APIs and artifacts. Design pressure this creates (and why several decisions above landed where they did):

- **Text artifacts before UI.** Service definitions are declarative files (3.8). Block manifests are machine-readable schemas (3.6). An agent can author a complete service without a canvas.
- **Discoverable block specs.** JSON Schema manifests let an agent enumerate available blocks, their properties, ports, and capability requirements — the same data that renders Designer config panels.
- **OpenAPI as tool surface.** The management API spec (3.10) is directly consumable as agent tooling; an MCP server per daemon is the natural packaging.
- **CLI parity.** A CLI with full API parity, connectable to all nodes in a System.
- **Agent-legible expressions.** The micro-Lisp (3.5) is trivially generated and validated by an LLM.

Target capability: a whole service can be built, configured, deployed, started, and introspected from a single agent prompt — in the Designer or via CLI.

---

## 5. Nomenclature

Keep **block** and **signal**. Replace the generic tier names. **OPEN**, current direction:

|nio|Candidate|Note|
|---|---|---|
|System|System / Mesh / Fleet||
|Instance|Node|de facto standard term|
|Service|Flow / Graph / Service|"Flow" matches Node-RED convention|

Constraint: avoid collisions with basic programming terminology.

---

## 6. Out of scope (for now)

- Cloud-hosted anything. The platform is self-hosted end to end.
- Multi-tenancy, orgs, RBAC.
- Signal schemas/typing (schemaless first).
- Block authorship in languages other than Rust (ABI permits it; tooling later).
- Rebuilding nio's full block library (port on demand).

---

## 7. Sequencing

1. **Block ABI spec** — guest exports, host function set (core + hardware capabilities), CBOR buffer conventions, lifecycle, manifest schema. Everything else builds against this contract.
2. Daemon skeleton: load/run blocks, service execution, local signal routing.
3. Pub/sub transport decision + cross-node signals.
4. Service definition file format + management API.
5. CLI + agent tooling.
6. Designer UI.
7. Leaf runtime + firmware build pipeline.
