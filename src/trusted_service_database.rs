//! Contract-only database boundary for trusted server services.
//!
//! This module deliberately contains policy and validation, not a database
//! adapter. No trusted service can use this contract to obtain a SQLite file,
//! a PostgreSQL URL, a raw SQL channel, or a core-table handle. A future
//! adapter must implement the contract behind an authenticated gateway and
//! remain disabled until the supervisor gate is enabled.

use crate::addons;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_ROWS_PER_REQUEST: u32 = 1_000;
pub const MAX_KEYS_PER_REQUEST: u32 = 1_000;

const MAX_IDENTITY_LEN: usize = 128;
const MAX_SQL_IDENTIFIER_LEN: usize = 63;
const RESERVED_SCHEMA_NAMES: &[&str] = &[
    "public",
    "pg_catalog",
    "information_schema",
    "pg_toast",
    "signage",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseBoundaryKind {
    /// Core-owned namespaced storage exposed through a controlled API.
    CoreStorageApi,
    /// Future PostgreSQL-only isolated schema and role, still behind a
    /// controlled API and never exposed as a connection string.
    PostgresAddonSchema,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseBackupScope {
    /// Backup/restore preserves only this service namespace and never starts
    /// the service or runs service migrations during restore.
    NamespaceOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedServiceDatabaseContract {
    pub addon_id: String,
    pub activation_instance: String,
    pub boundary: DatabaseBoundaryKind,
    pub storage_namespace: String,
    /// Present only for the future PostgreSQL schema boundary. This is a
    /// schema identifier, never a complete database URL or SQL fragment.
    pub postgres_schema: Option<String>,
    /// Present only for the future PostgreSQL schema boundary. The adapter
    /// must provision this role with least privilege and no core-table grants.
    pub postgres_role: Option<String>,
    pub max_request_bytes: usize,
    pub max_value_bytes: usize,
    pub max_rows_per_request: u32,
    pub max_keys_per_request: u32,
    pub allow_raw_sql: bool,
    pub allow_core_tables: bool,
    pub allow_migrations: bool,
    pub allow_sqlite_file_mount: bool,
    pub audit_required: bool,
    pub backup_scope: DatabaseBackupScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseContractError {
    Invalid(String),
    IdentityMismatch,
    Unsupported(String),
}

impl fmt::Display for DatabaseContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "{message}"),
            Self::IdentityMismatch => write!(
                formatter,
                "Database request identity does not match service"
            ),
            Self::Unsupported(message) => write!(formatter, "{message}"),
        }
    }
}

fn validate_identity(value: &str, label: &str) -> Result<(), DatabaseContractError> {
    if value.is_empty()
        || value.len() > MAX_IDENTITY_LEN
        || value.chars().any(|character| character.is_control())
    {
        return Err(DatabaseContractError::Invalid(format!(
            "Invalid trusted service {label}"
        )));
    }
    Ok(())
}

fn validate_sql_identifier(value: &str, label: &str) -> Result<(), DatabaseContractError> {
    if value.is_empty()
        || value.len() > MAX_SQL_IDENTIFIER_LEN
        || !value.chars().enumerate().all(|(index, character)| {
            if index == 0 {
                character.is_ascii_lowercase() || character == '_'
            } else {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
            }
        })
    {
        return Err(DatabaseContractError::Invalid(format!(
            "Invalid PostgreSQL {label}"
        )));
    }
    Ok(())
}

impl TrustedServiceDatabaseContract {
    pub fn core_storage_api(addon_id: &str, activation_instance: &str) -> Self {
        Self {
            addon_id: addon_id.to_string(),
            activation_instance: activation_instance.to_string(),
            boundary: DatabaseBoundaryKind::CoreStorageApi,
            storage_namespace: addon_id.to_string(),
            postgres_schema: None,
            postgres_role: None,
            max_request_bytes: MAX_REQUEST_BYTES,
            max_value_bytes: MAX_VALUE_BYTES,
            max_rows_per_request: MAX_ROWS_PER_REQUEST,
            max_keys_per_request: MAX_KEYS_PER_REQUEST,
            allow_raw_sql: false,
            allow_core_tables: false,
            allow_migrations: false,
            allow_sqlite_file_mount: false,
            audit_required: true,
            backup_scope: DatabaseBackupScope::NamespaceOnly,
        }
    }

