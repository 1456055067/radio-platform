//! Recording storage: S3-compatible, a local directory, or memory.

use std::sync::Arc;

use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use platform_common::{ScannerId, SiteId, TenantId, TransmissionId};
use serde::Deserialize;
use thiserror::Error;

/// Storage failures.
#[derive(Debug, Error)]
#[error("recording storage: {0}")]
pub struct StorageError(String);

fn err(e: impl std::fmt::Display) -> StorageError {
    StorageError(e.to_string())
}

/// Where recordings go.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageConfig {
    /// S3 or an S3-compatible service. Credentials come from the standard
    /// `AWS_*` environment variables (never the configuration file).
    S3 {
        /// Bucket.
        bucket: String,
        /// Region.
        region: Option<String>,
        /// Custom endpoint (MinIO, Ceph, …).
        endpoint: Option<String>,
        /// Allow plain HTTP to the endpoint (development only).
        #[serde(default)]
        allow_http: bool,
    },
    /// A local directory (single-node deployments).
    Local {
        /// Directory (must exist).
        dir: std::path::PathBuf,
    },
    /// In memory (tests and the dev stack; lost on restart).
    Memory,
}

/// Recording storage.
#[derive(Debug, Clone)]
pub struct RecordingStore {
    inner: Arc<dyn ObjectStore>,
}

/// Content type of stored recordings.
pub const CONTENT_TYPE: &str = "audio/ogg; codecs=opus";

impl RecordingStore {
    /// Opens the configured backend.
    ///
    /// # Errors
    /// Invalid settings or an unusable directory.
    pub fn open(cfg: &StorageConfig) -> Result<Self, StorageError> {
        let inner: Arc<dyn ObjectStore> = match cfg {
            StorageConfig::S3 {
                bucket,
                region,
                endpoint,
                allow_http,
            } => {
                let mut b = object_store::aws::AmazonS3Builder::from_env().with_bucket_name(bucket);
                if let Some(r) = region {
                    b = b.with_region(r);
                }
                if let Some(e) = endpoint {
                    b = b.with_endpoint(e);
                }
                Arc::new(b.with_allow_http(*allow_http).build().map_err(err)?)
            }
            StorageConfig::Local { dir } => {
                Arc::new(object_store::local::LocalFileSystem::new_with_prefix(dir).map_err(err)?)
            }
            StorageConfig::Memory => Arc::new(object_store::memory::InMemory::new()),
        };
        Ok(Self { inner })
    }

    /// Object key for a transmission:
    /// `<tenant>/<site>/<scanner>/<yyyy>/<mm>/<dd>/<transmission>.opus`.
    /// Every component is a UUID or a number, so keys never contain
    /// user-controlled text.
    #[must_use]
    pub fn key(
        tenant: TenantId,
        site: SiteId,
        scanner: ScannerId,
        started: time::OffsetDateTime,
        tx: TransmissionId,
    ) -> String {
        format!(
            "{tenant}/{site}/{scanner}/{:04}/{:02}/{:02}/{tx}.opus",
            started.year(),
            u8::from(started.month()),
            started.day()
        )
    }

    /// Stores a recording.
    ///
    /// # Errors
    /// Backend failures.
    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), StorageError> {
        let path = Path::parse(key).map_err(err)?;
        self.inner
            .put(&path, PutPayload::from(bytes))
            .await
            .map_err(err)?;
        Ok(())
    }

    /// Reads a recording.
    ///
    /// # Errors
    /// Missing object or backend failures.
    pub async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        let path = Path::parse(key).map_err(err)?;
        let r = self.inner.get(&path).await.map_err(err)?;
        Ok(r.bytes().await.map_err(err)?.to_vec())
    }

    /// Deletes a recording. Deleting a missing object succeeds, so
    /// retention sweeps on several replicas cannot fail each other.
    ///
    /// # Errors
    /// Backend failures.
    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let path = Path::parse(key).map_err(err)?;
        match self.inner.delete(&path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(err(e)),
        }
    }

    /// Backend description for status pages (no secrets).
    #[must_use]
    pub fn describe(&self) -> String {
        self.inner.to_string()
    }
}
