//! Strongly typed identifiers.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, utoipa::ToSchema)]
        #[serde(transparent)]
        #[schema(value_type = String, format = Uuid)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generates a new random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

uuid_id!(
    /// Tenant identifier.
    TenantId
);
uuid_id!(
    /// Geographic site identifier.
    SiteId
);
uuid_id!(
    /// Scanner identifier.
    ScannerId
);
uuid_id!(
    /// Operator control lease identifier.
    LeaseId
);
uuid_id!(
    /// Recorded or observed transmission identifier.
    TransmissionId
);
uuid_id!(
    /// Remote command request identifier.
    RequestId
);
uuid_id!(
    /// Scanner group identifier.
    GroupId
);

/// Agent identifier, taken from the agent's mTLS certificate identity.
///
/// Restricted to `[A-Za-z0-9._-]{1,128}`, so it is safe to log and to use in
/// URLs.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(try_from = "String", into = "String")]
#[schema(value_type = String)]
pub struct AgentId(String);

/// Error for an invalid [`AgentId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid agent id: must match [A-Za-z0-9._-]{{1,128}}")]
pub struct InvalidAgentId;

impl AgentId {
    /// Validates an agent identifier.
    ///
    /// # Errors
    /// Returns [`InvalidAgentId`] if the value is empty, too long or
    /// contains other characters.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidAgentId> {
        let value = value.into();
        let ok = (1..=128).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if ok {
            Ok(Self(value))
        } else {
            Err(InvalidAgentId)
        }
    }

    /// The identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AgentId {
    type Error = InvalidAgentId;
    fn try_from(v: String) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}

impl From<AgentId> for String {
    fn from(v: AgentId) -> Self {
        v.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_validation() {
        assert!(AgentId::new("site-a.agent_1").is_ok());
        for bad in ["", "a b", "a/b", "ä", &"x".repeat(129)] {
            assert!(AgentId::new(bad).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<AgentId>("\"../etc\"").is_err());
    }

    #[test]
    fn uuid_ids_round_trip() {
        let id = ScannerId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<ScannerId>(&json).unwrap(), id);
        assert_eq!(id.to_string().parse::<ScannerId>().unwrap(), id);
    }
}
