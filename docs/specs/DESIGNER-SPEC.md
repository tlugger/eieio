# Designer Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion. **Depends on:** SCOPE.md (§2, §3.8, §3.10–3.12, §4), DAEMON-SPEC.md §9 (the API it consumes), ABI-SPEC.md §11 (manifests drive its UI), EXPR-SPEC.md. **Markers:** **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

The Designer is the optional visual management surface: create Systems, attach nodes, design services on a canvas, configure blocks, deploy, start/stop, and inspect running signal flow. Two constraints define its architecture more than any feature does:

1. **Never the system of record** (SCOPE §3.8). Daemons own their config as files; the Designer's DB holds only what daemons _can't_ know: System groupings, node connection info, registry sources. If the Designer's DB is lost, no System loses anything but its address book.
2. **Peer client, not privileged client** (SCOPE §4). Everything the Designer does goes through the same daemon API an agent or CLI uses. Any Designer-only capability is an architecture bug.

---

## 1. Stack

**PROPOSED** — the choice below is settled; the marker comes off when `eieio-m9s.1` builds it.

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

- **Proxy, not peer-to-daemon-from-browser** (PROPOSED): all daemon API calls route browser → Designer backend → daemon. Rationale: node tokens never reach the browser, CORS/TLS mess stays server-side, and mixed-reachability networks (Designer can reach nodes the operator's laptop can't) work. Streams (taps, logs) are re-streamed over the same hop.
- Node registration: address + token (+ CA material when SCOPE §3.11 resolves), health polling → `last_seen`, capability/limit discovery via `GET /node` cached for deploy-time validation.
- Registry browsing: query block registries, cache manifests (the palette's data source).
- **Designer auth itself is v1-minimal** (PROPOSED): single-operator assumption (SCOPE §6 — no multi-tenancy); a single login/token gate on the app. Nothing fancier until someone needs it.

### 3.1 The Designer's own HTTP surface (normative)

Two kinds of endpoint, and the split is the whole design. Everything the Designer *itself* knows is a small REST surface; everything a **node** knows is reached by proxy and is never re-modelled here.

```
POST   /api/session                       { password } -> session cookie
DELETE /api/session

GET    /api/systems                       [{ id, name }]
POST   /api/systems                       { name }
DELETE /api/systems/{id}

GET    /api/nodes                         [{ id, system_id, name, class, address,
                                             last_seen, capabilities, limits }]
POST   /api/nodes                         { system_id, name, address, token }
DELETE /api/nodes/{id}
POST   /api/nodes/{id}/probe              refresh last_seen + capabilities via GET /node

GET    /api/registries                    [{ id, url }]
POST   /api/registries                    { url, auth? }
GET    /api/blocks                        the manifest cache (the palette's data source),
                                          each entry carrying the `block_ref` it
                                          was fetched for (§2)

ANY    /api/nodes/{id}/daemon/{*path}     proxied to that node, verbatim
```

**A node's token never appears in a response.** It is write-only: supplied on `POST /api/nodes`, stored, and thereafter only ever attached to an outbound proxied request. The `nodes` representation above has no `token` field at all, which is stronger than omitting it per-handler — there is no serialization in which it can appear.

**The proxy is one catch-all, not a re-modelling of DAEMON §9.** `/api/nodes/{id}/daemon/{*path}` forwards method, path, query and body to that node's address, attaches its bearer token, and streams the response back — `text/event-stream` included, unbuffered, so §6's taps and logs are the same hop. A per-endpoint proxy would be DAEMON §9's table written a third time (after the daemon and the CLI), free to drift from both; a catch-all cannot drift, because it knows nothing about what it is forwarding. This is also what keeps §8's parity rule true by construction: the browser reaches exactly the operations a node serves, no more and no fewer.

**A block is identified by its whole reference, never by its name.** `manifest_cache` is keyed by `block_ref` (§2), and a service file's `block` field is matched against that key verbatim — no parsing, no stripping of registry or tag. A manifest's own `name` does not identify it: two registries may publish `temp-sensor`, two versions of `filter` may declare different ports and properties (ABI §11.1), and a reference naming a registry with a port does not even split on its first colon. Every one of those failures presents identically — a block rendered with another block's ports, properties and capability requirements — so the rule is exact match, and the cache is asked for what was actually pulled.

**The browser is the operator, so the proxy does not restrict which daemon operations it may reach.** The proxy exists to keep the token server-side and to solve mixed reachability (§3), not to be an authorization layer — v1 has one operator (SCOPE §6), and a second one is where this needs revisiting.

## 4. Service editing model

- **Read-modify-write of service files** through `GET/PUT /services/{s}`. The canvas is a _view of a TOML file_. Round-trip fidelity is a hard requirement: comments and formatting of hand-edited files SHOULD survive a Designer edit. The editor is not the Designer's own: SERVICE §9 makes a preserving edit the format's contract and `eio-service` implements it, so the backend reaches that crate rather than growing a second writer. Not by the WASM route §1 uses for `expr`: `eio-service` is a `std` crate and the backend is Rust, so it is an ordinary dependency. `expr` is compiled to WASM because the *browser* needs it on every keystroke, which is a different requirement with a different answer. A canvas whose idea of what a service file may say differed from the CLI's would be two formats.
- **Layout lives in the service file** under the daemon-ignored `[ui]` table (DAEMON-SPEC §2): node positions, canvas viewport, notes. Rationale: the service file stays the single portable artifact — git-clone a service onto a fresh node and the Designer renders it laid out; agents can read/write layout like anything else. The daemon's ignore-contract keeps this honest.
- Conflict handling (file changed on disk / by an agent since read): the daemon's, not the Designer's. DAEMON §9.3 makes an overwrite conditional on the `ETag` a `GET` returned, so a stale `PUT` is refused with the current text and a diff before it reaches the disk — the Designer's part is to carry the tag it read and to render the refusal, and it could not silent-overwrite if it tried. Agents and humans editing the same files is the _expected_ condition, not an edge case (SCOPE §4).

## 5. Canvas and editing UX

- **Shell: one navigator, library on demand.** nio's four always-present columns (System rail, service list, canvas, block library) become a rail, a single indented System → Node → Service tree, and the canvas; the block library opens over the canvas when a block is being added. Same hierarchy, less permanent chrome — a self-hosted operator with two nodes should not spend a third of the window on three list rows. **Run state is shown in the tree and the available action on the toolbar, and they are inverse** (`▷` in the tree means running; `▷` on the toolbar means *start*). nio did this and never labelled it; label it.
- **Palette** from cached manifests: block cards with description, ports, capability badges. Capability badges cross-check against the target node's capabilities — a `gpio` block dragged toward a node without GPIO warns _at design time_ (the SCOPE §3.3 validation, surfaced early).
- **The block on canvas: nio's two-line card, plus what nio had no need for.** A coloured square holding a 2–4 character abbreviation, then the instance name in bold over the block type in grey. That card answers "what is this" and "what kind of thing is it" with no legend, and is the highest-value visual the original has to give. Two additions, each earning its space: **terminals are named on the card** (an output port's label, and ABI §6.4's reserved error port rendered as a distinct terminal), so a fan-out is readable without tracing a wire to its source; and **an unmet capability is badged on the block itself**, which is the §3.3 check above made visible where the mistake is being made rather than at deploy.
  - **The abbreviation is derived, never authored**, as nio's was: initials of the block name's hyphen-separated words, 2–4 characters, falling back to the first three letters of a single word — `temp-sensor` → `TS`, `rolling-average` → `RA`, `filter` → `Fil`. A name of more than four words takes the first four initials; the avatar is a fixed-width square, so this is a truncation rather than a choice about which words matter. nio's rule read capitals out of CamelCase type names; ours reads a kebab-case manifest `name`, which is the same rule against a different convention.
  - **The colour is a stable function of the block name and carries no meaning.** It is an aid to recognition, not a category code, and inventing semantics for it later would be a breaking change to something nobody was told was significant. It appears **on canvas only** — palette rows are uniform — which is what keeps it a locator rather than a taxonomy. (Both rules reconstructed from the nio archives.)
- **Block config is a modal, not a docked inspector.** Double-click a block, get that block and nothing else: its name, then its properties, then `accept`/`cancel`. This follows nio and is a deliberate rejection of the always-present sidebar — the owner's reason, recorded because it is the whole point: *"it focused our attention on one block, only the properties that make it up. It's less information overload, which I felt systems like Node-RED overcomplicated."* A modal is also an honest commit point, which the ETag flow in §4 wants anyway.

  The one real argument against a modal is that it hides the graph, and a property is an *expression over an incoming signal* (ABI §11) — so writing `$temp` means knowing what the upstream block emits, and on a canvas that answer is on screen. **The modal answers it instead of the canvas answering it.** Alongside nio's `?` (the block's own manifest documentation, inline), the modal lists the fields reaching this block's input, resolved from the upstream block's manifest. That is strictly better than reading them off the graph, where they were never actually written down.
