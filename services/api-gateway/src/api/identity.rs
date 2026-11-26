//! Caller identity (`/me`) and agent listing.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use platform_auth::{Permission, Resource, SiteScope};
use platform_common::{SiteId, TenantId};
use platform_storage::Page;
use platform_storage::model::Agent;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::error::ApiError;
use crate::state::{AppState, Auth};

/// The authenticated caller, as the gateway sees it.
///
/// Clients use this to decide which controls to show. It is advisory only:
/// every request is authorized again on the server.
#[derive(Debug, Serialize, ToSchema)]
pub struct Me {
    /// OIDC subject.
    pub subject: String,
    /// Display name, if the token carried one.
    pub name: Option<String>,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Site scope: `null` means every site in the tenant.
    pub sites: Option<Vec<SiteId>>,
    /// Granted permissions.
    pub permissions: Vec<Permission>,
    /// Session expiry.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: time::OffsetDateTime,
}

/// Returns the caller's identity, scope and permissions.
#[utoipa::path(get, path = "/api/v1/me", responses((status = 200, body = Me), (status = 401)), security(("bearer" = [])))]
pub async fn me(auth: Auth) -> Json<Me> {
    let p = auth.principal;
    Json(Me {
        subject: p.subject,
        name: p.name,
        tenant_id: p.tenant_id,
        sites: match p.sites {
            SiteScope::All => None,
            SiteScope::Sites(s) => Some(s.into_iter().collect()),
        },
        permissions: p.permissions.into_iter().collect(),
        expires_at: p.expires_at,
    })
}

/// Agent listing filters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct AgentQuery {
    /// Only agents at this site.
    pub site_id: Option<SiteId>,
    /// Page size.
    pub limit: Option<u32>,
    /// Cursor.
    pub cursor: Option<String>,
}

/// An agent registration with its live connection state.
#[derive(Debug, Serialize, ToSchema)]
pub struct AgentView {
    /// Stored registration.
    #[serde(flatten)]
    pub agent: Agent,
    /// Whether the agent is connected to this gateway now.
    pub connected: bool,
}

/// Lists registered agents within the caller's site scope.
#[utoipa::path(get, path = "/api/v1/agents", params(AgentQuery), responses((status = 200, body = Page<AgentView>)), security(("bearer" = [])))]
pub async fn list_agents(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<AgentQuery>,
) -> Result<Json<Page<AgentView>>, ApiError> {
    let p = &auth.principal;
    state
        .authorize(
            &auth,
            Permission::ScannerView,
            Resource {
                tenant_id: p.tenant_id,
                site_id: q.site_id,
            },
            None,
        )
        .await?;
    let scope: Option<Vec<SiteId>> = match (q.site_id, p.sites.as_filter()) {
        (Some(s), _) => Some(vec![s]),
        (None, f) => f,
    };
    let page = super::PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    }
    .page()?;
    let agents = state
        .store
        .list_agents(p.tenant_id, scope.as_deref(), &page)
        .await?;
    Ok(Json(Page {
        items: agents
            .items
            .into_iter()
            .map(|a| AgentView {
                connected: state.hub.is_connected(&a.id),
                agent: a,
            })
            .collect(),
        next_cursor: agents.next_cursor,
    }))
}
