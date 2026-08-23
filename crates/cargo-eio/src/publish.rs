//! `cargo eio publish` — package the block as an OCI artifact, push it, and sign it with
//! cosign (SDK-SPEC §5, SCOPE §3.6).
//!
//! # The round trip is the spec
//!
//! `crates/daemon/src/registry.rs` is the one implementation of what a pull accepts (DAEMON
//! §4.1, §4.2), so this module's job is not "produce an OCI artifact" in general — it is
//! "produce exactly the bytes that puller will take": the `application/wasm` layer media
//! type, an image manifest rather than an index at the tag a version names, and, when signed,
//! the `sha256-<hex>.sig` tag carrying a single `application/vnd.dev.cosign.simplesigning.v1
//! +json` layer with a `dev.cosignproject.cosign/signature` annotation. Every constant below
//! is pinned to agree with that file; a second spelling of any of them here would be a second
//! place the two could drift apart.
//!
//! # cosign 3.x needs to be told to produce that shape
//!
//! Measured against cosign 3.1.3: `cosign sign --key <key> <ref>` on its own produces the
//! *new* Sigstore bundle format — an image index at the `.sig` tag pointing at a manifest
//! whose one layer is `application/vnd.dev.sigstore.bundle.v0.3+json` — which
//! `registry.rs`'s `signature()` cannot even read as "unsigned", because it refuses an index
//! outright (DAEMON §4.1) before it gets far enough to notice there is no simplesigning layer
//! on it. [`sign`] passes the three flags that undo that default; see its doc comment for why
//! all three are needed together.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, anyhow, bail};
use clap::Args;
use eio_manifest::Manifest;

use crate::build::{self, BuildArgs};
use crate::oci::{self, Push};

/// The media type of the layer carrying the block — pinned to agree with
/// `crates/daemon/src/registry.rs`'s `WASM_LAYER` (DAEMON §4.1).
const WASM_LAYER: &str = "application/wasm";

/// The config every artifact here carries.
///
/// A block is described by its `eio:manifest` custom section (ABI §4.4) and the
/// `manifest.json` `build` writes beside it, not by a second, OCI-shaped config, so there is
/// nothing for this blob to say. `registry.rs` never reads it — only `layers` matters to a
/// pull — so its one job is satisfying the OCI image manifest schema's requirement that a
/// `config` be present at all.
const EMPTY_CONFIG: &str = "application/vnd.oci.empty.v1+json";
const EMPTY_CONFIG_BYTES: &[u8] = b"{}";

/// The manifest's own media type (not a layer's) — an image manifest, never an index, because
/// `registry.rs` refuses the latter outright (DAEMON §4.1) and a block is architecture-
/// independent WASM, so there is nothing for an index to vary over.
const IMAGE_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";

#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Where to push: `<host>[:<port>]/<namespace...>`.
    ///
    /// The block's name and version (from its manifest) are appended, so `ghcr.io/tlugger`
    /// publishes `ghcr.io/tlugger/<name>:<version>` — the same reference a service file or a
    /// daemon's pull would name (SCOPE §3.6).
    pub registry: String,

    /// Sign the pushed artifact with this cosign private key (DAEMON §4.2).
    ///
    /// Unsigned by default: `require_signed` defaults to `false` (DAEMON §4.2), and a first
    /// publish has no key to sign with. `cosign generate-key-pair` writes one.
    #[arg(long, value_name = "PATH")]
    pub key: Option<PathBuf>,

    /// Basic auth username for the registry's token endpoint, when it requires one.
    ///
    /// The daemon's puller never authenticates — it pulls anonymously and from public
    /// repositories only (DAEMON §4.1) — but a push is a write, and few registries accept an
    /// anonymous one. Read from `EIO_REGISTRY_USERNAME` when not given, so CI does not have to
    /// spell a secret onto its own command line.
    #[arg(long, value_name = "USER")]
    pub username: Option<String>,

    /// Basic auth password (or token) for the registry's token endpoint.
    ///
    /// Read from `EIO_REGISTRY_PASSWORD` when not given, for the reason `username` reads its
    /// own fallback from the environment.
    #[arg(long, value_name = "PASSWORD")]
    pub password: Option<String>,

    /// Path to the block's `Cargo.toml`. Defaults to cargo's own search from here.
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,
}

