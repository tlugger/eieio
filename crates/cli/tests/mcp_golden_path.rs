//! The plan's "transcript criterion": a real MCP client drives the real `eio mcp` server, over
//! its real stdio transport, through DAEMON-SPEC §9's golden path — create/put a service,
//! start it, open a tap, read it, stop it — against a real, in-process daemon.
//!
//! # Why this boots its own daemon rather than using `Harness`
//!
//! `crates/daemon/src/api/tests.rs`'s `Harness` is `#[cfg(test)]`-gated *inside* `eio-daemon`
//! itself (`crates/daemon/src/api.rs`'s `#[cfg(test)] pub mod tests;`), so it is compiled only
//! when `eio-daemon`'s own test binary is built — `--cfg test` is not passed when `eio-daemon`
//! is pulled in as a dev-dependency's *library*, which is what `eio-cli` does. This was checked
//! directly: a probe test naming `eio_daemon::api::tests::Harness` from this crate fails to
//! compile with "cannot find `tests` in `api` ... the item is gated here \[`#[cfg(test)]`\]".
//! `Harness` is therefore genuinely unreachable from here, exactly the case the plan's
//! verification section anticipates ("if some step is genuinely unreachable in-process,
//! implement everything that is reachable and report precisely what is not and why").
//!
//! What *is* reachable is `eio_daemon::run_node` — `lib.rs`'s own public counterpart, written
//! for precisely this ("a test outside this crate can call ... without spawning the binary").
//! It does not hand back the port it bound (there is no such API, `run_node` is a synchronous
//! `main`-body, not a harness), so this picks a free one itself by binding then releasing a
//! `TcpListener` before writing `node.toml`, then waits for `run_node`'s own bind to succeed by
//! polling the address — a small, accepted TOCTOU: nothing else in this test process or the
//! child it spawns claims a port, so nothing else can win the race in between.
//!
//! # What is driven, and how
//!
//! The daemon runs in-process, as a task on this test's own runtime, reachable only over a
//! real loopback socket (`DAEMON-SPEC §9`'s whole surface is HTTP; there is no shortcut here).
//! `eio mcp` runs as a real child *process* (`rmcp::transport::TokioChildProcess`), pointed at
//! a scratch `nodes.toml` naming that daemon, exactly as an operator's or agent's machine would
//! be set up. A plain `()` MCP client (`rmcp::ServiceExt`) is the one issuing `tools/call` —
//! this is not a call into `eio_cli::mcp`'s own Rust functions, it is the JSON-RPC wire
//! protocol, framed over the child's real stdin/stdout, exactly as an agent would speak it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::Value;

/// Cargo's per-integration-test scratch directory, cleaned with `target/`.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

/// A free loopback port, released before this returns — see this file's module doc for the
/// small race that leaves open, and why nothing here can lose it.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
        .port()
}

/// Boots a real node on a fresh port, with `transform` and `emitter` (ABI §13.2's golden
/// blocks) already in its cache — `transform` for the service the golden path deploys,
/// `emitter` because it is the one golden block that emits unprompted, which is what makes
/// `read_tap` see a real signal that actually travelled a connection rather than a fixture.
/// Returns the address to put in `nodes.toml` and the token `auth/token` was minted with.
async fn boot_daemon(root: &Path) -> (SocketAddr, String) {
    let port = free_port();
    std::fs::create_dir_all(root).expect("the data directory");
    std::fs::write(
        root.join("node.toml"),
        format!("id = \"golden-path\"\n[api]\nlisten = \"127.0.0.1:{port}\"\n"),
    )
    .expect("writing node.toml");

    for (name, file) in [("transform", "transform.wasm"), ("emitter", "emitter.wasm")] {
        let entry = root.join("blocks").join(name).join("1.0.0");
        std::fs::create_dir_all(&entry).expect("the cache entry");
        std::fs::copy(
            eio_conformance::golden::build().join(file),
            entry.join("block.wasm"),
        )
        .expect("the golden blocks are built");
    }

    let bus = std::sync::Arc::new(eio_daemon::observe::Bus::default());
    let data_dir = root.to_path_buf();
    tokio::spawn(async move {
        // `run_node` runs until asked to stop (SIGINT/SIGTERM); this test never asks, and lets
        // the whole process teardown at the end reclaim it, the same way `Harness`'s own
        // `axum::serve` task is never joined either (see `crates/daemon/src/api/tests.rs`).
        let _ = eio_daemon::run_node(&data_dir, bus).await;
    });

    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("a loopback address");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the node never started listening on {addr}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let token = std::fs::read_to_string(root.join("auth").join("token"))
        .expect("the node minted a token")
        .trim()
        .to_string();
    (addr, token)
}

/// Writes a `nodes.toml` naming `addr`/`token` as `"kitchen"`, and returns the directory to
/// give `eio mcp` as `XDG_CONFIG_HOME`.
fn write_nodes_toml(config_home: &Path, addr: SocketAddr, token: &str) {
    let eieio = config_home.join("eieio");
    std::fs::create_dir_all(&eieio).expect("the config directory");
    std::fs::write(
        eieio.join("nodes.toml"),
        format!("[nodes.kitchen]\naddr = \"http://{addr}\"\ntoken = \"{token}\"\n"),
    )
    .expect("writing nodes.toml");
}

