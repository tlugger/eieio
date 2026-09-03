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
//! Sections 1–6 below write `nodes.toml` by hand rather than through `eio node add`: they
//! predate `eio node add --class` (eieio-x7g.5 landed the guard first; the flag came with the
//! integration fix eieio-x7g.9 covers) and still stand in for the other ways a `class` key
//! reaches the file — an operator hand-editing it, or a Designer export (eieio-m9s.6) —
//! the same way any other TOML file in this system is authored (SERVICE §9). Section 7 below
//! is the flag itself: `eio node add --class` is real now, and `Config::add` takes a
//! `NodeClass` argument — see `src/config.rs`.

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

// ─── 7: `eio node add --class` itself (eieio-x7g.9) ───
//
// Everything above hand-writes `nodes.toml`; these three drive the flag `Config::add` takes.

/// Parses `nodes.toml` and returns the `[nodes.<name>]` table, so the tests below can assert
/// on `class`'s presence or absence structurally — via the parsed document, not by scanning
/// the file's text for the substring `"class"` — which is the same choice `nodes_export.rs`
/// makes for the analogous JSON assertions, and for the same reason: a structural read cannot
/// be fooled by the string appearing somewhere unrelated (a comment, a different node's own
/// key), where a substring search silently would be.
fn node_table(nodes_toml_path: &Path, name: &str) -> toml::Value {
    let text = std::fs::read_to_string(nodes_toml_path).expect("reading nodes.toml");
    let doc: toml::Value = toml::from_str(&text).expect("nodes.toml parses as TOML");
    doc.get("nodes")
        .and_then(|nodes| nodes.get(name))
        .unwrap_or_else(|| panic!("no [nodes.{name}] table in:\n{text}"))
        .clone()
}

#[test]
fn add_dash_dash_class_leaf_writes_the_class_key() {
    let config_home = scratch("add-class-leaf");
    let path = config_home.join("eieio").join("nodes.toml");

    ok(
        &config_home,
        &[
            "node",
            "add",
            "porch-sensor",
            "--addr",
            "http://10.0.0.7:7777",
            "--class",
            "leaf",
            "--default",
        ],
    );

    let table = node_table(&path, "porch-sensor");
    assert_eq!(
        table.get("class").and_then(toml::Value::as_str),
        Some("leaf"),
        "--class leaf must write class = \"leaf\": {table:?}"
    );

    // Not just present as text — effective: a reaching command is refused, naming the class.
    let refusal = refused(&config_home, &["node", "info"]);
    assert!(refusal.contains("leaf"), "{refusal}");
}

#[test]
fn add_dash_dash_class_daemon_writes_no_class_key() {
    // THE test that matters most (see this crate's plan): `--class daemon` must write NO
    // `class` key at all, because `NodeEntry::class`'s `skip_serializing_if` exists precisely
    // to keep an explicitly-`daemon` entry — and every `nodes.toml` written before `class`
    // existed — from gaining a redundant key the next time the file is saved. A test that
    // instead checked for `class = "daemon"` would pass against code that serializes the
    // default unconditionally, which is exactly the regression this field's
    // `skip_serializing_if` exists to prevent — it would be asserting the wrong string and
    // passing vacuously (this file's own module doc / the plan for this bead).
    //
    // Asserted structurally (parsed TOML, `!table.contains_key`), not by scanning the file's
    // raw text for the substring `"class"` — see `node_table`'s doc.
    let config_home = scratch("add-class-daemon");
    let path = config_home.join("eieio").join("nodes.toml");

    ok(
        &config_home,
        &[
            "node",
            "add",
            "kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--class",
            "daemon",
        ],
    );

    let table = node_table(&path, "kitchen");
    assert!(
        !table
            .as_table()
            .expect("a node entry is a TOML table")
            .contains_key("class"),
        "--class daemon must write NO class key at all, not class = \"daemon\": {table:?}"
    );
}

#[test]
fn add_without_class_flag_behaves_the_same_as_dash_dash_class_daemon() {
    let config_home = scratch("add-class-omitted");
    let path = config_home.join("eieio").join("nodes.toml");

    ok(
        &config_home,
        &[
            "node",
            "add",
            "kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--default",
        ],
    );

    let table = node_table(&path, "kitchen");
    assert!(
        !table
            .as_table()
            .expect("a node entry is a TOML table")
            .contains_key("class"),
        "omitting --class must write no class key, same as --class daemon: {table:?}"
    );

    // Effective, not just textual: a reaching command gets past the guard unrefused.
    let refusal = refused(&config_home, &["node", "info"]);
    assert!(refusal.contains("no token configured"), "{refusal}");
    assert!(!refusal.contains("leaf"), "{refusal}");
}
