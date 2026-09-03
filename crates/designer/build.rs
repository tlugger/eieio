//! Guarantees `designer/dist` exists before `rust-embed` looks for it.
//!
//! `src/assets.rs` embeds the SPA with `#[derive(RustEmbed)] #[folder = ".../designer/dist"]`,
//! and rust-embed makes an **absent folder a compile error** — not an empty embed. So on any
//! checkout where the SPA has not been built, this crate does not compile, and neither does
//! `cargo build --workspace`, and neither does `just ci`. That is what kept CI red from
//! 2026-08-24 to 2026-09-03.
//!
//! The obvious fix — commit a `.gitkeep` inside `dist/` — was tried (commit `51231c7`) and is
//! **structurally doomed**: `vite build` empties `outDir` before writing, so the first `npm run
//! build` deletes the tracked file, and the next `git add` commits the deletion. That is
//! exactly what happened in `9029eeb`, eight commits later, silently.
//!
//! So the directory is created here instead, by the crate that needs it, every build. An empty
//! `dist` is already a supported state — `just ci` calls a machine with no npm degraded rather
//! than broken, and `assets.rs` serves no UI rather than failing — so creating it empty changes
//! nothing except that the build works.
fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("designer")
        .join("dist");
    if let Err(error) = std::fs::create_dir_all(&dist) {
        // Not a hard failure: if the directory cannot be created, rust-embed's own error is
        // clearer about what is wrong than anything this could say.
        println!("cargo::warning=could not create {}: {error}", dist.display());
    }
    println!("cargo::rerun-if-changed=build.rs");
}
