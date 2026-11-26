//! Scanner telemetry: conversion of `GSI`/`PSI` data into
//! [`platform_common::TelemetrySnapshot`], transmission detection, and an
//! async collector that emits snapshots and events.

#![warn(missing_docs)]

mod collector;
mod detector;
mod snapshot;

pub use collector::{CollectionMode, CollectorConfig, CollectorOutput, run_collector};
pub use detector::{DetectorConfig, TransmissionDetector};
pub use snapshot::{materially_different, snapshot_from_info};
