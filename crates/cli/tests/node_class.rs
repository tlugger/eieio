//! `eio` learns a node's `class` (SCOPE §3.7's amendment, eieio-x7g.5) — the guard that refuses
//! to dial a `leaf`-class node rather than reporting the connection error dialling one gives,
//! indistinguishable from a daemon that is genuinely down (LEAF §7).
//!
//! Same posture as `tests/node_config.rs`: every test here runs the real binary with
//! `XDG_CONFIG_HOME` pointed at a scratch directory that starts out empty, `HOME` cleared, and
//! no test reaches the network or a real daemon. A "reaching" command — one that dials a node
//! over HTTP once resolution succeeds — is proven to have gotten *past* resolution the same way
//! `tests/node_config.rs` already does: by naming an entry with no token and reading the
//! deterministic, local "no token configured" failure `client::connect` produces before it would
//! ever open a socket (`src/client.rs`'s `connect`). A `leaf` entry short-circuits before that
//! check even runs, which is exactly what distinguishes "refused by the guard" from "refused for
//! any other reason" in the tests below.
//!
//! `nodes.toml` is written by hand here, not through `eio node add`, because there is no
//! `eio node add --class` — SCOPE §3.7's `class` is something an operator (or a Designer export,
//! eieio-m9s.6) hand-edits into the file, the same way any other TOML file in this system is
//! authored (SERVICE §9). See `src/config.rs`'s `Config::add` for why.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Cargo's per-integration-test scratch directory, cleaned with `target/`.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

/// Writes `nodes.toml` directly, skipping `eio node add` (see this file's module doc).
fn write_nodes_toml(config_home: &Path, text: &str) -> PathBuf {
    let dir = config_home.join("eieio");
    std::fs::create_dir_all(&dir).expect("creating the config directory");
    let path = dir.join("nodes.toml");
    std::fs::write(&path, text).expect("writing nodes.toml");
    path
}

/// Runs `eio <args>` with `XDG_CONFIG_HOME` set to `config_home`. `HOME` is cleared for the same
/// reason `tests/node_config.rs` clears it: a resolution bug should fail loudly on a nonexistent
/// `$HOME/.config` rather than silently land on a real machine.
fn eio(config_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_eio"))
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("HOME")
        .output()
        .expect("eio runs")
}