pub fn run(args: &PublishArgs) -> anyhow::Result<()> {
    // Checked before the build below spends any time: `registry`, `username` and `password`
    // are known from the command line alone, so a typo in any of them should fail before a
    // block author waits on a compile to find out.
    let (host, namespace) = args.registry.split_once('/').ok_or_else(|| {
        anyhow!(
            "`{}` names no namespace under a host; expected `<host>/<namespace>` (SCOPE §3.6)",
            args.registry
        )
    })?;
    // The same rule `registry.rs`'s `locate` applies to a pull (DAEMON §4.1): a namespace is
    // not a host, so publishing under one that does not look like a registry would produce an
    // artifact nothing can ever be configured to pull back.
    if !(host.contains('.') || host.contains(':') || host == "localhost") {
        bail!(
            "`{host}` does not look like a registry host (no `.`, no `:`, and it is not \
             `localhost`); a daemon's puller would refuse to treat this as one (DAEMON §4.1)"
        );
    }
    if namespace.is_empty() {
        bail!("`{}` names no namespace under `{host}`", args.registry);
    }
    let credentials = credentials(args)?;

    let built = build::run(&BuildArgs {
        manifest_path: args.manifest_path.clone(),
    })?;

    let tag = ref_tag(&built.manifest)?;
    let repository = format!("{namespace}/{}", built.manifest.name);
    let reference = format!("{host}/{repository}:{tag}");

    let mut push = Push::new(host, &repository, credentials.clone());

    let config_digest = push
        .blob(EMPTY_CONFIG_BYTES)
        .context("pushing the empty config blob")?;
    let wasm_digest = push
        .blob(&built.wasm)
        .with_context(|| format!("pushing {}", built.wasm_path.display()))?;

    let manifest = image_manifest(
        &config_digest,
        &wasm_digest,
        built.wasm.len(),
        &built.manifest,
    );
    let digest = push
        .manifest(&tag, manifest.as_bytes())
        .context("pushing the image manifest")?;

    println!(
        "Pushed {} v{} to {reference}",
        built.manifest.name, built.manifest.version
    );
    println!("    digest   {digest}");

    if let Some(key) = &args.key {
        sign(host, &repository, &digest, key, credentials.as_ref())?;
        println!("    signed   cosign, key {}", key.display());
    }

    Ok(())
}

/// The manifest's `version` as an OCI tag, or a refusal when the two disagree about what a
/// version may contain.
///
/// ABI §11.1 requires semver, and semver's build-metadata suffix (`1.0.0+20130313144700`) is
/// legal there but **not** a legal OCI tag character — the distribution spec's tag grammar is
/// `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`, and `+` is not in it. Refused here, plainly, rather
/// than pushed and left to a registry's own 4xx: a block author who hits this needs a spec
/// decision (does a published version drop build metadata, replace `+`, or refuse it as this
/// does), not a tool that guessed one on its own (CLAUDE.md's prime directive).
fn ref_tag(manifest: &Manifest) -> anyhow::Result<String> {
    let version = &manifest.version;
    let valid = version
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && version.len() <= 128;
    if !valid {
        bail!(
            "{version:?} is valid semver (ABI §11.1) but not a legal OCI tag — the distribution \
             spec's tag grammar excludes `+`, which semver's build-metadata suffix uses; this is \
             a spec gap between ABI §11.1 and SCOPE §3.6 that needs a decision, not a guess"
        );
    }
    Ok(version.clone())
}

/// `(username, password)`, from the flags or their environment fallbacks — required together
/// or not at all, since a registry's Basic auth needs both halves.
fn credentials(args: &PublishArgs) -> anyhow::Result<Option<oci::Credentials>> {
    let username = args
        .username
        .clone()
        .or_else(|| std::env::var("EIO_REGISTRY_USERNAME").ok());
    let password = args
        .password
        .clone()
        .or_else(|| std::env::var("EIO_REGISTRY_PASSWORD").ok());
    match (username, password) {
        (Some(username), Some(password)) => Ok(Some((username, password))),
        (None, None) => Ok(None),
        _ => bail!(
            "--username and --password (or EIO_REGISTRY_USERNAME / EIO_REGISTRY_PASSWORD) must \
             be given together"
        ),
    }
}

