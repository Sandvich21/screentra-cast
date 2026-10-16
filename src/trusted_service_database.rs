//! Contract-only database boundary for trusted server services.
//!
//! This module deliberately contains policy and validation, not a database
//! adapter. No trusted service can use this contract to obtain a SQLite file,
//! a PostgreSQL URL, a raw SQL channel, or a core-table handle. A future
//! adapter must implement the contract behind an authenticated gateway and
//! remain disabled until the supervisor gate is enabled.

use crate::addons;
use crate::db::Database;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

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
                if self.storage_namespace != self.addon_id {
                    return Err(DatabaseContractError::Invalid(
                        "Core storage API namespace must match addon identity".to_string(),
                    ));
                }
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
                validate_prefix(prefix)?;
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

fn validate_prefix(prefix: &str) -> Result<(), DatabaseContractError> {
    if prefix.len() > MAX_VALUE_BYTES || prefix.contains('/') || prefix.contains('\\') {
        return Err(DatabaseContractError::Invalid(
            "Invalid trusted service database prefix".to_string(),
        ));
    }
    if prefix.chars().any(|character| character.is_control()) {
        return Err(DatabaseContractError::Invalid(
            "Invalid trusted service database prefix".to_string(),
        ));
    }
    Ok(())
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseResponse {
    Value(Option<String>),
    Written,
    Deleted(bool),
    Entries(Vec<(String, String)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseGatewayError {
    Contract(DatabaseContractError),
    Storage(String),
}

impl fmt::Display for DatabaseGatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => write!(formatter, "{error}"),
            Self::Storage(_) => write!(formatter, "Trusted service database operation failed"),
        }
    }
}

/// SQLite implementation of the controlled boundary. It exposes only the
/// existing core-owned addon_storage methods; it never exposes a connection,
/// filesystem path, SQL string, or database file to the caller.
#[derive(Clone)]
pub(crate) struct TrustedServiceSqliteGateway {
    db: Arc<Database>,
    contract: TrustedServiceDatabaseContract,
}

impl TrustedServiceSqliteGateway {
    pub(crate) fn new(
        db: Arc<Database>,
        contract: TrustedServiceDatabaseContract,
    ) -> Result<Self, DatabaseGatewayError> {
        contract
            .validate()
            .map_err(DatabaseGatewayError::Contract)?;
        if contract.boundary != DatabaseBoundaryKind::CoreStorageApi {
            return Err(DatabaseGatewayError::Contract(
                DatabaseContractError::Unsupported(
                    "SQLite gateway requires the core storage API boundary".to_string(),
                ),
            ));
        }
        Ok(Self { db, contract })
    }

    pub(crate) async fn handle(
        &self,
        request: DatabaseRequest,
    ) -> Result<DatabaseResponse, DatabaseGatewayError> {
        request
            .validate_against(&self.contract)
            .map_err(DatabaseGatewayError::Contract)?;
        let addon_id = &self.contract.storage_namespace;
        match request.operation {
            DatabaseOperation::Get { key } => self
                .db
                .get_addon_storage(addon_id, &key)
                .map(DatabaseResponse::Value)
                .map_err(DatabaseGatewayError::Storage),
            DatabaseOperation::Put { key, value_json } => self
                .db
                .set_addon_storage(addon_id, &key, &value_json, &Utc::now().to_rfc3339())
                .map(|()| DatabaseResponse::Written)
                .map_err(DatabaseGatewayError::Storage),
            DatabaseOperation::Delete { key } => self
                .db
                .delete_addon_storage(addon_id, &key)
                .map(DatabaseResponse::Deleted)
                .map_err(DatabaseGatewayError::Storage),
            DatabaseOperation::List { prefix, limit } => self
                .db
                .list_addon_storage(addon_id, &prefix, limit)
                .map(DatabaseResponse::Entries)
                .map_err(DatabaseGatewayError::Storage),
        }
    }
}

/// PostgreSQL implementation of the same controlled core storage API. The
/// per-addon schema/role mode is intentionally rejected until its grants and
/// disposable permission probe exist.
#[derive(Clone)]
pub(crate) struct TrustedServicePostgresGateway {
    store: Arc<crate::postgres::PostgresStore>,
    contract: TrustedServiceDatabaseContract,
}

impl TrustedServicePostgresGateway {
    pub(crate) fn new(
        store: Arc<crate::postgres::PostgresStore>,
        contract: TrustedServiceDatabaseContract,
    ) -> Result<Self, DatabaseGatewayError> {
        contract
            .validate()
            .map_err(DatabaseGatewayError::Contract)?;
        if contract.boundary != DatabaseBoundaryKind::CoreStorageApi {
            return Err(DatabaseGatewayError::Contract(
                DatabaseContractError::Unsupported(
                    "PostgreSQL schema gateway is not enabled".to_string(),
                ),
            ));
        }
        Ok(Self { store, contract })
    }

