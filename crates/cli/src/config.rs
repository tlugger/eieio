//! Multi-node context: `~/.config/eieio/nodes.toml` (eieio-yck.1's DESIGN).
//!
//! Deliberately **not** the working tree. SCOPE §3.11's bearer token (DAEMON §9.1) is the
//! whole of what stands between a caller and deploying arbitrary WASM to a node, and a file a
//! project's `.gitignore` merely *asks* not to be committed is a file that eventually is one.
//! `~/.config` can never be inside a git checkout, so there is no discipline to rely on — this
//! is the strongest form of the no-token-in-a-working-tree rule DESIGNER's eieio-8yq.10 note
//! and SERVICE §9's editor both already keep to for the files that *are* in one.
//!
//! Honours `XDG_CONFIG_HOME` because that is what "the user's config directory" means on every
//! platform this binary ships to (SCOPE §3.7 keeps leaf tiers off this binary entirely — nothing
//! here has to run on an MCU). No project-local fallback: one was considered and rejected in
//! the same DESIGN note this module implements, because a fallback the token could also live in
//! would reopen exactly the accidental-commit risk the whole design exists to close.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// A node's class (SCOPE §3.7's amendment): whether `eio` may ever dial `addr` over HTTP at
/// all.
///
/// Deserializes from exactly `"daemon"` or `"leaf"` — lowercase, matching the spec's own
/// spelling — and anything else is a config error naming the bad value, not a silent fall back
/// to [`NodeClass::Daemon`]. A typo here is either a leaf an operator wrongly believes `eio` can
/// reach, or a daemon `eio` refuses for no reason, and both are the kind of wrong answer this
/// whole field exists to stop happening quietly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum NodeClass {
    /// Serves DAEMON §9's management API over HTTP — every node `eio` could reach before this
    /// field existed, and what an absent `class` key still means.
    #[default]
    Daemon,
    /// An MCU-tier node (LEAF-SPEC): serves no HTTP at all, by design (LEAF §7). `Config::resolve`
    /// refuses to hand one to a caller that is about to dial it.
    Leaf,
}

impl NodeClass {
    /// Whether this is the value an absent `class` key means — used to keep a `Daemon` entry
    /// from gaining a redundant key the next time `nodes.toml` is written (see [`NodeEntry`]).
    pub(crate) fn is_default(&self) -> bool {
        *self == NodeClass::Daemon
    }
}

/// One configured node.
///
/// `Debug` is hand-written, not derived (below): this is the one type in the process that
/// holds a node's bearer token in memory across its whole lifetime, so it is the one type
/// where "nothing happens to print it" is not good enough — a `{:?}` reachable from a derive
/// is a `{:?}` a future change can reach by accident. Structural, not disciplinary, the same
/// posture `client.rs`'s `envelope_error` already keeps for the wire (eieio-yck.1, eieio-8yq.10).
#[derive(Clone, Serialize, Deserialize)]
pub struct NodeEntry {
    /// The management API's base URL, e.g. `http://10.0.0.5:7777` (DAEMON §9).
    pub addr: String,
    /// The bearer token from that node's `auth/token` (DAEMON §9.1). Optional: a node an
    /// operator has not yet copied a token for can still be named for `list`/`set-default`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// What kind of node this is (SCOPE §3.7). Absent in the file means [`NodeClass::Daemon`],
    /// via `#[serde(default)]`, and is omitted on write right back when it is `Daemon`, via
    /// `skip_serializing_if` — the same pairing `token` above uses, and for the same reason:
    /// every `nodes.toml` written before this field existed keeps meaning exactly what it meant,
    /// and keeps being written back without gaining a redundant key.
    #[serde(default, skip_serializing_if = "NodeClass::is_default")]
    pub class: NodeClass,
}

impl std::fmt::Debug for NodeEntry {
    /// Renders `token` as present-or-absent, never as its bytes. `class` is not secret and is
    /// rendered plainly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeEntry")
            .field("addr", &self.addr)
            .field(
                "token",
                &self
                    .token
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .field("class", &self.class)
            .finish()
    }
}

/// The file itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// The node `--node` resolves to when it is not given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Every configured node, by name.
    #[serde(default)]
    pub nodes: BTreeMap<String, NodeEntry>,
}

/// `$XDG_CONFIG_HOME/eieio`, or `~/.config/eieio` when that is unset or empty.
///
/// Read fresh on every call rather than cached: this binary is one process per invocation, so
/// there is no lifetime across which a cached answer could go stale, and a test that wants a
/// different answer sets the environment of the child process it spawns (matching every other
/// integration test in this crate) rather than mutating this one's.
fn config_dir() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("eieio"));
    }
    let home = std::env::var_os("HOME")
        .context("neither XDG_CONFIG_HOME nor HOME is set; cannot find nodes.toml")?;
    Ok(PathBuf::from(home).join(".config").join("eieio"))
}

/// `<config dir>/nodes.toml`.
pub fn path() -> Result<PathBuf> {
    Ok(config_dir()?.join("nodes.toml"))
}

