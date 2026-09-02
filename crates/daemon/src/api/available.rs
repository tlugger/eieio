//! `GET /blocks/available` and `GET /blocks/available/{reference}` — browsing a registry,
//! from the node that would run the block (DAEMON-SPEC §9.8).
//!
//! `crate::api::blocks` answers what this node has **installed**; this module answers what it
//! *could* install, without installing anything. The split exists because the node — not a
//! client like the Designer — holds the registry credentials (`auth/registries.toml`,
//! §4.1) and enforces the signature policy (§4.2): a client browsing independently would see a
//! *different* view (private repositories it has no credential for) and could offer a block
//! this node would then refuse to pull. So the answer to "what can I use here" has to come
//! from here, and it is per node by construction — two nodes with different registries
//! configured genuinely offer different blocks.
//!
//! # What these two endpoints honestly promise, and what they do not
//!
//! **A registry cannot be enumerated from nothing.** There is no operation this node could
//! call with zero arguments that means "everything a registry has": `GET /v2/_catalog` is an
//! *optional* extension in the OCI Distribution Specification, and is refused outright by
//! GitHub Container Registry and commonly gated elsewhere even for a credentialed caller. So
//! `GET /blocks/available` does not attempt it and takes a `repository` instead — the OCI path
//! between a registry and a tag, e.g. `ghcr.io/tlugger/filter` — and lists that repository's
//! *tags* via `GET /v2/<repository>/tags/list`, an operation the specification requires for
//! any repository that exists. What comes back is a list of candidate references, not
//! manifests: fetching a manifest (and the wasm layer inside it — see below) for every tag a
//! repository has ever published does not scale to "browse this repository" and this endpoint
//! does not pretend it does. A client that wants to know what one of those references actually
//! is calls the second endpoint.
//!
//! **`GET /blocks/available/{reference}` answers one reference's manifest**, fetched exactly
//! as installing it would fetch it — verified digest, verified signature — and then discarded
//! rather than cached or loaded. ABI §11's manifest is not carried by the OCI manifest or its
//! (empty) config blob; it lives in the block's own `eio:manifest` custom section (ABI §4.3),
//! so answering this honestly means fetching the wasm layer, the same bytes `POST /blocks/pull`
//! would fetch — there is no cheaper request that would tell the truth. Nothing is written to
//! the block cache and `GET /blocks` is unchanged by a browse; installing stays a separate,
//! deliberate `POST /blocks/pull`.
//!
//! # The gate that matters most
//!
//! Both endpoints refuse a host this node has no entry for in `auth/registries.toml`, and this
//! is stricter than an ordinary pull: §4.1 lets a pull reach an *unconfigured* host
//! anonymously, because a pull's reference came from a service file an operator already wrote.
//! A browse's reference or repository is handed to this node by whichever authenticated caller
//! made the HTTP request, so an unconstrained browse would let that caller direct this node's
//! outbound fetches at any host that speaks OCI — and the node's own configuration is the
//! allow-list that stops that (§9.8). [`crate::registry::Registry::is_configured`] is the one
//! place this is checked, and [`crate::registry::Registry::browse`] and
//! [`crate::registry::Registry::tags`] both apply it before making a single request.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::api::error::{ApiError, Kind};

/// A candidate reference `GET /blocks/available` found, not yet inspected.
///
/// No manifest: listing a repository's tags is one request, and fetching a manifest for each
/// would be one more per tag (see this module's doc). A caller that wants one inspects it with
/// `GET /blocks/available/{reference}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AvailableTag {
    /// `<repository>:<tag>` — a reference `GET /blocks/available/{reference}` or `POST
    /// /blocks/pull` accepts as-is.
    pub reference: String,
}

/// What `GET /blocks/available` takes.
#[derive(Debug, Deserialize, IntoParams)]
pub struct AvailableQuery {
    /// The repository to list tags for: `[registry/][namespace/]name`, no tag or digest of its
    /// own (DAEMON §4.1's own repository half of a reference). Required: a registry cannot be
    /// enumerated with nothing named (this module's doc).
    pub repository: Option<String>,
}

