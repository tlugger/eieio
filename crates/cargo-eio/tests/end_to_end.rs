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

use cargo_eio::build::{PROFILE, SHADOW_STACK_BYTES, shadow_stack};

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

    // SDK §5.2's shadow-stack default, reaching the template through its own
    // `.cargo/config.toml` (§5.1) rather than through the flag `build` passes. Pinned on the
    // generated repo because that file is the one a block author edits, and a template that
    // shipped without it would put every new block back on `wasm-ld`'s 1 MiB stack — 17
    // declared pages, which LEAF §4.2's v1 leaf refuses outright.
    assert_eq!(
        eio_manifest::Module::read(&wasm)
            .expect("a readable module")
            .min_pages,
        Some(1),
        "the template's module declares more than one page of linear memory"
    );
    assert_eq!(
        stack_sizes(
            &std::fs::read_to_string(root.join(".cargo/config.toml"))
                .expect("the template wrote a .cargo/config.toml")
        ),
        vec![SHADOW_STACK_BYTES],
        "the generated `.cargo/config.toml` does not carry `build::SHADOW_STACK_BYTES` — \
         the template's placeholder was not substituted, or came from somewhere else"
    );

    // And the same block with that file removed, which is the *other* route to the same
    // number: `build`'s own low-priority `build.rustflags`. Nothing else in this suite
    // exercises it, because every block it builds sits under a `.cargo/config.toml` whose
    // `[target.<triple>]` rustflags outrank it by design (§5.2).
    std::fs::remove_file(root.join(".cargo/config.toml")).expect("removing the cargo config");
    let output = eio(&root, &["build"]);
    assert!(output.status.success(), "{}", transcript(&output));
    let wasm = std::fs::read(module(&root)).expect("the module was built");
    assert_eq!(
        eio_manifest::Module::read(&wasm)
            .expect("a readable module")
            .min_pages,
        Some(1),
        "with no `.cargo/config.toml`, `cargo eio build`'s own `build.rustflags` is the only \
         thing setting the shadow stack, and it has stopped working"
    );

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
fn the_generated_workflow_carries_the_tag_triggered_publish_job() {
    // SDK §5's "the template repo's CI runs build/test/publish on tag" — checked as a string
    // match rather than parsed as YAML, because the claim under test is "the job is there and
    // calls the right subcommand", not "this is well-formed GitHub Actions", which a schema
    // nobody here owns would be a stranger thing for this crate to depend on. Costs no compile.
    let scratch = scratch("workflow");
    let root = new_block(&scratch);
    let workflow = std::fs::read_to_string(root.join(".github/workflows/ci.yml"))
        .expect("the workflow was written");

    assert!(
        workflow.contains("tags:"),
        "triggers on a tag push: {workflow}"
    );
    assert!(
        workflow.contains("publish:") && workflow.contains("cargo eio publish"),
        "a publish job that calls the subcommand this issue implements: {workflow}"
    );
    assert!(
        workflow.contains("COSIGN_KEY") && workflow.contains("if [ -f cosign.key ]"),
        "signing stays optional — no key configured means no --key flag (DAEMON §4.2): {workflow}"
    );
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

#[test]
fn the_golden_blocks_build_through_the_tooling_a_block_author_uses() {
    // ABI §13.2's five blocks are the closest thing the repository has to blocks somebody
    // else wrote, and `cargo eio build` is what somebody else would run. Asserted here rather
    // than in a shell recipe so it runs wherever the rest of the suite does — and what it
    // buys over the plain `cargo build` the conformance harness does is the ABI §4 load-time
    // check on every one of them, at the point that can say which block stopped being
    // loadable.
    let blocks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/blocks");

    for name in ["transform", "filter", "counter", "emitter", "gpio-echo"] {
        let manifest = blocks.join(name).join("Cargo.toml");
        let output = eio(
            &blocks,
            &[
                "build",
                "--manifest-path",
                manifest.to_str().expect("a UTF-8 path"),
            ],
        );
        assert!(output.status.success(), "{name}: {}", transcript(&output));

        let manifest_json = blocks
            .join("target/wasm32-unknown-unknown/release/manifest.json")
            .to_path_buf();
        let written = std::fs::read_to_string(&manifest_json).expect("a manifest was written");
        let parsed = eio_manifest::parse(&written).expect("and it parses under ABI §11.1");
        assert_eq!(
            parsed.name, name,
            "the manifest describes the block just built"
        );

        // SDK §5.2's `-zstack-size` default over the blocks that had 17 declared pages
        // before it existed. Reaching them here through `examples/blocks/.cargo/config.toml`,
        // whose `[target.<triple>]` rustflags outrank the `build.rustflags` this command
        // passes (§5.2) — the tool's own default is exercised by
        // `the_template_builds_and_passes_its_own_tests_unedited`, on a block with that file
        // removed. `crates/conformance/tests/golden.rs` pins the same page count over the
        // plain `cargo build` the harness uses.
        let lib = name.replace('-', "_");
        let wasm =
            std::fs::read(blocks.join(format!("target/wasm32-unknown-unknown/release/{lib}.wasm")))
                .expect("the module was built");
        assert_eq!(
            eio_manifest::Module::read(&wasm)
                .expect("a readable module")
                .min_pages,
            Some(1),
            "{name} declares more than one page of linear memory"
        );
    }
}

/// SDK §5.2's invocation, as the specification states it right now.
///
/// Extracted rather than restated, the way `crates/manifest/tests/roundtrip.rs` extracts ABI
/// §11's example manifest instead of keeping a second copy of it: a literal in this file
/// would be one more place the number is written, which is the defect this test exists to
/// close.
fn spec_config_overrides() -> Vec<String> {
    let spec = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/specs/SDK-SPEC.md",
    ))
    .expect("SDK-SPEC.md is two directories up from this crate");

    let fenced = spec
        .split_once("### 5.2 Size-optimization defaults")
        .expect("SDK-SPEC has a §5.2")
        .1
        .split_once("```\n")
        .expect("§5.2 opens with a fenced invocation")
        .1
        .split_once("```")
        .expect("the fence closes")
        .0;

    fenced
        .lines()
        .filter_map(|line| line.trim().strip_prefix("--config "))
        .map(|setting| setting.trim_matches('\'').to_string())
        .collect()
}

