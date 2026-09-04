//! The node: what `node.toml` says, and the directory it says it about (DAEMON-SPEC §2, §2.1).
//!
//! A node's configuration is a file on the node (SCOPE §3.8), and this module is the whole of
//! the daemon's relationship with it: where the directory tree is, what the file may contain,
//! and what a node runs on when it says nothing.
//!
//! # The one file the daemon writes
//!
//! SERVICE §2 forbids a host to write a *service* file, and the reason does not reach this
//! one. A service file round-trips through a human, the Designer or an agent, so a daemon that
//! rewrote it would leave a git checkout dirty after every deploy; `node.toml` describes the
//! node to itself and has to exist before anything can be authored at all. So a data directory
//! with no `node.toml` is a fresh node rather than an error: [`Node::open`] builds the tree,
//! mints an id and writes the file — once. A second boot reads the id it wrote, because an id
//! that changed per boot would identify nothing (DAEMON §2.1).
//!
//! What it writes is [`TEMPLATE`], a commented document with one substitution in it, and not a
//! serialization of the defaults. Two reasons, and the second is the load-bearing one: the
//! file an operator opens should say what each knob means, and the workspace's `toml`
//! dependency carries `parse` and `serde` without `display` on purpose — SERVICE §2 is why —
//! so there is nothing here to serialize *with*, and there should not be.
//!
//! # Every field is optional except the id
//!
//! A node that stated nothing would still have to run under ABI §10's budgets and ABI §9.7's
//! limits, since §10 does not admit an unbudgeted callback. So the defaults are real values
//! rather than an absence, and they are the ones DAEMON §2.1 publishes. `deny_unknown_fields`
//! throughout, for the reason ABI §11.1 and SERVICE §3 both give: a typo'd knob that silently
//! meant nothing is a node running on a default its operator believes they changed.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use eio_expr::EvalLimits;
use eio_host_core::{ExprBudgets, Limits};
use serde::Deserialize;

use crate::engine::Budgets;
use crate::registry::{Credential, Signing};

/// The default management API address (DAEMON §2.1, §9).
///
/// Loopback while SCOPE §3.11's transport security is OPEN. The API deploys arbitrary WASM to
/// the node and its only gate is a bearer token over plaintext HTTP; a default reaching every
/// interface would make installing the package the exposing act rather than a deliberate one.
/// The port is arbitrary and pinned: nothing in the platform had claimed one.
const DEFAULT_LISTEN: &str = "127.0.0.1:7373";

/// The largest payload one instance may emit or be delivered, in bytes (ABI §9.7).
///
/// Public because `dev run-block` states the same number on its command line (DAEMON §12).
/// ABI §9.7 declines to give these a floor (SCOPE §3.4 OPEN), which is why the daemon has to
/// choose one — and why the choice has a name rather than being a literal in two places.
pub const DEFAULT_MAX_PAYLOAD: u32 = 64 * 1024;

/// The largest number of signals in one batch (ABI §9.7).
pub const DEFAULT_MAX_BATCH: u32 = 1024;

/// What this daemon bounds one callback's emissions at (ABI §9.7 rule 9): **nothing**.
///
/// A daemon states this value the way a leaf states 4 096 (LEAF §4.3), and `None` is the
/// statement rather than the absence of one — the descriptor publishes no
/// `max_emission_bytes`, and a block reading it learns that this host does not refuse an
/// emission for the queue's sake.
///
/// It is not a knob, and that is the finding rather than an omission. A number here would be
/// a backpressure policy, and backpressure is **OPEN** (SCOPE §3.4): on a leaf the bound is
/// forced by a fixed heap and a `handle_alloc_error` that resets the node, while on a daemon
/// the queue lives on an OS allocator behind ABI §10's fuel budget, so what to do when a
/// block emits faster than the graph drains is the open question and not a number to pick in
/// passing. When §3.4 closes, this becomes `[limits] max_emission_bytes` in `node.toml` and a
/// field on `GET /node` beside the other two.
pub const MAX_EMISSION_BYTES: Option<u32> = None;

/// How many work items one instance's mailbox holds (DAEMON §5).
pub const DEFAULT_MAILBOX: usize = 64;

/// How many random bytes a node id is minted from. Hex-encoded, so the id is twice this long.
const ID_BYTES: usize = 8;

/// How many random bytes the API token is minted from (DAEMON §9.1).
///
/// Four times the node id's, and for a different job: an id is an opaque label anyone may
/// know, and this is the only thing between a caller and deploying arbitrary WASM to this node
/// while transport security is still OPEN (SCOPE §3.11).
const TOKEN_BYTES: usize = 32;

