//! `eio mcp`: the agent surface for a whole System, over stdio (SCOPE §4).
//!
//! # Packaging: a mode of the CLI, not an endpoint of a node
//!
//! Settled in `docs/SCOPE.md` §4, not here: three packagings were on the table (an MCP server
//! inside each daemon, a sidecar deriving tools from `/openapi.json`, or a mode of the CLI),
//! and the CLI won because it already holds the multi-node context, the token handling and the
//! API client (`client.rs`, `config.rs`) that any of the other two would have to grow from
//! scratch. `docs/specs/DAEMON-SPEC.md` §9 restates the other half: "a node serves REST and
//! nothing else" — this module is the only thing in the workspace that speaks MCP, and nothing
//! it does reaches a node except over that REST surface, exactly like every other command in
//! this crate.
//!
//! # Multi-node is the point
//!
//! An agent holding a System addresses more than one node without reconnecting (SCOPE §4), so
//! every tool below takes a `node` argument naming an entry in `~/.config/eieio/nodes.toml`
//! (`config.rs`) — there is no "current node" this server remembers between calls, and no tool
//! falls back to `nodes.toml`'s configured default the way the plain `eio` commands do: a
//! default that silently applied here would be exactly the "which node did that touch" an
//! agent juggling several cannot afford to ask about after the fact. [`list_nodes`] is how an
//! agent discovers the roster before naming one.
//!
//! # Tool derivation: compile-time, from this crate's own request types — not a live fetch
//!
//! `rmcp-openapi` (a sibling crate that turns an OpenAPI document into MCP tools at runtime)
//! was evaluated and rejected. It would need a node's `/openapi.json` reachable *before* this
//! server can answer `tools/list`, which is backwards for the packaging decision above: the
//! tool surface is the same set of daemon operations regardless of which node in `nodes.toml`
//! a given call ends up naming, so making the list itself depend on one particular node being
//! up would tie a System-wide capability to a single node's availability — the same shape of
//! mistake §4's table rejected an MCP-server-per-daemon for, one level up.
//!
//! So every tool here is a plain `#[tool]`-annotated method (`rmcp-macros`'s `#[tool_router]`),
//! with its JSON input schema derived at *compile* time by `schemars` from a request struct in
//! this file, and its `Tool::description` derived from that method's own Rust doc comment
//! (`rmcp-macros`' default when no `description = "..."` is given). Every doc comment below
//! that corresponds to a DAEMON-SPEC §9 operation is copied verbatim from that handler's own
//! doc comment in `crates/daemon/src/api/*.rs` — which is also where `utoipa` reads
//! `/openapi.json`'s `summary`/`description` from (DAEMON §9: "an operation's description is
//! user-facing documentation"). `tests/mcp_openapi_drift.rs` is the enforcement: it calls
//! `eio_daemon::api::openapi::Document::openapi()` in-process (the same dev-dependency
//! `tests/openapi_surface.rs` already uses, eieio-yck.3) and asserts, for every tool that maps
//! to a DAEMON operation, that the two texts agree once whitespace is normalized — so a
//! handler's doc comment moving out from under this file's copy of it is a build failure, not a
//! silent drift. `list_nodes` is the one tool with no DAEMON operation behind it (`nodes.toml`
//! is this CLI's file, not a node's), and is excluded from that comparison by name, explicitly,
//! rather than by the comparison happening to not notice it.
//!
//! # Auth: structural, not disciplinary
//!
//! Every tool resolves its `node` argument through [`crate::client::connect`], the same
//! function every other command in this crate uses, so a tool call is indistinguishable from
//! an operator's own `eio services ...` for auth purposes: the token comes from `nodes.toml`,
//! travels in the `Authorization` header `Client` attaches, and every error a tool can return
//! is built from a response body (`client::envelope_error`) or from `nodes.toml`'s own contents
//! (`Config::resolve`'s errors, which name configured nodes, never tokens) — never from the
//! outgoing request, which is where the header lives. `read_tap`/`read_logs` open their own
//! short-lived connection rather than going through `Client` (see their doc comments for why),
//! and follow the same rule: their error path is [`crate::client::envelope_error`], unchanged.
//! `config.rs`'s `NodeEntry` carries a hand-written `Debug` that redacts the token for the same
//! reason: this server never formats one, and if it ever did by accident, that accident could
//! not print it either. `tests/mcp_token_leak.rs` asserts both halves.