impl Config {
    /// Reads `nodes.toml`, or an empty configuration if it does not exist yet.
    ///
    /// Absence is not an error: the first `eio node add` on a machine has nothing to read, and
    /// asking an operator to touch the file into existence first would be busywork this command
    /// can do itself.
    pub fn load() -> Result<Config> {
        let path = path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Writes `nodes.toml`, creating its directory if this is the first node on this machine.
    ///
    /// The directory and the file are both created narrow: `0600` on the file because it is
    /// where a bearer token lands (DAEMON §9.1) the same way `auth/token` is on a node, and the
    /// directory is `mkdir -p`'d rather than assumed to already exist, which is what lets this
    /// be the first command anyone runs on a fresh machine.
    pub fn save(&self) -> Result<()> {
        let path = path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("rendering nodes.toml")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("restricting {}", path.display()))?;
        }
        Ok(())
    }

    /// Adds or replaces a node, minting no defaults: `addr` is required, `token` is whatever
    /// the caller supplied (including nothing, for a node named before its token is known).
    /// Always `Daemon`-class: there is no `eio node add --class` today, so the only way a
    /// `nodes.toml` entry becomes `leaf` is an operator hand-editing the file, the same way
    /// every other TOML file in this system is authored (SERVICE §9).
    pub fn add(&mut self, name: String, addr: String, token: Option<String>, class: NodeClass) {
        self.nodes.insert(name, NodeEntry { addr, token, class });
    }

    /// Removes a node. Clears `default` too, when it named this one — a default pointing at a
    /// node the file no longer lists would resolve to nothing and say why in the least useful
    /// possible way.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        if self.nodes.remove(name).is_none() {
            bail!("no configured node named `{name}`{}", self.known());
        }
        if self.default.as_deref() == Some(name) {
            self.default = None;
        }
        Ok(())
    }

    /// Sets which node `--node` resolves to when it is omitted.
    pub fn set_default(&mut self, name: &str) -> Result<()> {
        if !self.nodes.contains_key(name) {
            bail!("no configured node named `{name}`{}", self.known());
        }
        self.default = Some(String::from(name));
        Ok(())
    }

    /// Resolves `--node <name>`, or the configured default, to the node it names.
    ///
    /// A caller who names neither, on a machine with no default, gets an error naming every
    /// configured node — the recovery in one message, rather than a bare "no node" that sends
    /// them to read a file they may not know the location of.
    ///
    /// **Refuses a `leaf`-class entry** (SCOPE §3.7, LEAF §7) rather than handing it back: every
    /// caller of this — `client::connect`, `mcp.rs`'s `resolve_node` — dials the node over HTTP
    /// immediately afterward, and a leaf serves none, so this is the one place the guard needs to
    /// live for it to hold everywhere reaching a node does, rather than being a discipline each
    /// call site could forget. Naming-only operations (`eio node list`, `remove`, `set-default`)
    /// never call this — see [`Config::remove`] and [`Config::set_default`] — so a leaf entry
    /// still lists, sets as default, and removes exactly as a daemon entry does.
    pub fn resolve<'a>(&'a self, requested: Option<&'a str>) -> Result<(&'a str, &'a NodeEntry)> {
        let name = requested
            .or(self.default.as_deref())
            .with_context(|| format!("no --node given and no default set{}", self.known()))?;
        let entry = self
            .nodes
            .get(name)
            .with_context(|| format!("no configured node named `{name}`{}", self.known()))?;
        if entry.class == NodeClass::Leaf {
            bail!(
                "node `{name}` is a leaf-class node, which serves no management API by design \
                 (LEAF §7); deploy to it through a firmware build, not `eio`"
            );
        }
        Ok((name, entry))
    }

    /// `" (configured: a, b, c)"`, or `" (no nodes configured; see `eio node add`)"`.
    ///
    /// Never includes a token: this is spliced into error messages, and an error naming what
    /// went wrong must not be the one place a token leaks.
    fn known(&self) -> String {
        if self.nodes.is_empty() {
            return String::from(" (no nodes configured; see `eio node add`)");
        }
        let names: Vec<&str> = self.nodes.keys().map(String::as_str).collect();
        format!(" (configured: {})", names.join(", "))
    }
}

#[cfg(test)]
mod tests {
    //! `NodeEntry::fmt` is the structural half of the auth-leak requirement `mcp.rs`'s module
    //! doc restates (eieio-yck.1, eieio-8yq.10): a test that greps a command's *output* for a
    //! token is necessary and not sufficient, because it says nothing about a `{:?}` nobody
    //! happened to call yet. This asserts the redaction directly, on the type, so a later
    //! `format!("{entry:?}")` dropped into a `tracing::debug!` or a `dbg!()` cannot reintroduce
    //! the leak the derive would have allowed.

    use super::*;

    #[test]
    fn no_debug_rendering_of_a_node_entry_can_print_its_token() {
        let with_token = NodeEntry {
            addr: String::from("http://10.0.0.5:7777"),
            token: Some(String::from("s3cr3t-token-do-not-print-me")),
            class: NodeClass::default(),
        };
        let rendered = format!("{with_token:?}");
        assert!(
            !rendered.contains("s3cr3t-token-do-not-print-me"),
            "NodeEntry's Debug leaked the token: {rendered}"
        );
        assert!(rendered.contains("redacted"), "{rendered}");
        assert!(
            rendered.contains("http://10.0.0.5:7777"),
            "the address is not secret and Debug should still be useful: {rendered}"
        );

        let without_token = NodeEntry {
            addr: String::from("http://10.0.0.5:7777"),
            token: None,
            class: NodeClass::default(),
        };
        assert!(format!("{without_token:?}").contains("none"));

        // The same guarantee holds through `Config`, which a caller is more likely to have a
        // handle to than a bare `NodeEntry`.
        let mut config = Config::default();
        config.add(
            String::from("kitchen"),
            String::from("http://10.0.0.5:7777"),
            Some(String::from("s3cr3t-token-do-not-print-me")),
            NodeClass::Daemon,
        );
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("s3cr3t-token-do-not-print-me"),
            "Config's derived Debug reached the token through NodeEntry: {rendered}"
        );
    }
}