fn ok(config_home: &Path, args: &[&str]) -> String {
    let output = eio(config_home, args);
    assert!(
        output.status.success(),
        "eio {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

fn refused(config_home: &Path, args: &[&str]) -> String {
    let output = eio(config_home, args);
    assert!(
        !output.status.success(),
        "eio {args:?} was supposed to fail, and printed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).expect("utf-8 stderr")
}

/// A minimal `nodes.toml`: one node named `kitchen`, no token, set as the default so `--node`
/// need not be passed — and, critically, no token, so a "reaching" command that gets past the
/// class guard fails deterministically and locally on `connect`'s own token check rather than
/// ever opening a socket (this file's module doc).
fn no_class_toml() -> String {
    String::from(
        "default = \"kitchen\"\n\n\
         [nodes.kitchen]\n\
         addr = \"http://10.0.0.5:7777\"\n",
    )
}

// ─── 1: the compatibility promise — no `class` key behaves exactly as it does today ───

#[test]
fn absent_class_reaches_the_token_check_unrefused_and_is_never_written_back() {
    let config_home = scratch("absent-class");
    let path = write_nodes_toml(&config_home, &no_class_toml());

    // A "reaching" command gets past resolution (proven by reaching the deterministic,
    // network-free "no token configured" failure) rather than being refused by the guard.
    let refusal = refused(&config_home, &["node", "info"]);
    assert!(
        refusal.contains("no token configured"),
        "an absent `class` must behave exactly as before eieio-x7g.5: {refusal}"
    );
    assert!(
        !refusal.contains("leaf"),
        "an absent `class` must never be refused as a leaf: {refusal}"
    );

    // Naming-only commands work too, unaffected either way.
    let listing = ok(&config_home, &["node", "list"]);
    assert!(listing.contains("kitchen"), "{listing}");

    // Writing the file back (`node add` of a second, unrelated node exercises `Config::save`)
    // must not mint a `class` key for an entry that never had one.
    ok(
        &config_home,
        &["node", "add", "garage", "--addr", "http://10.0.0.9:7777"],
    );
    let text = std::fs::read_to_string(&path).expect("reading nodes.toml");
    assert!(
        !text.contains("class"),
        "an absent `class` must not gain the key on the next write: {text}"
    );
}

// ─── 2: `class = "daemon"` behaves the same as absent ───

#[test]
fn explicit_daemon_class_behaves_the_same_as_absent() {
    let config_home = scratch("explicit-daemon");
    write_nodes_toml(
        &config_home,
        "default = \"kitchen\"\n\n\
         [nodes.kitchen]\n\
         addr = \"http://10.0.0.5:7777\"\n\
         class = \"daemon\"\n",
    );

    let refusal = refused(&config_home, &["node", "info"]);
    assert!(
        refusal.contains("no token configured"),
        "class = \"daemon\" must behave exactly like an absent `class`: {refusal}"
    );
    assert!(!refusal.contains("leaf"), "{refusal}");

    let listing = ok(&config_home, &["node", "list"]);
    assert!(listing.contains("kitchen"), "{listing}");
}

// ─── 3: `class = "leaf"` on a command that reaches the node: refused, non-zero, names "leaf" ───

#[test]
fn leaf_class_is_refused_before_dialing_naming_the_class() {
    let config_home = scratch("leaf-refused");
    write_nodes_toml(
        &config_home,
        "default = \"porch-sensor\"\n\n\
         [nodes.porch-sensor]\n\
         addr = \"http://10.0.0.7:7777\"\n\
         class = \"leaf\"\n",
    );

    // `eio node info` — the one `eio node` subcommand that reaches a node.
    let refusal = refused(&config_home, &["node", "info"]);
    assert!(refusal.contains("leaf"), "{refusal}");
    assert!(
        !refusal.to_lowercase().contains("connection")
            && !refusal.to_lowercase().contains("refused connection"),
        "the message must be about the node's class, not a failed request: {refusal}"
    );

    // The guard lives in one place (`Config::resolve`), so a second, unrelated command that
    // reaches a node is refused the same way, with no per-command guard to have forgotten.
    let refusal = refused(&config_home, &["services", "list"]);
    assert!(refusal.contains("leaf"), "{refusal}");
}

// ─── 4: `class = "leaf"` on a command that only names it: still succeeds ───

#[test]
fn leaf_class_still_permits_naming_only_operations() {
    let config_home = scratch("leaf-naming-only");
    let path = write_nodes_toml(
        &config_home,
        "[nodes.porch-sensor]\n\
         addr = \"http://10.0.0.7:7777\"\n\
         class = \"leaf\"\n\n\
         [nodes.kitchen]\n\
         addr = \"http://10.0.0.5:7777\"\n",
    );

    // `list` names every configured node, leaf included — refusing it because one entry
    // happens to be a leaf would be a worse bug than the one this bead fixes.
    let listing = ok(&config_home, &["node", "list"]);
    assert!(listing.contains("porch-sensor"), "{listing}");
    assert!(listing.contains("kitchen"), "{listing}");

    // `set-default` only writes a name into the file; it never resolves, let alone dials.
    ok(&config_home, &["node", "set-default", "porch-sensor"]);
    let text = std::fs::read_to_string(&path).expect("reading nodes.toml");
    assert!(text.contains("default = \"porch-sensor\""), "{text}");

    // `remove` is the same shape: it drops a name from the map without ever resolving it.
    ok(&config_home, &["node", "remove", "porch-sensor"]);
    let after = ok(&config_home, &["node", "list"]);
    assert!(!after.contains("porch-sensor"), "{after}");
}

// ─── 5: an unknown class value is a config error naming the bad value ───

#[test]
fn an_unknown_class_value_is_a_config_error_naming_it() {
    let config_home = scratch("unknown-class");
    write_nodes_toml(
        &config_home,
        "[nodes.kitchen]\n\
         addr = \"http://10.0.0.5:7777\"\n\
         class = \"toaster\"\n",
    );

    // The file fails to parse at all — `Config::load` fails before resolution, so even a
    // naming-only command like `list` surfaces it, never a silent fall back to `daemon`.
    let refusal = refused(&config_home, &["node", "list"]);
    assert!(
        refusal.contains("toaster"),
        "an unknown class must name the bad value, not silently become `daemon`: {refusal}"
    );
}

// ─── 6: round-trip preserves an explicit `leaf` and never mints a `class` for an absent one ───

#[test]
fn round_trip_preserves_leaf_and_never_mints_class_for_absent() {
    let config_home = scratch("round-trip");
    let path = write_nodes_toml(
        &config_home,
        "[nodes.porch-sensor]\n\
         addr = \"http://10.0.0.7:7777\"\n\
         class = \"leaf\"\n\n\
         [nodes.kitchen]\n\
         addr = \"http://10.0.0.5:7777\"\n",
    );

    // Trigger a write (`Config::save`) via a command that touches neither entry directly.
    ok(
        &config_home,
        &["node", "add", "garage", "--addr", "http://10.0.0.9:7777"],
    );

    let text = std::fs::read_to_string(&path).expect("reading nodes.toml");
    assert!(
        text.contains("class = \"leaf\""),
        "porch-sensor's class must survive a write it was not party to: {text}"
    );
    // Exactly one `class` key in the whole file: porch-sensor's. `kitchen` and the newly added
    // `garage` — both `Daemon` — must not have gained one.
    assert_eq!(
        text.matches("class").count(),
        1,
        "an absent (or default) `class` must not be minted on write: {text}"
    );
}