use std::time::Duration;

use anyhow::{Context, Result};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::schemars;
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::Value;

use crate::client;

/// Runs `eio mcp`: serves the Model Context Protocol over stdio until the peer disconnects.
///
/// A dedicated single-threaded runtime, not `#[tokio::main]` on the whole binary: every other
/// command in this crate is synchronous end to end (`main.rs`'s module doc: "one process making
/// one call and exiting"), and this is the only command that needs an executor at all — for
/// `rmcp`'s stdio transport, and for [`tokio::task::spawn_blocking`] around the same blocking
/// `ureq` calls every other command already makes through [`crate::client::Client`].
pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime `eio mcp` needs for its stdio transport")?;
    runtime.block_on(serve())
}

async fn serve() -> Result<()> {
    let server = McpServer::new();
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .context("starting the MCP server on stdio")?;
    running.waiting().await.context("serving MCP over stdio")?;
    Ok(())
}

/// The MCP server. No state of its own — every tool resolves its `node` argument fresh, per
/// call (see the module doc's "multi-node is the point"), so there is nothing here to hold.
/// `#[tool_router]` below generates `Self::tool_router()`, which is what `#[tool_handler]`'s
/// generated `call_tool`/`list_tools` call by default — there is no router to store in a
/// field, only a type to dispatch through.
pub struct McpServer;

