//! `cargo eio`, end to end (SDK-SPEC §5).
//!
//! Every test here generates a block and runs the real subcommands over it, because the
//! claims being made are about what a block author's first hour looks like and none of them
//! can be checked any other way: that the template builds unedited, that the module it
//! produces is one a *host* accepts, that the size profile is enforced rather than suggested,
//! and that a broken block fails loudly.
//!
//! The generated blocks depend on this checkout through `--sdk-path` (§5.1). That is what
//! makes "the template builds out of the box" a measurement today, before `eio-sdk` reaches
//! crates.io.
//!
//! **Each generated block costs a full dependency compile**, so the tests that need one are
//! deliberately few and the failure cases share a single block. Where that groups several
//! assertions into one test, each carries its own message — an isolated failure is worth
//! something, a minute of CI per assertion is not.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Where generated blocks go: cargo's own per-test scratch directory, cleaned with `target/`.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

/// This checkout's `crates/block-sdk`, for `--sdk-path`.
fn sdk_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../block-sdk")
}

/// Runs `cargo eio <args>` in `dir`.
///
/// `CARGO_BIN_EXE_cargo-eio` is the binary this test crate was built alongside, so no test
/// here can pass against a stale `cargo install`.
fn eio(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-eio"))
        .arg("eio")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cargo-eio runs")
}

