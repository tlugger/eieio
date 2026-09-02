//! Serving the built SPA (DESIGNER-SPEC §1): `tower-http`'s `ServeDir` over a real directory
//! on disk first, this crate's own compile-time `rust-embed` copy of the same tree as the
//! fallback when there is nothing on disk to find.
//!
//! # Why both, and not `rust-embed` alone
//!
//! §1 asks for the SPA "out of the binary via `rust-embed` behind `tower-http`'s `ServeDir`",
//! and the two crates earn their place doing two different jobs rather than one wrapping the
//! other: `ServeDir` answers from whatever is at `--assets-dir` right now, with no rebuild —
//! the SPA agent building `designer/dist` in parallel with this crate, and any later `npm
//! run build` an operator runs beside an already-running binary, both take effect on the very
//! next request. `rust-embed`'s compiled-in copy is what makes the *bare binary* claim (§1)
//! true when there is no sibling `dist/` on the machine at all — copied in from whatever was
//! in `designer/dist` when this crate itself was built, which is `.gitkeep` alone until the
//! SPA agent's tree is merged with this one (see this crate's own top-level report for
//! exactly what that means for `cargo build` today).
//!
//! `axum-embed` was rejected for the reason the plan for this crate states: it pins
//! `axum-core ^0.4` and this workspace's `axum` 0.8 needs `^0.5`, so it does not resolve.

use std::path::PathBuf;

use axum::Router;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;
use tower_http::services::ServeDir;

/// This crate's compile-time copy of `designer/dist`.
///
/// Read at compile time relative to *this* crate's manifest, not the workspace root — cargo
/// resolves `$CARGO_MANIFEST_DIR` per crate, and `crates/designer` is two levels below
/// `designer/dist`'s parent.
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../designer/dist"]
struct Embedded;

/// The whole asset-serving stack, mounted as `Router::fallback_service`.
///
/// `ServeDir::fallback` (not `not_found_service`) is deliberate: it hands the request to the
/// embedded lookup below and keeps *that* lookup's own status code, so an asset present in
/// the compiled-in copy but not on disk still answers `200` rather than being forced to `404`
/// by `ServeDir`'s own not-found handling.
pub fn service(dist_dir: PathBuf) -> ServeDir<Router<()>> {
    ServeDir::new(dist_dir)
        .append_index_html_on_directories(true)
        .fallback(Router::new().fallback(embedded_or_index))
}

/// The embedded fallback: this path, then `index.html` (SPA client-side routing), then a 404
/// that says why — never a panic, because an operator building the daemon side of this
/// feature before the SPA exists is exactly the situation this function has to survive.
async fn embedded_or_index(request: Request) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    if let Some(response) = respond(path) {
        return response;
    }
    if let Some(response) = respond("index.html") {
        return response;
    }
    (
        StatusCode::NOT_FOUND,
        "no Designer UI is embedded in this binary and none was found on disk at \
         --assets-dir; `designer/dist` was empty when this binary was built — run \
         `just designer-build` (or `just run-designer`, which does it) and rebuild",
    )
        .into_response()
}

fn respond(path: &str) -> Option<Response> {
    let file = Embedded::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(
        (
            [(header::CONTENT_TYPE, mime.essence_str().to_owned())],
            file.data.into_owned(),
        )
            .into_response(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whichever way the embed went, `/` answers rather than panicking.
    ///
    /// **This asserts both branches deliberately.** It used to assert only the empty one,
    /// because `designer/dist` held nothing but `.gitkeep` while the SPA was built in
    /// parallel (eieio-m9s.1's split) — and it then failed the moment `just designer-build`
    /// existed and someone ran it, which is a test encoding a temporary fact about a
    /// developer's working tree rather than a property of the code.
    ///
    /// Both outcomes are correct and which one holds is a build input, so the test says so:
    /// an empty embed MUST fall through to a `404` that explains itself, and a populated one
    /// MUST serve the SPA's entry point.
    ///
    /// The oracle is `Embedded::get`, deliberately, and **not** `Embedded::iter`: in a debug
    /// build `rust-embed` compiles in the file *list* but reads the *bytes* from disk on each
    /// `get`, so the two disagree the moment `designer/dist` changes without a rebuild —
    /// `iter` reports what was there at compile time and `get` reports what is there now.
    /// Asking the same way the handler asks is the only way this test cannot go stale for the
    /// reason its predecessor did. (Measured: emptying `dist` without rebuilding makes `iter`
    /// non-empty and `get` `None`.)
    #[tokio::test]
    async fn the_index_is_served_when_embedded_and_explained_when_not() {
        let embedded = Embedded::get("index.html").is_some();
        let request = Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .expect("a minimal request");
        let response = embedded_or_index(request).await;

        if embedded {
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "designer/dist was embedded, so / must serve the SPA"
            );
        } else {
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "designer/dist was empty, so / must explain that rather than panic"
            );
        }
    }
}