/// Where a node looks for the key it verifies signatures with (DAEMON §2.1, §4.2).
///
/// Inside `auth/`, which is created owner-only, though a public key is not secret: what makes
/// it the right directory is that it is the node's key material, and an operator restoring a
/// node from a backup should not have to be told it lives somewhere else.
const DEFAULT_KEY: &str = "auth/cosign.pub";

/// The `node.toml` a fresh node is provisioned with (DAEMON §2.1).
///
/// `{id}` is the only substitution. Everything else is a comment or a value equal to the
/// default it documents, so an operator can uncomment-and-edit rather than look the shape up.
const TEMPLATE: &str = "\
# This node, as it describes itself (DAEMON-SPEC §2.1).
#
# Written once, when the daemon found a data directory with no node.toml in it. It is
# yours from here: nothing rewrites or reformats this file.

# The node's identity. Opaque, and stable for the life of the node -- the Designer keys a
# node by this, so that renaming one or giving it a new address is not a migration.
id = \"{id}\"

# An optional label for people. Nothing resolves by it.
# name = \"kitchen-pi\"

[api]
# Where the management API listens.
#
# Loopback, because the API deploys arbitrary WASM to this node and transport security is
# still an open question (SCOPE §3.11). Set it to 0.0.0.0:7373 to let the Designer and
# other machines reach this node -- deliberately, on a network you trust.
listen = \"127.0.0.1:7373\"

[limits]
# The largest payload and batch any block instance may emit or be delivered (ABI §9.7).
# The ABI gives these no floor, so they are stated rather than assumed.
max_payload = 65536
max_batch = 1024

[budgets]
# What one guest callback may consume before it is killed (ABI §10).
#
# `fuel` is roughly one unit per WASM instruction. `deadline_ms` is the backstop for a
# callback that is blocked rather than busy, which fuel cannot see.
fuel = 100000000
deadline_ms = 1000

[budgets.expr]
# What one property expression may consume (EXPR §9). Expressions are pure and
# terminating by construction; these bound how much work a terminating one may do.
max_fuel = 100000
max_depth = 128
max_range = 65536
max_value_bytes = 262144

[executor]
# How many work items one block instance's mailbox holds before its senders feel it
# (DAEMON §5). A depth of one is legal and means every sender waits for the previous item.
mailbox = 64

[blocks]
# Whether a block this node cannot verify a signature for may run (DAEMON §4.2). A
# present signature is checked either way -- this decides what is acceptable, not
# whether to look. Turning it on without a key at the path below refuses every pull.
require_signed = false
# The public key signatures are verified against: PEM, as `cosign public-key` writes
# it. Relative paths are resolved against this data directory.
key = \"auth/cosign.pub\"
";

/// A node: its identity, its budgets, and where its files are (DAEMON §2).
#[derive(Debug, Clone)]
pub struct Node {
    /// The node's identity — opaque, stable, minted on first boot (DAEMON §2.1).
    pub id: String,
    /// A label for people. Nothing resolves by it.
    pub name: Option<String>,
    /// Where the management API listens (DAEMON §9).
    ///
    /// Parsed and validated here; nothing binds it yet, because nothing serves it yet
    /// (eieio-8yq.4). A bound port that accepts into a backlog nobody answers is worse for a
    /// client than a closed one, so the address is checked at boot and used later.
    pub listen: SocketAddr,
    /// What every instance on this node reports as its limits (ABI §5.2, §9.7).
    pub limits: Limits,
    /// What one guest entry and one expression may consume (ABI §10, EXPR §9).
    pub budgets: Budgets,
    /// The depth of every instance's mailbox (DAEMON §5).
    pub mailbox: usize,
    /// What this node will accept a block on the strength of (DAEMON §4.2).
    pub signing: Signing,
    /// Credentials for private OCI registries, keyed by host (DAEMON §2.1, §13). Read from
    /// `auth/registries.toml`, not `node.toml` — see that file's own doc comment for why.
    pub credentials: BTreeMap<String, Credential>,
    /// The management API's bearer token (DAEMON §9.1).
    pub token: String,
    /// The data directory this node was opened on.
    root: PathBuf,
}