    pub fn validate(&self) -> Result<(), DatabaseContractError> {
        addons::validate_addon_storage_namespace(&self.addon_id)
            .map_err(DatabaseContractError::Invalid)?;
        validate_identity(&self.activation_instance, "activation instance")?;
        addons::validate_addon_storage_namespace(&self.storage_namespace)
            .map_err(DatabaseContractError::Invalid)?;
        if self.max_request_bytes == 0 || self.max_request_bytes > MAX_REQUEST_BYTES {
            return Err(DatabaseContractError::Invalid(
                "Trusted service database request limit is invalid".to_string(),
            ));
        }
        if self.max_value_bytes == 0 || self.max_value_bytes > MAX_VALUE_BYTES {
            return Err(DatabaseContractError::Invalid(
                "Trusted service database value limit is invalid".to_string(),
            ));
        }
        if self.max_rows_per_request == 0 || self.max_rows_per_request > MAX_ROWS_PER_REQUEST {
            return Err(DatabaseContractError::Invalid(
                "Trusted service database row limit is invalid".to_string(),
            ));
        }
        if self.max_keys_per_request == 0 || self.max_keys_per_request > MAX_KEYS_PER_REQUEST {
            return Err(DatabaseContractError::Invalid(
                "Trusted service database key limit is invalid".to_string(),
            ));
        }
        if self.allow_raw_sql
            || self.allow_core_tables
            || self.allow_migrations
            || self.allow_sqlite_file_mount
            || !self.audit_required
            || self.backup_scope != DatabaseBackupScope::NamespaceOnly
        {
            return Err(DatabaseContractError::Unsupported(
                "Trusted service database contract grants a forbidden operation".to_string(),
            ));
        }

        match self.boundary {
            DatabaseBoundaryKind::CoreStorageApi => {
                if self.postgres_schema.is_some() || self.postgres_role.is_some() {
                    return Err(DatabaseContractError::Invalid(
                        "Core storage API cannot carry PostgreSQL identity".to_string(),
                    ));
                }
            }
            DatabaseBoundaryKind::PostgresAddonSchema => {
                let schema = self.postgres_schema.as_deref().ok_or_else(|| {
                    DatabaseContractError::Invalid(
                        "PostgreSQL service schema is required".to_string(),
                    )
                })?;
                let role = self.postgres_role.as_deref().ok_or_else(|| {
                    DatabaseContractError::Invalid(
                        "PostgreSQL service role is required".to_string(),
                    )
                })?;
                validate_sql_identifier(schema, "service schema")?;
                validate_sql_identifier(role, "service role")?;
                if RESERVED_SCHEMA_NAMES.contains(&schema) {
                    return Err(DatabaseContractError::Unsupported(
                        "Trusted service cannot use a reserved or core schema".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn binds_identity(&self, addon_id: &str, activation_instance: &str) -> bool {
        self.addon_id == addon_id && self.activation_instance == activation_instance
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseOperation {
    Get { key: String },
    Put { key: String, value_json: String },
    Delete { key: String },
    List { prefix: String, limit: u32 },
}

impl DatabaseOperation {
    pub fn validate(
        &self,
        contract: &TrustedServiceDatabaseContract,
    ) -> Result<(), DatabaseContractError> {
        contract.validate()?;
        match self {
            Self::Get { key } | Self::Delete { key } => validate_key(key),
            Self::Put { key, value_json } => {
                validate_key(key)?;
                if value_json.len() > contract.max_value_bytes {
                    return Err(DatabaseContractError::Invalid(
                        "Trusted service database value is too large".to_string(),
                    ));
                }
                serde_json::from_str::<serde_json::Value>(value_json).map_err(|_| {
                    DatabaseContractError::Invalid(
                        "Trusted service database value must be valid JSON".to_string(),
                    )
                })?;
                Ok(())
            }
            Self::List { prefix, limit } => {
                validate_key(prefix)?;
                if *limit == 0 || *limit > contract.max_rows_per_request {
                    return Err(DatabaseContractError::Invalid(
                        "Trusted service database list limit is invalid".to_string(),
                    ));
                }
                Ok(())
            }
        }
    }
}

fn validate_key(key: &str) -> Result<(), DatabaseContractError> {
    if key.is_empty() || key.len() > MAX_VALUE_BYTES || key.contains('/') || key.contains('\\') {
        return Err(DatabaseContractError::Invalid(
            "Invalid trusted service database key".to_string(),
        ));
    }
    if key.chars().any(|character| character.is_control()) {
        return Err(DatabaseContractError::Invalid(
            "Invalid trusted service database key".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub operation: DatabaseOperation,
}

impl DatabaseRequest {
    pub fn validate_against(
        &self,
        contract: &TrustedServiceDatabaseContract,
    ) -> Result<(), DatabaseContractError> {
        contract.validate()?;
        if !contract.binds_identity(&self.addon_id, &self.activation_instance) {
            return Err(DatabaseContractError::IdentityMismatch);
        }
        self.operation.validate(contract)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_contract() -> TrustedServiceDatabaseContract {
        TrustedServiceDatabaseContract::core_storage_api("example.service", "activation-1")
    }

    fn postgres_contract() -> TrustedServiceDatabaseContract {
        let mut contract = sqlite_contract();
        contract.boundary = DatabaseBoundaryKind::PostgresAddonSchema;
        contract.postgres_schema = Some("addon_example_service".to_string());
        contract.postgres_role = Some("addon_example_service".to_string());
        contract
    }

    #[test]
    fn core_storage_contract_is_file_and_sql_free() {
        let contract = sqlite_contract();
        assert!(contract.validate().is_ok());
        assert!(!contract.allow_sqlite_file_mount);
        assert!(!contract.allow_raw_sql);
        assert!(!contract.allow_core_tables);
        assert!(!contract.allow_migrations);
        assert!(contract.postgres_schema.is_none());
        assert!(contract.postgres_role.is_none());
    }

    #[test]
    fn postgres_contract_requires_isolated_schema_and_role() {
        assert!(postgres_contract().validate().is_ok());

        let mut missing_schema = postgres_contract();
        missing_schema.postgres_schema = None;
        assert!(missing_schema.validate().is_err());

        let mut public_schema = postgres_contract();
        public_schema.postgres_schema = Some("public".to_string());
        assert!(public_schema.validate().is_err());
    }

    #[test]
    fn forbidden_database_permissions_fail_closed() {
        let mut contract = sqlite_contract();
        contract.allow_raw_sql = true;
        assert!(matches!(
            contract.validate(),
            Err(DatabaseContractError::Unsupported(_))
        ));
    }

    #[test]
    fn database_requests_bind_addon_and_activation_identity() {
        let contract = sqlite_contract();
        let request = DatabaseRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-1".to_string(),
            operation: DatabaseOperation::Put {
                key: "state".to_string(),
                value_json: r#"{"enabled":true}"#.to_string(),
            },
        };
        assert!(request.validate_against(&contract).is_ok());

        let mut wrong_instance = request.clone();
        wrong_instance.activation_instance = "other-activation".to_string();
        assert!(matches!(
            wrong_instance.validate_against(&contract),
            Err(DatabaseContractError::IdentityMismatch)
        ));
    }

    #[test]
    fn database_operations_reject_raw_sql_shapes_and_unbounded_lists() {
        let contract = sqlite_contract();
        let invalid_key = DatabaseRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-1".to_string(),
            operation: DatabaseOperation::Get {
                key: "../core".to_string(),
            },
        };
        assert!(invalid_key.validate_against(&contract).is_err());

        let invalid_limit = DatabaseRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-1".to_string(),
            operation: DatabaseOperation::List {
                prefix: "".to_string(),
                limit: MAX_ROWS_PER_REQUEST + 1,
            },
        };
        assert!(invalid_limit.validate_against(&contract).is_err());
    }
}
