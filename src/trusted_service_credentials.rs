//! Credential isolation boundary for trusted server services.
//!
//! The first runtime phase does not mint or transport service secrets. This
//! module defines the future broker contract while the default implementation
//! fails closed. Lease metadata is intentionally not a credential value and is
//! never persisted in runtime records, manifests, backups, or client payloads.

use std::collections::BTreeSet;
use std::fmt;

const SUPPORTED_CREDENTIAL_CAPABILITIES: &[&str] = &[
    "storage",
    "read:clients",
    "read:layouts",
    "client_extension",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialScope {
    pub addon_id: String,
    pub activation_instance: String,
    pub capabilities: Vec<String>,
}

impl CredentialScope {
    pub fn validate(&self) -> Result<(), CredentialError> {
        if self.addon_id.is_empty()
            || self.addon_id.len() > 64
            || self
                .addon_id
                .chars()
                .any(|character| character.is_control())
            || self.activation_instance.is_empty()
            || self.activation_instance.len() > 128
            || self
                .activation_instance
                .chars()
                .any(|character| character.is_control())
        {
            return Err(CredentialError::Invalid(
                "Invalid trusted service credential scope".to_string(),
            ));
        }
        crate::addons::validate_addon_storage_namespace(&self.addon_id)
            .map_err(CredentialError::Invalid)?;
        crate::addons::validate_addon_storage_namespace(&self.activation_instance)
            .map_err(CredentialError::Invalid)?;
        if self.capabilities.len() > 8
            || self
                .capabilities
                .iter()
                .any(|capability| capability.is_empty() || capability.len() > 64)
        {
            return Err(CredentialError::Invalid(
                "Invalid trusted service credential scope".to_string(),
            ));
        }
        let mut capabilities = BTreeSet::new();
        if self.capabilities.iter().any(|capability| {
            !SUPPORTED_CREDENTIAL_CAPABILITIES.contains(&capability.as_str())
                || !capabilities.insert(capability)
        }) {
            return Err(CredentialError::Invalid(
                "Invalid trusted service credential scope".to_string(),
            ));
        }
        Ok(())
    }
}

/// Opaque broker metadata. It is deliberately not a bearer token or secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialLease {
    pub lease_id: String,
    pub expires_at: String,
}

impl CredentialLease {
    pub fn validate(&self) -> Result<(), CredentialError> {
        if self.lease_id.is_empty()
            || self.lease_id.len() > 128
            || !self.lease_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            })
            || self.expires_at.is_empty()
            || self.expires_at.len() > 64
            || self
                .expires_at
                .chars()
                .any(|character| character.is_control())
            || chrono::DateTime::parse_from_rfc3339(&self.expires_at).is_err()
        {
            return Err(CredentialError::Invalid(
                "Invalid trusted service credential lease metadata".to_string(),
            ));
        }
        Ok(())
    }

    pub fn validate_at(
        &self,
        now: chrono::DateTime<chrono::FixedOffset>,
    ) -> Result<(), CredentialError> {
        self.validate()?;
        let expires_at = chrono::DateTime::parse_from_rfc3339(&self.expires_at).map_err(|_| {
            CredentialError::Invalid(
                "Invalid trusted service credential lease metadata".to_string(),
            )
        })?;
        if expires_at <= now {
            return Err(CredentialError::Invalid(
                "Trusted service credential lease is expired".to_string(),
            ));
        }
        Ok(())
    }
}

pub trait TrustedServiceCredentialBroker {
    fn issue(&mut self, scope: &CredentialScope) -> Result<CredentialLease, CredentialError>;
    fn revoke(&mut self, lease: &CredentialLease) -> Result<(), CredentialError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    Invalid(String),
    Blocked(String),
    NotFound,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "{message}"),
            Self::Blocked(message) => write!(formatter, "{message}"),
            Self::NotFound => write!(formatter, "Credential lease not found"),
        }
    }
}

