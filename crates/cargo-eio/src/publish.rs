//! `cargo eio publish` — package the block as an OCI artifact, push it, and sign it with
//! cosign (SDK-SPEC §5, SCOPE §3.6).
//!
//! # The round trip is the spec
//!
//! `crates/daemon/src/registry.rs` is the one implementation of what a pull accepts (DAEMON
//! §4.1, §4.2), so this module's job is not "produce an OCI artifact" in general — it is
//! "produce exactly the bytes that puller will take": the `application/wasm` layer media
//! type, and an image manifest rather than an index at the tag a version names. Every constant
//! below is pinned to agree with that file; a second spelling of any of them here would be a
//! second place the two could drift apart.
//!
//! # Signing: cosign's default *shape*, kept offline
//!
//! `registry.rs`'s verifier accepts both of cosign's shapes (DAEMON §4.2): the legacy "simple
//! signing" envelope, and — as of eieio-8yq.18 — cosign 3.1.3's *default* shape, a Sigstore
//! bundle (a DSSE envelope over an in-toto statement) attached at the OCI 1.1
//! referrers-*fallback* tag, `sha256-<hex>` with **no** `.sig` suffix (that suffix is only the
//! legacy tag's). [`sign`] therefore drops the one flag that used to force the *legacy* shape,
//! `--new-bundle-format=false` — left at its own default, cosign 3.1.3 already writes the
//! bundle shape above, so there is nothing left to force.
//!
//! Two flags still have to be passed together, and neither is about the shape — both are about
//! staying offline, and measured (not assumed) against cosign 3.1.3 against this crate's own
//! fake registry:
//!
//! - `--use-signing-config=false`. Cosign's own default, `--use-signing-config=true`, fetches a
//!   TUF-provided signing config from `tuf-repo-cdn.sigstore.dev` before it gets anywhere near
//!   signing — confirmed by forcing all non-loopback traffic through a dead proxy and watching
//!   cosign fail there, on that host, before touching the registry this call was given. A
//!   publish has no business depending on Sigstore's public TUF repository being reachable,
//!   let alone phoning home to it by default.
//! - `--tlog-upload=false`, which is what actually keeps the signature off the public
//!   transparency log. It cannot be passed *alone*: with `--use-signing-config` left at its own
//!   default, cosign 3.1.3 refuses outright ("`--tlog-upload=false` is not supported with
//!   `--signing-config` or `--use-signing-config`") before either flag's own effect matters —
//!   so `--use-signing-config=false` is a precondition for `--tlog-upload=false` being accepted
//!   at all, not merely a second, independent offline measure.
//!
//! One consequence worth stating plainly: the bundle this produces carries no transparency-log
//! inclusion proof and no signing-config-derived certificate chain, because both of the things
//! that would populate them were the two flags above. That is not a shortfall relative to what
//! `registry.rs` needs — DAEMON §4.2 verifies a bundle exactly as it verifies simple signing,
//! against a key the node already holds, and never needs to consult a log to do it — but it
//! does mean a bundle this tool produces carries strictly less than one produced by `cosign`'s
//! own unmodified defaults would. See [`sign`]'s doc comment for the offline-verifiability
//! consequence that follows from it.

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