/// One reference's manifest, fetched to answer `GET /blocks/available/{reference}` and not
/// installed.
#[derive(Debug, Serialize, ToSchema)]
pub struct AvailableBlock {
    /// The reference this manifest was fetched for.
    pub reference: String,
    /// The block's manifest (ABI §11): ports, properties schema, capabilities, versions.
    pub manifest: serde_json::Value,
}

/// `GET /blocks/available`: the tags a configured repository has, uninstalled (DAEMON §9.8).
#[utoipa::path(
    get,
    path = "/blocks/available",
    tag = "blocks",
    params(AvailableQuery),
    responses(
        (status = 200, description = "The repository's tags, as candidate references", body = Vec<AvailableTag>),
        (status = 400, description = "No `repository` was given", body = ApiError),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 422, description = "The repository named no configured registry, or the registry could not be listed", body = ApiError),
    ),
)]
pub async fn list(
    State(shared): State<crate::api::State>,
    Query(query): Query<AvailableQuery>,
) -> Result<Json<Vec<AvailableTag>>, ApiError> {
    let Some(repository) = query.repository.filter(|value| !value.trim().is_empty()) else {
        return Err(ApiError::new(
            Kind::BadRequest,
            "`GET /blocks/available` takes `?repository=[registry/][namespace/]name`; a \
             registry has no catalog this node promises to enumerate (DAEMON §9.8)",
        ));
    };

    let registry = shared.registry.clone();
    let for_task = repository.clone();
    let tags = tokio::task::spawn_blocking(move || registry.tags(&for_task))
        .await
        .map_err(|error| ApiError::new(Kind::Internal, error.to_string()))?
        .map_err(|reason| unresolvable(&repository, reason))?;

    Ok(Json(
        tags.into_iter()
            .map(|tag| AvailableTag {
                reference: format!("{repository}:{tag}"),
            })
            .collect(),
    ))
}

/// `GET /blocks/available/{reference}`: one reference's manifest, without installing it
/// (DAEMON §9.8).
#[utoipa::path(
    get,
    path = "/blocks/available/{*reference}",
    tag = "blocks",
    responses(
        (status = 200, description = "The reference's manifest", body = AvailableBlock),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 422, description = "The reference named no configured registry, did not resolve, or the artifact was refused", body = ApiError),
    ),
)]
pub async fn inspect(
    State(shared): State<crate::api::State>,
    Path(reference): Path<String>,
) -> Result<Json<AvailableBlock>, ApiError> {
    let registry = shared.registry.clone();
    let for_task = reference.clone();
    let wasm = tokio::task::spawn_blocking(move || registry.browse(&for_task))
        .await
        .map_err(|error| ApiError::new(Kind::Internal, error.to_string()))?
        .map_err(|reason| unresolvable(&reference, reason))?;

    // `_unaided`, for the reason `crate::api::blocks::list` gives: this endpoint answers "here
    // is the manifest", and nothing between here and a deploy will compile it (§4.3).
    let manifest = eio_manifest::validate_unaided(&wasm, None).map_err(|error| {
        unresolvable(
            &reference,
            format!("this artifact is not a loadable block: {error}"),
        )
    })?;

    Ok(Json(AvailableBlock {
        reference,
        manifest: serde_json::to_value(&manifest).unwrap_or(serde_json::Value::Null),
    }))
}

/// `Kind::Unresolvable`, carrying what was being asked about — the same shape
/// `crate::api::blocks::pull` answers a reference that did not resolve with.
fn unresolvable(subject: &str, reason: impl std::fmt::Display) -> ApiError {
    ApiError::detailed(
        Kind::Unresolvable,
        reason.to_string(),
        serde_json::json!({ "block": subject }),
    )
}

#[cfg(test)]
mod tests {
    use crate::api::tests::Harness;
    use crate::registry::fake::Fake;

