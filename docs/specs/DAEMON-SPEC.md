# Daemon Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion per-subsystem. **Depends on:** SCOPE.md, ABI-SPEC.md, EXPR-SPEC.md. **Markers:** Settled decisions are stated plainly. **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

The daemon is the daemon-class node runtime (SCOPE §3.7): a single compiled Rust binary that establishes a node, executes services, and exposes the management API. This spec covers its internal architecture; the leaf runtime shares the starred (★) subsystems and is specified separately later.

---

## 1. Crate architecture

Monorepo workspace. The load-bearing split is **host-core vs daemon**: everything the leaf runtime will also need lives in `host-core` and MUST stay `no_std`-compatible (alloc allowed).

```
crates/
  abi/           ★ The ABI's shared vocabulary: §8 status codes, §3 sentinels,
                   §9.6 alignment. No dependencies. Read by both host-core and
                   the guest SDK
  host-core/     ★ ABI implementation: lifecycle driver, memory conventions,
                   status/size protocol, capability validation, router core,
                   property resolution (ABI §11.1's required/default rule)
  expr/          ★ Expression language: parser, static analysis, interpreter,
                   budgets (EXPR-SPEC). no_std. Also compiled to WASM for
                   Designer in-browser linting (DESIGNER-SPEC §5)
  signal/        ★ CBOR batch/signal encode-decode (minicbor). no_std
  manifest/      ★ Manifest schema types, parsing, import-section cross-check
  service/         Service file schema, parsing and validation (SERVICE-SPEC).
                   Deliberately NOT ★, for two reasons: nothing on a leaf tier
                   parses one (SCOPE §3.7 deploys leaf targets by firmware
                   build, not by config push), and `toml` cannot compile for a
                   target without atomics — measured on `riscv32imc`, where
                   `alloc::sync` does not exist. Shared by the daemon, the CLI
                   and the Designer's backend, which is why it is a crate and
                   not a daemon module
  daemon/          Binary: tokio runtime, wasmtime engine, OCI client,
                   management API, state store, pub/sub bridge
  block-sdk/       Guest-side (SDK-SPEC); published as `eio-sdk`
  block-sdk-macros/  The `#[block]` attribute macro (SDK-SPEC §1). A separate
                   crate because the language requires it: a proc-macro crate
                   can export nothing but macros. Host-compiled, so not ★
  test-host/       Native in-process host for testing blocks (SDK-SPEC §6.1).
                   A *host*, so it depends on host-core; separate from block-sdk
                   so a guest never can
  cargo-eio/       Block build/publish tooling (SDK-SPEC §5)
  conformance/     Reference harness + golden blocks (ABI §13)
```

The expression conformance vectors are **not** here: they are data files at the repository root in `expr-tests/` (EXPR §11), because a host written in another language must be able to consume them without building a Rust crate. `conformance/` holds the ABI harness, which is Rust by nature — it drives a WASM engine.

★-marked crates are shared with the leaf runtime and MUST stay `no_std`-compatible (`alloc` allowed). `daemon` and `cargo-eio` are `std` binaries; `block-sdk` is `no_std` by necessity (it compiles into guests).

Conformance implication: `host-core` driven by wasmtime (daemon) and by WAMR (leaf) MUST pass the same harness — the shared crate is how divergence is prevented structurally, not just tested for.

The daemon's half of that is `crates/daemon/src/conformance.rs`: a `#[cfg(test)]` module taking `eio-conformance` as a **dev-dependency** and running the reference suite's scenario files through §5.1's binding. A dev-dependency and not a lib target on this crate, because the table above gives `eio-daemon` no import path on purpose — the reusable half of the host is `host-core`, and a lib target here would be a second answer to what another crate may link against. Scenarios needing a capability namespace the daemon implements no functions in are reported skipped by name, which is how that gap stays visible as the daemon grows. Today that is `eio:timer`, `eio:gpio`, `eio:i2c` and `eio:http`: `eio:core` (ABI §7.0) and `eio:state` (§7.2, backed by §10's store) are linked, so the state scenarios run here as well as against the reference host.

**Where a rule lives follows from what it is about, not from who happens to call it.** ABI §11.1's `required`/`default` precedence is the worked example, because all three plausible homes were arguable. Not `manifest`: a manifest describes what a *block* says about itself, and a deployment's supplied values are not that. Not `daemon`: the rule is pure ABI semantics with no engine and no configuration *format* in it, and leaving it there would mean the leaf runtime — whose configuration source is shaped differently — reimplementing the precedence from scratch, which is the silent divergence this split exists to prevent. So `host-core`, which is also the only crate that *can* hold it: the rule consumes `manifest`'s `Manifest` and produces `host-core`'s `PropertySource`, and the dependency runs host-core → manifest. The daemon reaches it from `--prop` flags today and from service files later; the leaf reaches it from whatever it reads. One implementation, two hosts.

The same reasoning is why **`abi` is a crate and not a module of `host-core`**, which is where ABI §8's codes started. `host-core` is the *host* half of the boundary: it drives a guest through its lifecycle and resolves properties, and depends on `expr` and `manifest` to do it. But §8's codes, §3's sentinels and §9.6's alignment are not host rules — a guest compares against every one of them, and `eio-sdk` needs them too. Left in `host-core`, a block reaching for `ERR_THROTTLED` would compile the expression interpreter and the manifest parser into its `.wasm`; re-declared in the SDK, the platform would hold two hand-maintained copies of a table the two sides MUST agree on byte for byte. So they sit below both, in a crate with no dependencies at all — which is the property worth protecting, since anything added there is added to every block that ships. ABI §12's version is deliberately *not* among them: `eio_manifest::Abi` already holds the packed form together with the compatibility rule that gives it meaning, and a bare constant in `abi` would be a second spelling of the number sitting next to the one implementation that knows what to do with it. `host-core` re-exports the lot, so a host still has one import for the ABI and the move is invisible at its call sites.

The same reasoning puts **both halves of ABI §9.7 in `host-core`**, and it is worth stating because they arrived at different times and the split looked survivable. §9.7 is one rule read in two directions: the host "rejects `emit` beyond `max_payload` with `ERR_LIMIT`" and "never delivers batches beyond" the limits its descriptor published. Neither half has an engine or a queue in it — the numbers come from the instance descriptor (ABI §5.2) and the answer is a refusal, not a delivery — so both belong beside each other, and a leaf runtime that reimplemented the inbound half would be free to disagree with the daemon about which batches a block is entitled to never see. Concretely: the driver takes the batch *decoded* and encodes it itself, because the guest is handed canonical CBOR (§6.1) while `prop`'s `signal_idx` indexes the same call's signals (§7.1), and a host supplying those by two paths could supply two different batches. Refusing is therefore its own outcome rather than a status: the guest was never called, so nothing is counted against it (§8), and the daemon's part is only saying what the refusal means to an operator (§11).

For the same reason the **property scope is the driver's**, not its caller's. ABI §7.1 answers `prop` "for the duration of the current callback", so `host-core` holds the instance's property context and opens a scope around every guest call it makes. A host cannot forget to open one, cannot leave one open across callbacks, and cannot pair a callback with the wrong batch — which is the ABI rule most likely to be implemented twice and slightly differently.

**Naming.** Directory names are exactly as listed above. Package names are `eio`-prefixed and imported with underscores:

|Directory|Package|Import path|
|---|---|---|
|`abi/`|`eio-abi`|`eio_abi`|
|`host-core/`|`eio-host-core`|`eio_host_core`|
|`expr/`|`eio-expr`|`eio_expr`|
|`signal/`|`eio-signal`|`eio_signal`|
|`manifest/`|`eio-manifest`|`eio_manifest`|
|`service/`|`eio-service`|`eio_service`|
|`daemon/`|`eio-daemon`|—|
|`block-sdk/`|`eio-sdk`|`eio_sdk`|
|`block-sdk-macros/`|`eio-sdk-macros`|`eio_sdk_macros`|
|`test-host/`|`eio-test-host`|`eio_test_host`|
|`cargo-eio/`|`cargo-eio`|—|
|`conformance/`|`eio-conformance`|`eio_conformance`|