/// The OCI image manifest a block's artifact is (DAEMON §4.1): one config the puller never
/// reads, and one layer at the exact media type it checks for.
fn image_manifest(
    config_digest: &str,
    wasm_digest: &str,
    wasm_len: usize,
    manifest: &Manifest,
) -> String {
    format!(
        r#"{{"schemaVersion":2,"mediaType":"{IMAGE_MANIFEST}","config":{{"mediaType":"{EMPTY_CONFIG}","digest":"{config_digest}","size":{config_len}}},"layers":[{{"mediaType":"{WASM_LAYER}","digest":"{wasm_digest}","size":{wasm_len}}}],"annotations":{{"org.opencontainers.image.title":"{name}","org.opencontainers.image.version":"{version}"}}}}"#,
        config_len = EMPTY_CONFIG_BYTES.len(),
        name = json_escape(&manifest.name),
        version = json_escape(&manifest.version),
    )
}

/// Escapes `text` for embedding in a JSON string literal.
///
/// ABI §11.1 already constrains `name` to `[a-z0-9._-]` and `version` to semver, neither of
/// which contains a character this needs to escape — but a hand-built JSON literal that
/// assumed that rather than handling the general case would be a landmine for whichever field
/// gets annotated here next.
fn json_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{0}'..='\u{1f}' => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Signs the pushed manifest at `digest` with cosign, in the legacy "simple signing" shape
/// `crates/daemon/src/registry.rs` verifies (DAEMON §4.2) rather than the Sigstore bundle
/// format cosign 3.x defaults to producing.
///
/// The three flags below undo that default, and — measured against cosign 3.1.3 — no one of
/// them is enough alone: `--use-signing-config=false` stops cosign fetching a TUF-provided
/// signing config (which a bare `--tlog-upload=false` is refused without, since v3 considers
/// that flag meaningless while a signing config is in play), `--new-bundle-format=false`
/// selects the artifact shape, and `--tlog-upload=false` is what then keeps the signature off
/// the public Rekor log. All three together also make this call reach no network beyond the
/// registry it is signing against — confirmed by cosign's own consent banner for the public
/// Sigstore service, which appears with any one of the three omitted and does not with all
/// three present. That matters independent of the format: this runs from a block author's
/// machine or CI, and a publish should not depend on Sigstore's public infrastructure being
/// reachable, let alone silently phone home to it.
fn sign(
    host: &str,
    repository: &str,
    digest: &str,
    key: &Path,
    credentials: Option<&oci::Credentials>,
) -> anyhow::Result<()> {
    let mut command = Command::new("cosign");
    command.args([
        "sign",
        "--yes",
        "--use-signing-config=false",
        "--new-bundle-format=false",
        "--tlog-upload=false",
    ]);
    command.arg("--key").arg(key);
    if oci::scheme(host) == "http" {
        command.arg("--allow-http-registry");
    }
    if let Some((username, password)) = credentials {
        command.arg("--registry-username").arg(username);
        command.arg("--registry-password").arg(password);
    }
    command.arg(format!("{host}/{repository}@{digest}"));

    // A missing `cosign` is not this command's failure to compile a working artifact — the
    // push above already succeeded — so it gets its own message rather than bubbling up as
    // "No such file or directory" against a binary name the author may not recognize as
    // external. Consistent with `build`'s own posture on an external tool (SDK §5.2's
    // `wasm-opt` decision): a missing tool is a clear, actionable error, never a panic, and
    // signing stays optional because `require_signed` defaults to `false` (DAEMON §4.2).
    let status = command.status().map_err(|error| match error.kind() {
        ErrorKind::NotFound => anyhow!(
            "cosign not found on PATH. Install it \
             (https://docs.sigstore.dev/system_config/installation/), or publish without \
             --key to skip signing — DAEMON §4.2's `require_signed` defaults to false, so an \
             unsigned artifact is a normal first publish"
        ),
        _ => anyhow!("running cosign: {error}"),
    })?;
    if !status.success() {
        bail!("cosign sign failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;

    use super::*;
    use crate::fake_registry::Fake;

    /// Serializes this module's tests against each other.
    ///
    /// Every test here calls [`run`], which reads `EIO_REGISTRY_USERNAME`/`_PASSWORD`
    /// ([`credentials`]), and the signed-publish test below also *writes*
    /// `COSIGN_PASSWORD` for the `cosign` child process to inherit. `std::env::set_var`'s
    /// safety contract requires nothing else in the process read or write an environment
    /// variable while it runs — a requirement about the whole process, not just one name —
    /// and this lock is what keeps that true under a parallel `cargo test`. `oci::tests`
    /// touches no environment variable and is unaffected.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Holds [`ENV_LOCK`] for the rest of the calling test.
    fn serialized() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Sets an environment variable for the rest of the calling test, restoring whatever was
    /// there before (or unsetting it) on drop — including on a panic, since a test's default
    /// unwind still runs destructors.
    struct EnvVar {
        name: &'static str,
        previous: Option<String>,
    }

    impl EnvVar {
        /// Sets `name` to `value`. The caller must be holding [`ENV_LOCK`] for as long as this
        /// lives — that is the whole of what makes the `unsafe` here sound.
        fn set(name: &'static str, value: &str) -> EnvVar {
            let previous = std::env::var(name).ok();
            // SAFETY: `serialized()` is held by every caller of `EnvVar::set` in this module,
            // so no other thread in this process reads or writes an environment variable while
            // this runs.
            unsafe {
                std::env::set_var(name, value);
            }
            EnvVar { name, previous }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            // SAFETY: see `EnvVar::set` — the same lock is still held by the test that created
            // this guard, since it is dropped before the guard returned by `serialized()` is.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.name, value),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    /// `examples/blocks/filter`'s `Cargo.toml` — ABI §13.2's golden filter block, already
    /// built by other suites, so `publish`'s own tests cost no extra crate compile.
    fn filter_manifest_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/blocks/filter/Cargo.toml")
    }

    fn args(fake: &Fake, key: Option<PathBuf>) -> PublishArgs {
        PublishArgs {
            registry: format!("{}/tlugger", fake.host()),
            key,
            username: None,
            password: None,
            manifest_path: Some(filter_manifest_path()),
        }
    }

    /// `sha256:<hex>` over `bytes` — the test's own copy of the digest rule, so an assertion
    /// about a pushed manifest's digest does not depend on the code under test to compute it.
    fn digest_of(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        use sha2::Digest as _;

        let mut hex = String::with_capacity(64);
        for byte in sha2::Sha256::digest(bytes) {
            let _ = write!(hex, "{byte:02x}");
        }
        format!("sha256:{hex}")
    }

    #[test]
    fn an_unsigned_publish_pushes_exactly_what_the_daemons_puller_reads() {
        let _guard = serialized();
        let fake = Fake::start();
        run(&args(&fake, None)).expect("publish");

        let manifest_bytes = fake
            .manifest("tlugger/filter", "1.0.0")
            .expect("the manifest was pushed at its version tag");
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();

        // `crates/daemon/src/registry.rs`'s `parse_manifest` refuses anything with a top-level
        // `manifests` array (an index) outright (DAEMON §4.1) — this artifact must never be one.
        assert!(manifest.get("manifests").is_none(), "never an index");
        assert_eq!(manifest["mediaType"], IMAGE_MANIFEST);
        let layers = manifest["layers"].as_array().expect("a layers array");
        assert_eq!(layers.len(), 1, "one wasm layer, the whole of a block");
        assert_eq!(
            layers[0]["mediaType"], WASM_LAYER,
            "the exact media type `registry.rs`'s `wasm_layer` filters on"
        );

        let wasm_digest = layers[0]["digest"].as_str().expect("a digest string");
        let pushed_wasm = fake
            .blob(wasm_digest)
            .expect("the layer's digest names a blob this registry actually has");
        assert_eq!(
            digest_of(&pushed_wasm),
            wasm_digest,
            "content-addressed honestly"
        );

        // `examples/blocks` is its own cargo workspace (CLAUDE.md), so its `target/` sits at
        // that root rather than under `filter/` — two levels up from the block's `Cargo.toml`.
        let built_wasm = std::fs::read(
            filter_manifest_path()
                .parent()
                .and_then(Path::parent)
                .expect("examples/blocks/filter/Cargo.toml has two ancestors")
                .join("target/wasm32-unknown-unknown/release/filter.wasm"),
        )
        .expect("the module `cargo eio build` just produced");
        assert_eq!(
            pushed_wasm, built_wasm,
            "the pushed layer is the exact module this publish built, not a re-derived copy"
        );

        // Unsigned: no artifact at all sits at the tag DAEMON §4.2's `signature()` would look
        // under, which is what makes an unsigned pull (`require_signed` defaulting to false)
        // succeed rather than merely "not refuse".
        let sig_tag = format!(
            "sha256-{}.sig",
            digest_of(&manifest_bytes).trim_start_matches("sha256:")
        );
        assert!(
            fake.manifest("tlugger/filter", &sig_tag).is_none(),
            "an unsigned publish leaves nothing at the signature tag"
        );
    }

    #[test]
    fn a_bare_host_with_no_namespace_is_refused_before_anything_is_pushed() {
        let _guard = serialized();
        let fake = Fake::start();
        let mut publish_args = args(&fake, None);
        publish_args.registry = fake.host();
        let error = run(&publish_args).expect_err("no namespace under the host");
        assert!(format!("{error:#}").contains("namespace"), "{error:#}");
    }

    #[test]
    fn a_signed_publish_produces_a_signature_the_daemons_verifier_accepts() {
        let _guard = serialized();
        // No real key is ever committed: a throwaway P-256 keypair is generated fresh, in a
        // scratch directory, for this test alone (the eieio-7d8.22 constraint).
        let scratch = std::env::temp_dir().join(format!(
            "eio-cargo-eio-publish-sign-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).expect("a scratch directory");
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
                     (eieio-7d8.22) — `publish` itself treats a missing cosign as an ordinary, \
                     actionable error rather than a panic, but proving the signed round trip \
                     needs the real binary"
                )
            });
        assert!(status.success(), "cosign generate-key-pair failed");

        // `cosign sign` (unlike the `generate-key-pair` above) is invoked by `run` itself, deep
        // inside `sign`, and inherits *this* process's environment rather than one this test
        // controls directly — so the empty passphrase has to be set here for that child to
        // find, exactly as a real publish relies on a block author's own shell already having
        // `COSIGN_PASSWORD` set.
        let _password = EnvVar::set("COSIGN_PASSWORD", "");

        let fake = Fake::start();
        run(&args(&fake, Some(key))).expect("a signed publish");

        let manifest_bytes = fake
            .manifest("tlugger/filter", "1.0.0")
            .expect("the manifest was pushed");
        let digest = digest_of(&manifest_bytes);
        let sig_tag = format!("sha256-{}.sig", digest.trim_start_matches("sha256:"));
        let sig_manifest_bytes = fake
            .manifest("tlugger/filter", &sig_tag)
            .expect("a signature artifact at the tag DAEMON §4.2's `signature()` reads");
        let sig_manifest: serde_json::Value = serde_json::from_slice(&sig_manifest_bytes).unwrap();

        // Exactly the shape `registry.rs`'s `signature()` parses: an image manifest (not the
        // Sigstore-bundle index cosign 3.x defaults to — this file's own module doc explains
        // why three flags are needed to avoid it), one layer at cosign's simplesigning media
        // type, carrying the signature as an annotation rather than as the layer's own bytes.
        assert!(sig_manifest.get("manifests").is_none(), "never an index");
        let layers = sig_manifest["layers"].as_array().expect("a layers array");
        assert_eq!(layers.len(), 1);
        assert_eq!(
            layers[0]["mediaType"],
            "application/vnd.dev.cosign.simplesigning.v1+json"
        );
        let signature_b64 = layers[0]["annotations"]["dev.cosignproject.cosign/signature"]
            .as_str()
            .expect("the annotation `registry.rs`'s `COSIGN_SIGNATURE` reads");
        let payload_digest = layers[0]["digest"].as_str().expect("a digest string");
        let payload = fake
            .blob(payload_digest)
            .expect("the signed payload was pushed as the layer's blob");

        // `Signed::check` in `crates/daemon/src/registry.rs`, restated: a DER-encoded P-256
        // signature, verified over the payload's exact bytes, whose own critical.image digest
        // names the manifest actually pulled — the third check that makes the first two mean
        // anything.
        use base64::Engine as _;
        use p256::ecdsa::signature::Verifier as _;
        let signature_der = base64::engine::general_purpose::STANDARD
            .decode(signature_b64)
            .expect("valid base64");
        let signature =
            p256::ecdsa::Signature::from_der(&signature_der).expect("a DER ECDSA signature");
        let pubkey_pem = std::fs::read_to_string(&pubkey).expect("cosign wrote a public key");
        let verifying_key: p256::ecdsa::VerifyingKey =
            p256::pkcs8::DecodePublicKey::from_public_key_pem(&pubkey_pem)
                .expect("an SPKI-encoded P-256 public key");
        verifying_key
            .verify(&payload, &signature)
            .expect("the signature verifies under the key `cosign generate-key-pair` wrote");

        let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(
            payload["critical"]["image"]["docker-manifest-digest"], digest,
            "the signature is over the manifest this publish actually pushed"
        );

        std::fs::remove_dir_all(&scratch).ok();
    }
}
