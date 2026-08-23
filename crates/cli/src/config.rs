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

/// One configured node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeEntry {
    /// The management API's base URL, e.g. `http://10.0.0.5:7777` (DAEMON §9).
    pub addr: String,
    /// The bearer token from that node's `auth/token` (DAEMON §9.1). Optional: a node an
    /// operator has not yet copied a token for can still be named for `list`/`set-default`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
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
    pub fn add(&mut self, name: String, addr: String, token: Option<String>) {
        self.nodes.insert(name, NodeEntry { addr, token });
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
    pub fn resolve<'a>(&'a self, requested: Option<&'a str>) -> Result<(&'a str, &'a NodeEntry)> {
        let name = requested
            .or(self.default.as_deref())
            .with_context(|| format!("no --node given and no default set{}", self.known()))?;
        let entry = self
            .nodes
            .get(name)
            .with_context(|| format!("no configured node named `{name}`{}", self.known()))?;
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