`block-sdk-macros/` follows `block-sdk/`'s existing exception rather than the directory rule, so the pair reads as one thing: a block author sees `eio-sdk` and never names the macro crate, which `eio-sdk` re-exports.

`cargo-eio` is the sole exception: cargo discovers subcommands by binary name, so it cannot be prefixed differently. No crate overrides its `[lib] name` — package name and import path differ only by the `-`→`_` substitution cargo already performs.

The prefix is not cosmetic. `eio-sdk` publishes to crates.io (SCOPE §7.1), and a published crate cannot depend on path-only crates, so every crate reachable from it — `signal` (SDK-SPEC §2) and `expr` (SDK-SPEC §6.1, `TestHost` evaluates with the real interpreter) at minimum — MUST carry a publishable name. The bare names `signal`, `expr`, and `manifest` are already taken on crates.io.

**JSON parsing in `manifest`.** The manifest is JSON (ABI §11) and `manifest/` is ★, so the parser has to work with `alloc` and no `std`, on a target with no atomics. `serde` + `serde_json`, both with `default-features = false, features = ["alloc"]`, do: verified building for both `just check-nostd` targets. They are used with `deny_unknown_fields`, which is what makes ABI §11.1's strictness rules fall out of the derive rather than out of hand-written checks — duplicate keys, unknown fields, unknown enum variants and type mismatches are all reported with a line and column. `serde_json_core` was the obvious alternative and is unusable here: it deserializes into fixed-size buffers and cannot own strings, which a manifest of arbitrary port and property names needs. A hand-written parser would trade a well-tested dependency for the same feature list reimplemented, and the leaf runtime gains nothing from it.

