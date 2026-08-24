//! eieio-7d8.33: `cargo eio publish` and this crate's own [`Registry::pull`], checked against
//! each other.
//!
//! eieio-7d8.22 proved `publish`'s artifact against every rule this file's parent module
//! enforces — media types, the `sha256-<hex>.sig` tag, the cosign annotation, the DER/P-256/
//! SimpleSigning check, all reimplemented in `cargo-eio`'s own tests. What it could not do is
//! call [`Registry::pull`] itself, because `registry` has never had a path out of this crate.
//! That is one reimplementation away from the drift this pairing exists to prevent: if both
//! halves independently misread the same paragraph of DAEMON §4.1/§4.2 the same way, neither
//! suite would ever see it.
//!
//! This module closes that gap by driving the *real* `cargo_eio::publish::run`, called
//! in-process (see `cargo-eio`'s `src/lib.rs` for why it now has a lib target at all) against
//! a real OCI registry, then handing the exact reference it pushed to this crate's own,
//! private [`Registry::pull`] — reachable here with no visibility change at all, since this
//! file lives inside the crate that owns it.
//!
//! # The one registry
//!
//! [`cargo_eio::fake_registry::Fake`] — `publish`'s own in-process registry, not this crate's
//! `registry::fake::Fake`. This crate's fixture is deliberately GET-only (a pull never
//! writes: see its own doc), so it has nowhere for a real push to land; giving it a write side
//! would mean re-deriving the OCI push protocol `cargo_eio::fake_registry` already implements
//! and cargo-eio's own suite already measures against a real, separate `cosign` process. One
//! registry that already speaks both halves of the protocol, reused rather than duplicated, is
//! what makes this a round trip through a real registry and not a registry built to make the
//! round trip work.
//!
//! # Why not the other two options the issue named
//!
//! - **A new test-only crate depending on both `cargo-eio` and `eio-daemon`.** A whole
//!   workspace member for one test, when a dev-dependency already gets there for less and
//!   creates no cycle (`cargo-eio` depends on `eio-manifest` and `eio-conformance`, neither of
//!   which depends back on `eio-daemon`).
//! - **Promoting `registry::fake::Fake` out of `cfg(test)`.** Read-only by design; making it
//!   `pub` would not give a real publish anywhere to push to, so it does not even address the
//!   problem without first growing a write side — at which point it is not "promoting" the
//!   existing fixture, it is rewriting `cargo_eio::fake_registry` a second time.
//!
//! Both would also have widened `eio-daemon`'s own public surface for no reason: `registry`
//! stays exactly as private as it always was, and the only new surface anywhere is
//! `cargo-eio`'s own `publish` module and its `testing`-gated `fake_registry`, which
//! `cargo-eio` already owns.

use std::path::{Path, PathBuf};
use std::process::Command;

use cargo_eio::fake_registry::Fake;
use cargo_eio::publish::{self, PublishArgs};

use super::{Registry, Signing};

/// `examples/blocks/filter`'s `Cargo.toml` — ABI §13.2's golden filter block, already built by
/// `cargo-eio`'s own suite, so this test costs no extra crate compile beyond what that suite
/// already pays.
fn filter_manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/blocks/filter/Cargo.toml")
}

/// `examples/blocks` is its own cargo workspace (CLAUDE.md), so its `target/` sits at that
/// root rather than under `filter/` — two ancestors up from the block's `Cargo.toml`.
fn examples_root() -> PathBuf {
    filter_manifest_path()
        .parent()
        .and_then(Path::parent)
        .expect("examples/blocks/filter/Cargo.toml has two ancestors")
        .to_path_buf()
}

/// The module `cargo eio build` (inside `publish::run`) just produced, read back off disk —
/// never re-derived, so an assertion against it is an assertion against the exact bytes that
/// were pushed.
fn built_wasm() -> Vec<u8> {
    std::fs::read(examples_root().join("target/wasm32-unknown-unknown/release/filter.wasm"))
        .expect("the module `publish::run` just built")
}

/// The manifest `cargo eio build` wrote beside the module, parsed under ABI §11.1 — the name
/// and version a real publish actually tagged its push with, rather than a value this test
/// assumes.
fn built_manifest() -> eio_manifest::Manifest {
    let written = std::fs::read_to_string(
        examples_root().join("target/wasm32-unknown-unknown/release/manifest.json"),
    )
    .expect("manifest.json was written beside the module");
    eio_manifest::parse(&written).expect("and it parses under ABI §11.1")
}

