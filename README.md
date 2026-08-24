# eieio

Build a distributed stream-processing system by wiring blocks together — then deploy it across a fleet of Raspberry Pis, or bake it into an ESP32's firmware, from the same design.

> **Status: early implementation, and further along than "skeleton."** Blocks are written in Rust against a real SDK, built and tested with `cargo eio`, and driven by a conformance suite that runs on three engines. A daemon loads them, routes signals, serves a management API, and moves signals *between nodes* over MQTT. The `eio` CLI reaches every node in a System, and `eio mcp` gives an agent the same reach. The Designer's scaffold stands — server, shell, and the expression language compiled to WASM — but the canvas does not edit yet. The leaf runtime is unstarted. See [Status](#status).

---

## What it is

A self-hosted platform for building systems out of small, composable, sandboxed units of computation.

|Term|Meaning|
|---|---|
|**System**|A group of nodes communicating over pub/sub|
|**Node**|One device running eieio — a Pi, a server, an ESP32|
|**Service**|A graph of configured, connected blocks on one node; the unit of deployment|
|**Block**|An executable unit: input signal → logic → optional output signal|
|**Signal**|A batch of dict-shaped data flowing between blocks|

A service is a text file. Blocks are WebAssembly modules pulled from an OCI registry by reference; the node loads them, wires their ports, and starts moving signals. Cross-device flow is just blocks — a `publisher` on one node and a `subscriber` on another.

```toml
name = "kitchen"
autostart = true
overflow = "drop-oldest"          # or "backpressure" (the default)

connections = [
  "b7k2.out -> f3m9.in",
  "f3m9.above -> k1p8.in",        # fan-out: each destination gets the whole batch
  "f3m9.err -> q4tv.in",          # the error port, addressable as a source only
]

[blocks.b7k2]
name  = "Thermometer"
block = "ghcr.io/tlugger/temp-sensor:1.0.0"
[blocks.b7k2.props]
interval_ms = "5000"

[blocks.f3m9]
name  = "Too cold?"
block = "filter:1.2.0"
[blocks.f3m9.props]
reading   = "(float $temp)"       # evaluated per signal; fails per signal if absent
threshold = "18.0"
```

A block instance is its **id** (`b7k2`), never its name — names are labels and may change. Every property is an expression, evaluated host-side per signal, so blocks stay dumb and reusable and behaviour lives in configuration rather than in a fork.

## Why it exists