/// The cargo that invoked this test.
fn cargo(dir: &Path, args: &[&str]) -> Output {
    Command::new(std::env::var_os("CARGO").expect("cargo invoked this test"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cargo runs")
}

/// `stdout` and `stderr` together, for an assertion message that says what happened.
fn transcript(output: &Output) -> String {
    format!(
        "status: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Generates a block called `my-block` and returns its root.
fn new_block(scratch: &Path) -> PathBuf {
    let output = eio(
        scratch,
        &[
            "new",
            "my-block",
            "--sdk-path",
            sdk_path().to_str().expect("a UTF-8 path"),
        ],
    );
    assert!(output.status.success(), "{}", transcript(&output));
    scratch.join("my-block")
}

/// The emitted module, given a block root.
fn module(root: &Path) -> PathBuf {
    root.join("target/wasm32-unknown-unknown/release/my_block.wasm")
}

/// Replaces `from` with `to` in `path`, failing if it was not there.
///
/// A test that "broke" a file it did not actually change passes for the wrong reason and
/// keeps passing after the template moves on.
fn rewrite(path: &Path, from: &str, to: &str) {
    let source =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let rewritten = source.replace(from, to);
    assert_ne!(
        rewritten,
        source,
        "{} does not contain {from:?}",
        path.display()
    );
    std::fs::write(path, rewritten).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

#[test]
fn the_template_builds_and_passes_its_own_tests_unedited() {
    let scratch = scratch("unedited");
    let root = new_block(&scratch);

    // Not `build` then `test`: `test` runs both of SDK §6's layers itself (§5.3), and that it
    // does is half the claim.
    let output = eio(&root, &["test"]);
    assert!(output.status.success(), "{}", transcript(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Conformance: 1 scenario(s) passed"),
        "the conformance layer ran: {}",
        transcript(&output)
    );

    // ABI §4: the module passes the full load-time check through the implementation a host
    // uses — and against the manifest the `#[block]` macro embedded (§4.4), since no registry
    // manifest was supplied.
    let wasm = std::fs::read(module(&root)).expect("the module was built");
    let embedded = eio_manifest::validate(&wasm, None).expect("a host would load this module");

    let written =
        std::fs::read_to_string(root.join("target/wasm32-unknown-unknown/release/manifest.json"))
            .expect("manifest.json was written beside the module");
    let parsed = eio_manifest::parse(&written).expect("and it parses under ABI §11.1");
    assert_eq!(parsed, embedded, "the written manifest is the embedded one");
    assert_eq!(parsed.name, "my-block");
    assert_eq!(parsed.input_index("in"), Some(0));
    assert_eq!(parsed.output_index("out"), Some(0));
    assert_eq!(parsed.prop_id("next"), Some(0));
}

#[test]
fn the_size_profile_is_enforced_not_suggested() {
    // SDK §5.2's whole point. The block's own `[profile.release]` is rewritten to the loosest
    // settings a block author could plausibly land on — including `panic = "unwind"`, which
    // SDK §4 forbids — and `cargo eio build` must produce the same small module regardless.
    let scratch = scratch("profile");
    let root = new_block(&scratch);
    rewrite(
        &root.join("Cargo.toml"),
        "panic = \"abort\"\nopt-level = \"z\"\nlto = true\nstrip = true",
        "panic = \"unwind\"\nopt-level = 0\nlto = false\nstrip = false",
    );

    let output = eio(&root, &["build"]);
    assert!(output.status.success(), "{}", transcript(&output));
    let enforced = std::fs::metadata(module(&root))
        .expect("the module was built")
        .len();

    // And the same crate built the way its own manifest now asks, which is what the override
    // is measured against: without enforcement, this is what would ship.
    let output = cargo(
        &root,
        &["build", "--release", "--target", "wasm32-unknown-unknown"],
    );
    assert!(output.status.success(), "{}", transcript(&output));
    let unenforced = std::fs::metadata(module(&root))
        .expect("the module was built")
        .len();

    assert!(
        unenforced > enforced * 4,
        "the loosened profile should produce a far larger module ({unenforced} vs {enforced}); \
         if it does not, this test has stopped measuring anything"
    );
}

#[test]
fn a_broken_block_fails_loudly() {
    // Three failure modes on one block, in an order that leaves each one's damage behind:
    // the module compiles once and the failures are cheap after that.
    let scratch = scratch("broken");
    let root = new_block(&scratch);

    // 1. A conformance expectation that does not hold. Proving the harness layer can fail:
    //    one wired in but never exercised in the failing direction is one nobody would notice
    //    was inert. `182a` is 42; `182b` is 43.
    let scenario = root.join("conformance/lifecycle.json");
    rewrite(&scenario, "a1616e182a", "a1616e182b");
    let output = eio(&root, &["test"]);
    assert!(!output.status.success(), "{}", transcript(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("conformance scenario(s) failed"),
        "the failing scenario is reported: {}",
        transcript(&output)
    );

    // 2. No scenarios at all: the native layer still runs, and `test` says which half of
    //    SDK §6 it covered rather than reporting a pass that means less than it looks.
    std::fs::remove_dir_all(root.join("conformance")).expect("removing the scenarios");
    let output = eio(&root, &["test"]);
    assert!(output.status.success(), "{}", transcript(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("ran the native tests only"),
        "{}",
        transcript(&output)
    );

    // 3. A block name ABI §11.1 refuses. The `#[block]` macro rejects it at expansion, so the
    //    failure arrives through the compiler — which is the point: one rule, one
    //    implementation, enforced where the author can act on it.
    rewrite(&root.join("src/lib.rs"), "\"my-block\"", "\"My-Block\"");
    let output = eio(&root, &["build"]);
    assert!(!output.status.success(), "{}", transcript(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("My-Block"),
        "the rejection names the offending value: {}",
        transcript(&output)
    );
}

#[test]
fn the_template_is_rustfmt_clean() {
    // A generated file `cargo fmt --check` rejects makes a block author's first commit a
    // formatting commit, and teaches them the template is not held to the standard the
    // platform holds itself to. Costs no compile.
    let scratch = scratch("rustfmt");
    let root = new_block(&scratch);

    let output = cargo(&root, &["fmt", "--check"]);
    assert!(output.status.success(), "{}", transcript(&output));
}

#[test]
fn a_name_abi_11_1_refuses_is_refused_before_anything_is_written() {
    let scratch = scratch("refused-name");

    // The last is §11.1-legal and cargo-illegal: a block name is a registry reference
    // component and may carry a dot, a cargo package name may not (§5.1).
    for name in ["My-Block", "-leading", "a name", "my.block"] {
        let output = eio(&scratch, &["new", name]);
        assert!(
            !output.status.success(),
            "{name:?}: {}",
            transcript(&output)
        );
        assert!(
            !scratch.join(name).exists(),
            "{name:?} left a directory behind"
        );
    }
}
