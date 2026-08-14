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

## 3. Backend responsibilities

- **Proxy, not peer-to-daemon-from-browser** (PROPOSED): all daemon API calls route browser → Designer backend → daemon. Rationale: node tokens never reach the browser, CORS/TLS mess stays server-side, and mixed-reachability networks (Designer can reach nodes the operator's laptop can't) work. Streams (taps, logs) are re-streamed over the same hop.
- Node registration: address + token (+ CA material when SCOPE §3.11 resolves), health polling → `last_seen`, capability/limit discovery via `GET /node` cached for deploy-time validation.
- Registry browsing: query block registries, cache manifests (the palette's data source).
- **Designer auth itself is v1-minimal** (PROPOSED): single-operator assumption (SCOPE §6 — no multi-tenancy); a single login/token gate on the app. Nothing fancier until someone needs it.

## 4. Service editing model

- **Read-modify-write of service files** through `GET/PUT /services/{s}`. The canvas is a _view of a TOML file_. Round-trip fidelity is a hard requirement: comments and formatting of hand-edited files SHOULD survive a Designer edit. The editor is not the Designer's own: SERVICE §9 makes a preserving edit the format's contract and `eio-service` implements it, so the backend reaches that crate rather than growing a second writer — the same WASM trick §1 uses for `expr`, and the same reason. A canvas whose idea of what a service file may say differed from the CLI's would be two formats.
- **Layout lives in the service file** under the daemon-ignored `[ui]` table (DAEMON-SPEC §2): node positions, canvas viewport, notes. Rationale: the service file stays the single portable artifact — git-clone a service onto a fresh node and the Designer renders it laid out; agents can read/write layout like anything else. The daemon's ignore-contract keeps this honest.
- Conflict handling (file changed on disk / by an agent since read): compare content hash on PUT, surface a diff, never silent-overwrite. Agents and humans editing the same files is the _expected_ condition, not an edge case (SCOPE §4).

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

- The Designer backend exposes an **MCP server** wrapping its own operations (list systems/nodes, read/write service files, deploy, start/stop, open taps, query manifests) — an agent connected to it can do everything the canvas can, because both drive the same daemon APIs.
- **PROPOSED, v1-optional:** an in-Designer agent panel (chat pane driving the MCP surface) — "build me a service that reads the BME280 every 30s and publishes to sensors/office" materializing on the canvas. Architecturally free given the MCP server; sequenced after core editing works.
- No Designer feature may exist that an agent can't reach through MCP + daemon APIs. (Test: the golden-path demo script runs twice — once clicked, once prompted.)

## 9. Explicitly out of scope (v1)

Multi-user/RBAC (SCOPE §6), cloud hosting, canvas undo-collaboration (CRDTs etc.), block authoring/IDE features (blocks come from repos + registry), custom canvas engine, mobile.

## 10. Expansion list (for the in-depth pass)

Service-file ↔ canvas mapping (normative, incl. `[ui]` schema), tap inspector UX and sampling controls, palette search/filtering, node onboarding flow (token exchange ergonomics), manifest cache invalidation, MCP tool schema, diff/conflict UX, leaf pipeline integration contract, System-level topology view (cross-node pub/sub edges — depends on OPEN SCOPE §3.9).