    /// A golden block's bytes — ABI §13.2's transform, the same fixture `Harness::start`
    /// already seeds the cache with, here published from a fake registry instead so
    /// `eio_manifest::validate_unaided` has a real `eio:manifest` custom section to find.
    fn golden_transform() -> Vec<u8> {
        std::fs::read(eio_conformance::golden::build().join("transform.wasm"))
            .expect("the golden blocks are built")
    }

    /// A harness whose node has `fake` configured in `auth/registries.toml`, keyed by its
    /// host exactly as `Registry::is_configured` looks one up (DAEMON §2.1, §13).
    async fn harness_with(test: &str, fake: &Fake) -> Harness {
        let host = fake.host();
        Harness::start_with(test, move |root| {
            std::fs::create_dir_all(root.join("auth")).expect("auth/");
            std::fs::write(
                root.join("auth").join("registries.toml"),
                format!("[\"{host}\"]\ntoken = \"t\"\n"),
            )
            .expect("a registries.toml");
        })
        .await
    }

    /// DAEMON §9.8's security-relevant requirement: a host this node has no entry for in
    /// `auth/registries.toml` is refused, by both endpoints, and the refusal names the host.
    #[tokio::test]
    async fn an_unconfigured_registry_is_refused_by_both_endpoints() {
        let harness = Harness::start("available-unconfigured").await;
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"not a real block, never fetched");

        let listing = harness
            .get(&format!(
                "/blocks/available?repository={}/filter",
                fake.host()
            ))
            .await;
        assert_eq!(listing.status, 422, "{}", listing.body);
        assert_eq!(listing.json()["error"], "unresolvable");
        let message = listing.json()["message"]
            .as_str()
            .expect("a message")
            .to_string();
        assert!(
            message.contains(&fake.host()) && message.contains("not a configured registry"),
            "the refusal must name the host: {message}"
        );

        let inspected = harness
            .get(&format!(
                "/blocks/available/{}",
                fake.reference("filter", "1.0.0")
            ))
            .await;
        assert_eq!(inspected.status, 422, "{}", inspected.body);
        assert_eq!(inspected.json()["error"], "unresolvable");
        assert!(
            inspected.json()["message"]
                .as_str()
                .unwrap_or_default()
                .contains(&fake.host()),
            "{}",
            inspected.body
        );
    }

    /// The gate actually gates, and both endpoints work once it is satisfied.
    #[tokio::test]
    async fn a_configured_registry_is_browsable() {
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", &golden_transform());
        let harness = harness_with("available-configured", &fake).await;

        let listing = harness
            .get(&format!(
                "/blocks/available?repository={}/filter",
                fake.host()
            ))
            .await;
        assert_eq!(listing.status, 200, "{}", listing.body);
        let tags = listing.json();
        let tags = tags.as_array().expect("a JSON array");
        assert_eq!(tags.len(), 1, "{tags:?}");
        assert_eq!(
            tags[0]["reference"],
            fake.reference("filter", "1.0.0"),
            "{tags:?}"
        );

        let inspected = harness
            .get(&format!(
                "/blocks/available/{}",
                fake.reference("filter", "1.0.0")
            ))
            .await;
        assert_eq!(inspected.status, 200, "{}", inspected.body);
        let body = inspected.json();
        assert_eq!(body["reference"], fake.reference("filter", "1.0.0"));
        assert!(body["manifest"].is_object(), "{body}");
    }

    /// A browse does not install: `GET /blocks` before and after a browse of the same
    /// reference answers the same set.
    #[tokio::test]
    async fn browsing_does_not_populate_the_block_cache() {
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", &golden_transform());
        let harness = harness_with("available-no-install", &fake).await;

        let before = harness.get("/blocks").await.json();
        let inspected = harness
            .get(&format!(
                "/blocks/available/{}",
                fake.reference("filter", "1.0.0")
            ))
            .await;
        assert_eq!(inspected.status, 200, "{}", inspected.body);
        let after = harness.get("/blocks").await.json();
        assert_eq!(
            before, after,
            "a browse must not change what `GET /blocks` reports"
        );
    }
}