- **Every property input is an expression editor** rendered from the manifest's property schema (ABI §11), with: WASM-`expr` linting on keystroke, a signal-dependence badge (constant vs per-signal), and the manifest-declared type shown as the expected result type. Literal-only values render as plain typed fields that read/write trivial expressions (the UI affordance noted in ABI §11).
- **Connections**: drag port-to-port; fan-out by connecting one output to many inputs (duplication semantics shown, nio-style); the reserved error port (ABI §6.4) rendered as a distinct terminal on every block, connectable like any other.
- Service lifecycle controls (start/stop/reload) with validation errors from the daemon rendered inline on the offending block/property/connection (spans from EXPR §8 map to editor positions).

## 6. Live inspection

- **Taps**: click a connection on a running service → `POST /taps` → a live sampled signal stream, with expression-failure events annotated in-stream (DAEMON §6). **Two surfaces, sequenced.** A tap first renders into the docked panel below, because that panel exists for `/logs/stream` regardless, gives history that can be scrolled and searched, and carries no unmeasured cost. The on-canvas rendering — a throughput badge on the edge itself, a signal inspector on click — is the better idea and is where this is going: it puts the question and the answer in the same place and makes "where is data moving" ambient across the whole graph. It follows once the cost of a badge updating on many edges at once has been *measured* rather than assumed, which is the one performance unknown this surface has. This gets priority over aesthetics — and note it is the one major surface with **no nio precedent to imitate** (SCOPE §3.12's correction): nio observed a connection by wiring a Logger block into it. Design it from what an operator needs, not from an archive.
- **Logs**: per-service/per-instance streamed views (`/logs/stream`), filterable, correlated to canvas selection. nio's logger panel is worth copying closely, and it is reconstructed: a dockable panel over the canvas with a `clear` control and an expand toggle, lines of `[timestamp][LEVEL][service.block] <payload>`, level settable per service *and* per block, historical lines loaded before the stream is joined. It printed **every** signal rather than a sample, which is right for a log and wrong for a tap — the two surfaces differ deliberately.
- Node dashboard: per-System health, service statuses, restart counts, error summaries.

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

Service-file ↔ canvas mapping (normative, incl. `[ui]` schema), tap inspector UX and sampling controls, palette search/filtering, node onboarding flow (token exchange ergonomics), manifest cache invalidation, MCP tool schema, diff/conflict UX, leaf pipeline integration contract, System-level topology view (cross-node pub/sub edges — depends on OPEN SCOPE §3.9).