impl McpServer {
    /// A server with every tool below registered.
    pub fn new() -> Self {
        Self
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── request shapes: one per distinct tool signature, `node` first in every one, because
//     `crate::config::Config` never has a "current node" this server can assume (the module
//     doc's "multi-node is the point") ───

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NodeOnly {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NodeService {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// The service's name — its file's stem (SERVICE §1).
    service: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PullBlockParams {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// The block reference to pull, e.g. `ghcr.io/eieio/transform:1.0.0` (DAEMON §4.1).
    reference: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PutServiceParams {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// The service's name — must equal the definition's own `name` (SERVICE §1).
    service: String,
    /// The service file's text, verbatim (SERVICE §1). Not a rendering of a parse.
    definition: String,
    /// The `ETag` this write must name to overwrite an existing service (DAEMON §9.3): the
    /// value `get_service` answered with, or `"*"` to overwrite whatever is there. Omit only
    /// to create a service that does not exist yet — a `PUT` to one that does exist without
    /// this fails with `428` (`precondition_required`), deliberately, so an agent cannot
    /// clobber a definition it never read.
    #[serde(default)]
    if_match: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InstanceStateParams {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// The service the instance belongs to.
    service: String,
    /// The block instance's id, as the service file spells it (SERVICE §2).
    instance: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReclaimOrphanParams {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// The orphaned namespace, as `list_orphans` reports it: `"service:instance"` (DAEMON §10).
    namespace: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateTapParams {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// The service the connection is in.
    service: String,
    /// The connection, as the service file spells it: `"t1.out -> t2.in"` (SERVICE §5).
    connection: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NodeTap {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// The tap's id, as `create_tap`/`list_taps` reported it.
    tap: String,
}

/// How many events to collect, and how long to wait for them, before `read_tap`/`read_logs`
/// answer with whatever arrived (see [`bounded_sse`] for why this exists at all).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadTapParams {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// The tap's id, as `create_tap`/`list_taps` reported it.
    tap: String,
    /// Stop once this many events have arrived. Default 20.
    #[serde(default)]
    max_events: Option<u32>,
    /// Stop after this many seconds even if fewer events arrived. Default 5, capped at 60 —
    /// an MCP tool call answers an agent waiting on it, unlike `eio tap`, which follows a
    /// connection forever for a person watching a terminal.
    #[serde(default)]
    timeout_seconds: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadLogsParams {
    /// Which node to address, by its name in `nodes.toml` (see the `list_nodes` tool).
    node: String,
    /// Only lines from this service, if given.
    #[serde(default)]
    service: Option<String>,
    /// Only lines from this instance, if given (DAEMON §11).
    #[serde(default)]
    instance: Option<String>,
    /// Stop once this many lines have arrived. Default 20.
    #[serde(default)]
    max_events: Option<u32>,
    /// Stop after this many seconds even if fewer lines arrived. Default 5, capped at 60.
    #[serde(default)]
    timeout_seconds: Option<u32>,
}

// ─── tools: one per DAEMON §9 operation this crate's `client::ENDPOINTS` addresses, plus
//     `list_nodes`, which is this CLI's own and maps to no operation (see the module doc) ───

// `vis = "pub"`: `Self::tool_router()` is otherwise crate-private, and `tests/mcp_openapi_drift.rs`
// and `tests/mcp_token_leak.rs` need `list_all()`'s tool metadata (names, descriptions, input
// schemas) from outside this crate, without spinning up a transport to ask for it.
//
// `#[allow(missing_docs)]`: the generated `pub fn tool_router()` has no doc comment of its own
// to give it (`rmcp_macros` writes the function, not this file), and it is test-facing plumbing
// rather than part of this binary's own documented surface.
#[allow(missing_docs)]
#[tool_router(vis = "pub")]
impl McpServer {
    /// What this node is: identity, limits, budgets and versions.
    ///
    /// Everything needed to decide whether a service can run here before deploying one — which is
    /// what makes it the first call an agent makes (SCOPE §4).
    #[tool(name = "node_info")]
    async fn node_info(
        &self,
        Parameters(p): Parameters<NodeOnly>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.node_info()).await
    }

    /// Every block cached on this node, with its manifest.
    ///
    /// The catalogue a service can be built from here without pulling anything. An entry whose
    /// bytes will not validate against ABI §4 is omitted and logged rather than failing the
    /// listing — one corrupt cache entry should not hide the rest of the node's blocks.
    ///
    /// `publisher` and `subscriber` (DAEMON §6.3) are always in it, whether or not anything has
    /// ever been cached: they are host-native, so being on this node at all is being available,
    /// and an agent or the Designer's palette (SCOPE §4) has no other way to learn they exist.
    #[tool(name = "list_blocks")]
    async fn list_blocks(
        &self,
        Parameters(p): Parameters<NodeOnly>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.list_blocks()).await
    }

    /// Pulls a block into this node's cache.
    ///
    /// Digest-verified, and signature-checked against this node's policy (DAEMON §4.1, §4.2).
    /// Pulling something already cached is not an error and does not re-fetch: the cache is
    /// consulted first, always, which is what lets a node with a warm cache work offline.
    #[tool(name = "pull_block")]
    async fn pull_block(
        &self,
        Parameters(p): Parameters<PullBlockParams>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.pull_block(&p.reference)).await
    }

    /// Every service on this node and its state.
    ///
    /// Names come from `services/*.toml` (DAEMON §2), so a file dropped in by hand or by a git
    /// checkout appears here after the next `reload` or restart, with no registration step.
    #[tool(name = "list_services")]
    async fn list_services(
        &self,
        Parameters(p): Parameters<NodeOnly>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.list_services()).await
    }

    /// One service: its definition text and its state.
    ///
    /// The `ETag` is the version a `PUT` must name to overwrite this definition (§9.3). It is
    /// opaque: a client carries it back in `If-Match` and never computes one.
    #[tool(name = "get_service")]
    async fn get_service(
        &self,
        Parameters(p): Parameters<NodeService>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || {
            let detail = client::connect(Some(&p.node))?.get_service(&p.service)?;
            Ok(merge_etag(detail.value, detail.etag))
        })
        .await
    }

    /// Writes a service definition, after checking its precondition and validating it.
    ///
    /// The body is the service file's text. Overwriting a service that already exists requires
    /// `If-Match` carrying the `ETag` a `GET` returned; a definition that does not validate, and one
    /// whose precondition fails, both change nothing — not the file, not the running service. The
    /// path's name must equal the body's `name` (SERVICE §1).
    ///
    /// On success the service is brought to what the file says, exactly as `reload` would.
    #[tool(name = "put_service")]
    async fn put_service(
        &self,
        Parameters(p): Parameters<PutServiceParams>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || {
            let (value, etag) = client::connect(Some(&p.node))?.put_service(
                &p.service,
                &p.definition,
                p.if_match.as_deref(),
            )?;
            Ok(merge_etag(value, etag))
        })
        .await
    }

    /// Deletes a service's definition file (DAEMON §9).
    ///
    /// Removes **the file only**, never the running graph's memory of it and never its
    /// `eio:state` (§10's "nothing removes a namespace as a side effect" is untouched by this
    /// endpoint on purpose — a deleted service's namespaces become orphans, reclaimable only
    /// through `DELETE /state/orphans/{namespace}`).
    ///
    /// **Refused while running**, with `409`: a `DELETE` that stopped a live service on a
    /// mistyped name is exactly the accident this API avoids elsewhere (§9.3's precondition,
    /// §10's orphan check), so this one asks for the same two calls rather than one that quietly
    /// does both. `POST /services/{s}/stop` first, then this.
    #[tool(name = "delete_service")]
    async fn delete_service(
        &self,
        Parameters(p): Parameters<NodeService>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || {
            client::connect(Some(&p.node))?.delete_service(&p.service)?;
            Ok(serde_json::json!({ "deleted": p.service }))
        })
        .await
    }

    /// Why a service is errored, structured.
    ///
    /// The same envelope every other failure uses (§9.2), so a client renders one code path. A
    /// service that is running or stopped has no errors and answers `404` — there is nothing to
    /// report, and an empty 200 would make "no errors" and "no such service" the same answer.
    #[tool(name = "service_errors")]
    async fn service_errors(
        &self,
        Parameters(p): Parameters<NodeService>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.service_errors(&p.service)).await
    }

    /// Starts a service, whatever its `autostart` says.
    ///
    /// Re-reads the file first, so a definition edited on disk takes effect. This is the deliberate
    /// override of the file's `autostart`, and `reload` is the deliberate revert (DAEMON §9.4).
    #[tool(name = "start_service")]
    async fn start_service(
        &self,
        Parameters(p): Parameters<NodeService>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.start_service(&p.service)).await
    }

