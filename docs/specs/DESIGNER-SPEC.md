# Designer Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion. **Depends on:** SCOPE.md (§2, §3.8, §3.10–3.12, §4), DAEMON-SPEC.md §9 (the API it consumes), ABI-SPEC.md §11 (manifests drive its UI), EXPR-SPEC.md. **Markers:** **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

The Designer is the optional visual management surface: create Systems, attach nodes, design services on a canvas, configure blocks, deploy, start/stop, and inspect running signal flow. Two constraints define its architecture more than any feature does:

1. **Never the system of record** (SCOPE §3.8). Daemons own their config as files; the Designer's DB holds only what daemons _can't_ know: System groupings, node connection info, registry sources. If the Designer's DB is lost, no System loses anything but its address book.
2. **Peer client, not privileged client** (SCOPE §4). Everything the Designer does goes through the same daemon API an agent or CLI uses. Any Designer-only capability is an architecture bug.

---

## 1. Stack

Settled and built (`eieio-m9s.1`); §1.1 records why it changed from SvelteKit.

- **Frontend: Vite + Svelte 5, as a single-page app.** Not SvelteKit, and not Next.js.
- **Backend: `crates/designer` — an `axum` binary in this workspace** (package `eio-designer`), serving the built SPA out of the binary via `rust-embed` behind `tower-http`'s `ServeDir`.
- **Storage: `rusqlite` with the `bundled` feature**, migrations via `rusqlite_migration`. Registry-scale data only (§2); zero-ops matches the self-hosted posture (SCOPE §6).
- **Canvas: `@xyflow/svelte`** (Svelte Flow). A custom canvas engine is explicitly rejected scope for v1 (§9).
- **Shared Rust in the browser:** the `expr` crate compiled to WASM powers in-editor expression linting — parse errors with spans, unbound symbols, signal-dependence badge (EXPR §10 semantics, the *same interpreter code* the daemon runs). This is the payoff of the `no_std` crate split (DAEMON §1) landing in the UI.
- Ships as a container image and a bare binary; localhost-first.

### 1.1 Why the server is Rust and not the frontend framework

Earlier drafts named SvelteKit, with "Next.js acceptable substitute — decide at build time, nothing below depends on the choice". Both halves of that turned out to be wrong, and the reasons are recorded because they are the kind that get re-litigated:

- **Next.js cannot be substituted**, because §1's in-browser `expr` is not negotiable. `wasm-pack`'s ESM output has failed to load under Next since 2021; every community workaround is a `webpack:` config hook, and Next 16 made Turbopack the default, whose `wasm-bindgen` support is undocumented. A framework that cannot reliably load the interpreter is not a substitute for one that can.
- **Something below did depend on the choice** — the resource floor, which SCOPE §6's self-hosted posture and §3.7's Pi-class targets both care about. Measured, under load, one core: **axum 8.5 MB resident against 82.5 MB for Node**; Next.js's own issue tracker documents a ~95 MB `output: standalone` baseline. A bare install is a static binary of a few megabytes with no Node, no Bun and no Deno on the machine at all.
- **The decisive reason is §4, not the megabytes.** §4 requires that the Designer's idea of what a service file may say cannot differ from the CLI's. A Rust backend does not *reproduce* that guarantee, it *links* it: `eio-service`'s preserving `toml_edit` writer, `eio-manifest` and `eio-expr` are function calls, not a second implementation to keep in agreement. The daemon already depends on `axum` 0.8 and already re-streams SSE through `axum::response::sse::Sse`, so §3's proxy is the code this repository has, used again.

**What this costs, stated plainly:** the session gate, the routing and the typed server↔client boundary are hand-written rather than given by a full-stack framework, and Rust and TypeScript types can drift where a single-language stack could not. There is no SSR — irrelevant for a localhost canvas, and the thing to revisit if that ever stops being what this is. **Multi-user is the inversion to watch**: sessions and RBAC are where a full-stack framework earns its keep, and SCOPE §6 excludes them today.