/// Default broker. It cannot issue, store, or revoke a service credential.
#[derive(Debug, Default, Clone, Copy)]
pub struct DisabledCredentialBroker;

impl TrustedServiceCredentialBroker for DisabledCredentialBroker {
    fn issue(&mut self, _scope: &CredentialScope) -> Result<CredentialLease, CredentialError> {
        Err(CredentialError::Blocked(
            "Trusted service credential issuance is not enabled".to_string(),
        ))
    }

    fn revoke(&mut self, _lease: &CredentialLease) -> Result<(), CredentialError> {
        Err(CredentialError::Blocked(
            "Trusted service credential revocation is not enabled".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> CredentialScope {
        CredentialScope {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-1".to_string(),
            capabilities: vec!["storage".to_string()],
        }
    }

    #[test]
    fn disabled_broker_fails_closed_without_issuing_a_credential() {
        let mut broker = DisabledCredentialBroker;
        let error = broker
            .issue(&scope())
            .expect_err("credential issuance must remain disabled");
        assert!(matches!(error, CredentialError::Blocked(_)));
    }

    #[test]
    fn credential_scope_rejects_unknown_and_duplicate_capabilities() {
        let mut invalid = scope();
        invalid.capabilities = vec!["unknown".to_string()];
        assert!(matches!(
            invalid.validate(),
            Err(CredentialError::Invalid(_))
        ));

        invalid.capabilities = vec!["storage".to_string(), "storage".to_string()];
        assert!(matches!(
            invalid.validate(),
            Err(CredentialError::Invalid(_))
        ));

        invalid = scope();
        invalid.addon_id = "../outside".to_string();
        assert!(matches!(
            invalid.validate(),
            Err(CredentialError::Invalid(_))
        ));

        invalid = scope();
        invalid.activation_instance = "../activation".to_string();
        assert!(matches!(
            invalid.validate(),
            Err(CredentialError::Invalid(_))
        ));
    }

    #[test]
    fn credential_scope_rejects_control_characters() {
        let mut invalid = scope();
        invalid.activation_instance.push('\n');
        assert!(matches!(
            invalid.validate(),
            Err(CredentialError::Invalid(_))
        ));
    }

    #[test]
    fn credential_lease_metadata_is_bounded_and_non_secret_shaped() {
        let valid = CredentialLease {
            lease_id: "lease-1".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
        };
        valid.validate().expect("valid lease metadata should pass");

        let invalid_id = CredentialLease {
            lease_id: "lease/1".to_string(),
            ..valid.clone()
        };
        assert!(matches!(
            invalid_id.validate(),
            Err(CredentialError::Invalid(_))
        ));
        let invalid_expiry = CredentialLease {
            expires_at: "2030-01-01\n00:00:00Z".to_string(),
            ..valid
        };
        assert!(matches!(
            invalid_expiry.validate(),
            Err(CredentialError::Invalid(_))
        ));
        let invalid_timestamp = CredentialLease {
            lease_id: "lease-2".to_string(),
            expires_at: "not-a-timestamp".to_string(),
        };
        assert!(matches!(
            invalid_timestamp.validate(),
            Err(CredentialError::Invalid(_))
        ));
    }

    #[test]
    fn credential_lease_expiry_must_be_in_the_future() {
        let lease = CredentialLease {
            lease_id: "lease-1".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
        };
        lease
            .validate_at(chrono::DateTime::parse_from_rfc3339("2029-12-31T23:59:59Z").unwrap())
            .expect("future lease should pass");
        assert!(matches!(
            lease
                .validate_at(chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z").unwrap()),
            Err(CredentialError::Invalid(_))
        ));
    }

    #[test]
    fn disabled_broker_never_accepts_a_revoke_as_activation() {
        let mut broker = DisabledCredentialBroker;
        let lease = CredentialLease {
            lease_id: "lease-1".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
        };
        assert!(matches!(
            broker.revoke(&lease),
            Err(CredentialError::Blocked(_))
        ));
    }
}