    /// Stops a service, keeping its definition.
    ///
    /// ABI §5.1 step 5 for every instance: each is told to stop and its thread is joined, rather
    /// than having its mailbox closed underneath it. Stopping something already stopped is not an
    /// error — the caller asked for a state, and it is in it.
    #[tool(name = "stop_service")]
    async fn stop_service(
        &self,
        Parameters(p): Parameters<NodeService>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.stop_service(&p.service)).await
    }

    /// Re-reads the file and brings the service to what it says.
    ///
    /// Including its `autostart`: a service the file marks `autostart = false` ends stopped even if
    /// it was running because somebody called `start`. The file is the source of truth (SCOPE
    /// §3.8), and this is the operation that says so (DAEMON §9.4).
    #[tool(name = "reload_service")]
    async fn reload_service(
        &self,
        Parameters(p): Parameters<NodeService>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.reload_service(&p.service)).await
    }

    /// What one block instance has stored in `eio:state`.
    ///
    /// A debugging view of the node's state store (DAEMON §10), read through the same store and the
    /// same namespace the instance itself writes to — so what this shows is what the block would
    /// read back. Keys and values are opaque bytes to the ABI (§7.2), so both are reported base64,
    /// with a UTF-8 key and a canonically rendered value alongside where the bytes admit one.
    ///
    /// The instance need not be running: state outlives an instance, which is the whole point of it
    /// (ABI §5.1's "restart = new instance"). It need only be an instance the service declares.
    #[tool(name = "instance_state")]
    async fn instance_state(
        &self,
        Parameters(p): Parameters<InstanceStateParams>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || {
            client::connect(Some(&p.node))?.instance_state(&p.service, &p.instance)
        })
        .await
    }

    /// Every namespace this node's state store holds that no service file currently declares.
    ///
    /// DAEMON §10's safe default — nothing ever garbage-collects a namespace on its own — means a
    /// node accumulates these with no way to see them short of reading `state.redb` directly. This
    /// is that way: a scan of the store, each pair checked against the service files actually on
    /// disk right now, the same read [`instance_state`] performs one instance at a time.
    ///
    /// A namespace a *stopped* service declares does not appear here — stopping does not undeclare
    /// an instance, only running does that, so its state is exactly as reachable as it was before
    /// it stopped. What appears here is state an id removed from its file, or a deleted service,
    /// has stranded.
    #[tool(name = "list_orphans")]
    async fn list_orphans(
        &self,
        Parameters(p): Parameters<NodeOnly>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.orphans()).await
    }

    /// Reclaims exactly one orphaned namespace — the escape hatch for DAEMON §10's safe default.
    ///
    /// **This is the only operation that ever deletes a namespace.** Deleting a service, editing
    /// its file to drop an instance, restarting, and rebooting the node all leave state where it
    /// is; only this endpoint, named at exactly one namespace, removes anything. That is the whole
    /// point of eieio-8yq.13: the default stays safe, and reclaiming becomes possible instead of
    /// requiring a hand edit of `state.redb`.
    ///
    /// Refuses — deleting nothing — when `{namespace}` does not parse into a `service:instance`
    /// pair of valid ids, and refuses again, for a different reason, when it does but a service
    /// file on this node currently declares that instance: that is live state, not an orphan, and
    /// an API that let a typo delete it would be exactly the accident this default exists to
    /// prevent.
    #[tool(name = "reclaim_orphan")]
    async fn reclaim_orphan(
        &self,
        Parameters(p): Parameters<ReclaimOrphanParams>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || {
            client::connect(Some(&p.node))?.reclaim_orphan(&p.namespace)?;
            Ok(serde_json::json!({ "reclaimed": p.namespace }))
        })
        .await
    }

    /// Taps a connection, and answers the handle to stream it by.
    ///
    /// The connection must be one the service's file declares — a tap on an edge that does not
    /// exist would stream nothing forever, which is indistinguishable from a service that is
    /// simply quiet, and is the worst possible answer for a debugging tool.
    #[tool(name = "create_tap")]
    async fn create_tap(
        &self,
        Parameters(p): Parameters<CreateTapParams>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.create_tap(&p.service, &p.connection))
            .await
    }

    /// Every tap this node is holding.
    #[tool(name = "list_taps")]
    async fn list_taps(
        &self,
        Parameters(p): Parameters<NodeOnly>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || client::connect(Some(&p.node))?.list_taps()).await
    }

    /// Stops a tap and releases its ring.
    ///
    /// A client that simply disconnects releases the same resources — the subscription and the
    /// ring go with the stream — so this is for a caller that wants the registration gone too.
    #[tool(name = "delete_tap")]
    async fn delete_tap(
        &self,
        Parameters(p): Parameters<NodeTap>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || {
            client::connect(Some(&p.node))?.delete_tap(&p.tap)?;
            Ok(serde_json::json!({ "deleted": p.tap }))
        })
        .await
    }

    /// Streams what travels the tapped connection (DAEMON §9.6).
    ///
    /// Server-sent events. `signals` carries a batch as EXPR §7.6 canonical text, `expr_failure` a
    /// property expression that failed for a signal (code, span, message), `discarded` a batch that
    /// was routed and not delivered, and `lagged` the exact number of observations this reader
    /// missed while it was behind — the stream is complete until a client cannot keep up, and
    /// precisely quantified from then on.
    ///
    /// Bounded, unlike `eio tap`: this answers after `max_events` events or `timeout_seconds`,
    /// whichever comes first, with whatever it collected — nothing, if the connection stayed
    /// quiet, which is itself an answer an agent waiting on a tool call needs, since a call that
    /// could block forever is not one an agent can safely make (see `bounded_sse`'s doc comment).
    #[tool(name = "read_tap")]
    async fn read_tap(
        &self,
        Parameters(p): Parameters<ReadTapParams>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || {
            let (addr, token) = resolve_node(&p.node)?;
            let path = format!("/taps/{}/stream", p.tap);
            bounded_sse(
                &addr,
                &token,
                &path,
                &[],
                p.max_events.unwrap_or(DEFAULT_MAX_EVENTS),
                bounded_timeout(p.timeout_seconds),
            )
        })
        .await
    }

    /// Streams this node's log lines, filtered.
    ///
    /// Server-sent events, one `log` event per line, carrying the level, the message and the
    /// `(service, instance)` the line came from. Guest `log` calls (ABI §7.0) appear here tagged
    /// like the daemon's own.
    ///
    /// Bounded the same way `read_tap` is, for the same reason: an MCP tool call answers in
    /// bounded time rather than following the node's log forever the way `eio logs` does.
    #[tool(name = "read_logs")]
    async fn read_logs(
        &self,
        Parameters(p): Parameters<ReadLogsParams>,
    ) -> Result<Json<Value>, ErrorData> {
        run_blocking(move || {
            let (addr, token) = resolve_node(&p.node)?;
            let mut query = Vec::new();
            if let Some(service) = &p.service {
                query.push(("service", service.as_str()));
            }
            if let Some(instance) = &p.instance {
                query.push(("instance", instance.as_str()));
            }
            bounded_sse(
                &addr,
                &token,
                "/logs/stream",
                &query,
                p.max_events.unwrap_or(DEFAULT_MAX_EVENTS),
                bounded_timeout(p.timeout_seconds),
            )
        })
        .await
    }

    /// Every node this CLI can address, from `~/.config/eieio/nodes.toml` — the roster the `node`
    /// argument on every other tool draws from (SCOPE §4). Never reports a token, only whether
    /// one is configured, for the same reason `eio node list` does not (DAEMON §9.1).
    ///
    /// Local to this machine's configuration file: no daemon operation answers this — `nodes.toml`
    /// is this CLI's alone (`config.rs`'s module doc) — and there is deliberately no tool here
    /// that writes it. `eio node add`/`remove`/`set-default` stay how an operator manages the
    /// roster, the same way `eio service` stays how a service file gets authored (this crate's own
    /// module doc, "`service` is local; everything else is a node it named").
    #[tool(name = "list_nodes")]
    async fn list_nodes(&self) -> Result<Json<Value>, ErrorData> {
        run_blocking(|| {
            let config = crate::config::Config::load()?;
            let nodes: Vec<Value> = config
                .nodes
                .iter()
                .map(|(name, entry)| {
                    serde_json::json!({
                        "name": name,
                        "addr": entry.addr,
                        "token_configured": entry.token.is_some(),
                        "default": config.default.as_deref() == Some(name.as_str()),
                    })
                })
                .collect();
            Ok(serde_json::json!({ "nodes": nodes }))
        })
        .await
    }
}