/// Every `-zstack-size=<n>` written literally in `text`, as numbers.
///
/// A `-zstack-size=` followed by something that is not a decimal — the template's
/// placeholder, and nothing else in this repository — yields nothing, which is what makes an
/// empty result the assertion that a file hard-codes no stack size at all.
fn stack_sizes(text: &str) -> Vec<u32> {
    text.match_indices("-zstack-size=")
        .filter_map(|(at, needle)| {
            text[at + needle.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        })
        .collect()
}

#[test]
fn the_shadow_stack_size_is_written_once() {
    // The number reaches four places and is *chosen* in one. `build::SHADOW_STACK_BYTES` is
    // that one: `build` formats its `--config` override from it and `new` renders the
    // template's `.cargo/config.toml` from it, so neither can drift. What is left is the two
    // restatements a Rust constant cannot reach — a specification, and a separate cargo
    // workspace — and this test is what makes those fail loudly instead of quietly building
    // at a different stack size. That was the whole defect: a copy that had wandered to
    // 32 KiB still produces a one-page module and still passes every `min_pages == 1`
    // assertion in this repository.

    // SDK §5.2 is the decision, not a copy of it, so the spec keeps stating the number and
    // the code is pinned against what it states — including the four profile overrides, which
    // reach cargo on the same command line and have exactly the same drift to fear.
    let stated = spec_config_overrides();
    let passed: Vec<String> = PROFILE
        .iter()
        .map(|setting| (*setting).to_string())
        .chain([shadow_stack()])
        .collect();
    assert_eq!(
        stated, passed,
        "`cargo eio build` no longer passes what SDK §5.2 says it passes — amend the spec and \
         the code together, which is this repository's prime directive"
    );

    // `examples/blocks/` is its own cargo workspace: nothing in it can `use` a constant from
    // this crate, so its `.cargo/config.toml` is a genuine copy and stays one. It is checked
    // instead. `crates/conformance/tests/golden.rs` pins the *effect* of that file — all five
    // golden blocks at one page — and cannot see the value, because the harness deliberately
    // depends on nothing of the Rust block toolchain.
    let golden = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/blocks/.cargo/config.toml"
    );
    let golden = std::fs::read_to_string(golden).expect("the golden blocks' cargo config");
    assert_eq!(
        stack_sizes(&golden),
        vec![SHADOW_STACK_BYTES],
        "examples/blocks/.cargo/config.toml has drifted from `build::SHADOW_STACK_BYTES` \
         ({SHADOW_STACK_BYTES}); it is a separate cargo workspace and cannot read the constant, \
         so it is checked here instead"
    );

    // And the template carries the placeholder rather than a number, so `cargo eio new` has
    // no copy to drift at all. Asserted on the source because a rendered one proves only that
    // *this* build agreed with itself.
    let template = concat!(env!("CARGO_MANIFEST_DIR"), "/template/cargo-config.toml.in");
    let template = std::fs::read_to_string(template).expect("the template's cargo config");
    assert!(
        template.contains("{{stack_size}}"),
        "the template's `.cargo/config.toml` no longer renders its stack size from \
         `build::SHADOW_STACK_BYTES`"
    );
    assert_eq!(
        stack_sizes(&template),
        Vec::<u32>::new(),
        "the template's `.cargo/config.toml` has grown a hard-coded stack size again"
    );
}