/// Spawns `eio mcp` with `XDG_CONFIG_HOME` set to `config_home` and connects a plain MCP
/// client to it over its real stdio transport — the real binary, the real protocol.
async fn connect(config_home: &Path) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_eio"));
    command
        .arg("mcp")
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("HOME")
        .stderr(std::process::Stdio::null());
    let transport = TokioChildProcess::new(command).expect("spawning `eio mcp`");
    ().serve(transport)
        .await
        .expect("the MCP initialize handshake")
}

/// Calls `name`, asserts the call did not fail as a protocol-level tool error, and returns its
/// `structured_content` — the `Json<Value>` every tool in `mcp.rs` answers with.
async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: Value,
) -> Value {
    let params = CallToolRequestParams::new(String::from(name))
        .with_arguments(arguments.as_object().expect("a JSON object").clone());
    let result = client
        .call_tool(params)
        .await
        .unwrap_or_else(|error| panic!("`{name}` failed as a protocol error: {error}"));
    assert_ne!(
        result.is_error,
        Some(true),
        "`{name}` answered a tool-level error: {result:?}"
    );
    let rendered = format!("{result:?}");
    result
        .structured_content
        .unwrap_or_else(|| panic!("`{name}` answered no structured content: {rendered}"))
}

/// One block instance feeding another, with `emitter` as the source (ABI §13.2): it emits
/// `{n: 7}` once a second, unprompted, which is what makes `e1.out -> t1.in` a connection a
/// real running guest drives rather than a fixture (matching
/// `crates/daemon/src/api/tests.rs`'s own `wired_timer`, for the same reason).
const DEFINITION: &str = "\
name = \"kitchen\"
autostart = false
connections = [\"e1.out -> t1.in\"]

[blocks.e1]
block = \"emitter:1.0.0\"

[blocks.t1]
block = \"transform:1.0.0\"
[blocks.t1.props]
val = \"(+ $n 1)\"
";

#[tokio::test(flavor = "multi_thread")]
async fn a_service_is_built_deployed_started_tapped_and_stopped_through_mcp_alone() {
    let root = scratch("golden-path-node");
    let (addr, token) = boot_daemon(&root.join("data")).await;

    let config_home = scratch("golden-path-config");
    write_nodes_toml(&config_home, addr, &token);
    let client = connect(&config_home).await;

    // "create/put a service"
    let put = call(
        &client,
        "put_service",
        serde_json::json!({
            "node": "kitchen",
            "service": "kitchen",
            "definition": DEFINITION,
        }),
    )
    .await;
    assert_eq!(put["state"], "stopped", "autostart = false: {put}");
    assert_eq!(put["name"], "kitchen", "{put}");
    assert!(
        put["etag"].is_string(),
        "a create answers an etag too: {put}"
    );

    // `get_service` is where the definition text round-trips (DAEMON §9: `PUT`'s own answer is
    // just the summary `put_service`'s output above already checked).
    let fetched = call(
        &client,
        "get_service",
        serde_json::json!({ "node": "kitchen", "service": "kitchen" }),
    )
    .await;
    assert_eq!(fetched["definition"], DEFINITION, "{fetched}");

    // "start it"
    let started = call(
        &client,
        "start_service",
        serde_json::json!({ "node": "kitchen", "service": "kitchen" }),
    )
    .await;
    assert_eq!(started["state"], "running", "{started}");

    let listed = call(
        &client,
        "list_services",
        serde_json::json!({ "node": "kitchen" }),
    )
    .await;
    assert_eq!(
        listed
            .as_array()
            .expect("a list")
            .iter()
            .find(|entry| entry["name"] == "kitchen")
            .expect("kitchen is listed")["state"],
        "running",
        "{listed}"
    );

    // "open a tap"
    let tap = call(
        &client,
        "create_tap",
        serde_json::json!({
            "node": "kitchen",
            "service": "kitchen",
            "connection": "e1.out -> t1.in",
        }),
    )
    .await;
    assert_eq!(tap["service"], "kitchen", "{tap}");
    assert_eq!(tap["instance"], "e1", "{tap}");
    let tap_id = tap["id"].as_str().expect("an id").to_string();

    // "read it": `emitter` fires on its own hard-coded one-second period, so this genuinely
    // waits on wall time — generous against CI jitter, not a tight race (matching
    // `crates/daemon/src/api/tests.rs`'s own tap-stream tests, which wait on the same block).
    let read = call(
        &client,
        "read_tap",
        serde_json::json!({
            "node": "kitchen",
            "tap": tap_id,
            "max_events": 2,
            "timeout_seconds": 15,
        }),
    )
    .await;
    let events = read["events"].as_array().expect("an events array");
    assert!(
        !events.is_empty(),
        "read_tap saw nothing from a real running emitter in 15s: {read}"
    );
    assert!(
        events.iter().any(|event| event["event"] == "signals"),
        "a real signal, over the tap: {read}"
    );

    // "stop it"
    let stopped = call(
        &client,
        "stop_service",
        serde_json::json!({ "node": "kitchen", "service": "kitchen" }),
    )
    .await;
    assert_eq!(stopped["state"], "stopped", "{stopped}");

    client.cancel().await.ok();
}