    pub(crate) async fn handle(
        &self,
        request: DatabaseRequest,
    ) -> Result<DatabaseResponse, DatabaseGatewayError> {
        request
            .validate_against(&self.contract)
            .map_err(DatabaseGatewayError::Contract)?;
        let addon_id = &self.contract.storage_namespace;
        match request.operation {
            DatabaseOperation::Get { key } => self
                .store
                .get_addon_storage(addon_id, &key)
                .await
                .map(DatabaseResponse::Value)
                .map_err(DatabaseGatewayError::Storage),
            DatabaseOperation::Put { key, value_json } => self
                .store
                .set_addon_storage(addon_id, &key, &value_json, &Utc::now().to_rfc3339())
                .await
                .map(|()| DatabaseResponse::Written)
                .map_err(DatabaseGatewayError::Storage),
            DatabaseOperation::Delete { key } => self
                .store
                .delete_addon_storage(addon_id, &key)
                .await
                .map(DatabaseResponse::Deleted)
                .map_err(DatabaseGatewayError::Storage),
            DatabaseOperation::List { prefix, limit } => self
                .store
                .list_addon_storage(addon_id, &prefix, limit)
                .await
                .map(DatabaseResponse::Entries)
                .map_err(DatabaseGatewayError::Storage),
        }
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

    #[tokio::test]
    async fn sqlite_gateway_round_trip_is_namespaced_and_identity_bound() {
        let path = std::env::temp_dir().join(format!(
            "screentra-service-db-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let db =
            Arc::new(Database::open(path.to_str().expect("database path")).expect("open database"));
        db.set_addon_storage(
            "other.service",
            "private",
            r#"{"owner":"other"}"#,
            "2026-10-15T00:00:00Z",
        )
        .expect("seed other namespace");
        let gateway =
            TrustedServiceSqliteGateway::new(db.clone(), sqlite_contract()).expect("gateway");

        let put = DatabaseRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-1".to_string(),
            operation: DatabaseOperation::Put {
                key: "state".to_string(),
                value_json: r#"{"owner":"example"}"#.to_string(),
            },
        };
        assert_eq!(
            gateway.handle(put).await.unwrap(),
            DatabaseResponse::Written
        );
        for (key, value) in [
            ("state_extra", r#"{"owner":"example"}"#),
            ("stateXextra", r#"{"owner":"example"}"#),
        ] {
            assert_eq!(
                gateway
                    .handle(DatabaseRequest {
                        addon_id: "example.service".to_string(),
                        activation_instance: "activation-1".to_string(),
                        operation: DatabaseOperation::Put {
                            key: key.to_string(),
                            value_json: value.to_string(),
                        },
                    })
                    .await
                    .unwrap(),
                DatabaseResponse::Written
            );
        }
        assert_eq!(
            gateway
                .handle(DatabaseRequest {
                    addon_id: "example.service".to_string(),
                    activation_instance: "activation-1".to_string(),
                    operation: DatabaseOperation::Get {
                        key: "state".to_string(),
                    },
                })
                .await
                .unwrap(),
            DatabaseResponse::Value(Some(r#"{"owner":"example"}"#.to_string()))
        );
        assert_eq!(
            gateway
                .handle(DatabaseRequest {
                    addon_id: "example.service".to_string(),
                    activation_instance: "activation-1".to_string(),
                    operation: DatabaseOperation::List {
                        prefix: "".to_string(),
                        limit: 10,
                    },
                })
                .await
                .unwrap(),
            DatabaseResponse::Entries(vec![
                ("state".to_string(), r#"{"owner":"example"}"#.to_string(),),
                (
                    "stateXextra".to_string(),
                    r#"{"owner":"example"}"#.to_string(),
                ),
                (
                    "state_extra".to_string(),
                    r#"{"owner":"example"}"#.to_string(),
                ),
            ])
        );
        assert_eq!(
            gateway
                .handle(DatabaseRequest {
                    addon_id: "example.service".to_string(),
                    activation_instance: "activation-1".to_string(),
                    operation: DatabaseOperation::List {
                        prefix: "state_".to_string(),
                        limit: 10,
                    },
                })
                .await
                .unwrap(),
            DatabaseResponse::Entries(vec![(
                "state_extra".to_string(),
                r#"{"owner":"example"}"#.to_string(),
            ),])
        );
        let wrong_identity = gateway
            .handle(DatabaseRequest {
                addon_id: "other.service".to_string(),
                activation_instance: "activation-1".to_string(),
                operation: DatabaseOperation::Get {
                    key: "private".to_string(),
                },
            })
            .await
            .expect_err("cross-addon request must be rejected");
        assert!(matches!(
            wrong_identity,
            DatabaseGatewayError::Contract(DatabaseContractError::IdentityMismatch)
        ));
        assert_eq!(
            db.get_addon_storage("other.service", "private").unwrap(),
            Some(r#"{"owner":"other"}"#.to_string())
        );
        assert_eq!(
            gateway
                .handle(DatabaseRequest {
                    addon_id: "example.service".to_string(),
                    activation_instance: "activation-1".to_string(),
                    operation: DatabaseOperation::Delete {
                        key: "state".to_string(),
                    },
                })
                .await
                .unwrap(),
            DatabaseResponse::Deleted(true)
        );
        assert!(
            gateway
                .handle(DatabaseRequest {
                    addon_id: "example.service".to_string(),
                    activation_instance: "activation-1".to_string(),
                    operation: DatabaseOperation::Get {
                        key: "state".to_string(),
                    },
                })
                .await
                .unwrap()
                == DatabaseResponse::Value(None)
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    #[ignore = "requires a disposable PostgreSQL database supplied through SIGNAGE_DATABASE_URL"]
    async fn postgres_gateway_round_trip_is_namespaced_and_identity_bound() {
        crate::postgres::apply_initial_schema()
            .await
            .expect("apply PostgreSQL migrations");
        let store = Arc::new(
            crate::postgres::PostgresStore::connect()
                .await
                .expect("connect to PostgreSQL"),
        );
        let addon_id = format!("test.gateway.{}", uuid::Uuid::new_v4());
        let other_addon_id = format!("test.gateway.other.{}", uuid::Uuid::new_v4());
        let mut contract =
            TrustedServiceDatabaseContract::core_storage_api(&addon_id, "activation-1");
        let gateway = TrustedServicePostgresGateway::new(store.clone(), contract.clone())
            .expect("PostgreSQL gateway");

        assert_eq!(
            gateway
                .handle(DatabaseRequest {
                    addon_id: addon_id.clone(),
                    activation_instance: "activation-1".to_string(),
                    operation: DatabaseOperation::Put {
                        key: "state".to_string(),
                        value_json: r#"{"owner":"gateway"}"#.to_string(),
                    },
                })
                .await
                .expect("put through PostgreSQL gateway"),
            DatabaseResponse::Written
        );
        assert_eq!(
            gateway
                .handle(DatabaseRequest {
                    addon_id: addon_id.clone(),
                    activation_instance: "activation-1".to_string(),
                    operation: DatabaseOperation::Get {
                        key: "state".to_string(),
                    },
                })
                .await
                .expect("get through PostgreSQL gateway"),
            DatabaseResponse::Value(Some(r#"{"owner":"gateway"}"#.to_string()))
        );
        assert_eq!(
            gateway
                .handle(DatabaseRequest {
                    addon_id: addon_id.clone(),
                    activation_instance: "activation-2".to_string(),
                    operation: DatabaseOperation::Get {
                        key: "state".to_string(),
                    },
                })
                .await
                .expect_err("replaced PostgreSQL activation must be rejected"),
            DatabaseGatewayError::Contract(DatabaseContractError::IdentityMismatch)
        );
        store
            .set_addon_storage(
                &other_addon_id,
                "private",
                r#"{"owner":"other"}"#,
                "2026-10-15T00:00:00Z",
            )
            .await
            .expect("seed other namespace");
        let error = gateway
            .handle(DatabaseRequest {
                addon_id: other_addon_id.clone(),
                activation_instance: "activation-1".to_string(),
                operation: DatabaseOperation::Get {
                    key: "private".to_string(),
                },
            })
            .await
            .expect_err("cross-addon PostgreSQL request must be rejected");
        assert!(matches!(
            error,
            DatabaseGatewayError::Contract(DatabaseContractError::IdentityMismatch)
        ));
        assert_eq!(
            store
                .get_addon_storage(&other_addon_id, "private")
                .await
                .expect("read other namespace"),
            Some(r#"{"owner":"other"}"#.to_string())
        );
        store
            .delete_addon_storage_namespace(&addon_id)
            .await
            .expect("clean gateway namespace");
        store
            .delete_addon_storage_namespace(&other_addon_id)
            .await
            .expect("clean other namespace");
        contract.activation_instance = "replaced-activation".to_string();
        assert!(TrustedServicePostgresGateway::new(store, contract).is_ok());
    }
}