impl Node {
    /// Opens `root` as a node's data directory, provisioning it if it is fresh.
    ///
    /// The directory tree of DAEMON §2 is created whether or not it existed, so a node whose
    /// `state/` was deleted between boots comes back rather than failing on the first write.
    /// `node.toml` is written only when it is absent — that is what makes the id stable.
    pub fn open(root: &Path) -> anyhow::Result<Node> {
        std::fs::create_dir_all(root)
            .with_context(|| format!("creating the data directory {}", root.display()))?;
        let layout = Layout { root };
        for path in [
            layout.services(),
            layout.blocks(),
            layout.precompiled(),
            layout.state(),
        ] {
            std::fs::create_dir_all(&path)
                .with_context(|| format!("creating {}", path.display()))?;
        }
        provision_auth(&layout.auth())?;
        let token = provision_token(&layout.token())?;
        let credentials = load_registries(&layout.registries())?;

        let path = layout.node_toml();
        if !path.exists() {
            let id = mint_id()?;
            std::fs::write(&path, TEMPLATE.replace("{id}", &id))
                .with_context(|| format!("writing {}", path.display()))?;
            tracing::info!(node = %id, path = %path.display(), "provisioned a new node");
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let file: File = toml::from_str(&text)
            .with_context(|| format!("{} is not a valid node configuration", path.display()))?;
        file.into_node(root.to_path_buf(), token, credentials)
    }

    /// Where this node's files are (DAEMON §2).
    pub fn layout(&self) -> Layout<'_> {
        Layout { root: &self.root }
    }
}

/// The paths of DAEMON §2's tree, named once so nothing spells one twice.
#[derive(Debug, Clone, Copy)]
pub struct Layout<'a> {
    root: &'a Path,
}

impl Layout<'_> {
    /// The data directory itself.
    pub fn root(&self) -> &Path {
        self.root
    }

    /// `node.toml`: this file (§2.1).
    fn node_toml(&self) -> PathBuf {
        self.root.join("node.toml")
    }

    /// `services/`: one service definition per file, named for the service (DAEMON §2).
    pub fn services(&self) -> PathBuf {
        self.root.join("services")
    }

    /// `pubsub.toml`: bus name, broker candidates, optional pin (DAEMON §2, §7.1).
    ///
    /// Its own file and not a `node.toml` table, on purpose: `node.toml` is the operator's
    /// once written — the daemon creates it and never rewrites or normalizes it (§2.1) — and
    /// a bus pin has to be settable by the Designer through §9's API the way a service file
    /// is. A node with nothing at this path runs no bridge (§7.1), which is the normal case
    /// and not a boot failure.
    pub fn pubsub(&self) -> PathBuf {
        self.root.join("pubsub.toml")
    }

    /// `blocks/`: the pull cache, `<name>/<version>/block.wasm` (DAEMON §4).
    pub fn blocks(&self) -> PathBuf {
        self.root.join("blocks")
    }

    /// `precompiled/`: compiled blocks, keyed by content and engine (DAEMON §4.3).
    ///
    /// Derived, and deleting it costs nothing but a cold start — which is why it is a
    /// directory of its own rather than files scattered through `blocks/`.
    pub fn precompiled(&self) -> PathBuf {
        self.root.join("precompiled")
    }

    /// `state/`: what backs `eio:state` (DAEMON §10, ABI §7.2).
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    /// `state/state.redb`: the store itself (DAEMON §10).
    ///
    /// Here rather than at each caller because a node's paths are this type's answer, and the
    /// three places that open the store — `run`, the API's test harness, and boot's — spelling
    /// the join themselves is three chances to open a different file than the daemon writes.
    pub fn state_store(&self) -> PathBuf {
        self.state().join(crate::state::Store::FILE)
    }

    /// `auth/`: token material (SCOPE §3.11).
    ///
    /// Created owner-only at boot, because it is where the API's token lands (§9.1).
    pub fn auth(&self) -> PathBuf {
        self.root.join("auth")
    }

    /// `auth/token`: the management API's bearer token (DAEMON §9.1).
    pub fn token(&self) -> PathBuf {
        self.auth().join("token")
    }

    /// `auth/registries.toml`: OCI registry credentials, keyed by host (DAEMON §2.1, §13).
    ///
    /// Not a `node.toml` table: `node.toml` is the templated file an operator reads and edits,
    /// and DAEMON §9's API may expose it, and a bearer token has no business in either. Beside
    /// the management API's own token and `cosign.pub`, inside the same owner-only directory.
    pub fn registries(&self) -> PathBuf {
        self.auth().join("registries.toml")
    }
}

