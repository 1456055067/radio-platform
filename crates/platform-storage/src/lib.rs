//! Tenant-scoped persistence for the radio platform.
//!
//! [`Store`] is implemented by [`PgStore`] (PostgreSQL, the production
//! backend) and [`MemoryStore`] (tests and single-process development). Both
//! must pass the same conformance suite in `tests/conformance.rs`.

#![warn(missing_docs)]

pub mod audit_chain;
pub mod bus;
mod memory;
pub mod model;
mod page;
pub mod pg_transport;
mod postgres;
mod store;

pub use bus::{MemoryBus, MessageBus, PgBus};
pub use memory::MemoryStore;
pub use page::{DEFAULT_PAGE, MAX_PAGE, Page, PageRequest};
pub use pg_transport::{
    CheckedTarget, PgDestination, PgTarget, PgTransportError, TransportVerdict,
};
pub use postgres::{MIGRATOR, PgStore};
pub use store::{Store, StoreError, StoreResult};
