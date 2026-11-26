//! Sites and agent registration.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use ipnet::IpNet;
use platform_auth::{Permission, Resource};
use platform_common::{AgentId, SiteId};
use platform_storage::model::{Agent, AuditOutcome, NewSite, Site};
use platform_storage::{Page, StoreError};
use serde::Deserialize;
use utoipa::ToSchema;

use super::PageQuery;
use crate::error::ApiError;
use crate::state::{AppState, AuditTarget, Auth};

/// Create-site request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSite {
    /// Name (1–128 characters, unique per tenant).
    pub name: String,
    /// Description (≤ 1024 characters).
    pub description: Option<String>,
    /// Latitude (−90..90).
    pub latitude: Option<f64>,
    /// Longitude (−180..180).
    pub longitude: Option<f64>,
    /// Authorized scanner networks (CIDR), e.g. `192.168.50.0/24`.
    pub allowed_networks: Vec<String>,
}

/// Lists sites visible to the caller.
#[utoipa::path(get, path = "/api/v1/sites", params(PageQuery), responses((status = 200, body = Page<Site>)), security(("bearer" = [])))]
pub async fn list_sites(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<Site>>, ApiError> {
    let p = &auth.principal;
    state
        .authorize(
            &auth,
            Permission::ScannerView,
            Resource {
                tenant_id: p.tenant_id,
                site_id: None,
            },
            None,
        )
        .await?;
    let scope = p.sites.as_filter();
    Ok(Json(
        state
            .store
            .list_sites(p.tenant_id, scope.as_deref(), &q.page()?)
            .await?,
    ))
}

/// Creates a site.
#[utoipa::path(post, path = "/api/v1/sites", request_body = CreateSite, responses((status = 201, body = Site), (status = 400), (status = 409)), security(("bearer" = [])))]
pub async fn create_site(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Json(req): Json<CreateSite>,
) -> Result<(StatusCode, Json<Site>), ApiError> {
    let tenant = auth.principal.tenant_id;
    let target = AuditTarget {
        action: "site.create",
        resource_type: "site",
        resource_id: None,
    };
    state
        .authorize(
            &auth,
            Permission::SiteManage,
            Resource {
                tenant_id: tenant,
                site_id: None,
            },
            Some(&target),
        )
        .await?;
    if req
        .description
        .as_ref()
        .is_some_and(|d| d.chars().count() > 1024)
    {
        return Err(ApiError::bad_request("description too long"));
    }
    if req.latitude.is_some_and(|v| !(-90.0..=90.0).contains(&v))
        || req
            .longitude
            .is_some_and(|v| !(-180.0..=180.0).contains(&v))
    {
        return Err(ApiError::bad_request("coordinates out of range"));
    }
    if req.allowed_networks.len() > 64 {
        return Err(ApiError::bad_request("at most 64 networks"));
    }
    let mut nets = Vec::new();
    for n in &req.allowed_networks {
        let net: IpNet = n
            .parse()
            .map_err(|_| ApiError::bad_request(format!("invalid CIDR `{n}`")))?;
        if net.prefix_len() == 0 {
            return Err(ApiError::bad_request(
                "a network covering the entire address space is not allowed",
            ));
        }
        nets.push(net.trunc());
    }
    state.store.ensure_tenant(tenant, "tenant").await?;
    let site = state
        .store
        .create_site(NewSite {
            tenant_id: tenant,
            name: req.name,
            description: req.description,
            latitude: req.latitude,
            longitude: req.longitude,
            allowed_networks: nets,
        })
        .await?;
    let target = AuditTarget {
        resource_id: Some(site.id.to_string()),
        ..target
    };
    state
        .audit(
            &auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({ "name": site.name }),
        )
        .await;
    Ok((StatusCode::CREATED, Json(site)))
}

/// Agent registration request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterAgent {
    /// Agent identity: must equal the DNS SAN (or CN) of its client certificate.
    #[schema(value_type = String)]
    pub agent_id: AgentId,
    /// Site served by the agent.
    pub site_id: SiteId,
    /// Description.
    pub description: Option<String>,
}

/// Registers (or moves) an agent within the caller's tenant.
#[utoipa::path(post, path = "/api/v1/agents", request_body = RegisterAgent, responses((status = 201), (status = 409)), security(("bearer" = [])))]
pub async fn register_agent(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Json(req): Json<RegisterAgent>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let tenant = auth.principal.tenant_id;
    let target = AuditTarget {
        action: "agent.register",
        resource_type: "agent",
        resource_id: Some(req.agent_id.to_string()),
    };
    state
        .authorize(
            &auth,
            Permission::SiteManage,
            Resource {
                tenant_id: tenant,
                site_id: Some(req.site_id),
            },
            Some(&target),
        )
        .await?;
    state.store.get_site(tenant, req.site_id).await?;
    match state.store.get_agent(&req.agent_id).await {
        // An agent identity can never be claimed by another tenant.
        Ok(existing) if existing.tenant_id != tenant => {
            state
                .audit(
                    &auth,
                    &target,
                    AuditOutcome::Denied,
                    serde_json::json!({ "reason": "identity in use" }),
                )
                .await;
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "agent identity already registered",
            ));
        }
        Ok(_) | Err(StoreError::NotFound) => {}
        Err(e) => return Err(e.into()),
    }
    let agent = state
        .store
        .upsert_agent(Agent {
            id: req.agent_id,
            tenant_id: tenant,
            site_id: req.site_id,
            description: req.description,
            last_seen_at: None,
        })
        .await?;
    state
        .audit(
            &auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({ "site_id": agent.site_id }),
        )
        .await;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "agent_id": agent.id, "site_id": agent.site_id })),
    ))
}