/// Creates `auth/` with owner-only permissions (DAEMON §2.1, SCOPE §3.11).
///
/// The mode is set explicitly rather than left to the process umask: a token readable by every
/// user on the machine would be a node anyone on it can deploy to, and a umask is not a thing
/// the daemon gets to assume. Applied on every boot, so a directory that was widened by hand
/// or restored from an archive without modes is narrowed again.
fn provision_auth(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restricting {}", path.display()))?;
    }
    provision_gitignore(path)
}

/// Writes `auth/.gitignore` containing `*`, unless this node already has one (DAEMON §2.1,
/// §13).
///
/// An operator who version-controls their node data directory is one `git add -A` away from
/// committing the management API token, a registry credential, or both — a committed secret
/// is the failure that actually matters here, more than any one file's placement. So the whole
/// directory is made to ignore itself, on the same first boot that creates it. Written once,
/// like `node.toml`: an existing `auth/.gitignore` is left exactly as an operator left it,
/// including one they deliberately narrowed or deleted.
fn provision_gitignore(auth: &Path) -> anyhow::Result<()> {
    let path = auth.join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    std::fs::write(&path, "*\n").with_context(|| format!("writing {}", path.display()))
}

/// Reads the API token, minting one if this node has none yet (DAEMON §9.1).
///
/// Minted once and then read, for the same reason the id is: a token that changed per boot
/// would log every client out whenever the node restarted. Printed only by the boot that mints
/// it — a daemon that logged it on every boot would put the credential in every log shipper
/// that ever reads this node, which is the opposite of what a `0600` file is for.
fn provision_token(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(token) => Ok(String::from(token.trim())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut bytes = [0u8; TOKEN_BYTES];
            getrandom::fill(&mut bytes)
                .context("the system has no randomness to mint an API token from")?;
            let token = crate::blocks::hex(&bytes);

            std::fs::write(path, format!("{token}\n"))
                .with_context(|| format!("writing {}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("restricting {}", path.display()))?;
            }
            tracing::info!(
                token = %token,
                path = %path.display(),
                "minted this node's API token; it is printed once and readable from that file"
            );
            Ok(token)
        }
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Mints a node id (DAEMON §2.1).
///
/// Hex over random bytes: opaque by construction, safe in a path and in a URL, and carrying no
/// structure for anything to start parsing. Not [`eio_service::id::generate`]'s four
/// characters — that length is chosen for an id a human types into every connection in a
/// service file, and a node id is written by nobody and compared across a whole System.
fn mint_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; ID_BYTES];
    getrandom::fill(&mut bytes).context("the system has no randomness to mint a node id from")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// `node.toml`, exactly as written (DAEMON §2.1).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    api: Api,
    #[serde(default)]
    limits: LimitsSection,
    #[serde(default)]
    budgets: BudgetsSection,
    #[serde(default)]
    executor: ExecutorSection,
    #[serde(default)]
    blocks: BlocksSection,
}

impl File {
    /// Turns the file into the node it describes, checking what the types could not.
    fn into_node(
        self,
        root: PathBuf,
        token: String,
        credentials: BTreeMap<String, Credential>,
    ) -> anyhow::Result<Node> {
        anyhow::ensure!(!self.id.is_empty(), "`id` must not be empty");
        anyhow::ensure!(
            self.executor.mailbox > 0,
            "`[executor] mailbox` must have room for at least one item"
        );
        let listen: SocketAddr =
            self.api.listen.parse().with_context(|| {
                format!("`[api] listen` is not an address: {}", self.api.listen)
            })?;

        let eval = EvalLimits {
            max_fuel: self.budgets.expr.max_fuel,
            max_depth: self.budgets.expr.max_depth,
            max_range: self.budgets.expr.max_range,
            max_value_bytes: self.budgets.expr.max_value_bytes,
        };
        Ok(Node {
            id: self.id,
            name: self.name,
            listen,
            limits: Limits::new(
                self.limits.max_payload,
                self.limits.max_batch,
                MAX_EMISSION_BYTES,
            ),
            budgets: Budgets {
                fuel: self.budgets.fuel,
                deadline: std::time::Duration::from_millis(self.budgets.deadline_ms),
                // The decode bound is derived rather than configured, because EXPR §9 rule 9
                // makes it a constraint and not a preference: it MUST be at least the depth
                // expressions run at, and `ExprBudgets::new` raises it to meet that. Two
                // independent knobs would let a file express a host that cannot be built.
                expr: ExprBudgets::new(eval, eio_signal::MAX_DEPTH),
            },
            mailbox: self.executor.mailbox,
            signing: self.blocks.signing(&root)?,
            credentials,
            token,
            root,
        })
    }
}