A rebuild of [nio](https://web.archive.org/web/20190716020124/https://docs.n.io/) (niolabs, ~2014–2019, defunct). nio got the abstraction right — blocks, signals, services, per-signal expression properties, and a logger panel that showed you every signal a block saw. Three things killed it, and each is a design constraint here:

|nio's problem|eieio's answer|
|---|---|
|Python runtime: heavy, fragile, ran only where Python runs. No embedded story, ever.|Compiled Rust daemon; blocks are core WASM. Server down to MCU.|
|Platform state lived in the vendor's cloud. The company died and every deployment died with it.|Each node owns its configuration as files on disk. The Designer's database is an address book, never the system of record.|
|It did everything, for everyone.|Narrow: distributed stream processing with a real embedded path.|

## How it works

**Core WASM, not the component model.** Blocks target `wasm32-unknown-unknown` against a hand-written ABI — core WASM 1.0 plus exactly six measured proposals, which is what the Rust toolchain emits *and* wasm3 executes. The same module runs under wasmtime on a Pi and WAMR on an MCU. Install a block, restart the service, use it.

**The import section is the capability system.** A block cannot touch I2C or GPIO from inside the sandbox; it calls host functions and its manifest declares which it needs. Deploy-time validation reduces to "does this node provide what these blocks require."

**Expressions are host-side.** A small purpose-built Lisp — pure, deterministic, bounded, `no_std` — lives in the runtime. One interpreter per node; every block gets dynamic configuration for free.

```lisp
(if (> $temp 90) "critical" (if (> $temp 75) "warn" "ok"))
(str "sensor/" $device_id "/" (lower $kind))
```

Because it is `no_std` and dependency-free, the *same crate* compiles to WASM and runs in the Designer, linting a property as you type it. Not a JavaScript reimplementation — that would be a second definition of the language, free to disagree with the first, and nothing would notice. The browser build is checked against the same 484 conformance vectors the daemon is.

**Missing data is an error, not null.** `$temp` on a signal without `temp` fails that signal, visibly. Silent nulls turn config typos into 2 a.m. mysteries.

**Two node classes, one design flow.** Daemon-class nodes hot-load blocks and expose a management API. Leaf-class nodes get services AOT-compiled into firmware. Deploying to a leaf takes extra steps, not a different way of designing.

**Agents are a first-class client.** Services are text, manifests are JSON Schema, the API ships OpenAPI, expressions are trivially generated and validated. Anything the Designer can do, an agent can reach through the same API — `eio mcp` serves it as MCP over stdio, and because it is a mode of the CLI rather than an endpoint of the daemon, one agent session reaches every node in the System instead of one node per connection.

## Components

```
crates/
  abi/          ★ status codes, sentinels, alignment — the boundary's shared constants.
                  Dependency-free by rule: everything here ships in every block
  signal/       ★ CBOR value, signal and batch types
  expr/         ★ the expression language: parser, static analysis, interpreter, budgets
  manifest/     ★ manifest schema, parsing, WASM import/export cross-check
  host-core/    ★ engine-agnostic ABI host: lifecycle, memory, properties, state, timers,
                  router core
  block-sdk/    ★ the guest runtime block authors write against: allocator, panic handler,
                  Ctx, capabilities
  block-sdk-macros/  the #[block] macro: ABI exports, port enums, Prop<T>, eio:manifest section
  service/      the service-file schema, parser and validator
  daemon/       the node: tokio, wasmtime, executor, router, OCI client, management API,
                  MQTT pub/sub bridge
  cli/          the `eio` binary — management-API parity, multi-node, and `eio mcp`,
                  the MCP surface an agent drives a whole System through
  designer/     the Designer's server: axum, SQLite registry, and one catch-all proxy
                  to the nodes — a node's token never reaches the browser
  expr-wasm/    `expr` compiled for the browser, so the Designer lints a property
                  with the interpreter the daemon runs, not a copy of it
  cargo-eio/    `cargo eio new | build | test | publish` for block authors
  test-host/    runs a block natively, no wasm — SDK §6.1's fast inner loop
  conformance/  the reference wasmtime harness plus its scenario suite, run against
                  three engines: wasmtime, wasm3 and WAMR
designer/       the Designer's web app: Vite, Svelte 5, Svelte Flow. Built into
                `designer/dist`, which the server above embeds and serves
expr-tests/     host-agnostic vectors: the expression language, ABI property types,
                canonical CBOR
examples/
  blocks/       the five golden blocks of ABI §13.2, written with the SDK
  services/     sample service files
schemas/        published JSON Schemas: manifest, service
docs/
  SCOPE.md      settled decisions, OPEN items, vocabulary, sequencing — read first
  specs/        ABI-SPEC · EXPR-SPEC · SDK-SPEC · SERVICE-SPEC · DAEMON-SPEC · DESIGNER-SPEC
```

★ crates are shared with the future leaf runtime and stay `no_std` (`alloc` allowed); `just check-nostd` enforces it against two bare-metal targets. It is why `expr-wasm` is a crate of its own rather than a feature of `expr`: a `wasm-bindgen` dependency added there would ship in every block. Blocks live in their own repositories and publish to an OCI registry independently.

**The specs are normative, not descriptive.** Code does not drift from them, and a spec change lands in the same commit as the code it governs — the whole architecture depends on two independent host implementations agreeing byte for byte. [`CLAUDE.md`](CLAUDE.md) is the working guide.

## Developing

You need [`just`](https://just.systems) and `rustup`. The toolchain is pinned in `rust-toolchain.toml`. Node is needed for the Designer's web app and for nothing else — without it `just ci` runs everything but that suite, and says so rather than skipping quietly.

|Recipe|What it does|
|---|---|
|`just ci`|**The gate.** `fmt-check`, `lint`, `build`, `test`, `test-doc`, `test-golden`, `check-nostd`, `check-guest`, and `test-designer` where npm exists. Stages run concurrently. CI runs this and nothing else.|
|`just fmt` / `just fmt-check`|Format in place / fail instead of rewriting|
|`just lint`|`clippy` with warnings denied, plus a per-crate lint opt-in check|
|`just test` / `just test-golden`|The workspace suite / the golden blocks' own native tests|
|`just check-nostd`|Compile the ★ crates for Cortex-M4F and a no-atomics RISC-V target. A `std` dependency that sneaks in fails here|
|`just check-guest`|Build the SDK for `wasm32-unknown-unknown` with `panic=abort`, as a guest actually is|
|`just publish-dry-run`|Prove the crates.io publish set is publishable before a release tag needs it|
|`just run-daemon` / `just eio`|Run a node / run the CLI. Arguments pass through|
|`just run-designer`|Build the web app, then serve it from the Designer's own binary|
|`just designer-build` / `just test-designer`|The web app alone: build it, or run its suite|

Warnings are denied in `just lint`, never in a `Cargo.toml`, so a plain `cargo build` stays usable while the gate stays strict.

## Status

Bottom-up, most-specified first:

- [x] `signal`, `expr`, `manifest` — the `no_std` foundation, with conformance vectors
- [x] `host-core` + `daemon` — lifecycle, executor, router; `eio:state`, `eio:timer`, taps
- [x] `block-sdk`, `block-sdk-macros`, `cargo-eio`, `test-host` — blocks are written in Rust
- [x] `conformance` — reference harness, golden blocks, scenario suite on three engines
- [x] Service files, node config, block cache and management API
- [x] Pub/sub transport and cross-node signals — MQTT behind a swappable bridge
- [x] CLI and agent tooling — `eio` reaches every node, and `eio mcp` is the same surface for an agent
- [ ] Designer UI — *in progress.* The server, the app shell and `expr`-in-the-browser are built; editing on the canvas is next
- [ ] Leaf runtime and firmware build pipeline

Still **OPEN** in [`docs/SCOPE.md`](docs/SCOPE.md), and tracked there rather than here: normative floors for `max_payload`/`max_batch`; transport security on the LAN, per-node identity and revocation on the bus, and token rotation (the bus pre-shared key is a floor, not an answer); what `cargo eio publish` does with a SemVer `+build` suffix an OCI tag cannot hold; the metrics surface; and block failure policy.

## Prior art

- [nio docs](https://web.archive.org/web/20190716020124/https://docs.n.io/) and [workshops](https://web.archive.org/web/20181116060203/https://workshops.n.io/) (archived)
- [nio-blocks](https://github.com/nio-blocks) — the original block library
- [tlugger/nio](https://github.com/tlugger/nio) — the block development framework
- [tlugger/simulator](https://github.com/tlugger/simulator) — simulator blocks

The nio daemon and System Designer did not survive; their behaviour is being reverse-engineered from the above.

## License

Apache-2.0. See [LICENSE](LICENSE).