#[tool_handler(
    instructions = "Every tool takes a `node` argument naming an entry in this machine's \
                    `nodes.toml`; call `list_nodes` first to see what is configured. A whole \
                    service can be built, deployed, started and introspected from here: author \
                    its TOML yourself, then `put_service`, `start_service`, `create_tap` on one \
                    of its connections, `read_tap` to see what travels it, `stop_service` when \
                    done."
)]
impl ServerHandler for McpServer {}

// ─── plumbing shared by every tool above ───

/// Where an anyhow error not built from a request (see the module doc's "auth" section)
/// becomes the one this server answers a tool call with.
fn tool_error(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{error:#}"), None)
}

/// Runs `f` on the blocking pool and turns its result into what a tool method returns.
///
/// Every `Client` call is blocking `ureq` (`client.rs`'s module doc), and this server keeps a
/// single-threaded runtime alive across many tool calls rather than one process per call the
/// way the rest of this binary is shaped — so unlike a plain `eio services start`, a blocking
/// call made directly on this runtime's one thread would stall every other request the peer
/// sends while it waits, including a concurrent call naming a different, healthy node.
async fn run_blocking(
    f: impl FnOnce() -> Result<Value> + Send + 'static,
) -> Result<Json<Value>, ErrorData> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(Json(value)),
        Ok(Err(error)) => Err(tool_error(error)),
        Err(join_error) => Err(ErrorData::internal_error(
            format!("the node call panicked: {join_error}"),
            None,
        )),
    }
}