The daemon depends on the same `serde_json` with `std` enabled (§12's JSON batch input). That does not reach `manifest`: `just check-nostd` builds each ★ crate for a bare-metal target as its own package, so cargo unifies features across that build and not across the workspace. The gate is what makes the claim true rather than an assumption about resolver behaviour.

---

## 2. On-disk layout (source of truth, SCOPE §3.8)

```
/etc/eieio/                      (or --data-dir)
  node.toml                    node identity, listen addr, limits, budgets (§2.1)
  auth/                        token, TLS material (OPEN, SCOPE §3.11)
    token                      the management API's bearer token (§9.1)
  services/
    <name>.toml                one service definition per file
  blocks/                      OCI pull cache: <name>/<version>/block.wasm
  precompiled/                 <sha256>.<engine>.cwasm — a compiled block, for
                               those bytes on this engine (§4.3). Derived; safe
                               to delete
  state/state.redb             eio:state backing store, one file (§10)
```

- **Service definition format: TOML** — human-first, comment-friendly, agents handle it fine; `schemas/service.schema.json` publishes the equivalent structure regardless. One file = one service = the deployable unit. **SERVICE-SPEC.md is the normative document**, and it is a separate one because the Designer and the CLI build against it as much as the daemon does.
- Service file contents, in brief: the service name, block instances keyed by a short **id** with `name` as a mere label, a `block` reference and properties as expression strings under `props`, connections as `"<id>.<port> -> <id>.<port>"`, and an optional `[ui]` table the daemon MUST parse and MUST NOT interpret (Designer layout annotations — DESIGNER-SPEC §4).
- **A service file's stem MUST equal its `name`** (SERVICE §1). `kitchen.toml` declares `name = "kitchen"` or it is not loaded, and the mismatch is that service's error rather than the node's. Two things follow, and both are why the rule is here rather than left to convention: SERVICE §3's "unique per node" becomes structural — the filesystem refuses the second `kitchen.toml` before the daemon has to — and `PUT /services/{s}` (§9) knows which file it writes without an index mapping names to filenames, which would be state the API holds that the files do not.
- **The daemon never writes a service file** (SERVICE §2). Ids are minted by tooling at authoring time, so a reload never rewrites what a human or a git checkout put there.
- **`node.toml` is the one file the daemon does write**, and only to create it (§2.1). SERVICE §2's never-write rule is about *service* files and the reason does not carry over: a service file is authored by a human, the Designer or an agent and round-trips through them, where node.toml describes the node to itself and has to exist before anything can be authored at all. Once written it is the operator's; the daemon never rewrites or normalizes it.
- The management API is a thin CRUD layer over these files plus lifecycle commands. `PUT` writes the file, validates, and reports; it never holds state the file doesn't. Editing files directly on disk (or via git) and calling `POST /services/{s}/reload` is a first-class, supported path — this is the GitOps/agent story.

### 2.1 `node.toml` (normative)

What a node is, as against §2's service files, which are what it runs. Every field is OPTIONAL except `id`, and every default below is what a node runs on when the file omits it.

```toml
id = "n7k2p4qv"                  # REQUIRED. Opaque, stable, minted on first boot
name = "kitchen-pi"              # OPTIONAL. A label; nothing resolves by it

[api]
listen = "127.0.0.1:7373"        # Management API address (§9)

[limits]                         # ABI §9.7, per instance
max_payload = 65536              # bytes
max_batch = 1024                 # signals

[budgets]                        # ABI §10, per guest entry
fuel = 100000000
deadline_ms = 1000

[budgets.expr]                   # EXPR §9, per expression evaluation
max_fuel = 100000
max_depth = 128
max_range = 65536
max_value_bytes = 262144

[executor]
mailbox = 64                     # Work items one instance's mailbox holds (§5)

[blocks]                         # Pull policy (§4)
require_signed = false           # Refuse an artifact this node cannot verify a signature for
key = "auth/cosign.pub"          # The public key it is verified against; relative to the data dir
```

**Unknown fields MUST be rejected**, at every level, for the reason ABI §11.1 and SERVICE §3 both give: a typo'd knob that silently meant nothing is a node running on a default the operator believes they changed.

**`id` is the node's identity, and `name` is not.** This is SERVICE §2's decision one level up and for the same reason: the Designer's registry (DESIGNER §3) keys a node by something that has to survive a rename and a new DHCP lease, and a name that identified a node would make renaming one a migration. An id is opaque, and a host MUST NOT parse meaning out of it.

**First boot provisions.** A data directory with no `node.toml` is a fresh node, not an error: the daemon creates the directory tree of §2, mints an `id`, and writes a `node.toml` carrying it. `auth/` is created with owner-only permissions (0700 where the platform has them) because it is where SCOPE §3.11's token material lands (§9), and the management API's token is minted into it and printed once — the only time this node will show it (§9.1). Provisioning happens once — a second boot reads the id it wrote and MUST NOT mint another, since an id that changed per boot would identify nothing.

**`listen` defaults to loopback** while transport security is OPEN (SCOPE §3.11). The management API deploys arbitrary WASM to the node, its only gate is a bearer token, and it has no transport security yet; a default that published that to every interface would make "install the package" the exposing act rather than a deliberate one. Making a node reachable is one line in the generated file, and the generated file says so.

**Budgets configured here are the ones that run**, both of them: ABI §10's fuel and deadline bound a guest entry, and EXPR §9's bound each property expression the host evaluates for that guest (ABI §7.1). Neither has an ABI floor (SCOPE §3.4), which is why they are stated in a file rather than compiled in — and why a host with no `node.toml` still has to state them, which is what the defaults above are.

A budget below one of EXPR §9's floors is **raised** to it rather than refused, because a floor is what a conforming expression is entitled to rely on and a host that would not boot over a number the spec is willing to choose for it helps nobody. An operator who writes `max_fuel = 1` gets the floor, and a block written against §9 keeps working.

**`require_signed` defaults to false, and the default is not a recommendation.** It is what a node that has never been given a key can do: signature verification here is key-based (§4), so a node with `require_signed = true` and no key at `[blocks] key` refuses every pull, and defaulting a fresh node into that would make a first boot a configuration error. An operator who puts a key in `auth/` flips one line. The key is a *path* rather than the key itself for the same reason `listen` is an address and not a socket — it is material, it belongs in `auth/`, and a relative path is resolved against the data directory so a node's configuration stays movable with it.

The CBOR decode bound that travels with them is deliberately **not** a knob. EXPR §9 rule 9 makes it a constraint rather than a preference — the decode depth MUST be at least the expression depth, or an expression could construct a value the boundary then refuses to encode — so it is derived from `[budgets.expr] max_depth` and raised to meet it. An operator who could set the two independently could set them into that contradiction, and a file that can express an invalid host is a file a host has to validate rather than read.

## 3. Boot sequence

1. Load `node.toml`, provisioning the data directory if this is a fresh node (§2.1); bind the API listener (§9).
2. For each service file: parse → resolve block refs against cache (pull missing, §4) → validate (manifest/import cross-check per ABI §4.3, capability-vs-node check per SCOPE §3.3, expression static analysis per EXPR §10, connection graph check: ports exist, no dangling refs).
3. Start services marked `autostart = true`. Validation failure of one service MUST NOT prevent the daemon or other services from starting; the failed service surfaces as errored via API.

**One service's failure is that service's.** Step 2 runs per file and every way it can fail is contained: a file that will not parse, a stem that disagrees with its `name` (§2), a block the cache cannot answer for, a connection naming a port no manifest declares, an instance that will not configure or start. Each leaves that service **errored** and the node otherwise untouched — the daemon comes up, sibling services start, and the detail waits for `GET /services/{s}/errors` (§9). A node that refused to boot over one bad file would make every deploy able to take the node down with it, which is the failure mode SCOPE §3.8 keeps configuration on disk to avoid.

**Errored means structured, not stringly.** SERVICE §7 requires a caller to tell its validation classes apart without matching on a message, and boot adds three of its own — unreadable file, stem/`name` mismatch, and an unresolvable block reference (§4) — which are subject to the same rule for the same reason: the Designer renders a boot failure on the offending service, block or connection (DESIGNER §5), which a sentence does not permit.

A service that parses and validates but is not marked `autostart` is **loaded and stopped**, not errored: it is available to `POST /services/{s}/start` without re-reading anything. The three states a service is in after boot are therefore running, stopped, and errored.

## 4. Block manager

- Pulls OCI artifacts (SCOPE §3.6) by reference; verifies digest; verifies a cosign signature when the registry entry carries one, policy knob in `node.toml` (`require_signed = true|false`, §2.1).
- Load-time validation is exactly ABI §4: exports present, imports ⊆ `eio:*`, imports ⊆ manifest capabilities, ABI version accepted, embedded manifest (if present) matches registry manifest.
- Caches wasmtime-precompiled modules keyed by (digest, engine config hash) — cold-start matters on a Pi.
- Airgap/offline: cache is authoritative when the registry is unreachable; a service whose blocks are cached starts fine offline.

**How a reference names a cache entry.** §2's layout is `blocks/<name>/<version>/block.wasm`, and a service file carries a reference (SERVICE §4), so the mapping between them is normative:

```
reference = [ registry "/" ] [ namespace "/" ]... name ":" version
```

`name` is the last `/`-separated component before the tag and `version` is the tag, so `filter:1.2.0` and `ghcr.io/tlugger/filter:1.2.0` name the same cache entry. The registry and namespace are where a *pull* goes (that half of this section) and say nothing about where a pulled block sits, which is what lets a node resolve a service offline against a cache filled from anywhere.

**The tag is REQUIRED.** A reference without one is refused rather than defaulted to `latest`: the cache is keyed by version, and a service pinned to a moving tag would resolve to whatever was pulled last — reproducibility being the thing SCOPE §3.6 versions blocks for. **Digest-pinned references** (`name@sha256:…`) are refused with a distinct error, and remain refused after the pull half below: a digest names an artifact and not a version, so admitting one means deciding what directory it caches into, which is a change to §2's layout rather than an addition to this client.

Resolution is therefore two halves with one seam. The **read** half — reference to cache entry to bytes, and every way that fails — is what boot (§3) needs and is what makes the airgap claim above true. The **pull** half — the registry client, digest verification, signature policy and the precompiled artifact — fills the cache and is where a reference's registry and namespace are finally used.

### 4.1 The pull

**The cache is consulted first, always.** A pull happens on a cache *miss* and never on a hit, which is the whole of the airgap claim: a node whose blocks are cached makes no request, so it cannot be delayed or refused by a registry that is not there. The consequence is that a tag is immutable to a node once pulled — re-pulling one is an explicit operation (§9), not something a boot does behind an operator's back.

**A reference without a registry component cannot be pulled.** `filter:1.2.0` names a cache entry (above) and no host; on a miss it is an error naming the entry, not a request to a guessed registry. There is no implicit `docker.io`, for the reason there is no implicit `latest`. The first `/`-separated component of a reference is its registry when it contains a `.` or a `:`, or is exactly `localhost` — OCI's rule, and the one that makes `tlugger/filter:1.2.0` a namespace rather than a host. The repository is everything between the registry and the tag.

**Scheme: HTTPS, except a loopback registry.** `localhost`, `127.0.0.1` and `[::1]` are reached over HTTP; every other host is HTTPS and a node MUST NOT downgrade. The exception is not a convenience knob — it is the case where there is no network to be on, and it is what lets a registry that exists only for a test be one.

**What is fetched, in order.** `GET /v2/<repository>/manifests/<tag>`; on `401`, the `WWW-Authenticate: Bearer realm=…,service=…,scope=…` challenge is answered at `realm` and the request retried once with the token it returns. The **manifest** MUST be an OCI image manifest (`application/vnd.oci.image.manifest.v1+json`) or its Docker v2 equivalent; an image *index* is refused rather than resolved, because a WASM block is architecture-independent and an index would mean choosing between artifacts that should not differ. Exactly one layer MUST carry the media type `application/wasm`, and that layer's blob is the block. `GET /v2/<repository>/blobs/<digest>` fetches it.

**Digest verification is unconditional.** The layer's `digest` is recomputed over the received bytes and MUST match; the layer's `size` MUST match too, and is also what bounds the read, so a registry cannot answer a small blob with an unbounded stream. Only `sha256` is accepted. This is the verification that makes the rest of the section meaningful: everything below is about *which* artifact, and this is about whether these are its bytes.

**Anonymous pull only.** v1 talks to registries that serve a public repository — including those that answer `401` and mint an anonymous token for one, which is what `ghcr.io` and `docker.io` do. A registry that demands credentials is refused with an error that says so rather than one that says "not found", because the two are different things for an operator to do about. Credentialed access to private registries is §13's expansion item and is tracked as its own work; when it lands, where the credentials live is a decision about the data directory (SCOPE §3.8) and not about this client.

### 4.2 Signatures

Verification is **key-based**: the node holds a public key (`[blocks] key`, §2.1) and verifies a cosign signature made with the matching private key. Keyless (Fulcio/Rekor) verification is deliberately not the v1 posture, and the reason is this section's own airgap rule — keyless verification consults a transparency log *at verify time*, so a node that required it could not verify what it had already cached on a network that is not there. A key is a file, and a file works offline.

A signature is fetched at the tag cosign writes it to: `sha256-<hex>.sig`, where `<hex>` is the digest of the *image manifest* — so a node that has just pulled a manifest knows where to look without asking. That artifact's layer carries media type `application/vnd.dev.cosign.simplesigning.v1+json`; its blob is the signed payload, and the base64 signature is the layer annotation `dev.cosignproject.cosign/signature`. Verification is three checks, all of which MUST pass: the payload's digest matches the layer's, the signature verifies over the payload bytes under the node's key (ECDSA P-256 over SHA-256), and the payload's `critical.image.docker-manifest-digest` equals the manifest digest actually pulled. The third is the one that makes the other two mean anything — without it a valid signature over *some* artifact would authenticate *this* one.

`require_signed = false` (the default) means an unsigned artifact is accepted and a *present* signature that does not verify is still a refusal: a bad signature is evidence, and ignoring it because the policy did not demand one would make the knob a decision about whether to look rather than about what to accept. `require_signed = true` additionally refuses an artifact with no signature, and refuses a pull outright when the node has no key to check one against.

### 4.3 The precompiled artifact

Compiling a block on a Pi costs more than loading it, so a node keeps wasmtime's compiled form: `precompiled/<sha256>.<engine>.cwasm`, where `<sha256>` is the digest of the module's bytes and `<engine>` is a hash of the engine's compilation configuration.

Both halves of that key are load-bearing, and the digest is the module's *content* hash rather than a digest recorded from the pull. They are the same number for a pulled block — an OCI blob digest is the sha256 of the blob — but the content hash also keys a cache entry a human put there by hand, which a recorded pull digest could not, and it cannot go stale against the bytes it was compiled from. A `.cwasm` is therefore never invalidated: bytes that differ name a different file, and the old one is garbage rather than a hazard.

**Its own directory, and not `blocks/<name>/<version>/`.** Nothing in that key is a name or a version — two references that resolved to identical bytes are one compilation, and filing the artifact under one of them would hide that from the other. `blocks/` stays exactly what §4's read half resolves against, and `precompiled/` holds only derived files, which is what makes "delete it and lose nothing but a cold start" a true statement about a directory rather than about scattered files somebody has to identify first.

An artifact that fails to load is **not** an error. It is treated as a miss, the module is compiled, and the artifact is rewritten — the cache is an optimisation and a node that refused to boot over one is a node a corrupt file can take down. Writes are atomic (write a temporary, rename into place) so that a daemon killed mid-write leaves either the old artifact or none.

Loading one is the daemon's only `unsafe`, and the trust boundary is what justifies it: the file lives inside the node's own data directory, was written by this daemon from bytes it had already verified, and wasmtime independently refuses an artifact produced by an incompatible engine build. A node whose data directory is writable by an untrusted party has already lost — that directory holds the service files that say what to run.

## 5. Executor

The runtime embodiment of ABI §1 invariants:

- One wasmtime `Store` + instance per block instance. **One tokio task per block instance** owning the store (stores are `!Sync`; ownership model and serialization requirement align perfectly): the task loops over a bounded mpsc mailbox of work items (`Deliver{port, batch}`, `Timer{id}`, `GpioEdge{..}`, `HttpDone{..}`, `Stop`), invoking guest callbacks strictly sequentially. Serialized invocation falls out of the architecture rather than a lock.
- Fuel **and** epoch interruption per callback (ABI §10); budget from `node.toml`. Trap/exhaustion → instance DEAD → supervision (§8).
- Host functions (`eio:*`) implemented against the mailbox/router: `emit` enqueues to the router (never delivers inline — ABI §6.2); `prop` hits the expression engine with the callback's current-batch context; async capabilities post completions back into the mailbox.

**Placement: one OS thread per instance.** §5.1's store affinity note left this as "a `LocalSet` or a thread each"; it is a thread each, and each such thread runs a current-thread tokio runtime with a `LocalSet` carrying the instance's one task — so the task above is a task, and the thread is what bounds a hostile block's blast radius. The deciding case is ABI §10's spinner: a guest that spins holds its thread until a budget kills it, and on a shared `LocalSet` that is every other instance and the management API held with it. The cost is a thread per instance, which is a daemon-class cost; the leaf tier has its own runtime and is not bound by this choice.

**Where that stops scaling, and what replaces it.** Per instance: one thread, one current-thread runtime, one mailbox; per *runtime*, one shared epoch ticker, not one per instance. At Pi-class density — hundreds of instances, nearly all of them parked in a mailbox read — that is thread memory and nothing else, since a parked thread costs its stack and no scheduler attention. The ceiling is server-class density, thousands of instances on one node, where stack reservation and scheduler churn stop being free.

No such workload has been measured, and this decision stands until one is. It is recorded here so that when a node does hit the ceiling, the revisit starts from the trade-off rather than from scratch — and so that nobody pre-emptively pays for a density nobody has. Two candidate paths, in cost order:

- **Sharded `LocalSet`s** — M threads carrying K instances each. Cheapest change and the smallest departure: the store affinity, the serialization and the mailbox are all unchanged. What it gives up is exactly what the thread bought — a spinner's blast radius becomes its whole shard, for up to the deadline budget, rather than itself.
- **wasmtime async with epoch-yield** (`epoch_deadline_async_yield_and_update`) — a spinner yields rather than holding anything, at the price of a fiber stack per in-flight call. Two consequences make this the more expensive option than it looks: deadline attribution becomes load-dependent, so ABI §10's wall-clock budget stops meaning what an operator promised; and every host function must then not block a poll, which ABI §7.5 rules out for `i2c` up to milliseconds. It also reverses §5.1's decision to compile wasmtime's async machinery *out*, which is part of how "core WASM only" is enforced by absence here.

The choice is confined to this crate. `host-core` and the ABI know nothing of threads, and the leaf runtime has its own executor, so neither the shared driver nor a second host implementation is affected by whichever way it goes.

**Mailbox bound and what a full one means.** The mailbox is bounded and its depth is host configuration, with no floor. The executor offers a sender both answers to a full one and takes neither on the sender's behalf: a *waiting* send (backpressure, which propagates to whoever is producing too fast) and a *refusing* send that hands the work item back. Which one a connection uses is §6's per-connection overflow policy; the cross-device question is OPEN (SCOPE §3.4) and is not settled by the executor having a bound.

**Every sender gone is a stop, and a serviced instance stops on an explicit `Stop`.** A mailbox no sender can reach again cannot receive work, so the instance runs `eio_stop` (ABI §5.1 step 5) rather than idling; an instance is never left running with nothing that can reach it. That is the terminator for an instance with no service around it — the single-block path.

Inside a service it is not, and never was: the service holds a mailbox for every instance it owns, so "every sender gone" cannot become true while the service does. Cycles make the same point about the instances themselves. So a service stops its instances by posting `Stop` to each, and §6's delivery registry — which holds every instance's *current* mailbox so that §8 can replace one — does not change that. The rule a host must not break is the one above: an instance that nothing can reach must stop.

**Inbound is bounded; outbound observation is not.** What an instance produces — callback statuses, `error` details, expression failures, emissions, its death or its clean stop — leaves on an unbounded stream, because an observer that could stall a guest by reading slowly would be a worse defect than a queue that grows. Backpressure belongs on the inbound side, where slowing the sender down is a correct response. Routed emissions are not this stream: they travel through the *destination's* bounded mailbox (§6), which is where a slow consumer should be felt.

**Death is an event, not a log line.** An instance that dies reports the trap and its kind (ABI §5.1 step 6, §10) on that stream; supervision (§8) is its consumer. Until §8 exists it is logged and the thread ends.

### 5.1 Engine binding

`host-core` drives a guest through its `Engine` trait; this is the daemon's only implementation of it, and nothing outside it knows the engine is wasmtime. The leaf runtime writes the equivalent file against WAMR or wasm3, and the driver above both is the same code — that is what makes "divergence between the two hosts is a conformance bug" (ABI §13) enforceable rather than aspirational.

**Feature set.** wasmtime is depended on with `default-features = false` and `cranelift, runtime, std, anyhow, backtrace`. Threads, the component model and GC are therefore *compiled out* rather than switched off, which is a stronger reading of ABI §1's "core WASM only": with the features absent, the corresponding `Config` setters do not exist to be forgotten. `anyhow` gives `wasmtime::Error` its `From` conversion into `anyhow::Error`, so `?` works throughout the daemon — a conversion, not an alias: `anyhow::Context` does not apply to a wasmtime result, so a wasmtime error is given context by converting it first. `backtrace` earns its place because a trap is an instance's death (ABI §8) and the log line is all anyone gets.

**The accepted feature set, and nothing past it.** ABI §4.3 places feature conformance on the engine and nowhere else — `manifest` does no WASM feature gating — so this configuration is the only thing standing between a block using a proposal the leaf tier lacks and a leaf runtime that will refuse it at flash time.

The configuration states it *subtractively*: every proposal wasmtime knows of is disabled, and then exactly the MVP set is re-enabled. Not a list of `wasm_*(false)` calls, for two reasons.

- A list is a closed statement about a moving target. Whatever proposal a later wasmtime enables by default is admitted silently on the next `cargo update`, and blocks using it would run here and be refused by wasm3 — the divergence §1 exists to prevent, arriving through the one door the shared crates do not watch. Subtracting from "everything" refuses it instead, on a host nobody has touched.
- A list is order-sensitive in a way nothing checks. wasmtime rejects a `Config` that disables `simd` while `relaxed_simd` is still enabled, so the setters would have to be called in dependency order; the subtractive form never meets the question.

The base is wasmparser's own `MVP` set, less its `GC_TYPES` flag. That flag gates the `externref`/`anyref` *types* rather than any proposal, and wasmparser folds it into `MVP` only so the wider sets need not repeat it; a wasmtime built without the `gc` cargo feature — this one, per the feature set above — refuses to build an engine at all while it is set. The two decisions agree only once it is removed. `FLOATS` stays enabled: WASM 1.0 has floating point, and so does `expr`.

Added back on top are ABI §4.3's six: bulk memory, sign extension, reference types, multi-value, non-trapping float-to-int and mutable globals. Still refused, among everything else: SIMD and relaxed SIMD, tail calls, multi-memory, memory64, extended const, exceptions, GC, threads, and the component model — the last four by the cargo feature set as well, which is why no `Config` call can restore them.

**What the set costs a block author: nothing.** A stock `cargo build --release --target wasm32-unknown-unknown` produces a conformant module, with no flags and no post-processing. That is a correction rather than a convenience — this section previously said an unadorned Rust block was refused and that `-C target-feature=-bulk-memory` fixed it. Measured on rustc 1.97.1, the flag changes nothing: the `memory.copy` lives in `alloc::string::String::clone` inside the precompiled `rust-std`, which no `RUSTFLAGS` and no `-Z build-std` rebuilds. The restriction it was defending was itself the error (ABI §4.3, SCOPE §3.2). Hand-written `.wat` needs nothing either way.

**And the daemon is not the only host that says so.** `crates/conformance/tests/wasm3.rs` runs the same scenarios, and the same stock-built Rust block, on wasm3 — the leaf-class interpreter. That is what makes §1's "divergence between the two hosts is a conformance bug" a fact about this repository rather than an aspiration, and it is what the feature set above is measured against.

**Host functions reach the engine through the store, not the linker.** wasmtime wants host functions before instantiation and wants each of them `Send + Sync`; `host-core`'s `HostFn` is a boxed `FnMut` over `Rc`-shared state and is neither, because ABI §1.2 gives an instance one caller at a time and nothing needs atomics. So the linker defines the `eio:core` functions once, with ABI §7.0's exact signatures, and each definition captures only a slot index; the real handlers live in the store's data and `register` puts them there. Two consequences worth stating:

- `register` works *after* instantiation, which is the order `host-core` expects — build an instance, register what its capabilities call for, hand the whole thing to the lifecycle driver.
- Import signatures are checked by the engine at link time, which is exactly where ABI §4.3 puts them. The `manifest` cross-check is a superset "in namespaces and names only"; a module importing `eio:core` `log` with the wrong arity fails to instantiate.

A host function reaches guest memory through `Memory::data_and_store_mut`, which yields the bytes and the store's data from one disjoint borrow. The memory borrow ends with the call, which is ABI §9.3 — "host MUST NOT retain guest pointers past the call" — as a lifetime rather than as a rule.

**Export presence is resolved once, at instantiation.** `Engine::has_export` takes `&self` while wasmtime's export lookup needs `&mut Store`, and the answer cannot change for the life of an instance. The exported functions and their result arities are read off the module and kept.

**Trap classification.** Every arm discards the instance — ABI §5.1 offers no state to return to that is not "discard it" — but which death it was is what supervision (§8) and the operator's log need:

|wasmtime|`host-core`|ABI|
|---|---|---|
|`Trap::OutOfFuel`|`TrapKind::Fuel`|§10, execution budget exhausted|
|`Trap::Interrupt`|`TrapKind::Deadline`|§10, wall-clock deadline (epoch interruption)|
|any other `Trap`|`TrapKind::Trap`|§8, a guest trap|
|not a trap|`TrapKind::Engine`|§5.1 step 6, the engine or a host function failed|

**Budgets are armed inside `Engine::call`.** ABI §10's per-callback budget is refreshed by the engine binding rather than by the lifecycle driver, because the driver is `host-core`'s and knows nothing about fuel. `call` is the one place every guest entry passes through, so arming it there is exhaustive by construction — `eio_alloc` included, which is a guest call like any other and just as capable of spinning. Instantiation is armed too: a store with fuel metering enabled starts with none, and module initialisation (ABI §5.1 step 1) is guest code, so an unarmed store kills every block on the way in. Each entry gets the whole budget rather than a share of one: §10 budgets a *callback*, so nothing is banked and nothing is carried over.

**Both budgets, not either.** Fuel bounds *work* and is deterministic — the same block given the same batch dies at the same instruction on every run, which is what makes a fuel death reproducible rather than a thing that happens on a busy machine. It says nothing about the leaf tier, whose watchdog counts something else entirely; ABI §10 does not make budgets comparable across hosts, only mandatory. Epoch interruption bounds *wall-clock time*, which is what an operator actually promised, and it is the only one that sees a callback blocked in a host function, where no fuel is consumed at all. Implementing one would leave half the trap table above unreachable. Epoch interruption needs the epoch advanced by somebody, so the engine owns one ticker thread — per engine, not per instance — holding a weak handle, so that dropping the last engine is what ends it. Its period is the resolution of every deadline: a deadline is rounded up to whole ticks, and the ticker's phase is unrelated to when a guest was entered, so a deadline is enforced within one tick either side of what was asked for.

**Store affinity.** `Store<T>` is `!Send` here, because the handlers and the property context are `Rc`-shared. That is the ABI showing through rather than an accident, and it is what forces §5's placement decision: an instance must be *built* on the thread it will live on, so the executor hands a thread the ingredients rather than a finished instance. Never a work-stealing pool.

## 6. Router

Owns the service graph: the connection table, fan-out (duplicate batch per receiver — nio semantics), and delivery into destination mailboxes.

**The table is ★-shared; the delivery is not.** Which `(instance, output port)` reaches which `(instance, input port)`, the resolution of a service's *names* into the port indices ABI §5.2 fixes, and the duplication of a batch per receiver have no engine and no queue in them, so they live in `host-core` (§1) and the leaf runtime routes with the same code. What is host-specific is what a queue is and what a full one means.

Endpoints are indices, resolved once at build time, because ABI §5.2 makes the descriptor's name lists *be* the numbering and a table carrying names would re-derive it on every emission. Resolution refuses, rather than warns about: a name nothing declares; the error port as a *destination*, since ABI §6.4 makes it an output; and the same connection declared twice, which would deliver one batch twice. Fan-out order is declaration order.

What resolution does *not* check is whether a block declares a port named `err`. ABI §11.1 reserves that name in both directions, so such a manifest is rejected at load and no descriptor carrying one can reach the router. Checking it here as well would be a second statement of a manifest rule, in the crate whose whole purpose is that there is only one.

**`PORT_ERR` is routable and unrouted by default** (ABI §6.4). A service may wire it like any other output; one that does not gets §6.4's "logged and counted" for every error emission, and nothing else — an *ordinary* output nobody wired is an ordinary shape and says nothing.

**Delivery goes through a per-service mailbox registry, not through senders resolved once.** The connection table fixes which `(instance, port)` reaches which; *where* an instance is reachable is a separate question with a changing answer, because §8 restarts an instance in place and a restarted instance has a new mailbox. So the registry holds one slot per instance index and an emitting instance reads its destination's slot at delivery time. Baking the senders into each outlet when the service was built would mean a restarted instance was routed to by nobody — every peer would still name the channel the dead thread took with it, and supervision would restart the block while silently severing it from the graph. The registry is also what makes §5's "every sender gone" not the terminator for a serviced instance.

### 6.1 Where routing happens

**On the emitting instance's own thread, after its callback returned.** ABI §6.2 fixes the *when*; this fixes the *where*, and the two together are what make backpressure real: an instance waiting for room in a full destination is an instance not draining its own mailbox, so the pressure reaches whoever is feeding it. Routing from a central task draining §5's outbound event stream would look equivalent and quietly delete that, because that stream is unbounded on purpose (§5). An emission is therefore reported on the event stream **and** routed, through two different queues, deliberately.

Two consequences worth stating:

- **`eio_start` may emit** (ABI §5.1 step 3), so every mailbox in a service exists before its first instance is spawned. That is also the only order in which a *cyclic* graph can be wired at all.
- **A callback that trapped still has its emissions routed.** `emit` already returned zero, so the host has taken those batches; the guest dying afterwards does not un-take them. The instance is discarded (ABI §5.1 step 6) either way.

### 6.2 Bounded mailboxes and the overflow policy

**Bounded mailboxes; overflow policy per connection.** The default is to **block the emitter's queue-drain** — natural backpressure within a node. **Drop-oldest** is available as an opt-in for sensor-style flows. The cross-*device* question — delivery guarantees, ordering, and backpressure between nodes — is a different one and stays OPEN (SCOPE §3.4).

The two policies are the two answers §5's mailbox offers a sender, and neither is free-standing:

- **Backpressure** is the waiting send. Nothing is lost; a saturated graph slows down.
- **Drop-oldest** is the refusing send plus a **one-batch slot on the connection**. When the destination is full the newest batch takes the slot, and the batch it finds there is the one dropped; the slot is retried ahead of the next round of emissions. The batch a connection discards is always one of *its own*: a per-connection policy MUST NOT discard work another connection put in the shared mailbox, so a control flow set to backpressure keeps its guarantee even when a sensor flow into the same block does not.

**A connection whose destination is its own source never waits**, however it is configured. An instance is the only drain of its own mailbox, so waiting there cannot succeed — it is a deadlock rather than backpressure, and the batch is discarded and counted instead. Longer cycles are not locally detectable: a saturated cycle of two or more instances stalls those instances, which is the cost of in-node backpressure and is stated here rather than papered over. Every discard — unrouted error emission, drop-oldest replacement, full self-connection, gone receiver — is logged and counted.

### 6.3 Taps and system blocks

**Taps** (SCOPE §3.12): any connection can be tapped at runtime through the API (§9), which streams what travels along it. Expression evaluation failures (EXPR §8) are injected into the same stream as annotated events, because a property that failed for a signal is the most useful thing a tap can show and it is invisible in the batch.

**A tap observes the source endpoint, not the wire.** A connection is `from.port -> to.port`, and what travels it is exactly what `from` emitted on `port` — fan-out hands every destination an independent copy of one batch (ABI §6.2). So a tap resolves its connection to that endpoint and observes there. Two connections leaving one output port are therefore indistinguishable to a tap, which is correct rather than a limitation: they carry the same batch, and a tap that claimed otherwise would be inventing a difference. What *is* per-connection is a discard (§6.2), and that carries its own destination.

**Zero cost untapped, and precisely what that means.** An instance already reports what it emitted, what a callback returned and which expressions failed, on the event stream §11 drains; a tap subscribes to that stream rather than instrumenting the router. So a connection nobody is watching costs one atomic load — the check for whether anything is subscribed at all — and no copy, no allocation and no lock. Nothing in the emit or delivery path is conditional on a tap existing, which is the property worth having: tapping cannot change what a service does, only what an operator can see.
- **System blocks (PROPOSED):** `publisher` and `subscriber` blocks are **host-native**, not WASM — they appear in the palette/manifest system like any block but their implementation is the router's pub/sub bridge (§7). Rationale: they need transport internals and credentials no sandboxed block should hold; and every node class must have them even when it can't load WASM dynamically. The precedent is deliberate and narrow: system blocks are limited to transport endpoints (logger stays an ordinary WASM block).

## 7. Pub/sub bridge

Transport is OPEN (SCOPE §3.9). The bridge is the isolation layer that keeps it that way: a small trait (`publish(topic, batch)`, `subscribe(topic) -> stream`, connection lifecycle) implemented per transport candidate (MQTT first — **PROPOSED** rumqttc behind the trait), so the transport decision stays swappable until cross-node work forces it. Topic naming convention, QoS mapping, and retained-message posture are part of that later decision, not this spec.

Policy is OPEN (SCOPE §3.13); the daemon ships the _mechanism_: per-instance restart with exponential backoff and a restart-count circuit breaker escalating to service-errored. Re-instantiation = fresh `eio_configure` (ABI §5.1); durable state via `eio:state` only. **PROPOSED default policy:** restart-instance up to N times per window, then stop service and surface. Callback error returns (ABI §8) are counted/logged, never restart-triggering.

**Restarting one instance leaves the graph intact.** The old instance is stopped and joined before the new one is built, so no two lives of a block ever answer the same connections — ABI §1.2 admits one caller, not one per life. The replacement's mailbox is installed in §6's registry *before* it is spawned, for the same reason a service's mailboxes all exist before any of its instances do: a peer emitting during the gap queues its batch instead of finding a closed channel. Because every outlet reads the registry per delivery, no peer is rebuilt and none is consulted. The descriptor is unchanged, so the connection table resolved against it still describes the instance; a restart that renumbered a port would have rewired the service behind its own back.

**A restart re-instantiates, it does not recompile.** The service keeps the compiled module each instance was built from. That is not the block's bytes: compiled code is already resident for as long as any instance of it is, so a retained handle costs a refcount, where keeping the `.wasm` would mean every instance paying for its whole life for the moment it was compiled. Where the module comes from on a *cold* start is §4's, not this section's.

**Work the old instance had queued is gone with it.** That is what a restart is: the replacement did not run those callbacks and must not be told it did. Anything that had to survive was written through `eio:state` (ABI §7.2), which is the only continuity ABI §5.1 offers across lives.

## 9. Management API (SCOPE §3.10)

REST/JSON, with an OpenAPI document generated from the handlers and served at `/openapi.json`. **The document is the product, not a by-product.** SCOPE §4 makes an agent a peer client of the Designer, and §3.10 makes this spec its tool surface directly — so an operation's description is user-facing documentation, and an endpoint that appears in the document is a promise that it works. That is why generation is from the code rather than beside it, and why §9.5 requires the two to be checked against each other.

```
GET    /openapi.json                  this document (no auth)
GET    /node                          identity, limits, budgets, versions
GET    /blocks                        cached blocks + manifests
POST   /blocks/pull                   {reference} -> pull into the cache (§4.1)
GET    /services                      every service and its state
GET    /services/{s}                  definition text + state
PUT    /services/{s}                  write definition (validate first, §9.3)
GET    /services/{s}/errors           why a service is errored, structured
POST   /services/{s}/start            load from file and start
POST   /services/{s}/stop             stop, keep the definition
POST   /services/{s}/reload           re-read the file and apply it (§9.4)
GET    /services/{s}/state/{i}        what instance {i} has in eio:state (§10)
POST   /taps                          {service, connection} -> tap_id (§9.6)
GET    /taps                          the taps this node is holding
GET    /taps/{id}/stream              SSE: signals and expr failures (§9.6)
DELETE /taps/{id}                     stop tapping, release the ring
GET    /logs/stream                   SSE: log lines, filterable (§9.6, §11)
```

**State inspection is service-scoped, and that is not cosmetic.** This section sketched the endpoint as `GET /state/{instance}` while §10 had no store behind it; the store made the path impossible. SERVICE §2: "ids are unique within a service file and mean nothing outside it. Two services on one node may both contain `b7k2`, and they are not related." A node's store is keyed `(service, instance)` for exactly that reason (§10), so the endpoint carries both — and it joins the service-scoped family the rest of the `/services/{s}/…` operations already form.

It answers what the instance would read back: the same store, the same namespace, no cache in between (§2). Keys and values are opaque to ABI §7.2, so both are reported as bytes, with a UTF-8 key and a canonically rendered value (EXPR §7.6) offered *beside* them where the bytes admit one and omitted where they do not — a block storing something this daemon cannot decode is doing nothing wrong, and hiding such an entry would hide the state of exactly the block worth looking at. An instance the service declares and that has written nothing answers **no entries**, which is the same answer §7.2 gives for an absent key; an id the service does not declare is `404`. The instance need not be running: state outlives an instance, which is the whole of what it is for (ABI §5.1's "restart = new instance").

### 9.1 Auth

A per-node bearer token (SCOPE §3.11), minted on first boot into `auth/token` (§2) with owner-only permissions and printed once, at the boot that mints it. Every endpoint requires `Authorization: Bearer <token>` and answers `401` without it, with one exception: **`/openapi.json` is unauthenticated**, because it is a schema and holds nothing about this node that is not already public in this specification — and because a tool surface a client must already be authorized to discover is one nobody can bootstrap against.

Comparison is constant-time. Transport security stays OPEN (SCOPE §3.11) and this section does not resolve it; what it must not do is make the token weaker than the transport will eventually be.

### 9.2 The error envelope

Every failure — auth, routing, validation, a refused pull — answers the same JSON object, and no endpoint invents its own shape:

```json
{ "error": "unresolvable_block",
  "message": "block `ghcr.io/x/filter:1` of instance t1: ...",
  "detail": { }  }
```

`error` is a stable machine-readable slug and is what a client branches on; `message` is one sentence for a person and MUST NOT be parsed; `detail` is per-slug structured data, absent when there is none. The slug is the contract: SERVICE §7 already requires a caller to tell validation classes apart without matching on a message, and an API that collapsed them into prose would put that back. Renaming a slug is a breaking change to this API.

### 9.3 `PUT /services/{s}` validates before it writes

The body is the service file's text. The daemon validates it exactly as boot does — SERVICE §7 stage 1, then block resolution (§4, which MAY pull), then stage 2 — and **only then** writes it. A definition that fails validation answers `422` with the report in `detail` and **changes nothing**: not the file, not the running service.

This is the one place the API and the GitOps path (§2) deliberately differ, and the difference is only in the write. Editing a file by hand and calling `reload` can leave a service errored, because the edit already happened and the daemon is being told about it afterwards; a `PUT` is the daemon being asked to make the edit, and an operator asking for something invalid is told no rather than having a running service stopped on the strength of a typo. Both paths run the same validation and produce the same report, which is what makes them the same feature.

**The stem is the name.** `PUT /services/kitchen` whose body declares `name = "other"` is refused by SERVICE §1's rule, not silently filed under either — the path and the body disagree about what is being written, and guessing which one meant it is how a deploy lands somewhere nobody looked.

**The daemon writes the bytes it was given, and no others.** SERVICE §2's "a host MUST NOT write to a service file" is about *authoring*: minting an id, normalizing formatting, rewriting what a human or a git checkout put there. Storing a definition a client composed is not that, and the rule is preserved by the daemon never editing the text — no reformatting, no key reordering, no id insertion. Structural edits that preserve the rest of a file round-trip are the CLI's and the Designer's (eieio-8yq.8), which is precisely so that this endpoint stays a byte sink.

### 9.4 Reload applies the file, including its `autostart`

`reload` re-reads the file and brings the service to the state the file describes — so a service the file marks `autostart = false` ends **stopped**, even if it was running because somebody called `start`. The file is the source of truth (SCOPE §3.8), and a reload that preserved a runtime override would mean the file was not the answer to what the node is running.

`start` is therefore the deliberate override and `reload` is the deliberate revert. `start` re-reads the file too — nothing is cached between calls (§3) — and starts the service whatever its `autostart` says, because a caller naming the operation has said what they want more recently than the file has.

### 9.5 The document and the router are checked against each other

A conforming implementation MUST test that every route it serves is described in `/openapi.json` and that every path in the document is served. Enumerated from the router, not from a hand-maintained list: a list is a third place to forget an endpoint, and the failure this rule exists to prevent — a tool surface that promises what the daemon does not do, or hides what it does — is invisible in every other test.

### 9.6 Streaming: SSE, and what a stream promises

**Server-sent events, not WebSocket.** A tap and a log stream are one-way, which is the whole of what SSE does; it is curl-able, it is `EventSource` in a browser with no library, and reconnection with an event id is in the protocol rather than in every client. A bidirectional socket would buy adjusting a live tap's filter without re-creating it, which is not worth a protocol the Designer has to hand-roll reconnection for. Streams answer `text/event-stream` and are authenticated like every other endpoint (§9.1).

**Event names are the contract**, and a client dispatches on them: `signals` for a batch that travelled the tapped connection, `expr_failure` for a property expression that failed for a signal (EXPR §8: code, span and message), `discarded` for a batch that was routed and not delivered (§6.2), `lagged` for the paragraph below, and `log` on `/logs/stream`. A name not in that list is a name a client MAY ignore; adding one is not a breaking change, and changing what one means is.

**The ring buffer is bounded, and a slow reader is told exactly what it missed.** A tap holds a fixed number of observations for a client that is not keeping up, and the oldest go first — an operator watching a firehose through a browser should see recent signals, not a stalled node. What a tap MUST NOT do is skip silently: a debugging tool that quietly shows a subset is worse than one that shows less and says so. So a client that falls behind receives a `lagged` event carrying the exact number of observations it did not see, before the stream resumes. **That count is the sampling report**, and it is why "sampled" here needs no rate knob: the stream is complete until a reader cannot keep up, and precisely quantified from then on.

**Teardown is either explicit or a disconnect.** `DELETE /taps/{id}` removes a tap; so does the client going away, detected when the stream's send fails. A tap holds a subscription and a ring and nothing else, so releasing it releases everything — there is no separate reclaim, and a tap that outlived its reader would be a leak of exactly the kind §11's drain exists to prevent.

## 10. State store

Backs `eio:state` (ABI §7.2), namespaced `service/instance/key`. **redb**: pure-Rust embedded KV, one file at `state/state.redb`, no compaction daemon, and — measured — **no dependencies of its own** (`cargo tree` is a single node). That is what decided it against sled, measured the same way at 17 crates including `libc`, `parking_lot` and `crossbeam`, plus a background flush thread it configures by default; and against anything carrying a C library, which the arm release build would pay for. A node runs on a Pi with an SD card, and a store needing a maintenance daemon to stay healthy is operational burden a stream processor should not add.

**The trait boundary is `host-core`'s, and it is exactly ABI §7.2's three functions.** `StateStore` is `get`/`put`/`del` over opaque bytes, already scoped to one instance; the three host functions that decode `(key, key_len, buf, cap)` and apply §8's size convention are in `host-core` beside it, so the daemon, the reference conformance harness and the leaf runtime answer `state_get` with the *same* code. A leaf host implements the trait against flash and may answer `ERR_THROTTLED` for a wear budget (§7.2); the daemon never does, and the variant is plumbed anyway so that a block's back-off branch is the same code on both hosts.

**Namespacing is `(service, instance)`, and the composition is the store's, not the guest's.** A block writes `count`; the host makes that `(service, instance, "count")`. One table, one composite key, and redb orders tuples element-wise — so one instance's namespace is a contiguous range, which is what makes §9's inspection endpoint a scan. A per-instance handle holds its two components and cannot be talked out of them, so a cross-instance read is unconstructible rather than checked.

ABI §7.2 describes the scoping as `system/service/instance`. A node implements the part of it a node can know: **the system is not a key component**, because a node does not know its System — SCOPE §3.8 keeps Systems in the Designer's database and `node.toml` has no such field, deliberately, since a node must be usable with no Designer anywhere near it. One node belongs to one System, so the component would be a constant prefix on every key, which is padding and not namespacing.

**Durability is durable-on-return.** ABI §7.2 leaves the posture to the host; this host commits before `state_put` answers (redb's `Durability::Immediate`), because the property ABI §13.2's stateful counter exists to prove is that a count survives a restart, and a store that only usually survives one passes every test that does not pull the plug. Two consequences are stated rather than worked around:

- **The fsync is inside the guest's callback.** `state_put` is synchronous (§7.2 gives `eio:state` no completion callback), so the commit spends the callback's ABI §10 wall-clock deadline. A block writing on every signal faster than its deadline can absorb wants a larger deadline or fewer writes. The alternatives are worse: a background flush would make "durable" mean "probably", and an async commit would make `eio:state` a callback-shaped capability the ABI says it is not.
- **Writers serialize.** redb admits one write transaction at a time, so two instances putting concurrently queue — each on its own thread (§5), never on the reactor.

**Nothing garbage-collects a namespace.** An instance removed from a service file, or a service deleted, leaves its keys where they are. That is the safe default — a deploy that renamed an id would otherwise silently discard state a block is about to want, and state is the one thing on a node that cannot be rebuilt from a file — and reclaiming it deliberately is a management operation this specification does not yet have.

`dev run-block` (§12) gets the same store over an in-memory backend: the same table, the same keys, the same transactions, nothing persisted. One implementation, because a second one would be a second answer to what `eio:state` does — and the fast loop would be exercising it instead of the real one.

## 11. Observability

Structured logs through `tracing`: the daemon's own subsystems, and guest `log` calls (ABI §7.0) tagged with `(service, instance)` from the span the lifecycle driver has entered — so a block's line and the daemon's carry the same identity without the guest knowing either. `/logs/stream` (§9) is that stream, filtered.

**The observation bus is the one drain, and it has to exist.** Each instance reports what it observed on an unbounded channel (§5) — unbounded because an observer that could stall a guest by reading slowly would be a worse defect than a queue that grows. Unbounded means *something must read it*: a node that held those receivers and never drained them would accumulate every status, emission and expression failure for the life of the process. So a node drains each instance's stream into a per-node bus, and taps and `/logs/stream` subscribe to that. A node with no subscribers still drains; it just drops.

The bus is where the raw stream's one owner lives, and which owner that is depends on who is running the instance: the bus in a node, `dev run-block` in a `dev` command, the test in a test. That is why the executor hands the receiver out rather than choosing.

Metrics stay OPEN (SCOPE §3.12). `/metrics` is **reserved** and deliberately **not served** — the same rule §9 applies to `/state`: an endpoint published in the tool surface that cannot succeed is worse for an agent than one that is absent. The counters worth having when it lands are delivered and emitted batches per connection, callback duration, instance restarts and expression failures.

## 12. `dev` commands

Commands for block authors, under a `dev` subcommand so that the top-level verbs stay the node's. They operate on a `.wasm` file directly and have no service, no persistence and no API behind them; that is what makes them useful for a block that is not deployable yet, and it is why they are not a way to run a node.

They are a separate thing from the conformance harness (ABI §13.1), which lives in `conformance/`, injects faults, and is run by CI rather than by a person.

```
eio-daemon dev run-block <WASM> [--manifest PATH] [--prop NAME=EXPR]... 
                                [--batch JSON | --batch-file PATH]
                                [--input-port N] [--instance ID] [--service NAME]
                                [--max-payload BYTES] [--max-batch SIGNALS]
```

`run-block` performs ABI §4 load-time validation, resolves the property table per ABI §11.1, instantiates, then walks ABI §5.1 once: `eio_configure`, `eio_start`, one optional `eio_process_signals`, `eio_stop`. Emitted batches are printed rather than routed — there is no graph to route into — using EXPR §7.6's canonical rendering, because a second way of rendering a value is a second definition of what one is.

`--max-payload` and `--max-batch` are stated rather than defaulted-into-invisibility: ABI §9.7 gives them no floor (SCOPE §3.4 OPEN), so the command has values a block can read from its descriptor and a deployer can change.

**The JSON batch mapping.** `--batch` is a debug input, **not** a wire format: the batch encoding is canonical CBOR and nothing else (ABI §6.3.1). The mapping exists so that trying a block does not require producing a `.cbor` file by hand, and it is deliberately one-way. Three things do not survive it, and all three are the JSON data model being smaller than ABI §6.3's:

- Byte strings have no JSON spelling, so a batch containing one cannot be written this way.
- Int and float are told apart *lexically*: `1` is an int, `1.0` and `1e0` are floats. An integer literal between `i64::MAX` and `u64::MAX` is refused rather than rounded into a float; beyond `u64::MAX` the JSON reader has already made it a float and nothing survives to act on.
- Duplicate object keys collapse rather than being rejected as §6.3.1 rule 7 requires, because the JSON parser resolves them first.

NaN and infinity need no rule here: a literal that overflows `binary64` is refused while parsing.

**Logging.** Every line a run produces — the daemon's own and the guest's `log` calls alike — is emitted inside a span carrying `(service, instance)` per §11. `run-block` has no service, so `--service` supplies the name, defaulting to `dev`.

**Capabilities.** A block whose manifest declares a capability the host does not implement is refused at load, by name. That is SCOPE §3.3's deploy-time question asked where a deployer can act on it; the engine's own answer would name a missing symbol rather than the capability that asked for it.

`eio:state` is implemented (§10) and backed **in memory** for the run, which the command says on its way in rather than leaving to be discovered: a `dev` command has no data directory to keep a store in, so a stateful block round-trips its keys within one run and starts from nothing on the next. Persistence across runs is what a node is for.

## 13. Expansion list (for the in-depth pass)

Per-subsystem deep specs needed: router semantics under reload (in-flight signal disposition), OCI auth for private registries (§4.1 states the anonymous-only v1 posture this replaces), mailbox sizing defaults, multi-arch AOT artifact selection.
