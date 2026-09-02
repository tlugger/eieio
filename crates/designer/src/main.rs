//! The Designer's server binary (DESIGNER-SPEC).
//!
//! A thin `clap` shell, matching `eio-daemon`'s own split: everything this binary does lives
//! in `eio_designer`'s lib target, so a test can reach `router()` and `Shared` without
//! spawning this process.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

/// The Designer's command line.
#[derive(Debug, Parser)]
#[command(
    name = "eio-designer",
    version,
    about = "The eieio Designer's server: a small registry, a session gate, and a proxy to nodes"
)]
struct Cli {
    /// Where this Designer's own registry and password live (DESIGNER §2, `password.rs`).
    ///
    /// Created if it does not exist. Unlike a node's `/etc/eieio` (DAEMON §2.1), this is not
    /// a fixed system path: the Designer ships as a container image as much as a bare binary
    /// (DESIGNER §1), and a container's data lives wherever its volume is mounted.
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    /// Where the management API listens.
    ///
    /// Loopback by default, matching the daemon's own reasoning (`eio-daemon::node`'s
    /// `DEFAULT_LISTEN` doc): this process proxies to every node it has a token for, so a
    /// default reaching every interface would make installing the package the exposing act
    /// rather than a deliberate one.
    #[arg(long, default_value = "127.0.0.1:7474")]
    listen: String,

    /// Where the built SPA is, on disk (`assets.rs`). Falls back to this crate's own
    /// compile-time copy of `designer/dist` when nothing is found here.
    ///
    /// Defaults to a `dist/` directory beside this binary itself — the shape a container
    /// image or an installed bare binary ships in — rather than one relative to the current
    /// directory, which would depend on where an operator happened to be standing when they
    /// ran it.
    #[arg(long)]
    assets_dir: Option<PathBuf>,
}

fn default_assets_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("dist")))
        .unwrap_or_else(|| PathBuf::from("dist"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.data_dir)?;

    let password = eio_designer::password::provision(&cli.data_dir)?;
    let db = eio_designer::db::Db::open(&cli.data_dir.join("designer.sqlite3"))?;
    let shared = Arc::new(eio_designer::Shared::new(db, password));

    let assets_dir = cli.assets_dir.unwrap_or_else(default_assets_dir);
    let listener = tokio::net::TcpListener::bind(&cli.listen).await?;
    tracing::info!(
        listen = %cli.listen,
        data_dir = %cli.data_dir.display(),
        assets_dir = %assets_dir.display(),
        "Designer listening"
    );

    // Through `serve` rather than `axum::serve`, so a signal is a clean exit rather than a
    // killed process. `lib.rs` has always had the graceful path; this binary was not using it,
    // which the release pipeline noticed: the daemon's smoke job asserts exit 0 on SIGTERM and
    // the Designer's could not, because there was nothing to assert.
    eio_designer::serve(listener, shared, assets_dir, shutdown()).await?;
    tracing::info!("Designer stopped");
    Ok(())
}

/// Waits for the signal that means "stop".
///
/// `SIGTERM` because that is what an init system and `docker stop` send, and `SIGINT` because
/// that is what a terminal sends. The same two the daemon waits on (`eio-daemon`'s `lib.rs`),
/// deliberately: an operator should not have to remember which of this platform's two servers
/// stops cleanly on which signal.
async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(terminate) => terminate,
            Err(error) => {
                tracing::warn!(%error, "SIGTERM cannot be handled; waiting for SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
