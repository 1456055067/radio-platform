//! Scanner discovery at a site (`POST /api/v1/sites/{id}/discover`).
//!
//! The site's agent probes addresses with the read-only `MDL` command and
//! reports which answered. The request is checked twice, as for
//! enrollment: here against the site's authorized networks and the
//! gateway's allowed ports, and again by the agent against its own local
//! policy. Endpoints already enrolled are never probed. Nothing is enrolled
//! automatically.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ipnet::IpNet;
use platform_auth::{Permission, Resource};
use platform_common::wire::Discovered;
use platform_common::{AgentId, SiteId};
use platform_storage::PageRequest;
use platform_storage::model::{AuditOutcome, ScannerFilter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::hub::AgentOp;
use crate::state::{AppState, AuditTarget, Auth};

/// Discovery can take a few seconds for a /22.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Discovery request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoverRequest {
    /// Agent at this site that performs the probe.
    #[schema(value_type = String)]
    pub agent_id: AgentId,
    /// CIDRs to probe (default: the site's authorized networks). Each must
    /// lie inside them; at most 1024 addresses in total.
    pub networks: Option<Vec<String>>,
    /// UDP port (default 50536; must be an allowed scanner port).
    pub port: Option<u16>,
}

/// Discovery result.
#[derive(Debug, Serialize, ToSchema)]
pub struct DiscoverResponse {
    /// Unenrolled endpoints that answered like a scanner.
    pub candidates: Vec<Discovered>,
    /// Enrolled endpoints that were skipped.
    pub skipped_enrolled: usize,
}

/// Probes a site's networks for scanners that are not enrolled yet.
#[utoipa::path(post, path = "/api/v1/sites/{id}/discover", params(("id" = SiteId, Path)), request_body = DiscoverRequest, responses((status = 200, body = DiscoverResponse), (status = 403), (status = 422), (status = 501, description = "Agent too old for discovery"), (status = 503)), security(("bearer" = [])))]
pub async fn discover(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(site_id): Path<SiteId>,
    Json(req): Json<DiscoverRequest>,
) -> Result<Json<DiscoverResponse>, ApiError> {
    let tenant = auth.principal.tenant_id;
    let target = AuditTarget {
        action: "site.discover",
        resource_type: "site",
        resource_id: Some(site_id.to_string()),
    };
    let site = state.store.get_site(tenant, site_id).await?;
    state
        .authorize(
            &auth,
            Permission::ScannerManage,
            Resource {
                tenant_id: tenant,
                site_id: Some(site.id),
            },
            Some(&target),
        )
        .await?;
    let agent = state.store.get_agent(&req.agent_id).await?;
    if agent.tenant_id != tenant || agent.site_id != site.id {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the agent is not registered at this site",
        ));
    }
    let port = req.port.unwrap_or(50536);
    if !state.limits.scanner_ports.contains(&port) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("port {port} is not an allowed scanner port"),
        ));
    }
    let networks: Vec<IpNet> = match req.networks {
        None => site.allowed_networks.clone(),
        Some(list) => {
            if list.len() > 64 {
                return Err(ApiError::bad_request("at most 64 networks"));
            }
            let mut v = Vec::with_capacity(list.len());
            for n in &list {
                let net: IpNet = n
                    .parse()
                    .map_err(|_| ApiError::bad_request(format!("invalid CIDR `{n}`")))?;
                v.push(net.trunc());
            }
            v
        }
    };
    if networks.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the site has no authorized networks to probe",
        ));
    }
    if let Some(n) = networks
        .iter()
        .find(|n| !site.allowed_networks.iter().any(|a| a.contains(*n)))
    {
        state
            .audit(
                &auth,
                &target,
                AuditOutcome::Denied,
                serde_json::json!({ "reason": "network outside site", "network": n.to_string() }),
            )
            .await;
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{n} is outside the site's authorized networks"),
        ));
    }

    // Never probe anything already enrolled at this site or on this agent.
    let mut exclude: BTreeSet<SocketAddr> = state
        .store
        .scanners_for_agent(&agent.id)
        .await?
        .into_iter()
        .map(|s| SocketAddr::new(s.host, s.port))
        .collect();
    let mut page = PageRequest {
        limit: 200,
        cursor: None,
    };
    loop {
        let p = state
            .store
            .list_scanners(
                tenant,
                &ScannerFilter {
                    site_id: Some(site.id),
                    site_ids: None,
                    agent_id: None,
                },
                &page,
            )
            .await?;
        exclude.extend(p.items.iter().map(|s| SocketAddr::new(s.host, s.port)));
        match p.next_cursor {
            Some(c) if exclude.len() < 10_000 => page.cursor = Some(c),
            _ => break,
        }
    }
    let skipped_enrolled = exclude
        .iter()
        .filter(|e| e.port() == port && networks.iter().any(|n| n.contains(&e.ip())))
        .count();

    let outcome = state
        .hub
        .request(
            &agent.id,
            AgentOp::Discover {
                networks: networks.clone(),
                port,
                exclude: exclude.into_iter().collect(),
            },
            DISCOVERY_TIMEOUT,
        )
        .await;
    let candidates: Vec<Discovered> = match outcome {
        Ok(v) => serde_json::from_value(v)
            .map_err(|_| ApiError::new(StatusCode::BAD_GATEWAY, "malformed discovery result"))?,
        Err(e) => {
            state
                .audit(
                    &auth,
                    &target,
                    AuditOutcome::Failure,
                    serde_json::json!({ "error": e.to_string() }),
                )
                .await;
            return Err(e.into());
        }
    };
    state
        .audit(
            &auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({
                "agent_id": agent.id,
                "networks": networks.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "port": port,
                "found": candidates.len(),
            }),
        )
        .await;
    Ok(Json(DiscoverResponse {
        candidates,
        skipped_enrolled,
    }))
}