**Svelte Flow is a peer binding, not a port**, which is why the canvas did not decide the framework. `@xyflow/svelte` and `@xyflow/react` publish together over one shared `@xyflow/system` engine — pan/zoom, drag, connection validation and all edge-path maths live there, not in either binding. Every §5 and §6 requirement maps to a documented API: multiple typed `Handle`s per node for the output ports and ABI §6.4's reserved error terminal, `isValidConnection`, fan-out as default behaviour, `onedgeclick` with `interactionWidth` for click-to-tap, `EdgeLabel` for the throughput badge. `toObject()` returns `{nodes, edges, viewport}`, which is nearly the `[ui]` table already. If the Svelte binding ever stalls behind the shared engine, the swap is to the React binding and an SPA in React; the Rust backend is untouched, which is the point of putting the seam at HTTP.

## 2. Backend data model

```
systems        (id, name)
nodes          (id, system_id, name, class: daemon|leaf, address, auth_token,
                ca_material?, last_seen, capabilities_cache, limits_cache)
registries     (id, url, auth?)            block registry sources
manifest_cache (block_ref, manifest_json, fetched_at)
```

Notably absent: services, blocks-in-services, connections, layout — all of that lives in service definition files on nodes. The Designer reads them through the daemon API, edits them, and writes them back.

**`nodes` duplicates the CLI's `~/.config/eieio/nodes.toml`, and that is deliberate.** Both hold an address and a token per node, and neither is the other's cache. The CLI's file is a local operator's own credentials, on the machine they are sitting at, deliberately never inside a working tree (SCOPE §5.1); this table belongs to a *service* that may run in a container on another host and, per §3, is expected to reach nodes the operator's laptop cannot. Sharing one file would mean either putting the browser-facing process's writes into an operator's dotfile or giving the CLI a Designer to depend on — and `eio` needing nothing running is what makes it the tool that bootstraps a node before any Designer exists. What they MUST NOT do is disagree about what a node is called: node names are the operator's, and a node registered in both is the same node under the same name. Moving a set between them is a CLI export/import, not a sync protocol — there is no reconciliation here and none is wanted.

## 3. Backend responsibilities

