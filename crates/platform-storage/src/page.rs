//! Keyset pagination.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::StoreError;

/// Maximum page size.
pub const MAX_PAGE: u32 = 200;
/// Default page size.
pub const DEFAULT_PAGE: u32 = 50;

/// A page request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageRequest {
    /// Requested size (clamped to `1..=MAX_PAGE`; 0 means default).
    pub limit: u32,
    /// Opaque cursor from a previous page.
    pub cursor: Option<String>,
}

impl PageRequest {
    /// Effective limit.
    #[must_use]
    pub fn limit(&self) -> u32 {
        if self.limit == 0 {
            DEFAULT_PAGE
        } else {
            self.limit.min(MAX_PAGE)
        }
    }

    pub(crate) fn decoded_cursor(&self) -> Result<Option<String>, StoreError> {
        self.cursor
            .as_deref()
            .map(|c| {
                URL_SAFE_NO_PAD
                    .decode(c)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .ok_or_else(|| StoreError::Invalid("malformed cursor".into()))
            })
            .transpose()
    }
}

pub(crate) fn encode_cursor(raw: &str) -> String {
    URL_SAFE_NO_PAD.encode(raw)
}

/// A page of results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Page<T> {
    /// Items.
    pub items: Vec<T>,
    /// Cursor for the next page, if more items exist.
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Builds a page from up to `limit + 1` fetched items.
    pub(crate) fn from_overfetch(
        mut items: Vec<T>,
        limit: u32,
        key: impl Fn(&T) -> String,
    ) -> Self {
        let limit = limit as usize;
        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|i| encode_cursor(&key(i)))
        } else {
            None
        };
        Self { items, next_cursor }
    }
}