/// Inserts a `GET`/`PUT`'s `ETag` into its JSON body as an `"etag"` field, so `get_service`'s
/// and `put_service`'s answers carry what `put_service`'s own `if_match` needs next, the same
/// way the header does for `eio services pull`/`push` (`client.rs`'s `ServiceDetail`).
fn merge_etag(mut value: Value, etag: Option<String>) -> Value {
    if let Value::Object(map) = &mut value {
        map.insert(
            String::from("etag"),
            etag.map(Value::String).unwrap_or(Value::Null),
        );
    }
    value
}

/// `nodes.toml`'s entry for `node`, as `(addr, token)` — [`read_tap`](McpServer::read_tap) and
/// [`read_logs`](McpServer::read_logs)'s own resolution, because they do not go through
/// [`crate::client::Client`] (see [`bounded_sse`] for why) and so cannot ask it for a
/// [`crate::client::Client`] to call through.
fn resolve_node(node: &str) -> Result<(String, String)> {
    let config = crate::config::Config::load()?;
    let (name, entry) = config.resolve(Some(node))?;
    let token = entry.token.clone().with_context(|| {
        format!(
            "node `{name}` has no token configured; `eio node add {name} --addr {} --token \
             <TOKEN>` with the token from that node's auth/token (DAEMON §9.1)",
            entry.addr
        )
    })?;
    Ok((entry.addr.clone(), token))
}

