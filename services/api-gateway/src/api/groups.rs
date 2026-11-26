//! Scanner groups.
//!
//! Groups are tenant-wide and may span sites, so:
//! - creating and deleting a group needs `scanner_manage` **and** access to
//!   every site;
//! - adding or removing a member needs `scanner_manage` on that scanner's
//!   site;
//! - listings show only the members inside the caller's site scope, so a
//!   group never reveals scanners at sites the caller cannot see.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use platform_auth::{Permission, Resource, SiteScope};
use platform_common::{GroupId, ScannerId};
use platform_storage::model::{AuditOutcome, ScannerGroup};
use serde::Deserialize;
use utoipa::ToSchema;

use super::scanners::load;
use crate::error::ApiError;
use crate::state::{AppState, AuditTarget, Auth};

/// Group creation request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateGroup {
    /// Name (unique within the tenant).
    pub name: String,
}

async fn visible(state: &AppState, auth: &Auth, mut g: ScannerGroup) -> ScannerGroup {
    if let SiteScope::Sites(_) = &auth.principal.sites {
        let mut keep = Vec::with_capacity(g.members.len());
        for m in g.members {
            if let Ok(rec) = state.store.get_scanner(auth.principal.tenant_id, m).await
                && auth.principal.sites.covers(rec.site_id)
            {
                keep.push(m);
            }
        }
        g.members = keep;
    }
    g
}

fn tenant_wide(auth: &Auth) -> Result<(), ApiError> {
    match auth.principal.sites {
        SiteScope::All => Ok(()),
        SiteScope::Sites(_) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "groups can span sites; creating or deleting one needs access to every site",
        )),
    }
}

async fn manage_tenant(
    state: &AppState,
    auth: &Auth,
    target: &AuditTarget<'_>,
) -> Result<(), ApiError> {
    state
        .authorize(
            auth,
            Permission::ScannerManage,
            Resource {
                tenant_id: auth.principal.tenant_id,
                site_id: None,
            },
            Some(target),
        )
        .await?;
    if let Err(e) = tenant_wide(auth) {
        state
            .audit(
                auth,
                target,
                AuditOutcome::Denied,
                serde_json::json!({ "reason": "site-scoped principal" }),
            )
            .await;
        return Err(e);
    }
    Ok(())
}

/// Lists groups (members limited to the caller's site scope).
#[utoipa::path(get, path = "/api/v1/groups", responses((status = 200, body = Vec<ScannerGroup>)), security(("bearer" = [])))]
pub async fn list(
    State(state): State<Arc<AppState>>,
    auth: Auth,
) -> Result<Json<Vec<ScannerGroup>>, ApiError> {
    state
        .authorize(
            &auth,
            Permission::ScannerView,
            Resource {
                tenant_id: auth.principal.tenant_id,
                site_id: None,
            },
            None,
        )
        .await?;
    let groups = state.store.list_groups(auth.principal.tenant_id).await?;
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        out.push(visible(&state, &auth, g).await);
    }
    Ok(Json(out))
}

/// Gets one group.
#[utoipa::path(get, path = "/api/v1/groups/{id}", params(("id" = GroupId, Path)), responses((status = 200, body = ScannerGroup), (status = 404)), security(("bearer" = [])))]
pub async fn get(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<GroupId>,
) -> Result<Json<ScannerGroup>, ApiError> {
    state
        .authorize(
            &auth,
            Permission::ScannerView,
            Resource {
                tenant_id: auth.principal.tenant_id,
                site_id: None,
            },
            None,
        )
        .await?;
    let g = state.store.get_group(auth.principal.tenant_id, id).await?;
    Ok(Json(visible(&state, &auth, g).await))
}

/// Creates a group.
#[utoipa::path(post, path = "/api/v1/groups", request_body = CreateGroup, responses((status = 201, body = ScannerGroup), (status = 403), (status = 409)), security(("bearer" = [])))]
pub async fn create(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Json(req): Json<CreateGroup>,
) -> Result<(StatusCode, Json<ScannerGroup>), ApiError> {
    let target = AuditTarget {
        action: "group.create",
        resource_type: "group",
        resource_id: None,
    };
    manage_tenant(&state, &auth, &target).await?;
    let name = req.name.trim();
    let g = state
        .store
        .create_group(auth.principal.tenant_id, name)
        .await?;
    let target = AuditTarget {
        resource_id: Some(g.id.to_string()),
        ..target
    };
    state
        .audit(
            &auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({ "name": g.name }),
        )
        .await;
    Ok((StatusCode::CREATED, Json(g)))
}

/// Deletes a group. Its scanners are unaffected.
#[utoipa::path(delete, path = "/api/v1/groups/{id}", params(("id" = GroupId, Path)), responses((status = 204), (status = 403), (status = 404)), security(("bearer" = [])))]
pub async fn remove(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<GroupId>,
) -> Result<StatusCode, ApiError> {
    let target = AuditTarget {
        action: "group.delete",
        resource_type: "group",
        resource_id: Some(id.to_string()),
    };
    manage_tenant(&state, &auth, &target).await?;
    state
        .store
        .delete_group(auth.principal.tenant_id, id)
        .await?;
    state
        .audit(&auth, &target, AuditOutcome::Success, serde_json::json!({}))
        .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn membership(
    state: &AppState,
    auth: &Auth,
    group: GroupId,
    scanner: ScannerId,
    add: bool,
) -> Result<StatusCode, ApiError> {
    let target = AuditTarget {
        action: if add {
            "group.member.add"
        } else {
            "group.member.remove"
        },
        resource_type: "group",
        resource_id: Some(group.to_string()),
    };
    // Scanner must exist in the tenant and be manageable at its site.
    load(
        state,
        auth,
        scanner,
        Permission::ScannerManage,
        Some(&target),
    )
    .await?;
    let tenant = auth.principal.tenant_id;
    if add {
        state.store.add_group_member(tenant, group, scanner).await?;
    } else {
        state
            .store
            .remove_group_member(tenant, group, scanner)
            .await?;
    }
    state
        .audit(
            auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({ "scanner_id": scanner }),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Adds a scanner to a group (idempotent).
#[utoipa::path(put, path = "/api/v1/groups/{id}/members/{scanner_id}", params(("id" = GroupId, Path), ("scanner_id" = ScannerId, Path)), responses((status = 204), (status = 403), (status = 404)), security(("bearer" = [])))]
pub async fn add_member(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path((id, scanner)): Path<(GroupId, ScannerId)>,
) -> Result<StatusCode, ApiError> {
    membership(&state, &auth, id, scanner, true).await
}

/// Removes a scanner from a group.
#[utoipa::path(delete, path = "/api/v1/groups/{id}/members/{scanner_id}", params(("id" = GroupId, Path), ("scanner_id" = ScannerId, Path)), responses((status = 204), (status = 403), (status = 404)), security(("bearer" = [])))]
pub async fn remove_member(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path((id, scanner)): Path<(GroupId, ScannerId)>,
) -> Result<StatusCode, ApiError> {
    membership(&state, &auth, id, scanner, false).await
}