/// `publish`'s own arguments for pushing `filter` to `fake` under the `tlugger` namespace,
/// signing with `key` when given.
fn args(fake: &Fake, key: Option<PathBuf>) -> PublishArgs {
    let signing = key.is_some();
    PublishArgs {
        registry: format!("{}/tlugger", fake.host()),
        key,
        username: None,
        password: None,
        manifest_path: Some(filter_manifest_path()),
        // The throwaway key `generate_key` mints has an empty passphrase, and this is how
        // `cosign sign` learns it: on the child's own environment, set by `publish::run`.
        // Not `std::env::set_var` — that is `unsafe` because its contract is about the whole
        // process, and this test binary runs sibling tests whose `std::env::temp_dir` reads
        // one, so the contract cannot honestly be met here (see `PublishArgs::cosign_password`).
        cosign_password: signing.then(String::new),
    }
}

/// The exact reference a pull would name for what [`args`] just pushed — `<host>/<repository>:
/// <version>`, matching `Registry::locate`'s own reading of one (this module's parent).
fn reference(fake: &Fake, manifest: &eio_manifest::Manifest) -> String {
    format!(
        "{}/tlugger/{}:{}",
        fake.host(),
        manifest.name,
        manifest.version
    )
}

#[test]
fn a_real_unsigned_publish_pulls_clean_through_the_real_registry_client() {
    let fake = Fake::start();
    publish::run(&args(&fake, None)).expect("a real, unsigned publish");

    let manifest = built_manifest();
    let wasm = built_wasm();

    let pulled = Registry::new(Signing::default(), Default::default())
        .pull(&reference(&fake, &manifest))
        .expect(
            "this crate's own Registry::pull accepts what cargo-eio's own publish::run \
             actually produced",
        );
    assert_eq!(
        pulled, wasm,
        "the pulled bytes are exactly what was built and pushed, not a re-derived copy"
    );
}

/// Generates a throwaway P-256 keypair in `scratch` (no real key is ever committed — the
/// eieio-7d8.22 constraint, restated) and returns `(private key path, public key PEM)`.
fn generate_key(scratch: &Path) -> (PathBuf, String) {
    let key_prefix = scratch.join("cosign");
    let key = scratch.join("cosign.key");
    let pubkey = scratch.join("cosign.pub");
    let status = Command::new("cosign")
        .arg("generate-key-pair")
        .arg("--output-key-prefix")
        .arg(&key_prefix)
        .env("COSIGN_PASSWORD", "")
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "cosign not found on PATH ({error}); install it to run this test \
                 (eieio-7d8.33) — proving the signed round trip needs the real binary, the \
                 same way eieio-7d8.22's own tests do"
            )
        });
    assert!(status.success(), "cosign generate-key-pair failed");
    (
        key,
        std::fs::read_to_string(&pubkey).expect("cosign wrote a public key"),
    )
}

#[test]
fn a_real_signed_publish_verifies_under_this_crates_own_signature_check() {
    // Cover the signed path, not just the unsigned one (eieio-7d8.33's whole point): the
    // signature is where `publish` and `Registry::pull` are most likely to drift, since it is
    // the one rule with three independent facts to agree on — cosign's wire shape, the DSSE
    // Pre-Authentication Encoding, and which digest was actually signed.
    let scratch = std::env::temp_dir().join(format!(
        "eio-daemon-roundtrip-sign-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&scratch).expect("a scratch directory");
    let (key, pubkey_pem) = generate_key(&scratch);

    let fake = Fake::start();
    publish::run(&args(&fake, Some(key))).expect("a real, signed publish");

    let manifest = built_manifest();
    let wasm = built_wasm();
    let verifying_key: p256::ecdsa::VerifyingKey =
        p256::pkcs8::DecodePublicKey::from_public_key_pem(&pubkey_pem)
            .expect("an SPKI-encoded P-256 public key");

    let signing = Signing {
        require_signed: true,
        key: Some(verifying_key),
        key_path: String::from("eio-daemon-roundtrip-test"),
    };
    let pulled = Registry::new(signing, Default::default())
        .pull(&reference(&fake, &manifest))
        .expect(
            "this crate's own Signed::check accepts the bundle produced by cargo-eio's real \
             cosign invocation, under require_signed",
        );
    assert_eq!(pulled, wasm);

    std::fs::remove_dir_all(&scratch).ok();
}
