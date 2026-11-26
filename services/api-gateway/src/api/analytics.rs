//! Transmission analytics: activity over time and top values.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use platform_auth::{Permission, Resource};
use platform_common::{ScannerId, SiteId};
use platform_storage::model::{ActivityPoint, AnalyticsFilter, Bucket, TopBy, TopEntry};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use crate::error::ApiError;
use crate::state::{AppState, Auth};

/// Window parameters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct WindowQuery {
    /// Start (RFC 3339). Default: `to` minus one day (hourly) or 30 days
    /// (daily).
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub from: Option<OffsetDateTime>,
    /// End (RFC 3339). Default: now.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub to: Option<OffsetDateTime>,
    /// `hour` (window up to 31 days) or `day` (up to 366 days). Default
    /// `hour`.
    #[param(value_type = Option<Bucket>)]
    pub bucket: Option<Bucket>,
    /// Site.
    pub site_id: Option<SiteId>,
    /// Scanner.
    pub scanner_id: Option<ScannerId>,
    /// `top`: what to rank (default `talkgroup`).
    #[param(value_type = Option<TopBy>)]
    pub by: Option<TopBy>,
    /// `top`: entries (1-50, default 10).
    pub limit: Option<u32>,
}

impl WindowQuery {
    fn resolve(&self, auth: &Auth) -> Result<(AnalyticsFilter, Bucket), ApiError> {
        let bucket = self.bucket.unwrap_or(Bucket::Hour);
        let max = match bucket {
            Bucket::Hour => time::Duration::days(31),
            Bucket::Day => time::Duration::days(366),
        };
        let to = self.to.unwrap_or_else(platform_common::now_utc);
        let from = self.from.unwrap_or(match bucket {
            Bucket::Hour => to - time::Duration::DAY,
            Bucket::Day => to - time::Duration::days(30),
        });
        if from >= to {
            return Err(ApiError::bad_request("from must be before to"));
        }
        if to - from > max {
            return Err(ApiError::bad_request(format!(
                "window too long for {} buckets (at most {} days)",
                if bucket == Bucket::Hour {
                    "hourly"
                } else {
                    "daily"
                },
                max.whole_days()
            )));
        }
        Ok((
            AnalyticsFilter {
                from,
                to,
                site_id: self.site_id,
                site_ids: auth.principal.sites.as_filter(),
                scanner_id: self.scanner_id,
            },
            bucket,
        ))
    }
}

async fn authorize(state: &AppState, auth: &Auth, site: Option<SiteId>) -> Result<(), ApiError> {
    state
        .authorize(
            auth,
            Permission::EventView,
            Resource {
                tenant_id: auth.principal.tenant_id,
                site_id: site,
            },
            None,
        )
        .await
}

/// Activity series.
#[derive(Debug, Serialize, ToSchema)]
pub struct Activity {
    /// Bucket size.
    pub bucket: Bucket,
    /// Every bucket from `from` (floored) to `to`, including empty ones.
    pub points: Vec<ActivityPoint>,
}

/// Transmissions and airtime per hour or day.
#[utoipa::path(get, path = "/api/v1/analytics/activity", params(WindowQuery), responses((status = 200, body = Activity)), security(("bearer" = [])))]
pub async fn activity(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Activity>, ApiError> {
    authorize(&state, &auth, q.site_id).await?;
    let (f, bucket) = q.resolve(&auth)?;
    let found = state
        .store
        .transmission_activity(auth.principal.tenant_id, &f, bucket)
        .await?;
    let mut points = Vec::new();
    let mut at = bucket.floor(f.from);
    let mut it = found.into_iter().peekable();
    while at < f.to {
        match it.peek() {
            Some(p) if p.start == at => {
                if let Some(p) = it.next() {
                    points.push(p);
                }
            }
            _ => points.push(ActivityPoint {
                start: at,
                transmissions: 0,
                airtime_ms: 0,
            }),
        }
        at += bucket.duration();
    }
    Ok(Json(Activity { bucket, points }))
}

/// Ranked values.
#[derive(Debug, Serialize, ToSchema)]
pub struct Top {
    /// What was ranked.
    pub by: TopBy,
    /// Most transmissions first.
    pub entries: Vec<TopEntry>,
}

/// The busiest talkgroups, channels, frequencies, scanners or units.
#[utoipa::path(get, path = "/api/v1/analytics/top", params(WindowQuery), responses((status = 200, body = Top)), security(("bearer" = [])))]
pub async fn top(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Top>, ApiError> {
    authorize(&state, &auth, q.site_id).await?;
    let (f, _) = q.resolve(&auth)?;
    let by = q.by.unwrap_or(TopBy::Talkgroup);
    let limit = q.limit.unwrap_or(10);
    if !(1..=50).contains(&limit) {
        return Err(ApiError::bad_request("limit must be 1-50"));
    }
    let entries = state
        .store
        .transmission_top(auth.principal.tenant_id, &f, by, limit)
        .await?;
    Ok(Json(Top { by, entries }))
}
