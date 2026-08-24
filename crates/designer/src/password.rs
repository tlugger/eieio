//! Where the Designer's own login password comes from (DESIGNER-SPEC §3, v1-minimal).
//!
//! DESIGNER §3 asks for exactly one thing here: "a single login/token gate on the app.
//! Nothing fancier until someone needs it" — and leaves where the credential comes from
//! unstated. The plan for this crate says to decide deliberately rather than invent
//! something weaker than the precedent already in this repository, so this module follows
//! `eio-daemon`'s `node::provision_token` (DAEMON §9.1) as closely as the two situations
//! allow: **mint a random credential into a narrowly-permissioned file on first boot, and
//! print it once, at the boot that minted it.**
//!
//! # Why the same shape as a node's bearer token, and not a chosen password
//!
//! A node's token is not typed by a human day to day; it is copied once from where it was
//! printed. This crate's password *is* typed by a human, into a browser, on every session —
//! which is the one respect in which the two differ. Asking an operator to invent a good
//! password themselves is exactly the UX every credential-stuffing list exists because of;
//! minting one is the daemon's posture applied to a human-facing secret instead of a
//! machine-facing one, and it is a stronger default than any password an operator would
//! have typed on the spot to get the box running. An operator who wants a memorable
//! password instead can still set one, deliberately, through the escape hatch below — the
//! random mint is only ever what a *fresh* install gets without being asked.
//!
//! # Two sources, and only one of them touches disk
//!
//! - **`EIO_DESIGNER_PASSWORD`**, if set, wins outright and is never written anywhere. This
//!   is the container/orchestration path: a secret handed to the process by whatever already
//!   manages secrets for it (a compose file's `environment`, a Kubernetes `Secret`, a systemd
//!   `EnvironmentFile`) should not also be duplicated into a file this crate owns — one
//!   source of truth for the credential, chosen by whoever is deploying it.
//! - **`<data-dir>/password`**, minted if absent, read if present. This is the bare-binary
//!   path and the default: `just run-designer`, a laptop, a Pi with no secrets manager in
//!   front of it. `0600`, matching `auth/token`'s own permissions (DAEMON §2.1) — this file
//!   is the one thing standing between an operator's browser and this process's proxy to
//!   every node it can reach, so it gets exactly the same posture a node's own token does.
//!
//! Both are read fresh at boot; neither is cached past it, because there is exactly one boot
//! per process and nothing here changes without a restart.

use std::path::Path;

use anyhow::Context;

/// The environment variable that, if set, is the password outright (see module doc).
const ENV_OVERRIDE: &str = "EIO_DESIGNER_PASSWORD";

/// How many random bytes the minted password is drawn from. Same width as
/// `eio-daemon::node::TOKEN_BYTES` — this credential guards the same class of thing a node's
/// token does (arbitrary proxied access, here to every configured node at once), so it gets
/// the same entropy rather than a human-sized guess at "enough".
const PASSWORD_BYTES: usize = 32;

/// Resolves this Designer's login password: the environment override if set, otherwise the
/// file at `data_dir/password` — minted if this is a fresh data directory, read if not.
pub fn provision(data_dir: &Path) -> anyhow::Result<String> {
    provision_with(data_dir, std::env::var(ENV_OVERRIDE).ok().as_deref())
}

/// [`provision`], with the environment override passed in rather than read.
///
/// Split out so a test can exercise "the override wins" without mutating the real process
/// environment — `std::env::set_var` is a process-global and, since the 2024 edition, an
/// `unsafe fn` for exactly that reason. Dependency injection sidesteps the question rather
/// than answering it, which is the right call for a test this small.
fn provision_with(data_dir: &Path, env_override: Option<&str>) -> anyhow::Result<String> {
    if let Some(from_env) = env_override {
        let trimmed = from_env.trim();
        anyhow::ensure!(
            !trimmed.is_empty(),
            "{ENV_OVERRIDE} is set but empty; unset it to fall back to a minted password, or \
             give it one"
        );
        tracing::info!(
            "using the password from ${ENV_OVERRIDE}; nothing was written to {}",
            data_dir.display()
        );
        return Ok(String::from(trimmed));
    }

    let path = data_dir.join("password");
    match std::fs::read_to_string(&path) {
        Ok(existing) => Ok(String::from(existing.trim())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => mint(&path),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Mints a fresh password into `path`, `0600`, and prints it once.
fn mint(path: &Path) -> anyhow::Result<String> {
    let mut bytes = [0u8; PASSWORD_BYTES];
    getrandom::fill(&mut bytes).context("the system has no randomness to mint a password from")?;
    let password = hex(&bytes);

    std::fs::write(path, format!("{password}\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", path.display()))?;
    }

    // Printed at `info`, not `debug`: this is the one and only time this password is ever
    // shown, and an operator who has their log level turned down should still see it.
    tracing::info!(
        password = %password,
        path = %path.display(),
        "minted this Designer's login password; it is printed once and readable from that \
         file thereafter"
    );
    Ok(password)
}

/// Lowercase hex, matching `eio-daemon::blocks::hex`'s own rendering of minted secrets.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_data_dir_mints_a_narrow_file_and_a_second_boot_reads_it_back() {
        let dir =
            std::env::temp_dir().join(format!("eio-designer-password-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        // A clean slate even if a previous run of this test crashed before its own cleanup.
        let _ = std::fs::remove_file(dir.join("password"));

        let minted = provision(&dir).expect("a fresh boot mints one");
        assert_eq!(minted.len(), PASSWORD_BYTES * 2, "hex-encoded");

        let reread = provision(&dir).expect("a second boot reads the same file");
        assert_eq!(minted, reread, "the password is stable across boots");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("password"))
                .expect("the file exists")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the password file is owner-only");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_environment_override_wins_and_writes_nothing() {
        let dir = std::env::temp_dir().join(format!(
            "eio-designer-password-env-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let _ = std::fs::remove_file(dir.join("password"));

        let resolved = provision_with(&dir, Some("a-chosen-password"));

        assert_eq!(
            resolved.expect("the override resolves"),
            "a-chosen-password"
        );
        assert!(
            !dir.join("password").exists(),
            "an override must not also be written to disk"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
