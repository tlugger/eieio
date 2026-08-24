# Designer Specification

**Status:** Draft 1 — high-level; intended for in-depth expansion. **Depends on:** SCOPE.md (§2, §3.8, §3.10–3.12, §4), DAEMON-SPEC.md §9 (the API it consumes), ABI-SPEC.md §11 (manifests drive its UI), EXPR-SPEC.md. **Markers:** **PROPOSED** = drafted here, awaiting ratification. **OPEN** = tracked in SCOPE.md.

The Designer is the optional visual management surface: create Systems, attach nodes, design services on a canvas, configure blocks, deploy, start/stop, and inspect running signal flow. Two constraints define its architecture more than any feature does:

1. **Never the system of record** (SCOPE §3.8). Daemons own their config as files; the Designer's DB holds only what daemons _can't_ know: System groupings, node connection info, registry sources. If the Designer's DB is lost, no System loses anything but its address book.
2. **Peer client, not privileged client** (SCOPE §4). Everything the Designer does goes through the same daemon API an agent or CLI uses. Any Designer-only capability is an architecture bug.

---

## 1. Stack

**PROPOSED:**

- **SvelteKit** (SSR + API routes in one deployable, strong canvas-perf reputation; Next.js acceptable substitute — decide at build time, nothing below depends on the choice).
- **SQLite** for the backend DB — registry-scale data only (§2); zero-ops matches self-hosted posture (SCOPE §6).
- **Canvas: Svelte Flow / React Flow** (mature node-graph libraries; custom canvas is explicitly rejected scope for v1).
- **Shared Rust via WASM in the browser:** the `expr` crate compiled to WASM powers in-editor expression linting (parse errors with spans, unbound symbols, signal-dependence badge — EXPR §10 semantics, the _same interpreter code_ the daemon runs). Same trick available for `manifest`/service-file validation. This is the payoff of the no_std crate split (DAEMON-SPEC §1) landing in the UI.
- Ships as a container image + a bare binary/node target; localhost-first.

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

## 4. Service editing model

- **Read-modify-write of service files** through `GET/PUT /services/{s}`. The canvas is a _view of a TOML file_. Round-trip fidelity is a hard requirement: comments and formatting of hand-edited files SHOULD survive a Designer edit. The editor is not the Designer's own: SERVICE §9 makes a preserving edit the format's contract and `eio-service` implements it, so the backend reaches that crate rather than growing a second writer — the same WASM trick §1 uses for `expr`, and the same reason. A canvas whose idea of what a service file may say differed from the CLI's would be two formats.
- **Layout lives in the service file** under the daemon-ignored `[ui]` table (DAEMON-SPEC §2): node positions, canvas viewport, notes. Rationale: the service file stays the single portable artifact — git-clone a service onto a fresh node and the Designer renders it laid out; agents can read/write layout like anything else. The daemon's ignore-contract keeps this honest.
- Conflict handling (file changed on disk / by an agent since read): the daemon's, not the Designer's. DAEMON §9.3 makes an overwrite conditional on the `ETag` a `GET` returned, so a stale `PUT` is refused with the current text and a diff before it reaches the disk — the Designer's part is to carry the tag it read and to render the refusal, and it could not silent-overwrite if it tried. Agents and humans editing the same files is the _expected_ condition, not an edge case (SCOPE §4).

## 5. Canvas and editing UX

- **Palette** from cached manifests: block cards with description, ports, capability badges. Capability badges cross-check against the target node's capabilities — a `gpio` block dragged toward a node without GPIO warns _at design time_ (the SCOPE §3.3 validation, surfaced early).
- **Block config panels rendered from manifest property schemas** (ABI §11): every property input is an expression editor (per EXPR — everything is an expression) with: WASM-`expr` linting on keystroke, signal-dependence badge (constant vs per-signal), and the manifest-declared type shown as the expected result type. Literal-only values render as plain typed fields that read/write trivial expressions (the UI affordance noted in ABI §11).
- **Connections**: drag port-to-port; fan-out by connecting one output to many inputs (duplication semantics shown, nio-style); the reserved error port (ABI §6.4) rendered as a distinct terminal on every block, connectable like any other.
- Service lifecycle controls (start/stop/reload) with validation errors from the daemon rendered inline on the offending block/property/connection (spans from EXPR §8 map to editor positions).

## 6. Live inspection

- **Taps**: click a connection on a running service → `POST /taps` → live sampled signal stream rendered on-canvas (throughput badge on edges; expandable signal inspector; expression-failure events annotated in-stream, per DAEMON-SPEC §6). This is the nio killer feature (SCOPE §3.12) and gets priority over aesthetics.
- **Logs**: per-service/per-instance streamed views (`/logs/stream`), filterable, correlated to canvas selection.
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
