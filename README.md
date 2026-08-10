# eieio

Build a distributed stream-processing system by wiring blocks together on a canvas — then deploy it across a fleet of Raspberry Pis, or bake it into an ESP32's firmware, from the same design.

> **Status: early implementation.** The specs are complete through Draft 1 and the bottom four layers are built — `signal`, `expr`, `manifest`, and a `host-core` + `daemon` skeleton that loads a real WASM block and routes a signal between two instances. No SDK yet, so blocks are still hand-written `.wat`. See [Roadmap](#roadmap).

---

## What it is

eieio is a self-hosted platform for building systems out of small, composable, sandboxed units of computation:

|Term|Meaning|
|---|---|
|**System**|A group of nodes communicating over pub/sub|
|**Node**|One device running eieio — a Pi, a server, an ESP32|
|**Service**|A graph of configured, connected blocks on one node; the unit of deployment|
|**Block**|An executable unit: input signal → block logic → optional output signal|
|**Signal**|A batch of dict-shaped data flowing between blocks|

You write a **service** as a text file (or draw it on a canvas — the canvas is a view of the file). Blocks are WebAssembly modules pulled from an OCI registry by reference. The node loads them, wires their ports together, and starts moving signals. Blocks that span devices are just blocks: a `publisher` on one node and a `subscriber` on another are dragged onto their respective canvases like anything else.

```toml
# a service, in full
name = "office-temp"
autostart = true

[[blocks]]
name   = "sensor"
block  = "ghcr.io/eieio-blocks/bme280:1.0.0"
[blocks.props]
interval_ms = "(* 30 1000)"
bus         = "1"

[[blocks]]
name   = "hot"
block  = "ghcr.io/eieio-blocks/filter:1.2.0"
[blocks.props]
predicate = "(> $temp_c 27.0)"

[[connections]]
from = "sensor.out"
to   = "hot.in"
```

Every configuration property is an expression, evaluated per signal by the host — so blocks stay dumb and reusable, and behavior lives in configuration rather than in a fork of the block.

## Why it exists

This is a rebuild of [nio](https://web.archive.org/web/20190716020124/https://docs.n.io/) (niolabs, ~2014–2019, now defunct). nio got the abstraction right: blocks, signals, services, live signal inspection, and per-signal expression properties composed into something genuinely powerful. Three things killed it, and each one is a design constraint here:

|nio's problem|eieio's answer|
|---|---|
|Python runtime — heavy, fragile, and it could only run where Python runs. No embedded story, ever.|Compiled Rust daemon; blocks are core WASM modules. Runs from a server down to an MCU.|
|The platform's state lived in the vendor's cloud. The company died, and every deployment died with it.|Each node owns its configuration as text files on disk. The Designer's database is an address book, never the system of record. Delete it and you lose nothing but addresses.|
|It did everything, for everyone.|Narrow: distributed stream processing with a real embedded deployment path. Not a workflow engine, not a home automation suite.|

## How it works

**Blocks are core WASM modules** targeting `wasm32-unknown-unknown` against a hand-written ABI — deliberately not the component model, which does not exist at the MCU tier. The same module runs under wasmtime on a Pi and WAMR on an ESP32. Install a block, restart the service, use it: no daemon recompile, no dependency installation, no ambient trust.

**The import section is the capability system.** A block cannot touch I2C or GPIO from inside the sandbox; it calls host functions the runtime provides, and its manifest declares which it needs. Deploy-time validation reduces to "does this node provide what these blocks require" — which is what makes designing on a laptop and deploying to an MCU honest rather than aspirational.

**Expressions are evaluated host-side.** A small purpose-built Lisp — pure, deterministic, bounded, `no_std` — lives in the runtime, not in blocks. One interpreter per node, every block gets dynamic configuration for free:

```lisp
(if (> $temp 90) "critical" (if (> $temp 75) "warn" "ok"))
(str "sensor/" $device_id "/" (lower $kind))
```

**Two node classes, one wire protocol and one design flow.** Daemon-class nodes (Pi and up) hot-load blocks and expose a management API. Leaf-class nodes (ESP32 and friends) get their services AOT-compiled into firmware and flashed. Deploying to a leaf involves extra steps; it does not involve a different way of designing.

**Agents are a first-class client.** Not a chat box bolted onto a UI: service definitions are text, block manifests are JSON Schema, the management API ships an OpenAPI spec, and the expression language is trivially generated and validated. Anything the Designer can do, an agent can do, because both drive the same API. The test is that the demo runs twice — once clicked, once prompted.

## Repository layout

```
crates/
  signal/     ★ CBOR value, signal and batch types
  expr/       ★ the expression language: parser, analysis, interpreter, budgets
  manifest/   ★ manifest schema, parsing, WASM import cross-check
  host-core/  ★ engine-agnostic ABI host: lifecycle, memory, props, router core
  daemon/       the std binary: tokio, wasmtime, executor
expr-tests/     host-agnostic expression conformance vectors
docs/
  SCOPE.md              scope and decision record — read first
  specs/
    ABI-SPEC.md         host↔block binary contract; everything builds against this
    EXPR-SPEC.md        expression language: grammar, semantics, builtins, bounds
    SDK-SPEC.md         the Rust crate block authors write against
    DAEMON-SPEC.md      daemon-class node runtime architecture
    DESIGNER-SPEC.md    the visual management surface
```

★ crates are shared with the future leaf runtime and stay `no_std` (`alloc` allowed) — `just check-nostd` is what enforces it. The Designer will be a sibling SvelteKit app. Blocks live in their own repositories and publish to an OCI registry independently.

The specs are normative, not descriptive: code does not get to drift from them, and a spec change lands in the same commit as the code it governs. [`CLAUDE.md`](CLAUDE.md) is the working guide for that.

## Developing

You need [`just`](https://just.systems) and `rustup`. The toolchain is pinned in `rust-toolchain.toml`, so rustup installs the right compiler and targets on first use — there is nothing else to set up.

Everything goes through `just`; run it bare to list the recipes.

|Recipe|What it does|
|---|---|
|`just ci`|**The gate.** Runs `fmt-check`, `lint`, `build`, `test`, `check-nostd` in that order, stopping at the first failure. CI runs this and nothing else.|
|`just fmt`|Format in place (upstream rustfmt defaults).|
|`just fmt-check`|Formatting gate — fails instead of rewriting.|
|`just lint`|`clippy` with warnings denied, plus a check that every crate opts into the shared lint baseline.|
|`just build` / `just test`|Build and test the workspace.|
|`just check-nostd`|Compile the leaf-shareable crates for two bare-metal targets (Cortex-M4F and the no-atomics ESP32-C3 class RISC-V). A `std` dependency that sneaks into those crates fails here.|

Warnings are denied in `just lint`, never in a `Cargo.toml`, so a plain `cargo build` stays usable while the gate stays strict.

## Roadmap

Implementation is bottom-up, starting with the `no_std` crates the leaf runtime also needs:

- [x] `signal` — CBOR value, signal, and batch types
- [x] `expr` — the expression language, plus its conformance vector suite
- [x] `manifest` — manifest schema and WASM import cross-check
- [x] `host-core` + `daemon` skeleton — load a block, route a signal
- [ ] `block-sdk` + conformance harness and golden blocks
- [ ] Service files + management API
- [ ] Pub/sub transport and cross-node signals
- [ ] CLI and agent (MCP) tooling
- [ ] Designer UI
- [ ] Leaf runtime and firmware build pipeline

Open questions — delivery guarantees and backpressure, pub/sub transport and broker topology, supervision policy, transport security — are tracked as **OPEN** items in [`docs/SCOPE.md`](docs/SCOPE.md).

## Prior art

- [nio docs](https://web.archive.org/web/20190716020124/https://docs.n.io/) and [workshops](https://web.archive.org/web/20181116060203/https://workshops.n.io/) (archived)
- [nio-blocks](https://github.com/nio-blocks) — the original block library
- [tlugger/nio](https://github.com/tlugger/nio) — the block development framework
- [tlugger/simulator](https://github.com/tlugger/simulator) — simulator blocks

The nio daemon and System Designer did not survive; their behavior is being reverse-engineered from the above.

## License

Apache-2.0. See [LICENSE](LICENSE).