// Each section below carries `#[serde(default)]` on the *container*, so a field the file
// omits is taken from that section's `Default` and a section the file omits entirely is that
// `Default` whole. One statement of each number, in the `Default` impl, rather than the three
// a per-field `default = "..."` costs — the attribute, the function it names, and the `Default`
// that has to agree with both.

/// `[api]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Api {
    listen: String,
}

impl Default for Api {
    fn default() -> Api {
        Api {
            listen: String::from(DEFAULT_LISTEN),
        }
    }
}

/// `[limits]` (ABI §9.7).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct LimitsSection {
    max_payload: u32,
    max_batch: u32,
}

impl Default for LimitsSection {
    fn default() -> LimitsSection {
        LimitsSection {
            max_payload: DEFAULT_MAX_PAYLOAD,
            max_batch: DEFAULT_MAX_BATCH,
        }
    }
}

/// `[budgets]` (ABI §10).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct BudgetsSection {
    fuel: u64,
    deadline_ms: u64,
    expr: ExprSection,
}

impl Default for BudgetsSection {
    fn default() -> BudgetsSection {
        BudgetsSection {
            fuel: Budgets::DEFAULT_FUEL,
            deadline_ms: Budgets::DEFAULT_DEADLINE.as_millis() as u64,
            expr: ExprSection::default(),
        }
    }
}

/// `[budgets.expr]` (EXPR §9).
///
/// Defaults from [`EvalLimits::DEFAULT`] rather than restated, because EXPR §9 publishes them
/// and a node that quietly ran on different numbers would be a second answer to what the
/// reference budgets are.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct ExprSection {
    max_fuel: u32,
    max_depth: u32,
    max_range: u32,
    max_value_bytes: u32,
}

impl Default for ExprSection {
    fn default() -> ExprSection {
        let EvalLimits {
            max_fuel,
            max_depth,
            max_range,
            max_value_bytes,
        } = EvalLimits::DEFAULT;
        ExprSection {
            max_fuel,
            max_depth,
            max_range,
            max_value_bytes,
        }
    }
}

/// `[executor]` (DAEMON §5).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct ExecutorSection {
    mailbox: usize,
}

impl Default for ExecutorSection {
    fn default() -> ExecutorSection {
        ExecutorSection {
            mailbox: DEFAULT_MAILBOX,
        }
    }
}

/// `[blocks]` (DAEMON §4.2).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct BlocksSection {
    require_signed: bool,
    key: String,
}

impl Default for BlocksSection {
    fn default() -> BlocksSection {
        BlocksSection {
            // Not a recommendation — what a node that has never been given a key can do.
            // Defaulting a fresh node into refusing every pull would make a first boot a
            // configuration error (DAEMON §2.1).
            require_signed: false,
            key: String::from(DEFAULT_KEY),
        }
    }
}