/// The default and the ceiling `read_tap`/`read_logs` clamp their `timeout_seconds` to.
const DEFAULT_MAX_EVENTS: u32 = 20;
const DEFAULT_TIMEOUT_SECONDS: u32 = 5;
const MAX_TIMEOUT_SECONDS: u32 = 60;

fn bounded_timeout(requested: Option<u32>) -> Duration {
    let seconds = requested
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
        .clamp(1, MAX_TIMEOUT_SECONDS);
    Duration::from_secs(u64::from(seconds))
}

/// A marker error: [`bounded_sse`]'s read stopped because it collected `max_events`, not
/// because anything went wrong. [`client::read_sse`]'s loop only exits early on `Err`, so this
/// is how the closure below asks it to stop without that stop being mistaken for a failure.
#[derive(Debug)]
struct EnoughEvents;

impl std::fmt::Display for EnoughEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "collected enough events")
    }
}

impl std::error::Error for EnoughEvents {}

/// One SSE event as `read_tap`/`read_logs` report it: the event name, and its data, parsed as
/// JSON where it is JSON — every event DAEMON §9.6 defines is — and left as text otherwise.
fn event_json(name: &str, data: &str) -> Value {
    let parsed =
        serde_json::from_str::<Value>(data).unwrap_or_else(|_| Value::String(String::from(data)));
    serde_json::json!({ "event": name, "data": parsed })
}

/// Opens `path` on `addr` as an SSE stream (DAEMON §9.6) and collects up to `max_events`,
/// stopping early — as a normal, non-error outcome, not a failure of the call — once `timeout`
/// has passed with nothing more arriving.
///
/// A dedicated `ureq::Agent` per call, not [`crate::client::Client`]'s: that one is shared by
/// every other command in this crate, including `eio tap`/`eio logs`, which are meant to follow
/// a connection forever for a person watching a terminal and so carry no read timeout at all
/// (`client.rs`'s `UreqTransport::new`). An MCP tool call cannot make that same promise to an
/// agent waiting on a response, so this builds its own agent with `timeout_global` set to the
/// caller's bound instead of reusing that one — which is also why the error path still goes
/// through [`crate::client::envelope_error`] rather than a second rendering of it (the module
/// doc's "auth" section).
fn bounded_sse(
    addr: &str,
    token: &str,
    path: &str,
    query: &[(&str, &str)],
    max_events: u32,
    timeout: Duration,
) -> Result<Value> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut request = agent
        .get(format!("{addr}{path}"))
        .header("authorization", format!("Bearer {token}"));
    for (name, value) in query {
        request = request.query(*name, *value);
    }
    let response = request
        .call()
        .map_err(|error| anyhow::anyhow!("GET {path}: {error}"))?;
    let status = response.status().as_u16();
    if status >= 400 {
        let body = response
            .into_body()
            .read_to_vec()
            .context("reading the error response")?;
        return Err(client::envelope_error(status, &body));
    }

    let mut events = Vec::new();
    let reader = response.into_body().into_reader();
    let result = client::read_sse(std::io::BufReader::new(reader), |name, data| {
        events.push(event_json(name, data));
        if events.len() as u32 >= max_events {
            return Err(anyhow::Error::new(EnoughEvents));
        }
        Ok(())
    });
    let reached_max_events = match result {
        Ok(()) => false,
        Err(error) if error.downcast_ref::<EnoughEvents>().is_some() => true,
        // Anything else reaching here is `timeout_global` firing on a quiet stream, or the
        // peer closing the connection: both mean "nothing more arrived", a normal answer for a
        // bounded read rather than a failure of this call.
        Err(_) => false,
    };
    Ok(serde_json::json!({
        "events": events,
        "reached_max_events": reached_max_events,
    }))
}