- **Proxy, not peer-to-daemon-from-browser**: all daemon API calls route browser → Designer backend → daemon. Rationale: node tokens never reach the browser, CORS/TLS mess stays server-side, and mixed-reachability networks (Designer can reach nodes the operator's laptop can't) work. Streams (taps, logs) are re-streamed over the same hop.
- Node registration: address + token (+ CA material when SCOPE §3.11 resolves), health polling → `last_seen`, capability/limit discovery via `GET /node` cached for deploy-time validation.
- Registry browsing: query block registries, cache manifests (the palette's data source).
- **Designer auth itself is v1-minimal**: single-operator assumption (SCOPE §6 — no multi-tenancy); a single login/token gate on the app. Nothing fancier until someone needs it.

### 3.1 The Designer's own HTTP surface (normative)

Two kinds of endpoint, and the split is the whole design. Everything the Designer *itself* knows is a small REST surface; everything a **node** knows is reached by proxy and is never re-modelled here.

**This surface is a generated document too, for the reason DAEMON §9 gives about its own: "the document is the product, not a by-product."** `crates/designer` carried no schema generation at all until eieio-m9s.20, and the consequence was not hypothetical — the SPA hand-writes a TypeScript type for every body it reads, the parity check that holds those types against the daemon's live schemas could not reach this half, and three fields had already drifted: `id` and `system_id` were declared as strings against a server that serves integers, and `capabilities`/`limits` were declared required against a server that omits them until a probe succeeds. A table of field *names* in a specification, which is all this section used to be, does not catch any of that.

So: **`GET /api/openapi.json` serves this surface's generated document, unauthenticated**, on the same reasoning DAEMON §9.1 gives for the node's — it is a schema, it holds nothing a reader could not find in this specification, and a tool surface a client must already be authorized to discover is one nobody can bootstrap against. Every response type below is generated from the handler, not restated beside it.

**Two things the table below is now explicit about, because leaving them to a reader is what drifted.** An `id` and a `system_id` are **integers**: they are SQLite rowids, and the store is what mints them (§3). And `last_seen`, `capabilities` and `limits` are **absent until a probe succeeds** — a node the Designer has recorded but never reached has neither, and an absent field is the answer rather than an empty object, the same rule DAEMON §9.6 and ABI §11 keep everywhere else.

```
POST   /api/session                       { password } -> session cookie
DELETE /api/session

GET    /api/systems                       [{ id: int, name }]
POST   /api/systems                       { name }
DELETE /api/systems/{id}

GET    /api/openapi.json                  this document (no auth)

GET    /api/nodes                         [{ id: int, system_id: int, name, class,
                                             address, last_seen?, capabilities?,
                                             limits? }]
POST   /api/nodes                         { system_id, name, address, token,
                                            class? }   default "daemon"
DELETE /api/nodes/{id}
POST   /api/nodes/{id}/probe              refresh last_seen + capabilities via GET /node

GET    /api/registries                    [{ id, url }]
POST   /api/registries                    { url, auth? }
GET    /api/blocks                        the manifest cache (the palette's data source),
                                          each entry carrying the `block_ref` it
                                          was fetched for (§2)
PUT    /api/blocks/{reference}             cache one manifest the browser fetched
                                          through the proxy (§3.3)
DELETE /api/blocks/{reference}             forget one

ANY    /api/nodes/{id}/daemon/{*path}     proxied to that node, verbatim
```

**A node's token never appears in a response.** It is write-only: supplied on `POST /api/nodes`, stored, and thereafter only ever attached to an outbound proxied request. The `nodes` representation above has no `token` field at all, which is stronger than omitting it per-handler — there is no serialization in which it can appear.

**The proxy is one catch-all, not a re-modelling of DAEMON §9.** `/api/nodes/{id}/daemon/{*path}` forwards method, path, query and body to that node's address, attaches its bearer token, and streams the response back — `text/event-stream` included, unbuffered, so §6's taps and logs are the same hop. A per-endpoint proxy would be DAEMON §9's table written a third time (after the daemon and the CLI), free to drift from both; a catch-all cannot drift, because it knows nothing about what it is forwarding. This is also what keeps §8's parity rule true by construction: the browser reaches exactly the operations a node serves, no more and no fewer.

**A node's `class` is stated, not discovered, and it is the only field that could not be.** Everything else about a node comes back from a probe; a **leaf** answers no probe, because it serves no management API at all — its services are compiled into firmware (SCOPE §3.7, §7). So the class has to be told to the registry, and having been told, two things follow: `POST /api/nodes/{id}/probe` and the proxy both **refuse a leaf by name** rather than dialling it. A leaf's address reached over HTTP produces a connection error indistinguishable from a node that is down, which would report a fault against a node working exactly as designed — and would make `last_seen` mean two different things depending on class.

### 3.3 The cache is written by the browser, not by a second proxy

`manifest_cache` is filled by the browser: it fetches a manifest from a node through the catch-all proxy (`…/daemon/blocks/available/{reference}`, DAEMON §9.8) and `PUT`s what it got here. The Designer stores it and does not go and check.

**Why not a server-side `POST /api/nodes/{id}/blocks/browse`**, which is the obvious shape and was written and then reverted during implementation: it is a *per-endpoint proxy*, the thing §3.1 rejects by name. One of them is not DAEMON §9's table written a third time — but it is the first row of it, and the second is always easier to justify than the first. **The rule that keeps this honest is absolute rather than proportionate: the Designer backend reaches a node through the catch-all and through nothing else.** A rule with one exception has no edge anyone can check.

**What the browser sends is therefore trusted, and that is acceptable here for a stated reason.** §3.1 already establishes that the browser is the operator — the proxy is not an authorization layer, it exists to keep the token server-side and to solve mixed reachability. An operator who poisons their own palette cache sees wrong blocks in their own palette; nothing downstream believes it, because installing is `POST /blocks/pull` on the node, which re-fetches and re-verifies (DAEMON §4.1, §4.2) and has never heard of this cache. **This holds only while there is one operator** (SCOPE §6). A second one makes a poisoned cache someone else's problem, and this endpoint is where that would first bite.

**The Designer never speaks OCI.** `manifest_cache` is filled from a node, through the proxy: DAEMON §9.8's `/blocks/available` answers what a node could install, and this cache is a cache of *that* answer. The Designer holding its own registry client would be a third implementation of the OCI wire format, and worse, a *different view* — a node holds the registry credentials and enforces the signature policy, so a Designer browsing independently could offer a block that node would refuse to pull. The palette is therefore per node by construction, which is what it always was: two nodes with different registries configured offer different blocks.

**What makes an entry stale is whether its reference can move, not how old it is** (eieio-m9s.22). `manifest_cache` carries `fetched_at` (§2) and it is deliberately not the rule: time is wrong in both directions, because a mutable tag can drift a second after it is fetched and a digest-pinned reference never drifts at all. So:

- **A reference pinned by digest is never stale.** `…@sha256:…` names content, and content does not change. No revalidation, ever — and this is the case an operator should be steered toward, because it is the only one where the palette can be trusted offline.
- **A reference with a mutable tag is *unverified* the moment it is stored**, and the Designer says so rather than implying freshness. It is still shown — the palette must work against the cache, which is the whole reason the cache exists — but a reader that is about to *act* on it revalidates first against the node, through the catch-all proxy, the same way the entry was fetched.

**Revalidation is the browser's, for §3.3's own reason**: the backend reaches a node through the catch-all and through nothing else, so a server-side freshness check would be the per-endpoint proxy this section rejects. The browser compares what the node now reports (DAEMON §9's `/blocks` carries each cached block's digest) against what it stored, and re-`PUT`s on a change. Installing a block (`POST /blocks/pull`) invalidates that reference's entry, because the node has just re-fetched and re-verified it and its answer is now the better one.

**"About to act on it" is the distinction that matters, and it is not the palette.** Rendering a name in a list is a display. Rendering a block's *ports and properties* in the config modal, checking its capabilities against a node before a deploy, and resolving an expression failure's `prop` index to a property name (§6) are all claims about a block that is *running or about to run*. A stale manifest there shows an operator fields the deployed block does not have — and one of those three already carries a defensive guard written precisely because this hole was known (eieio-m9s.14's resolver falls back to the bare index rather than a confidently wrong name). That guard should become unnecessary, not permanent.

**A block is identified by its whole reference, never by its name.** `manifest_cache` is keyed by `block_ref` (§2), and a service file's `block` field is matched against that key verbatim — no parsing, no stripping of registry or tag. A manifest's own `name` does not identify it: two registries may publish `temp-sensor`, two versions of `filter` may declare different ports and properties (ABI §11.1), and a reference naming a registry with a port does not even split on its first colon. Every one of those failures presents identically — a block rendered with another block's ports, properties and capability requirements — so the rule is exact match, and the cache is asked for what was actually pulled.

**The browser is the operator, so the proxy does not restrict which daemon operations it may reach.** The proxy exists to keep the token server-side and to solve mixed reachability (§3), not to be an authorization layer — v1 has one operator (SCOPE §6), and a second one is where this needs revisiting.

### 3.2 Editing a service file is a stateless transform (normative)

```
POST   /api/service-edit    { toml, operations: [ … ] } -> { toml } | 422 { errors }
```

The Designer holds no service file. The browser `GET`s the text from a node through §3.1's proxy, sends it here with what the operator just did, receives new text, and `PUT`s that back to the node. Nothing is stored between the two, and this endpoint has no notion of *which* service it is editing — it takes text and returns text.

**Why a round trip to the server at all**, when the canvas already has the text: SERVICE §9 requires a structural edit to preserve everything it did not change — comments, key order, alignment, blank lines, quoting — and a value-tree parser cannot do that, because a value tree has no trivia. `eio-service`'s `Document` is this repository's editor and it is a `std` Rust crate, so the backend calls it directly. Re-implementing it in TypeScript would be a second editor to keep in agreement with the CLI's, and SERVICE §9's one-editor rule exists to prevent exactly that. This is *not* the WASM route §1 takes for `expr`: `expr` is compiled for the browser because linting has to happen on every keystroke, which is a different requirement with a different answer.

**Statelessness is what keeps SCOPE §3.8 true.** A Designer that held a service file between edits would be a second home for something the spec says lives on nodes, one crash away from disagreeing with the file it came from. Text in, text out, no session, no draft.

The operations are `Document`'s own (SERVICE §9): `add_block`, `remove_block`, `set_name`, `remove_name`, `set_prop`, `remove_prop`, `connect`, `disconnect`, `set_autostart`, `set_ui`, `remove_ui`. Several may be sent together, and they apply **in order and all-or-nothing**: SERVICE §9 says an edit that would make the file invalid MUST fail and change nothing, so a batch that fails at its third operation returns the original text unchanged along with what broke. A drag that adds a block and connects it is one edit, not two, and must not be able to leave a block wired to nothing.

**A batch that references what it just added supplies the id itself.** `add_block` takes an optional `id`; omitted, the endpoint mints one and reports it back. But a later operation in the *same* batch names an instance by id, and a minted id does not exist until the batch runs — so **a caller that needs to connect what it just added MUST supply the id on `add_block`**. The alternative was a forward-reference syntax naming an earlier operation's result, and it is deliberately not invented: it would put a second way of identifying an instance into the format, next to the one SERVICE §2 already settled, for a caller that is perfectly able to choose an id. A client holds the file it is editing, so it knows every id in use; `Document::add_block` refuses a duplicate regardless, and a stale `ETag` catches the concurrent case (§4). Minting stays for the caller that adds a block and does not need to name it in the same breath.

**The response is text, not a diff or a parse tree.** What the caller `PUT`s is what this returned, byte for byte, so there is no step between here and the node where formatting could be reconstructed differently.

**Conflict detection is the daemon's, not this endpoint's** (§4, DAEMON §9.3): the browser carries the `ETag` its `GET` returned and the node refuses a stale `PUT`. This endpoint never sees a node and cannot know what is on one.


**Reading one is the same transform, in the other direction** (eieio-m9s.37). `GET /services/{s}` answers the file's **text**, verbatim — DAEMON §9 is deliberate about that, because SERVICE §2 makes the daemon a reader and "a definition that came back reformatted would make every round trip through this API a diff". So a canvas needs the file *parsed*, and nothing in the browser parses TOML.

**The parse belongs where the edit already is.** `POST /api/service-edit` takes text plus operations and returns text; its read counterpart takes text and returns the structure a canvas draws. Both are stateless, hold no service identity, and reach no node — §3.3's rule that the backend reaches a node only through the catch-all is untouched, because neither endpoint reaches one at all.

This is not the Designer becoming a second reader of the service file format. It already links `eio-service` and drives `Document` for every edit, and §3.2's own reason applies unchanged: `eio-service` is this repository's one implementation of the format, and a second one — in Rust or in TypeScript — is what SERVICE §9's one-editor rule exists to prevent. **Compiling `eio-service` to wasm for the browser would honour that rule too**, since it is the same crate, and it was considered: it is rejected here for cost rather than correctness, because it adds a second wasm artifact to the bundle and a second build-pipeline step to do what one endpoint already can.

**What the parsed view MUST NOT become is a second source of truth.** It is derived from the text on every request and never stored; the text remains what a `PUT` sends back, and the `[ui]` preservation rule of §4.1 applies to that text, not to any structure derived from it.
## 4. Service editing model

- **Read-modify-write of service files** through `GET/PUT /services/{s}`. The canvas is a _view of a TOML file_. Round-trip fidelity is a hard requirement: comments and formatting of hand-edited files SHOULD survive a Designer edit. The editor is not the Designer's own: SERVICE §9 makes a preserving edit the format's contract and `eio-service` implements it, so the backend reaches that crate rather than growing a second writer. Not by the WASM route §1 uses for `expr`: `eio-service` is a `std` crate and the backend is Rust, so it is an ordinary dependency. `expr` is compiled to WASM because the *browser* needs it on every keystroke, which is a different requirement with a different answer. A canvas whose idea of what a service file may say differed from the CLI's would be two formats.
- **Layout lives in the service file** under the daemon-ignored `[ui]` table (DAEMON-SPEC §2): node positions, canvas viewport, notes. Rationale: the service file stays the single portable artifact — git-clone a service onto a fresh node and the Designer renders it laid out; agents can read/write layout like anything else. The daemon's ignore-contract keeps this honest.
- Conflict handling (file changed on disk / by an agent since read): the daemon's, not the Designer's. DAEMON §9.3 makes an overwrite conditional on the `ETag` a `GET` returned, so a stale `PUT` is refused with the current text and a diff before it reaches the disk — the Designer's part is to carry the tag it read and to render the refusal, and it could not silent-overwrite if it tried. Agents and humans editing the same files is the _expected_ condition, not an edge case (SCOPE §4).

### 4.1 The `[ui]` table (normative)

SERVICE §6 defines `[ui]` by refusing to define it — "It has no schema here and never will; a daemon that read a key inside it would make the Designer's layout format a thing the daemon has an opinion about." The daemon is right to be silent, which makes this the document that owes the answer. Until eieio-m9s.26 the answer was a TypeScript interface and nothing else.

**What the Designer writes**, and it is only two shapes, both inline tables under `[ui].<key>`:

|Key|Value|
|---|---|
|a block's id|`{ x = <float>, y = <float> }`|
|the literal `viewport`|`{ x = <float>, y = <float>, zoom = <float> }`|

Numbers are always emitted with a decimal point — a TOML float, never a bare integer — so a position that happens to be whole does not change type between one write and the next.

**What the Designer reads** is exactly `x`, `y` and `zoom`, and only when each is spelled as a bare TOML number. A key present but not a bare number is not read as that key; it falls through to the next rule.

**Everything else is preserved without being understood, and SERVICE §6 makes that a MUST rather than a courtesy** — "It MUST survive a read-modify-write unchanged, which is what makes it safe to put a human's canvas in a file a program rewrites." There are two scopes to it, and the second is the one that was broken:

- **A `[ui]` key outside the two shapes above** is never named by any operation, so an edit does not touch it. That includes an entry for a block id the service no longer declares: a stale annotation is inert, not an error.
- **An extra member inside a known entry's own inline table** — `locked = true` sitting beside `x` and `y` — is carried forward verbatim and re-spliced whenever that entry is rewritten for an unrelated reason, such as a drag. This is the case a reconstruct-from-the-model implementation loses, and it did: rebuilding a moved block's value from `{x, y}` dropped it silently the moment the block next moved.

**The Designer MUST NOT interpret a preserved member, and MUST NOT normalise one.** It is opaque text; splitting it is quote- and depth-aware precisely so a comma inside a string or a nested inline table survives as itself.

**Not yet reachable end to end, and stating so is the point** (eieio-m9s.26): nothing produces a *structured* `ui` from a real backend. `crates/daemon`'s `ServiceDetail` has no `ui` field, `crates/designer` has no handler that builds one, and `designer/src/lib/api/client.ts` still re-exports the whole service surface from the mock. So preservation is proven where it lives — the pure transform of DESIGNER §3.2 — and the wire representation of a preserved member is an open question for whoever wires the real path, not a settled part of this schema.

## 5. Canvas and editing UX

- **Shell: one navigator, library on demand.** nio's four always-present columns (System rail, service list, canvas, block library) become a rail, a single indented System → Node → Service tree, and the canvas; the block library opens over the canvas when a block is being added. Same hierarchy, less permanent chrome — a self-hosted operator with two nodes should not spend a third of the window on three list rows. **Run state is shown in the tree and the available action on the toolbar, and they are inverse** (`▷` in the tree means running; `▷` on the toolbar means *start*). nio did this and never labelled it; label it.
- **Palette** from cached manifests: block cards with description, ports, capability badges. Capability badges cross-check against the target node's capabilities — a `gpio` block dragged toward a node without GPIO warns _at design time_ (the SCOPE §3.3 validation, surfaced early).
- **The block on canvas: nio's two-line card, plus what nio had no need for.** A coloured square holding a 2–4 character abbreviation, then the instance name in bold over the block type in grey. That card answers "what is this" and "what kind of thing is it" with no legend, and is the highest-value visual the original has to give. Two additions, each earning its space: **terminals are named on the card** (an output port's label, and ABI §6.4's reserved error port rendered as a distinct terminal), so a fan-out is readable without tracing a wire to its source; and **an unmet capability is badged on the block itself**, which is the §3.3 check above made visible where the mistake is being made rather than at deploy.
  - **The abbreviation is derived, never authored**, as nio's was: initials of the block name's hyphen-separated words, 2–4 characters, falling back to the first three letters of a single word — `temp-sensor` → `TS`, `rolling-average` → `RA`, `filter` → `Fil`. A name of more than four words takes the first four initials; the avatar is a fixed-width square, so this is a truncation rather than a choice about which words matter. nio's rule read capitals out of CamelCase type names; ours reads a kebab-case manifest `name`, which is the same rule against a different convention.
  - **The colour is a stable function of the block name and carries no meaning.** It is an aid to recognition, not a category code, and inventing semantics for it later would be a breaking change to something nobody was told was significant. It appears **on canvas only** — palette rows are uniform — which is what keeps it a locator rather than a taxonomy. (Both rules reconstructed from the nio archives.)
- **Block config is a modal, not a docked inspector.** Double-click a block, get that block and nothing else: its name, then its properties, then `accept`/`cancel`. This follows nio and is a deliberate rejection of the always-present sidebar — the owner's reason, recorded because it is the whole point: *"it focused our attention on one block, only the properties that make it up. It's less information overload, which I felt systems like Node-RED overcomplicated."* A modal is also an honest commit point, which the ETag flow in §4 wants anyway.

  The one real argument against a modal is that it hides the graph, and a property is an *expression over an incoming signal* (ABI §11) — so writing `$temp` means knowing what the upstream block emits, and on a canvas that answer is on screen. **The modal answers it instead of the canvas answering it.** Alongside nio's `?` (the block's own manifest documentation, inline), the modal lists the fields reaching this block's input, beside nio's `?` for the block's own documentation.

  **Those field names are the Designer's own hint, not an ABI fact, and the difference is stated because it is load-bearing.** ABI §11.1 describes a block's ports by name and says nothing about the shape of a signal travelling one — deliberately: signals are dict-shaped and dynamic, and EXPR §6 makes a missing attribute an error rather than a null, which is the platform's chosen way of catching a config typo loudly. An optional advisory declaration in the manifest was considered for this and **declined**: every declaration is a thing that can be wrong, a block whose output shape depends on its input cannot state one honestly, and the failure it would prevent is already caught at the first signal.

  So the hint is best-effort and the UI MUST present it as such. It may come from a block's documentation, from what its properties reference, or from traffic already observed on a tap (§6) — and it may be **absent**, which is the ordinary case for a block nobody has run. A modal that renders an empty hint as "this port carries nothing" would be asserting something no one declared; it renders nothing instead. The hint's job is to save an operator from guessing, not to become a schema the platform does not have.
- **Every property input is an expression editor** rendered from the manifest's property schema (ABI §11), with: WASM-`expr` linting on keystroke, a signal-dependence badge (constant vs per-signal), and the manifest-declared type shown as the expected result type. Literal-only values render as plain typed fields that read/write trivial expressions (the UI affordance noted in ABI §11).
- **Connections**: drag port-to-port; fan-out by connecting one output to many inputs (duplication semantics shown, nio-style); the reserved error port (ABI §6.4) rendered as a distinct terminal on every block, connectable like any other.
- Service lifecycle controls (start/stop/reload) with validation errors from the daemon rendered inline on the offending block/property/connection (spans from EXPR §8 map to editor positions).

## 6. Live inspection

- **Taps**: click a connection on a running service → `POST /taps` → a live sampled signal stream, with expression-failure events annotated in-stream (DAEMON §6). **Two surfaces, sequenced.** A tap first renders into the docked panel below, because that panel exists for `/logs/stream` regardless, gives history that can be scrolled and searched, and carries no unmeasured cost. The on-canvas rendering — a throughput badge on the edge itself, a signal inspector on click — is the better idea and is where this is going: it puts the question and the answer in the same place and makes "where is data moving" ambient across the whole graph. It follows once the cost of a badge updating on many edges at once has been *measured* rather than assumed, which is the one performance unknown this surface has. This gets priority over aesthetics — and note it is the one major surface with **no nio precedent to imitate** (SCOPE §3.12's correction): nio observed a connection by wiring a Logger block into it. Design it from what an operator needs, not from an archive.
- **Logs**: per-service/per-instance streamed views (`/logs/stream`), filterable, correlated to canvas selection. nio's logger panel is worth copying closely, and it is reconstructed: a dockable panel over the canvas with a `clear` control and an expand toggle, lines of `[timestamp][LEVEL][service.block] <payload>`, level settable per service *and* per block, historical lines loaded before the stream is joined. It printed **every** signal rather than a sample, which is right for a log and wrong for a tap — the two surfaces differ deliberately.
- Node dashboard: per-System health, service statuses, restart counts, error summaries.

**A dropped stream reconnects; a refused one stops (normative).** Reconnection with an event id is in the protocol rather than in every client (DAEMON §9.6), and the Designer uses it: a tap or log stream whose *connection* fails — a node restarting, a response body ending, a socket refused — is retried with backoff and the panel says it is retrying, because a panel that silently stops updating is worse than one that says so. That reasoning stops exactly where retrying stops being able to work. A response that arrived and refused the request will refuse the identical request again, so **a status that cannot succeed by being repeated ends the stream**: the panel reports it stopped and which status stopped it, rather than showing a reconnect it knows will fail — which is both a spinner that means nothing and a request every few seconds against a server that cannot answer differently.

The line is drawn as a class, because a client cannot enumerate what a node, this Designer, or a reverse proxy in front of either will answer: **4xx is permanent, except `408` and `429`** — HTTP's own two "ask again" statuses — and 5xx and every transport-level failure are transient and reconnected. The two that arise in practice are `401` (below) and `404` on a tap the node has already released: DAEMON §9.6's "teardown is either explicit or a disconnect" means a released tap is gone, and reconnecting to its id is a request that can only ever `404` — a new tap comes from `POST /taps`, with a new id.

**A `401` reopens the login gate wherever it appears, streams included (normative).** §3.1's gate is what a `401` on any `/api` route raises, and a tap or log stream is not an exception to that just because it reports through a stream rather than a response — the Designer's own session may have expired, or a node's stored bearer token may have (DAEMON §9.1), and nothing in either wire contract distinguishes those two (§9.2's `message` MUST NOT be parsed). Both want the same thing on screen. A stream that reported `reconnecting` indefinitely while the same session's other requests raised the gate would be telling the operator two contradictory things about one session, which is worse than either answer alone.

## 7. Deployment flows (SCOPE §3.7)

One design flow, two deploy pipelines, selected by target node class:

- **Daemon-class**: `PUT` service file → validate → start. Errors inline.
- **Leaf-class**: the same service definition feeds a **firmware build pipeline**: Designer resolves AOT artifacts for the blocks (registry `aot` entries, ABI §11), invokes the leaf build (bundling runtime + AOT blocks + baked config), and produces a flashable image + flashing instructions (or drives a local flash tool where feasible). **The pipeline's mechanics belong to the leaf-runtime spec; the Designer's contract is only: same canvas, same service file, deploy button does the right thing per node class, extra flash steps surfaced as steps — never as a different design flow.**

## 8. Agent integration (SCOPE §4)

- **The Designer serves no MCP of its own.** SCOPE §4 settles the packaging: MCP is a mode of the CLI (`eio mcp`), and it already covers every operation DAEMON §9 exposes, across every node at once. A second implementation here would be nineteen tools that must not drift from the first, to reach the same daemon the first one reaches. An earlier draft of this section specified one; it is withdrawn rather than left as an option, because two agent surfaces over one API is a maintenance cost with no capability behind it.
- **The parity rule stands, and is what §8 is actually for.** No Designer feature may exist that an agent cannot reach through `eio mcp` plus the daemon API. This is a *constraint on the canvas*, not a description of a component: a canvas capability with no counterpart in DAEMON §9 is a bug in the daemon's surface, and the fix is to add the operation there — where the CLI, the Designer and an agent all reach it — never to add a private path the canvas alone can take. (Test: the golden-path demo script runs twice — once clicked, once prompted.)
- The Designer's own registry (§2) is the one thing `eio mcp` does not reach, and it needs no tools: it holds node addresses and tokens an operator types in once, not a surface an agent drives.
- **PROPOSED, v1-optional:** an in-Designer agent panel — "build me a service that reads the BME280 every 30s and publishes to sensors/office" materializing on the canvas. Sequenced after core editing works, and note it is no longer architecturally free: with no Designer-side MCP, a panel needs a path to an agent runtime, which is its own decision when it is taken.

## 9. Explicitly out of scope (v1)

Multi-user/RBAC (SCOPE §6), cloud hosting, canvas undo-collaboration (CRDTs etc.), block authoring/IDE features (blocks come from repos + registry), custom canvas engine, mobile.

## 10. Expansion list (for the in-depth pass)

Service-file ↔ canvas mapping (normative — the `[ui]` half is done, §4.1), tap inspector UX and sampling controls, palette search/filtering, node onboarding flow (token exchange ergonomics), manifest cache invalidation, MCP tool schema, diff/conflict UX, leaf pipeline integration contract, System-level topology view (cross-node pub/sub edges — depends on OPEN SCOPE §3.9).