impl BlocksSection {
    /// Reads the key, if there is one where this says (DAEMON §2.1, §4.2).
    ///
    /// An absent key is not an error even under `require_signed`: the refusal belongs to the
    /// pull, which is where an operator finds out what it was they could not verify. A key
    /// that is *there* and is not a key is an error at load, because it is a file the
    /// configuration deliberately points at and there is nothing else it could have meant.
    fn signing(self, root: &Path) -> anyhow::Result<Signing> {
        use p256::pkcs8::DecodePublicKey as _;

        let path = root.join(&self.key);
        let key = match std::fs::read_to_string(&path) {
            Ok(pem) => Some(
                p256::ecdsa::VerifyingKey::from_public_key_pem(&pem).map_err(|error| {
                    anyhow::anyhow!(
                        "`[blocks] key` points at {}, which is not a PEM public key: {error}",
                        path.display()
                    )
                })?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        Ok(Signing {
            require_signed: self.require_signed,
            key,
            key_path: path.display().to_string(),
        })
    }
}

/// One host's entry in `auth/registries.toml` (DAEMON §2.1, §13).
///
/// `deny_unknown_fields`, for the same reason `node.toml`'s sections carry it: a typo'd key
/// here is a credential silently not applied, discovered only when a pull that should have
/// worked does not.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryEntry {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

impl RegistryEntry {
    /// Turns one entry into the credential it names, or says which way it is ambiguous.
    fn credential(self, host: &str) -> anyhow::Result<Credential> {
        match (self.token, self.username, self.password) {
            (Some(token), None, None) => Ok(Credential::Bearer(token)),
            (None, Some(username), Some(password)) => Ok(Credential::Basic { username, password }),
            (None, Some(_), None) => anyhow::bail!(
                "`auth/registries.toml`'s entry for {host} has a `username` and no `password`"
            ),
            (None, None, Some(_)) => anyhow::bail!(
                "`auth/registries.toml`'s entry for {host} has a `password` and no `username`"
            ),
            (None, None, None) => anyhow::bail!(
                "`auth/registries.toml`'s entry for {host} names neither a `token` nor a \
                 `username` and `password`"
            ),
            (Some(_), _, _) => anyhow::bail!(
                "`auth/registries.toml`'s entry for {host} names both a `token` and a \
                 `username`/`password`; a registry takes one or the other"
            ),
        }
    }
}

/// Reads `auth/registries.toml`, if this node has one (DAEMON §2.1, §13).
///
/// Absent is the ordinary case — most nodes pull only public registries — and is not an error.
/// Keyed by host exactly as `Registry::pull` looks one up: see [`Registry::credential`]'s doc
/// comment for why an exact string, and not a suffix or prefix match, is the whole of what
/// keeps a credential meant for one host from ever reaching another.
fn load_registries(path: &Path) -> anyhow::Result<BTreeMap<String, Credential>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let raw: BTreeMap<String, RegistryEntry> = toml::from_str(&text)
        .with_context(|| format!("{} is not a valid registries file", path.display()))?;
    raw.into_iter()
        .map(|(host, entry)| {
            let credential = entry.credential(&host)?;
            Ok((host, credential))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scratch::scratch;

    /// Parses `text` as a `node.toml`, with no directory behind it.
    fn node(text: &str) -> anyhow::Result<Node> {
        toml::from_str::<File>(text)
            .map_err(anyhow::Error::from)
            .and_then(|file| {
                file.into_node(
                    PathBuf::from("/nonexistent"),
                    String::from("t"),
                    BTreeMap::new(),
                )
            })
    }

    #[test]
    fn an_id_and_nothing_else_is_a_whole_node() {
        // DAEMON §2.1: every field is optional except `id`, and the defaults are values
        // rather than an absence — ABI §10 does not admit an unbudgeted callback.
        let node = node("id = \"abc\"").expect("the smallest valid node.toml");
        assert_eq!(node.id, "abc");
        assert_eq!(node.name, None);
        assert_eq!(node.listen.to_string(), DEFAULT_LISTEN);
        assert_eq!(node.mailbox, DEFAULT_MAILBOX);
        assert_eq!(node.budgets, Budgets::default());
        assert_eq!(
            node.limits,
            Limits::new(DEFAULT_MAX_PAYLOAD, DEFAULT_MAX_BATCH, MAX_EMISSION_BYTES)
        );
    }

    #[test]
    fn the_provisioned_template_parses_to_the_defaults() {
        // The file a fresh node is given must describe the node it would have had without
        // one. Otherwise the documented values and the real ones drift the first time a
        // default changes and the template does not.
        let node = node(&TEMPLATE.replace("{id}", "abc")).expect("the template is valid");
        assert_eq!(node.budgets, Budgets::default(), "budgets");
        assert_eq!(
            node.limits,
            Limits::new(DEFAULT_MAX_PAYLOAD, DEFAULT_MAX_BATCH, MAX_EMISSION_BYTES),
            "limits"
        );
        assert_eq!(node.mailbox, DEFAULT_MAILBOX, "mailbox");
        assert_eq!(node.listen.to_string(), DEFAULT_LISTEN, "listen");
    }

    #[test]
    fn every_knob_is_read_from_the_file() {
        let node = node(
            r#"
            id = "n1"
            name = "kitchen-pi"
            [api]
            listen = "0.0.0.0:9000"
            [limits]
            max_payload = 128
            max_batch = 7
            [budgets]
            fuel = 5
            deadline_ms = 250
            [budgets.expr]
            max_fuel = 11000
            max_depth = 44
            max_range = 1300
            max_value_bytes = 14000
            [executor]
            mailbox = 3
            "#,
        )
        .expect("a fully stated node.toml");
        assert_eq!(node.name.as_deref(), Some("kitchen-pi"));
        assert_eq!(node.listen.to_string(), "0.0.0.0:9000");
        assert_eq!(node.limits, Limits::new(128, 7, MAX_EMISSION_BYTES));
        assert_eq!(node.budgets.fuel, 5);
        assert_eq!(node.budgets.deadline, std::time::Duration::from_millis(250));
        assert_eq!(node.budgets.expr.eval().max_fuel, 11000);
        assert_eq!(node.budgets.expr.eval().max_depth, 44);
        assert_eq!(node.budgets.expr.eval().max_range, 1300);
        assert_eq!(node.budgets.expr.eval().max_value_bytes, 14000);
        assert_eq!(node.mailbox, 3);
    }

    #[test]
    fn the_signing_policy_is_the_files_and_the_key_is_a_file() {
        // DAEMON §2.1, §4.2. Three states, and all three are reachable from a `node.toml`:
        // no key, a key, and a key that is not one.
        let root = scratch("node-signing");
        let key = root.join("auth").join("cosign.pub");
        std::fs::create_dir_all(key.parent().expect("auth/")).expect("auth/");

        let opened = |text: &str| {
            std::fs::write(root.join("node.toml"), text).expect("a node.toml");
            Node::open(&root)
        };

        let node = opened("id = \"n1\"").expect("a node with no key").signing;
        assert!(!node.require_signed, "the default is not to require one");
        assert!(node.key.is_none(), "and there is no key to require it with");
        assert!(
            node.key_path.ends_with("auth/cosign.pub"),
            "the default path is still reported, so a refusal can name it: {}",
            node.key_path
        );

        std::fs::write(&key, crate::registry::fake::KEY.pem()).expect("a public key");
        let node = opened("id = \"n1\"\n[blocks]\nrequire_signed = true")
            .expect("a node with a key")
            .signing;
        assert!(node.require_signed);
        assert_eq!(node.key, Some(crate::registry::fake::KEY.verifying()));

        std::fs::write(
            &key,
            "-----BEGIN PUBLIC KEY-----\nnope\n-----END PUBLIC KEY-----\n",
        )
        .expect("something that is not one");
        assert!(
            opened("id = \"n1\"").is_err(),
            "a file the configuration points at and that is not a key is an error at load"
        );
    }

    #[test]
    fn a_budget_below_the_floor_is_raised_to_it() {
        // EXPR §9's floors are what a conforming expression may rely on, so a node cannot
        // configure itself below one. Raised rather than refused, for the reason
        // `ExprBudgets::new` gives: a host that would not boot over a number the spec is
        // willing to choose for it.
        let node = node(
            "id = \"n1\"\n[budgets.expr]\nmax_fuel = 1\nmax_depth = 1\nmax_range = 1\n\
             max_value_bytes = 1",
        )
        .expect("valid, if optimistic");
        assert_eq!(node.budgets.expr.eval(), EvalLimits::FLOORS);
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        // The whole reason for `deny_unknown_fields`: a node running on a default its
        // operator believes they changed (ABI §11.1, SERVICE §3).
        assert!(node("id = \"n1\"\nmailbox = 8").is_err(), "top level");
        assert!(
            node("id = \"n1\"\n[executor]\nmailboxes = 8").is_err(),
            "a section"
        );
        assert!(
            node("id = \"n1\"\n[budgets.expr]\nmax_feul = 8").is_err(),
            "a nested section"
        );
        assert!(node("name = \"n1\"").is_err(), "and `id` is required");
    }

    #[test]
    fn a_node_that_could_not_run_is_refused_at_load() {
        assert!(
            node("id = \"n1\"\n[api]\nlisten = \"not-an-address\"").is_err(),
            "an address that will not bind is a boot failure, not a surprise at eieio-8yq.4"
        );
        assert!(
            node("id = \"n1\"\n[executor]\nmailbox = 0").is_err(),
            "a mailbox with no room would refuse every delivery forever"
        );
        assert!(node("id = \"\"").is_err(), "an empty id identifies nothing");
    }

    #[test]
    fn the_decode_bound_is_raised_to_the_expression_depth() {
        // EXPR §9 rule 9 is a constraint rather than a preference, so it is derived and not
        // configured: an expression may not be able to build a value the boundary refuses.
        let deep = node("id = \"n1\"\n[budgets.expr]\nmax_depth = 4096").expect("valid");
        assert_eq!(deep.budgets.expr.eval().max_depth, 4096);
        assert!(
            deep.budgets.expr.decode_depth() >= 4096,
            "the decode bound followed the expression depth up"
        );
    }

    #[test]
    fn a_fresh_directory_is_provisioned_once() {
        let root = scratch("provision");
        let first = Node::open(&root).expect("a fresh node comes up");

        for directory in ["services", "blocks", "state", "auth"] {
            assert!(
                root.join(directory).is_dir(),
                "{directory}/ was not created"
            );
        }
        assert!(
            root.join("node.toml").is_file(),
            "node.toml was not written"
        );
        assert!(!first.id.is_empty(), "an id was minted");

        let second = Node::open(&root).expect("a second boot");
        assert_eq!(
            first.id, second.id,
            "provisioning happens once: an id that changed per boot would identify nothing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn auth_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = scratch("auth-mode");
        Node::open(&root).expect("a fresh node");
        let mode = |path: &Path| {
            std::fs::metadata(path)
                .expect("auth/ exists")
                .permissions()
                .mode()
                & 0o777
        };
        let auth = root.join("auth");
        assert_eq!(mode(&auth), 0o700, "token material is not world-readable");

        // And a directory somebody widened is narrowed again, which is the half that makes
        // setting the mode on every boot worth doing rather than only on creation.
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o755))
            .expect("widening it");
        assert_eq!(mode(&auth), 0o755, "the test really did widen it");
        Node::open(&root).expect("a later boot");
        assert_eq!(mode(&auth), 0o700, "and the daemon narrowed it back");
    }

    #[test]
    fn auth_gitignore_is_provisioned_once_and_never_overwritten() {
        // The whole secret directory protects itself against a version-controlled data
        // directory: the API token and any registry credential, not just one filename
        // (DAEMON §2.1, §13).
        let root = scratch("auth-gitignore");
        Node::open(&root).expect("a fresh node");
        let gitignore = root.join("auth").join(".gitignore");
        assert_eq!(
            std::fs::read_to_string(&gitignore).expect("auth/.gitignore exists"),
            "*\n"
        );

        // An operator's own edit survives a later boot -- idempotent, not "resets every time".
        std::fs::write(&gitignore, "*\n!keep-me\n").expect("an operator's edit");
        Node::open(&root).expect("a later boot");
        assert_eq!(
            std::fs::read_to_string(&gitignore).expect("still there"),
            "*\n!keep-me\n",
            "an existing auth/.gitignore is not overwritten"
        );
    }

    #[test]
    fn registries_toml_is_read_into_credentials_keyed_by_host() {
        let root = scratch("node-registries");
        let node = Node::open(&root).expect("a fresh node");
        assert!(node.credentials.is_empty(), "no file, no credentials");

        std::fs::write(
            root.join("auth").join("registries.toml"),
            "[\"ghcr.io\"]\ntoken = \"abc\"\n\n\
             [\"registry.internal:5000\"]\nusername = \"node\"\npassword = \"secret\"\n",
        )
        .expect("a registries.toml");
        let node = Node::open(&root).expect("a second boot");
        assert!(
            matches!(node.credentials.get("ghcr.io"), Some(Credential::Bearer(t)) if t == "abc")
        );
        assert!(matches!(
            node.credentials.get("registry.internal:5000"),
            Some(Credential::Basic { username, password })
                if username == "node" && password == "secret"
        ));
    }

    #[test]
    fn a_registries_toml_entry_naming_neither_a_token_nor_a_login_is_refused_at_load() {
        let root = scratch("node-registries-ambiguous");
        Node::open(&root).expect("a fresh node");
        std::fs::write(root.join("auth").join("registries.toml"), "[\"ghcr.io\"]\n")
            .expect("an entry naming nothing");
        assert!(
            Node::open(&root).is_err(),
            "an entry naming neither a token nor a username/password is ambiguous"
        );
    }

    #[test]
    fn the_checked_in_example_is_the_file_a_node_is_provisioned_with() {
        // `examples/node.toml` is documentation — what an operator reads without booting a
        // node to see one. Pinned to the template rather than maintained beside it, because
        // an example that had drifted from what the daemon writes would be worse than none.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/node.toml");
        let example = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let id = example
            .lines()
            .find_map(|line| line.strip_prefix("id = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("the example carries an id");
        assert_eq!(example.trim_end(), TEMPLATE.replace("{id}", id).trim_end());
    }

    #[test]
    fn a_minted_id_is_opaque_and_stable_in_shape() {
        let id = mint_id().expect("randomness");
        assert_eq!(id.len(), ID_BYTES * 2);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "an id is safe in a path and in a URL: {id}"
        );
        assert_ne!(id, mint_id().expect("randomness"), "and it is random");
    }
}