/// Signs the pushed manifest at `digest` with cosign, at cosign 3.1.3's own default *shape* —
/// the Sigstore bundle format `crates/daemon/src/registry.rs` now verifies alongside legacy
/// simple signing (DAEMON §4.2, eieio-8yq.18) — kept offline by two flags this module's own
/// doc comment measures and justifies.
///
/// Verifying what results is consequently *partial by construction*, not by omission: with no
/// signing-config resolution and no transparency-log upload, the bundle carries no certificate
/// chain and no log inclusion proof for a verifier to check — there is nothing here that would
/// need a network call to verify, which is what makes `registry.rs`'s verification of it
/// offline-complete rather than "offline for now, best-effort for the rest". A bundle carrying
/// those extra fields (produced by `cosign`'s own unmodified defaults, or by a signing service
/// elsewhere in a fleet) is a case this tool does not produce and `registry.rs` does not need
/// to handle to accept what *this* function signs.
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

        // Unsigned: no artifact at all sits at either tag DAEMON §4.2's `signature()` would
        // look under — the legacy `.sig` tag or cosign's default referrers-fallback tag —
        // which is what makes an unsigned pull (`require_signed` defaulting to false) succeed
        // rather than merely "not refuse".
        let hex = digest_of(&manifest_bytes)
            .trim_start_matches("sha256:")
            .to_string();
        assert!(
            fake.manifest("tlugger/filter", &format!("sha256-{hex}.sig"))
                .is_none(),
            "an unsigned publish leaves nothing at the legacy signature tag"
        );
        assert!(
            fake.manifest("tlugger/filter", &format!("sha256-{hex}"))
                .is_none(),
            "an unsigned publish leaves nothing at the bundle referrers-fallback tag"
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

    /// DSSE's Pre-Authentication Encoding — restated from `crates/daemon/src/registry.rs`'s own
    /// private `dsse_pae`, for the same reason the rest of this module's tests restate rather
    /// than import that crate's checks (see the module-level test doc below).
    fn dsse_pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
        let mut pae = Vec::from(*b"DSSEv1");
        for part in [payload_type.as_bytes(), payload] {
            pae.push(b' ');
            pae.extend_from_slice(part.len().to_string().as_bytes());
            pae.push(b' ');
            pae.extend_from_slice(part);
        }
        pae
    }

    /// Generates a throwaway P-256 keypair in `scratch` (the eieio-7d8.22 constraint: no real
    /// key is ever committed) and returns `(private key path, public key PEM)`.
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
                     (eieio-7d8.22) — `publish` itself treats a missing cosign as an ordinary, \
                     actionable error rather than a panic, but proving the signed round trip \
                     needs the real binary"
                )
            });
        assert!(status.success(), "cosign generate-key-pair failed");
        (
            key,
            std::fs::read_to_string(&pubkey).expect("cosign wrote a public key"),
        )
    }

    /// A signed publish's Sigstore bundle, read back out of `fake` for `digest` — the manifest
    /// at the referrers-fallback tag (`sha256-<hex>`, no `.sig`: DAEMON §4.2), its one
    /// bundle-typed entry, and that entry's DSSE envelope.
    fn bundle_at(fake: &Fake, digest: &str) -> serde_json::Value {
        let fallback_tag = format!("sha256-{}", digest.trim_start_matches("sha256:"));
        let index_bytes = fake
            .manifest("tlugger/filter", &fallback_tag)
            .expect("a bundle artifact at the tag DAEMON §4.2's `bundle_signature` reads");
        let index: serde_json::Value = serde_json::from_slice(&index_bytes).unwrap();
        assert_eq!(
            index["manifests"][0]["artifactType"], "application/vnd.dev.sigstore.bundle.v0.3+json",
            "cosign's default artifact type for a signature (DAEMON §4.2)"
        );
        let inner_digest = index["manifests"][0]["digest"].as_str().expect("a digest");
        let inner_bytes = fake
            .manifest("tlugger/filter", inner_digest)
            .expect("the manifest the index entry names");
        let inner: serde_json::Value = serde_json::from_slice(&inner_bytes).unwrap();
        let layer_digest = inner["layers"][0]["digest"].as_str().expect("a digest");
        let bundle_bytes = fake
            .blob(layer_digest)
            .expect("the bundle was pushed as the layer's blob");
        serde_json::from_slice(&bundle_bytes).unwrap()
    }

    /// Verifies `bundle`'s DSSE envelope under `verifying_key` and checks its in-toto subject —
    /// `Signed::Bundle::check` in `crates/daemon/src/registry.rs`, restated. Every test in this
    /// module that touches the daemon's acceptance criteria restates the check inline rather
    /// than depending on `eio-daemon` (`cargo-eio` owns itself — CLAUDE.md), which is also what
    /// makes this a genuine round-trip proof rather than a tautology: two independent
    /// implementations of §4.2 agreeing is the property that matters, not one calling the
    /// other.
    fn bundle_verifies(
        bundle: &serde_json::Value,
        verifying_key: &p256::ecdsa::VerifyingKey,
        expected_digest: &str,
    ) -> Result<(), String> {
        use base64::Engine as _;
        use p256::ecdsa::signature::Verifier as _;

        let payload_type = bundle["dsseEnvelope"]["payloadType"]
            .as_str()
            .ok_or("no payloadType")?;
        let payload = base64::engine::general_purpose::STANDARD
            .decode(
                bundle["dsseEnvelope"]["payload"]
                    .as_str()
                    .ok_or("no payload")?,
            )
            .map_err(|error| format!("payload base64: {error}"))?;
        let sig_der = base64::engine::general_purpose::STANDARD
            .decode(
                bundle["dsseEnvelope"]["signatures"][0]["sig"]
                    .as_str()
                    .ok_or("no signature")?,
            )
            .map_err(|error| format!("signature base64: {error}"))?;
        let signature =
            p256::ecdsa::Signature::from_der(&sig_der).map_err(|error| format!("{error}"))?;

        verifying_key
            .verify(&dsse_pae(payload_type, &payload), &signature)
            .map_err(|error| format!("DSSE signature does not verify: {error}"))?;

        let statement: serde_json::Value =
            serde_json::from_slice(&payload).map_err(|error| format!("payload JSON: {error}"))?;
        assert_eq!(
            statement["predicateType"], "https://sigstore.dev/cosign/sign/v1",
            "`cosign sign`'s predicate, distinguishing a signature from an attestation \
             (DAEMON §4.2)"
        );
        let subject = format!(
            "sha256:{}",
            statement["subject"][0]["digest"]["sha256"]
                .as_str()
                .ok_or("no subject digest")?
        );
        if subject != expected_digest {
            return Err(format!("signed over {subject}, not {expected_digest}"));
        }
        Ok(())
    }

    #[test]
    fn a_signed_publish_produces_a_bundle_the_daemons_verifier_accepts() {
        let _guard = serialized();
        let scratch = std::env::temp_dir().join(format!(
            "eio-cargo-eio-publish-sign-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).expect("a scratch directory");
        let (key, pubkey_pem) = generate_key(&scratch);

        // `cosign sign` (unlike `generate_key`'s `generate-key-pair`) is invoked by `run`
        // itself, deep inside `sign`, and inherits *this* process's environment rather than one
        // this test controls directly — so the empty passphrase has to be set here for that
        // child to find, exactly as a real publish relies on a block author's own shell already
        // having `COSIGN_PASSWORD` set.
        let _password = EnvVar::set("COSIGN_PASSWORD", "");

        let fake = Fake::start();
        // No opt-out flags reach `cosign` at all (see `sign`'s doc comment): this is cosign
        // 3.1.3's own default artifact *shape*, kept offline by the two flags that survive.
        run(&args(&fake, Some(key))).expect("a signed publish");

        let manifest_bytes = fake
            .manifest("tlugger/filter", "1.0.0")
            .expect("the manifest was pushed");
        let digest = digest_of(&manifest_bytes);
        let bundle = bundle_at(&fake, &digest);

        let verifying_key: p256::ecdsa::VerifyingKey =
            p256::pkcs8::DecodePublicKey::from_public_key_pem(&pubkey_pem)
                .expect("an SPKI-encoded P-256 public key");
        bundle_verifies(&bundle, &verifying_key, &digest)
            .expect("the bundle cosign's defaults produced verifies under its own key");

        std::fs::remove_dir_all(&scratch).ok();
    }

    #[test]
    fn a_bundle_tampered_with_after_an_honest_cosign_sign_does_not_verify() {
        // Proves DAEMON §4.2's "a present signature is checked regardless of policy" against
        // the *real* shape cosign emits, not a hand-built fixture: sign honestly, then corrupt
        // exactly what a hostile registry could rewrite — the signature, and the signed
        // subject — and show each is refused where the untouched bundle was not.
        let _guard = serialized();
        let scratch = std::env::temp_dir().join(format!(
            "eio-cargo-eio-publish-tamper-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).expect("a scratch directory");
        let (key, pubkey_pem) = generate_key(&scratch);
        let _password = EnvVar::set("COSIGN_PASSWORD", "");

        let fake = Fake::start();
        run(&args(&fake, Some(key))).expect("a signed publish");
        let manifest_bytes = fake
            .manifest("tlugger/filter", "1.0.0")
            .expect("the manifest was pushed");
        let digest = digest_of(&manifest_bytes);
        let bundle = bundle_at(&fake, &digest);
        let verifying_key: p256::ecdsa::VerifyingKey =
            p256::pkcs8::DecodePublicKey::from_public_key_pem(&pubkey_pem)
                .expect("an SPKI-encoded P-256 public key");

        bundle_verifies(&bundle, &verifying_key, &digest).expect("the untampered bundle verifies");

        let mut bad_signature = bundle.clone();
        let sig = bad_signature["dsseEnvelope"]["signatures"][0]["sig"]
            .as_str()
            .unwrap()
            .to_string();
        let mut sig_bytes = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(&sig)
                .unwrap()
        };
        *sig_bytes.last_mut().unwrap() ^= 0xff;
        bad_signature["dsseEnvelope"]["signatures"][0]["sig"] = serde_json::Value::String({
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(&sig_bytes)
        });
        bundle_verifies(&bad_signature, &verifying_key, &digest)
            .expect_err("a flipped signature byte must not verify");

        let mut wrong_digest = bundle.clone();
        wrong_digest["dsseEnvelope"]["payload"] = {
            use base64::Engine as _;
            let statement = format!(
                r#"{{"_type":"https://in-toto.io/Statement/v1","subject":[{{"digest":{{"sha256":"{}"}},"annotations":{{}}}}],"predicateType":"https://sigstore.dev/cosign/sign/v1","predicate":{{}}}}"#,
                "0".repeat(64)
            );
            serde_json::Value::String(
                base64::engine::general_purpose::STANDARD.encode(statement.as_bytes()),
            )
        };
        bundle_verifies(&wrong_digest, &verifying_key, &digest)
            .expect_err("a rewritten (and now unsigned) subject digest must not verify");

        std::fs::remove_dir_all(&scratch).ok();
    }
}
