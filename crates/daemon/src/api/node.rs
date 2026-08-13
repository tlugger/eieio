//! `GET /node` — what this node is (DAEMON-SPEC §9).
//!
//! Identity, the limits and budgets every instance runs under, and the versions. The point of
//! it for an agent (SCOPE §4) is planning: whether a block will fit here is a question about
//! `limits` and `budgets`, and whether a service can use a capability is a question about
//! `capabilities` — all answerable before anything is deployed.

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;

/// This node, as `GET /node` reports it.
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeInfo {
    /// The node's opaque, stable identity (DAEMON §2.1). Nothing may parse meaning out of it.
    pub id: String,
    /// A label for people, if the operator set one. Nothing resolves by it.
    pub name: Option<String>,
    /// The daemon's version.
    pub version: String,
    /// The ABI version this daemon implements (ABI §12).
    pub abi: String,
    /// The capability namespaces a block may use on this node (ABI §7, SCOPE §3.3).
    ///
    /// A block whose manifest declares anything outside this list is refused at load, so this
    /// is what a service may be built from here rather than a list of what exists.
    pub capabilities: Vec<String>,
    /// What every instance reports as its limits (ABI §5.2, §9.7).
    pub limits: NodeLimits,
    /// What one guest entry and one expression may consume (ABI §10, EXPR §9).
    pub budgets: NodeBudgets,
    /// Whether this node refuses a block it cannot verify a signature for (DAEMON §4.2).
    pub require_signed: bool,
}

/// See [`NodeInfo::limits`].
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeLimits {
    /// The largest payload one instance may emit or be delivered, in bytes.
    pub max_payload: u32,
    /// The largest number of signals in one batch.
    pub max_batch: u32,
}

/// See [`NodeInfo::budgets`].
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeBudgets {
    /// What one guest callback may consume, roughly one unit per WASM instruction.
    pub fuel: u64,
    /// The wall-clock backstop for a callback that is blocked rather than busy, in ms.
    pub deadline_ms: u64,
    /// What one property expression may consume (EXPR §9).
    pub expr_max_fuel: u32,
}

/// What this node is: identity, limits, budgets and versions.
///
/// Everything needed to decide whether a service can run here before deploying one — which is
/// what makes it the first call an agent makes (SCOPE §4).
#[utoipa::path(
    get,
    path = "/node",
    tag = "node",
    responses(
        (status = 200, description = "This node's identity and the limits it runs blocks under", body = NodeInfo),
        (status = 401, description = "Missing or wrong bearer token", body = crate::api::error::ApiError),
    ),
)]
pub async fn get_node(State(shared): State<crate::api::State>) -> Json<NodeInfo> {
    let node = &shared.node;
    let eval = node.budgets.expr.eval();
    Json(NodeInfo {
        id: node.id.clone(),
        name: node.name.clone(),
        version: String::from(env!("CARGO_PKG_VERSION")),
        abi: {
            let abi = eio_manifest::Abi::CURRENT;
            format!("{}.{}", abi.major, abi.minor)
        },
        capabilities: crate::instance::IMPLEMENTED_CAPABILITIES
            .iter()
            .map(|capability| String::from(*capability))
            .collect(),
        limits: NodeLimits {
            max_payload: node.limits.max_payload,
            max_batch: node.limits.max_batch,
        },
        budgets: NodeBudgets {
            fuel: node.budgets.fuel,
            deadline_ms: node.budgets.deadline.as_millis() as u64,
            expr_max_fuel: eval.max_fuel,
        },
        require_signed: node.signing.require_signed,
    })
}
