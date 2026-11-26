//! Audit chain verification and audited export (Phase 6, ADR 0011).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use platform_auth::{Permission, Resource};
use platform_storage::PageRequest;
use platform_storage::audit_chain::{ChainBreak, Verifier};
use platform_storage::model::{AuditOutcome, AuditRecord};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::history::{AuditQuery, authorize_audit};
use crate::error::ApiError;
use crate::state::{AppState, AuditTarget, Auth};

/// Records read per store call while verifying.
const BATCH: u32 = 1000;
/// Most records in one export.
pub const MAX_EXPORT: usize = 100_000;

/// Result of verifying a tenant's audit chain.
#[derive(Debug, Serialize, ToSchema)]
pub struct Verification {
    /// The whole chain verified and matches the stored head.
    pub ok: bool,
    /// Records verified before the first problem (or all of them).
    pub verified: u64,
    /// Records written before the chain existed (not covered).
    pub unchained: u64,
    /// Stored head sequence number.
    pub head_seq: u64,
    /// Stored head hash (hex). Record it outside the database to detect a
    /// rewritten chain later.
    pub head_hash: String,
    /// The first problem found.
    pub failure: Option<ChainBreak>,
}

/// Verifies the caller's tenant audit chain end to end.
#[utoipa::path(get, path = "/api/v1/audit/verify", responses((status = 200, body = Verification)), security(("bearer" = [])))]
pub async fn verify(
    State(s): State<Arc<AppState>>,
    auth: Auth,
) -> Result<Json<Verification>, ApiError> {
    authorize_audit(&s, &auth).await?;
    let tenant = auth.principal.tenant_id;
    let head = s.store.audit_head(tenant).await?;
    let mut v = Verifier::default();
    let mut after = 0;
    let mut failure = None;
    'outer: loop {
        let batch = s.store.audit_chain(tenant, after, BATCH).await?;
        if batch.is_empty() {
            break;
        }
        for r in &batch {
            if r.seq.is_some_and(|q| q > head.seq) {
                break 'outer; // appended after the head was read
            }
            if let Err(e) = v.push(r) {
                failure = Some(e);
                break 'outer;
            }
            after = r.seq.unwrap_or(after);
        }
    }
    if failure.is_none()
        && let Err(e) = v.finish(head.seq, &head.hash)
    {
        failure = Some(e);
    }
    if let Some(f) = &failure {
        tracing::error!(tenant = %tenant, seq = f.seq, reason = %f.reason, "audit chain verification failed");
    }
    Ok(Json(Verification {
        ok: failure.is_none(),
        verified: v.verified,
        unchained: head.unchained,
        head_seq: head.seq,
        head_hash: head.hash,
        failure,
    }))
}

/// Export format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// One JSON record per line, with hashes (verifiable offline).
    #[default]
    Jsonl,
    /// Comma-separated values.
    Csv,
}

/// Export format parameter (the audit filters apply too).
#[derive(Debug, Deserialize, IntoParams)]
pub struct ExportQuery {
    /// Format (default `jsonl`).
    #[param(value_type = Option<ExportFormat>)]
    pub format: Option<ExportFormat>,
}

fn csv_field(v: &str) -> String {
    let mut s = v.to_owned();
    // Neutralise spreadsheet formulas.
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        s.insert(0, '\'');
    }
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

fn to_csv(rows: &[AuditRecord]) -> String {
    let mut out = String::from(
        "id,seq,at,actor,action,resource_type,resource_id,outcome,source_ip,details,prev_hash,hash\r\n",
    );
    for r in rows {
        let at =
            r.at.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
        let fields = [
            r.id.to_string(),
            r.seq.map(|s| s.to_string()).unwrap_or_default(),
            at,
            r.actor.clone(),
            r.action.clone(),
            r.resource_type.clone(),
            r.resource_id.clone().unwrap_or_default(),
            r.outcome.as_str().to_owned(),
            r.source_ip.map(|i| i.to_string()).unwrap_or_default(),
            r.details.to_string(),
            r.prev_hash.clone().unwrap_or_default(),
            r.hash.clone().unwrap_or_default(),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|f| csv_field(f))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    }
    out
}

/// Exports audit records (oldest first) matching the filters. Needs
/// `audit_view` and `export`; audited (`audit.export`). At most 100 000
/// records: narrow the filters otherwise (**422**).
#[utoipa::path(get, path = "/api/v1/audit/export", params(ExportQuery, AuditQuery), responses((status = 200, description = "JSON Lines or CSV"), (status = 422)), security(("bearer" = [])))]
pub async fn export(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<ExportQuery>,
    Query(fq): Query<AuditQuery>,
) -> Result<Response, ApiError> {
    let target = AuditTarget {
        action: "audit.export",
        resource_type: "audit",
        resource_id: None,
    };
    let res = Resource {
        tenant_id: auth.principal.tenant_id,
        site_id: None,
    };
    s.authorize(&auth, Permission::AuditView, res, Some(&target))
        .await?;
    s.authorize(&auth, Permission::Export, res, Some(&target))
        .await?;
    let filter = fq.filter()?;
    let format = q.format.unwrap_or_default();
    let mut rows: Vec<AuditRecord> = Vec::new();
    let mut cursor = None;
    loop {
        let page = s
            .store
            .list_audit(
                auth.principal.tenant_id,
                &filter,
                &PageRequest {
                    limit: platform_storage::MAX_PAGE,
                    cursor,
                },
            )
            .await?;
        rows.extend(page.items);
        if rows.len() > MAX_EXPORT {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("more than {MAX_EXPORT} records match; narrow the time range or filters"),
            ));
        }
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    rows.reverse();
    // Audit the export itself before returning it.
    s.audit(
        &auth,
        &target,
        AuditOutcome::Success,
        serde_json::json!({
            "records": rows.len(),
            "format": if format == ExportFormat::Csv { "csv" } else { "jsonl" },
            "from": fq.from.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
            "to": fq.to.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
        }),
    )
    .await;
    let (body, ctype, ext) = match format {
        ExportFormat::Jsonl => (
            rows.iter()
                .filter_map(|r| serde_json::to_string(r).ok())
                .map(|l| l + "\n")
                .collect::<String>(),
            "application/jsonl",
            "jsonl",
        ),
        ExportFormat::Csv => (to_csv(&rows), "text/csv; charset=utf-8", "csv"),
    };
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"audit.{ext}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_neutralises_formulas_and_quotes() {
        assert_eq!(csv_field("=cmd()"), "'=cmd()");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("plain"), "plain");
    }
}
