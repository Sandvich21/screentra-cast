use crate::backup_api;
use crate::addons::{AddonError, AddonManifest, AddonRecord, AddonWidgetRegistration, ClientAddonStatusReport};
use crate::db::{ClientDisplayRegistration, ClientRegistration, Database};
use crate::display::detect_displays;
use crate::models::{
    AddLayoutSetEntry, CreateLayoutSet, CreateLayoutSetItem, DisplayAssignment, DisplayInfo, DisplayMode, DisplayState,
    Layout, LayoutFolder, LayoutSetEntry, LayoutSetSummary, LayoutSetWithItems, SystemStatus, TemplateMeta,
    UpdateLayoutSet, UpdateLayoutSetEntry, WidgetPreset, WidgetPresetAsset, WidgetPresetMeta,
};
use crate::schedule::ScheduleEntry;
use crate::system::get_system_status;
use crate::traffic::{TrafficProvider, TrafficQuery};
use axum::{
    extract::{DefaultBodyLimit, Extension, FromRef, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json as AxumJson, Response, Html},
    routing::{get, post, patch, delete, put},
    Json, Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path as StdPath, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use uuid::Uuid;



pub type AppState = Arc<Database>;

fn internal_server_error() -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string())
}

#[derive(Clone)]
pub(crate) enum ClientRepository {
    Sqlite(Arc<Database>),
    Postgres(Arc<crate::postgres::PostgresStore>),
}

#[derive(Clone)]
pub(crate) enum AddonStorageRepository {
    Sqlite(Arc<Database>),
    Postgres(Arc<crate::postgres::PostgresStore>),
}

impl AddonStorageRepository {
    async fn set(
        &self,
        addon_id: &str,
        storage_key: &str,
        value_json: &str,
        updated_at: &str,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.set_addon_storage(addon_id, storage_key, value_json, updated_at),
            Self::Postgres(store) => store
                .set_addon_storage(addon_id, storage_key, value_json, updated_at)
                .await,
        }
    }

    async fn get(&self, addon_id: &str, storage_key: &str) -> Result<Option<String>, String> {
        match self {
            Self::Sqlite(db) => db.get_addon_storage(addon_id, storage_key),
            Self::Postgres(store) => store.get_addon_storage(addon_id, storage_key).await,
        }
    }

    async fn delete(&self, addon_id: &str, storage_key: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.delete_addon_storage(addon_id, storage_key),
            Self::Postgres(store) => store.delete_addon_storage(addon_id, storage_key).await,
        }
    }

    async fn delete_namespace(&self, addon_id: &str) -> Result<u64, String> {
        match self {
            Self::Sqlite(db) => db
                .delete_addon_storage_namespace(addon_id)
                .map(|count| count as u64),
            Self::Postgres(store) => store.delete_addon_storage_namespace(addon_id).await,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RepositoryBackend {
    Postgres,
    Sqlite,
}

pub(crate) fn repository_backend_from_value(
    value: Option<&str>,
) -> Result<RepositoryBackend, String> {
    match value {
        Some("postgres") => Ok(RepositoryBackend::Postgres),
        Some("sqlite") => Ok(RepositoryBackend::Sqlite),
        Some(value) => Err(format!(
            "SIGNAGE_REPOSITORY_BACKEND must be exactly 'postgres' or 'sqlite'; got '{value}'"
        )),
        None => Err(
            "SIGNAGE_REPOSITORY_BACKEND must be set explicitly; use 'postgres' for production or 'sqlite' for development/test only"
                .to_string(),
        ),
    }
}

pub(crate) fn configured_repository_backend() -> Result<RepositoryBackend, String> {
    let value = std::env::var("SIGNAGE_REPOSITORY_BACKEND").ok();
    repository_backend_from_value(value.as_deref())
}

#[derive(Clone)]
pub struct RuntimeState {
    legacy_db: Option<AppState>,
    client_repository: ClientRepository,
    addon_storage_repository: AddonStorageRepository,
    auth_repository: crate::auth::AuthRepository,
    backup_repository: backup_api::BackupMetadataRepository,
    pub(crate) metrics: crate::observability::Metrics,
}

impl RuntimeState {
    pub async fn from_env(db: Option<AppState>) -> Result<Self, String> {
        match configured_repository_backend()? {
            RepositoryBackend::Postgres => {
                crate::postgres::apply_initial_schema().await?;
                let store = Arc::new(crate::postgres::PostgresStore::connect().await?);
                Ok(Self {
                    legacy_db: None,
                    client_repository: ClientRepository::Postgres(store.clone()),
                    addon_storage_repository: AddonStorageRepository::Postgres(store.clone()),
                    auth_repository: crate::auth::AuthRepository::Postgres(store.clone()),
                    backup_repository: backup_api::BackupMetadataRepository::Postgres(store),
                    metrics: crate::observability::Metrics::new(),
                })
            }
            RepositoryBackend::Sqlite => {
                let db = db.ok_or_else(|| "SQLite state is required when PostgreSQL is disabled".to_string())?;
                Ok(Self::sqlite(db))
            }
        }
    }

    pub(crate) fn client_repository(&self) -> ClientRepository {
        self.client_repository.clone()
    }

    pub(crate) fn auth_repository(&self) -> crate::auth::AuthRepository {
        self.auth_repository.clone()
    }

    pub(crate) fn trusted_service_database_handler(
        &self,
        addon_id: &str,
        activation_instance: &str,
    ) -> Result<Arc<dyn crate::trusted_service_gateway::GatewayOperationHandler>, String> {
        let contract = crate::trusted_service_database::TrustedServiceDatabaseContract::core_storage_api(
            addon_id,
            activation_instance,
        );
        match &self.addon_storage_repository {
            AddonStorageRepository::Sqlite(db) => {
                let gateway = crate::trusted_service_database::TrustedServiceSqliteGateway::new(
                    db.clone(),
                    contract,
                )
                .map_err(|error| error.to_string())?;
                let audit = crate::trusted_service_database::SqliteDatabaseAuditSink::new(db.clone());
                Ok(Arc::new(
                    crate::trusted_service_database::SqliteDatabaseOperationHandler::new(
                        gateway, audit,
                    ),
                ))
            }
            AddonStorageRepository::Postgres(store) => {
                let gateway = crate::trusted_service_database::TrustedServicePostgresGateway::new(
                    store.clone(),
                    contract,
                )
                .map_err(|error| error.to_string())?;
                let audit = crate::trusted_service_database::PostgresDatabaseAuditSink::new(store.clone());
                Ok(Arc::new(
                    crate::trusted_service_database::PostgresDatabaseOperationHandler::new(
                        gateway, audit,
                    ),
                ))
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn trusted_service_gateway(
        &self,
        addon_id: &str,
        activation_instance: &str,
        peer_policy: crate::trusted_service_gateway::GatewayPeerPolicy,
    ) -> Result<(
        crate::trusted_service_gateway::TrustedServiceUnixGateway,
        Arc<dyn crate::trusted_service_gateway::GatewayOperationHandler>,
    ), String> {
        let handler = self.trusted_service_database_handler(addon_id, activation_instance)?;
        let contract = crate::trusted_service_database::TrustedServiceDatabaseContract::core_storage_api(
            addon_id,
            activation_instance,
        );
        let binding = crate::trusted_service_gateway::GatewayActivationBinding::new(contract);
        let gateway = crate::trusted_service_gateway::TrustedServiceUnixGateway::new(
            binding,
            peer_policy,
        )?;
        Ok((gateway, handler))
    }

    #[cfg(unix)]
    pub(crate) fn trusted_service_gateway_with_credential_lease(
        &self,
        addon_id: &str,
        activation_instance: &str,
        ledger: Arc<Mutex<crate::trusted_service_credentials::CredentialLeaseLedger>>,
        lease_id: String,
        peer_policy: crate::trusted_service_gateway::GatewayPeerPolicy,
    ) -> Result<
        (
            crate::trusted_service_gateway::TrustedServiceUnixGateway,
            Arc<dyn crate::trusted_service_gateway::GatewayOperationHandler>,
        ),
        String,
    > {
        let handler = self.trusted_service_database_handler(addon_id, activation_instance)?;
        let contract = crate::trusted_service_database::TrustedServiceDatabaseContract::core_storage_api(
            addon_id,
            activation_instance,
        );
        let binding = crate::trusted_service_gateway::GatewayActivationBinding::new(contract)
            .with_credential_lease(ledger, lease_id)?;
        let gateway = crate::trusted_service_gateway::TrustedServiceUnixGateway::new(
            binding,
            peer_policy,
        )?;
        Ok((gateway, handler))
    }

    pub fn sqlite(db: AppState) -> Self {
        let auth_repository = crate::auth::AuthRepository::Sqlite(db.clone());
        let backup_repository = backup_api::BackupMetadataRepository::Sqlite(db.clone());
        Self {
            legacy_db: Some(db.clone()),
            client_repository: ClientRepository::Sqlite(db.clone()),
            addon_storage_repository: AddonStorageRepository::Sqlite(db),
            auth_repository,
            backup_repository,
            metrics: crate::observability::Metrics::new(),
        }
    }
}

impl FromRef<RuntimeState> for ClientRepository {
    fn from_ref(state: &RuntimeState) -> Self {
        state.client_repository.clone()
    }
}

impl FromRef<RuntimeState> for AddonStorageRepository {
    fn from_ref(state: &RuntimeState) -> Self {
        state.addon_storage_repository.clone()
    }
}

impl FromRef<RuntimeState> for backup_api::BackupMetadataRepository {
    fn from_ref(state: &RuntimeState) -> Self {
        state.backup_repository.clone()
    }
}

type RegisteredClientRow = (
    String, String, String, String, Option<String>, Option<i64>, Option<f32>, Option<i64>, Option<i64>,
    Option<i64>, Option<i64>, Option<String>, Option<String>, String, i64, Option<String>, Option<String>,
    Option<i64>, Option<i64>, Option<String>, Option<String>, i64,
);

impl ClientRepository {
    async fn list_registered_clients(&self) -> Result<Vec<RegisteredClientRow>, String> {
        match self {
            Self::Sqlite(db) => db.list_registered_clients(),
            Self::Postgres(db) => db.list_registered_clients().await,
        }
    }

    async fn register_client(
        &self,
        client: &ClientRegistration,
        displays: &[ClientDisplayRegistration],
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.register_client(client, displays),
            Self::Postgres(db) => db.register_client(client, displays).await,
        }
    }

    async fn get_client_auth_token_hash(&self, client_id: &str) -> Result<Option<String>, String> {
        match self {
            Self::Sqlite(db) => db.get_client_auth_token_hash(client_id),
            Self::Postgres(db) => db.get_client_auth_token_hash(client_id).await,
        }
    }

    pub(crate) async fn get_display_assignment_and_last_seen(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<Option<(Option<String>, String)>, String> {
        match self {
            Self::Sqlite(db) => db.get_display_assignment_and_last_seen(client_id, display_index),
            Self::Postgres(db) => db
                .get_display_assignment_and_last_seen(client_id, display_index)
                .await
                .map(Some),
        }
    }

    async fn get_display_state(&self) -> Result<DisplayState, String> {
        match self {
            Self::Sqlite(db) => db.get_display_state(),
            Self::Postgres(db) => db.get_display_state().await,
        }
    }

    pub(crate) async fn get_active_schedules_full(
        &self,
        current_time: &str,
        day_of_week: i32,
    ) -> Result<Vec<ScheduleEntry>, String> {
        match self {
            Self::Sqlite(db) => db.get_active_schedules_full(current_time, day_of_week),
            Self::Postgres(db) => db.get_active_schedules_full(current_time, day_of_week).await,
        }
    }

    pub(crate) async fn get_schedule_runtime_active(&self, id: &str) -> Result<Option<bool>, String> {
        match self {
            Self::Sqlite(db) => db.get_schedule_runtime_active(id),
            Self::Postgres(db) => db.get_schedule_runtime_active(id).await,
        }
    }

    pub(crate) async fn set_schedule_runtime_active(&self, id: &str, active: bool) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.set_schedule_runtime_active(id, active),
            Self::Postgres(db) => db.set_schedule_runtime_active(id, active).await,
        }
    }

    pub(crate) async fn get_display_assignment(&self, display: &str) -> Result<Option<DisplayAssignment>, String> {
        match self {
            Self::Sqlite(db) => db.get_display_assignment(display),
            Self::Postgres(db) => db.get_display_assignment(display).await,
        }
    }

    pub(crate) async fn set_display_assignment(&self, display: &str, layout_id: Option<&str>) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.set_display_assignment(display, layout_id),
            Self::Postgres(db) => db.set_display_assignment(display, layout_id).await,
        }
    }

    async fn delete_display_assignment(&self, display: &str) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.delete_display_assignment(display),
            Self::Postgres(db) => db.delete_display_assignment(display).await,
        }
    }
    async fn get_display_overrides(&self) -> Result<std::collections::HashMap<String, (u32, u32)>, String> {
        match self {
            Self::Sqlite(db) => db.get_display_overrides(),
            Self::Postgres(db) => db.get_display_overrides().await,
        }
    }

    async fn set_display_override(&self, display: &str, width: u32, height: u32) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.set_display_override(display, width, height),
            Self::Postgres(db) => db.set_display_override(display, width, height).await,
        }
    }

    async fn clear_display_override(&self, display: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.clear_display_override(display),
            Self::Postgres(db) => db.clear_display_override(display).await,
        }
    }

    pub(crate) async fn count_stale_clients(&self, threshold_secs: i64) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.count_stale_clients(threshold_secs),
            Self::Postgres(db) => db.count_stale_clients(threshold_secs).await,
        }
    }
    async fn set_client_enrolled(&self, client_id: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.set_client_enrolled(client_id),
            Self::Postgres(db) => db.set_client_enrolled(client_id).await,
        }
    }

    async fn get_client_virtual_flag(&self, client_id: &str) -> Result<Option<bool>, String> {
        match self {
            Self::Sqlite(db) => db.get_client_virtual_flag(client_id),
            Self::Postgres(db) => db.get_client_virtual_flag(client_id).await,
        }
    }

    async fn get_client_assignment(&self, client_id: &str) -> Result<Option<String>, String> {
        match self {
            Self::Sqlite(db) => db.get_client_assignment(client_id),
            Self::Postgres(db) => db.get_client_assignment(client_id).await,
        }
    }

    async fn set_client_assignment(&self, client_id: &str, layout_id: Option<&str>) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.set_client_assignment(client_id, layout_id).map(|_| true),
            Self::Postgres(db) => db.set_client_assignment(client_id, layout_id).await,
        }
    }

    async fn bulk_set_display_layout(&self, client_id: &str, layout_id: Option<&str>) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.bulk_set_display_layout(client_id, layout_id),
            Self::Postgres(db) => db.bulk_set_display_layout(client_id, layout_id).await,
        }
    }

    async fn bulk_set_client_assignment(
        &self,
        client_ids: &[String],
        layout_id: Option<&str>,
    ) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.bulk_set_client_assignment(client_ids, layout_id),
            Self::Postgres(db) => db.bulk_set_client_assignment(client_ids, layout_id).await,
        }
    }

    async fn get_client_theme(&self, client_id: &str) -> Result<Option<String>, String> {
        match self {
            Self::Sqlite(db) => db.get_client_theme(client_id),
            Self::Postgres(db) => db.get_client_theme(client_id).await,
        }
    }

    async fn set_client_theme(&self, client_id: &str, theme: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.set_client_theme(client_id, theme),
            Self::Postgres(db) => db.set_client_theme(client_id, theme).await,
        }
    }

    async fn list_client_admin_users(
        &self,
        client_id: &str,
    ) -> Result<Option<(bool, Vec<crate::db::ClientAdminUser>)>, String> {
        match self {
            Self::Sqlite(db) => db.list_client_admin_users(client_id),
            Self::Postgres(db) => db.list_client_admin_users(client_id).await,
        }
    }

    async fn create_virtual_client(&self, id: &str, name: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.create_virtual_client(id, name),
            Self::Postgres(db) => db.create_virtual_client(id, name).await,
        }
    }

    async fn get_virtual_client(&self, id: &str) -> Result<Option<(String, bool)>, String> {
        match self {
            Self::Sqlite(db) => db.get_virtual_client(id),
            Self::Postgres(db) => db.get_virtual_client(id).await,
        }
    }

    async fn is_virtual_client(&self, id: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.is_virtual_client(id),
            Self::Postgres(db) => db.is_virtual_client(id).await,
        }
    }

    async fn update_virtual_client_name(&self, id: &str, name: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.update_virtual_client_name(id, name),
            Self::Postgres(db) => db.update_virtual_client_name(id, name).await,
        }
    }

    async fn set_virtual_client_layout(&self, id: &str, layout_id: &str) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.set_virtual_client_layout(id, layout_id),
            Self::Postgres(db) => db.set_virtual_client_layout(id, layout_id).await,
        }
    }

    async fn set_client_display_count(
        &self,
        client_id: &str,
        requested: Option<i64>,
    ) -> Result<Option<(i64, i64)>, String> {
        match self {
            Self::Sqlite(db) => db.set_client_display_count(client_id, requested),
            Self::Postgres(db) => db.set_client_display_count(client_id, requested).await,
        }
    }

    async fn client_exists(&self, client_id: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.client_exists(client_id),
            Self::Postgres(db) => db.client_exists(client_id).await,
        }
    }

    async fn set_client_touch_binding(&self, client_id: &str, binding_json: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.set_client_touch_binding(client_id, binding_json),
            Self::Postgres(db) => db.set_client_touch_binding(client_id, binding_json).await,
        }
    }

    pub(crate) async fn set_display_layout(
        &self,
        client_id: &str,
        display_index: i64,
        layout_id: Option<&str>,
    ) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.set_display_layout(client_id, display_index, layout_id),
            Self::Postgres(db) => db.set_display_layout(client_id, display_index, layout_id).await,
        }
    }

    async fn set_display_orientation(
        &self,
        client_id: &str,
        display_index: i64,
        orientation: &str,
    ) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.set_display_orientation(client_id, display_index, orientation),
            Self::Postgres(db) => db.set_display_orientation(client_id, display_index, orientation).await,
        }
    }

    async fn clear_client_display_command(&self, client_id: &str, display_index: i64) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.clear_client_display_command(client_id, display_index),
            Self::Postgres(db) => db.clear_client_display_command(client_id, display_index).await,
        }
    }

    async fn list_client_heartbeats(
        &self,
        client_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, Option<f64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>)>, String> {
        match self {
            Self::Sqlite(db) => db.list_client_heartbeats(client_id, limit),
            Self::Postgres(db) => db.list_client_heartbeats(client_id, limit).await,
        }
    }

    async fn delete_client_history(&self, client_id: &str) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.delete_client_history(client_id),
            Self::Postgres(db) => db.delete_client_history(client_id).await,
        }
    }

    async fn insert_interactive_submission(
        &self,
        client_id: &str,
        client_address: Option<&str>,
        interactive_layout_id: Option<&str>,
        interactive_layout_name: Option<&str>,
        widget_id: &str,
        widget_type: &str,
        widget_label: Option<&str>,
        payload_json: &str,
        collected_at: &str,
    ) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.insert_interactive_submission(
                client_id, client_address, interactive_layout_id, interactive_layout_name,
                widget_id, widget_type, widget_label, payload_json, collected_at,
            ),
            Self::Postgres(db) => db.insert_interactive_submission(
                client_id, client_address, interactive_layout_id, interactive_layout_name,
                widget_id, widget_type, widget_label, payload_json, collected_at,
            ).await,
        }
    }

    async fn list_interactive_submissions(
        &self,
        limit: i64,
        client_filter: Option<&str>,
        page_filter: Option<&str>,
        status_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        match self {
            Self::Sqlite(db) => db.list_interactive_submissions(limit, client_filter, page_filter, status_filter),
            Self::Postgres(db) => db.list_interactive_submissions(limit, client_filter, page_filter, status_filter).await,
        }
    }

    async fn count_interactive_submissions(&self, status_filter: Option<&str>) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.count_interactive_submissions(status_filter),
            Self::Postgres(db) => db.count_interactive_submissions(status_filter).await,
        }
    }

    async fn update_interactive_submission_sync_status(
        &self,
        ids: &[i64],
        status: &str,
        message: Option<&str>,
        sheet_id: Option<&str>,
    ) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.update_interactive_submission_sync_status(ids, status, message, sheet_id),
            Self::Postgres(db) => db.update_interactive_submission_sync_status(ids, status, message, sheet_id).await,
        }
    }

    async fn delete_interactive_submissions(&self, ids: &[i64]) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.delete_interactive_submissions(ids),
            Self::Postgres(db) => db.delete_interactive_submissions(ids).await,
        }
    }

    async fn list_templates(&self) -> Result<Vec<TemplateMeta>, String> {
        match self {
            Self::Sqlite(db) => db.list_templates(),
            Self::Postgres(db) => db.list_templates().await,
        }
    }

    async fn get_template(&self, id: &str) -> Result<Option<String>, String> {
        match self {
            Self::Sqlite(db) => db.get_template(id),
            Self::Postgres(db) => db.get_template(id).await,
        }
    }

    async fn save_template(
        &self,
        id: &str,
        name: &str,
        category: &str,
        is_builtin: bool,
        layout_json: &str,
        created_at: &str,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.save_template(id, name, category, is_builtin, layout_json, created_at),
            Self::Postgres(db) => db.save_template(id, name, category, is_builtin, layout_json, created_at).await,
        }
    }

    async fn delete_template(&self, id: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.delete_template(id),
            Self::Postgres(db) => db.delete_template(id).await,
        }
    }

    async fn list_widget_presets(
        &self,
        widget_type: Option<&str>,
    ) -> Result<Vec<WidgetPresetMeta>, String> {
        match self {
            Self::Sqlite(db) => db.list_widget_presets(widget_type),
            Self::Postgres(db) => db.list_widget_presets(widget_type).await,
        }
    }

    async fn get_widget_preset(&self, id: &str) -> Result<Option<WidgetPreset>, String> {
        match self {
            Self::Sqlite(db) => db.get_widget_preset(id),
            Self::Postgres(db) => db.get_widget_preset(id).await,
        }
    }

    async fn create_widget_preset(
        &self,
        id: &str,
        name: &str,
        widget_type: &str,
        props_json: &str,
        now: &str,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.create_widget_preset(id, name, widget_type, props_json, now),
            Self::Postgres(db) => db
                .create_widget_preset(id, name, widget_type, props_json, now)
                .await,
        }
    }

    async fn update_widget_preset(
        &self,
        id: &str,
        name: &str,
        widget_type: &str,
        props_json: &str,
        now: &str,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.update_widget_preset(id, name, widget_type, props_json, now),
            Self::Postgres(db) => db
                .update_widget_preset(id, name, widget_type, props_json, now)
                .await,
        }
    }

    async fn delete_widget_preset(&self, id: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.delete_widget_preset(id),
            Self::Postgres(db) => db.delete_widget_preset(id).await,
        }
    }

    async fn list_widget_preset_assets(
        &self,
        preset_id: &str,
    ) -> Result<Vec<WidgetPresetAsset>, String> {
        match self {
            Self::Sqlite(db) => db.list_widget_preset_assets(preset_id),
            Self::Postgres(db) => db.list_widget_preset_assets(preset_id).await,
        }
    }

    async fn upsert_widget_preset_asset(&self, asset: &WidgetPresetAsset) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.upsert_widget_preset_asset(
                &asset.id,
                &asset.preset_id,
                &asset.kind,
                &asset.key,
                &asset.url,
            ),
            Self::Postgres(db) => db.upsert_widget_preset_asset(asset).await,
        }
    }

    async fn delete_widget_preset_asset(
        &self,
        preset_id: &str,
        id: &str,
    ) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.delete_widget_preset_asset(preset_id, id),
            Self::Postgres(db) => db.delete_widget_preset_asset(preset_id, id).await,
        }
    }

    async fn list_layout_folders(&self) -> Result<Vec<LayoutFolder>, String> {
        match self {
            Self::Sqlite(db) => db.list_layout_folders(),
            Self::Postgres(db) => db.list_layout_folders().await,
        }
    }

    async fn get_layout_folder(&self, id: &str) -> Result<Option<LayoutFolder>, String> {
        match self {
            Self::Sqlite(db) => db.get_layout_folder(id),
            Self::Postgres(db) => db.get_layout_folder(id).await,
        }
    }

    async fn save_layout_folder(&self, folder: &LayoutFolder) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.save_layout_folder(folder),
            Self::Postgres(db) => db.save_layout_folder(folder).await,
        }
    }

    async fn delete_layout_folder(&self, id: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.delete_layout_folder(id),
            Self::Postgres(db) => db.delete_layout_folder(id).await,
        }
    }

    async fn list_layouts(&self) -> Result<Vec<Layout>, String> {
        match self {
            Self::Sqlite(db) => db.list_layouts(),
            Self::Postgres(db) => db.list_layouts().await,
        }
    }

    async fn save_layout(&self, layout: &Layout) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.save_layout(layout),
            Self::Postgres(db) => db.save_layout(layout).await,
        }
    }

    async fn delete_layout(&self, id: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.delete_layout(id),
            Self::Postgres(db) => db.delete_layout(id).await,
        }
    }

    pub(crate) async fn list_schedules(&self) -> Result<Vec<ScheduleEntry>, String> {
        match self {
            Self::Sqlite(db) => db.list_schedules(),
            Self::Postgres(db) => db.list_schedules().await,
        }
    }

    async fn create_schedule(&self, entry: &ScheduleEntry) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.create_schedule(entry),
            Self::Postgres(db) => db.create_schedule(entry).await,
        }
    }

    async fn get_schedule(&self, id: &str) -> Result<Option<ScheduleEntry>, String> {
        match self {
            Self::Sqlite(db) => db.get_schedule(id),
            Self::Postgres(db) => db.get_schedule(id).await,
        }
    }

    async fn update_schedule(&self, entry: &ScheduleEntry) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.update_schedule(entry),
            Self::Postgres(db) => db.update_schedule(entry).await,
        }
    }

    async fn delete_schedule(&self, id: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.delete_schedule(id),
            Self::Postgres(db) => db.delete_schedule(id).await,
        }
    }

    pub(crate) async fn log_schedule_run(
        &self,
        schedule_id: &str,
        action: &str,
        target: &str,
        success: bool,
        message: Option<&str>,
        occurred_at: &str,
    ) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.log_schedule_run(schedule_id, action, target, success, message, occurred_at),
            Self::Postgres(db) => db
                .log_schedule_run(schedule_id, action, target, success, message, occurred_at)
                .await,
        }
    }

    pub(crate) async fn record_audit_event(
        &self,
        actor: Option<&str>,
        role: Option<&str>,
        event_type: &str,
        target: Option<&str>,
        outcome: &str,
        detail: Option<&str>,
        occurred_at: &str,
    ) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.record_audit_event(
                actor, role, event_type, target, outcome, detail, occurred_at,
            ),
            Self::Postgres(db) => db
                .record_audit_event(actor, role, event_type, target, outcome, detail, occurred_at)
                .await,
        }
    }

    pub(crate) async fn list_audit_events(
        &self,
        limit: i64,
    ) -> Result<Vec<crate::models::AuditEvent>, String> {
        match self {
            Self::Sqlite(db) => db.list_audit_events(limit),
            Self::Postgres(db) => db.list_audit_events(limit).await,
        }
    }
    async fn list_schedule_runs(&self, limit: i64) -> Result<Vec<crate::models::ScheduleRun>, String> {
        match self {
            Self::Sqlite(db) => db.list_schedule_runs(limit),
            Self::Postgres(db) => db.list_schedule_runs(limit).await,
        }
    }

    async fn ensure_news_feed_registered(&self, url: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.ensure_news_feed_registered(url).map(|_| ()),
            Self::Postgres(db) => db.ensure_news_feed_registered(url).await.map(|_| ()),
        }
    }

    async fn get_news_feed(&self, url: &str) -> Result<Option<crate::models::NewsFeed>, String> {
        match self {
            Self::Sqlite(db) => db.get_news_feed(url),
            Self::Postgres(db) => db.get_news_feed(url).await,
        }
    }

    pub(crate) async fn list_news_feeds(&self) -> Result<Vec<crate::models::NewsFeed>, String> {
        match self {
            Self::Sqlite(db) => db.list_news_feeds(),
            Self::Postgres(db) => db.list_news_feeds().await,
        }
    }

    pub(crate) async fn upsert_news_feed(
        &self,
        url: &str,
        title: &str,
        headlines_json: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.upsert_news_feed(url, title, headlines_json, etag, last_modified),
            Self::Postgres(db) => db.upsert_news_feed(url, title, headlines_json, etag, last_modified).await,
        }
    }

    pub(crate) async fn mark_news_feed_fetched(&self, url: &str, fetched_at: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.mark_news_feed_fetched(url, fetched_at),
            Self::Postgres(db) => db.mark_news_feed_fetched(url, fetched_at).await,
        }
    }

    pub(crate) async fn record_news_feed_error(&self, url: &str, error: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.record_news_feed_error(url, error),
            Self::Postgres(db) => db.record_news_feed_error(url, error).await,
        }
    }

    pub(crate) async fn get_layout(&self, id: &str) -> Result<Option<Layout>, String> {
        match self {
            Self::Sqlite(db) => db.get_layout(id),
            Self::Postgres(db) => db.get_layout(id).await,
        }
    }

    pub(crate) async fn get_published_layout(&self, id: &str) -> Result<Option<Layout>, String> {
        match self {
            Self::Sqlite(db) => db.get_published_layout(id),
            Self::Postgres(db) => db.get_published_layout(id).await,
        }
    }

    pub(crate) async fn publish_layout_revision(&self, layout: &Layout, published_by: Option<&str>) -> Result<crate::models::LayoutRevision, String> {
        match self {
            Self::Sqlite(db) => db.publish_layout_revision(layout, published_by),
            Self::Postgres(db) => db.publish_layout_revision(layout, published_by).await,
        }
    }

    pub(crate) async fn list_layout_revisions(&self, id: &str) -> Result<Vec<crate::models::LayoutRevision>, String> {
        match self {
            Self::Sqlite(db) => db.list_layout_revisions(id),
            Self::Postgres(db) => db.list_layout_revisions(id).await,
        }
    }

    async fn get_display_config(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<(Option<String>, String), String> {
        match self {
            Self::Sqlite(db) => db.get_display_config(client_id, display_index),
            Self::Postgres(db) => db.get_display_config(client_id, display_index).await,
        }
    }

    async fn list_client_displays(
        &self,
        client_id: &str,
    ) -> Result<(bool, Option<String>, Vec<crate::db::ClientDisplayRecord>), String> {
        match self {
            Self::Sqlite(db) => db.list_client_displays(client_id),
            Self::Postgres(db) => db.list_client_displays(client_id).await,
        }
    }

    async fn get_layout_sets_for_layouts(
        &self,
        layout_ids: &[String],
    ) -> Result<std::collections::HashMap<String, LayoutSetWithItems>, String> {
        match self {
            Self::Sqlite(db) => db.get_layout_sets_for_layouts(layout_ids),
            Self::Postgres(db) => db.get_layout_sets_for_layouts(layout_ids).await,
        }
    }

    async fn list_layout_sets(&self) -> Result<Vec<LayoutSetSummary>, String> {
        match self {
            Self::Sqlite(db) => db.list_layout_sets(),
            Self::Postgres(db) => db.list_layout_sets().await,
        }
    }

    pub(crate) async fn get_refresh_epoch(&self) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.get_refresh_epoch(),
            Self::Postgres(db) => db.get_refresh_epoch().await,
        }
    }

    pub(crate) async fn bump_refresh_epoch(&self) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.bump_refresh_epoch(),
            Self::Postgres(db) => db.bump_refresh_epoch().await,
        }
    }

    pub(crate) async fn get_layout_set(&self, id: &str) -> Result<Option<LayoutSetWithItems>, String> {
        match self {
            Self::Sqlite(db) => db.get_layout_set(id),
            Self::Postgres(db) => db.get_layout_set(id).await,
        }
    }

    async fn create_layout_set(&self, set: &LayoutSetWithItems) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.create_layout_set(set),
            Self::Postgres(db) => db.create_layout_set(set).await,
        }
    }

    async fn update_layout_set(&self, set: &LayoutSetWithItems) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.update_layout_set(set),
            Self::Postgres(db) => db.update_layout_set(set).await,
        }
    }

    async fn delete_layout_set(&self, id: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.delete_layout_set(id),
            Self::Postgres(db) => db.delete_layout_set(id).await,
        }
    }

    async fn add_layout_set_entry(
        &self,
        layout_set_id: &str,
        entry: &AddLayoutSetEntry,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.add_layout_set_entry(layout_set_id, entry),
            Self::Postgres(db) => db.add_layout_set_entry(layout_set_id, entry).await,
        }
    }

    async fn update_layout_set_entry(
        &self,
        entry_id: &str,
        update: &UpdateLayoutSetEntry,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.update_layout_set_entry(entry_id, update),
            Self::Postgres(db) => db.update_layout_set_entry(entry_id, update).await,
        }
    }

    async fn delete_layout_set_entry(&self, entry_id: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.delete_layout_set_entry(entry_id),
            Self::Postgres(db) => db.delete_layout_set_entry(entry_id).await,
        }
    }

    async fn replace_layout_set_entries(
        &self,
        layout_set_id: &str,
        entries: Vec<LayoutSetEntry>,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.replace_layout_set_entries(layout_set_id, entries),
            Self::Postgres(db) => db.replace_layout_set_entries(layout_set_id, entries).await,
        }
    }

    async fn delete_virtual_client(&self, id: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.delete_virtual_client(id),
            Self::Postgres(db) => db.delete_virtual_client(id).await,
        }
    }

    async fn delete_client(&self, id: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.delete_client(id),
            Self::Postgres(db) => db.delete_client(id).await,
        }
    }

    pub(crate) async fn queue_client_command(
        &self,
        client_id: &str,
        action: &str,
        payload: Option<&str>,
        requested_at: &str,
        requested_by: Option<&str>,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.queue_client_command(client_id, action, payload, requested_at, requested_by),
            Self::Postgres(db) => db.queue_client_command(client_id, action, payload, requested_at, requested_by).await,
        }
    }

    async fn get_client_command(
        &self,
        client_id: &str,
    ) -> Result<Option<(String, String, Option<String>, String, Option<String>)>, String> {
        match self {
            Self::Sqlite(db) => db.get_client_command(client_id),
            Self::Postgres(db) => db.get_client_command(client_id).await,
        }
    }

    async fn delete_client_command_if_requested_at(
        &self,
        client_id: &str,
        requested_at: &str,
    ) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.delete_client_command_if_requested_at(client_id, requested_at),
            Self::Postgres(db) => db.delete_client_command_if_requested_at(client_id, requested_at).await,
        }
    }

    async fn clear_client_command(&self, client_id: &str) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.clear_client_command(client_id),
            Self::Postgres(db) => db.clear_client_command(client_id).await,
        }
    }

    async fn get_display_control(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<Option<(Option<String>, Option<String>, Option<i64>, Option<i64>, Option<f64>, bool, String, String)>, String> {
        match self {
            Self::Sqlite(db) => db.get_display_control(client_id, display_index),
            Self::Postgres(db) => db.get_display_control(client_id, display_index).await,
        }
    }

    async fn set_display_control_method(
        &self,
        client_id: &str,
        display_index: i64,
        method: &str,
    ) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.set_display_control_method(client_id, display_index, method),
            Self::Postgres(db) => db.set_display_control_method(client_id, display_index, method).await,
        }
    }

    async fn publish_scene_assignments(
        &self,
        client_id: &str,
        assignments: &[(i64, Option<String>)],
    ) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.publish_scene_assignments(client_id, assignments),
            Self::Postgres(db) => db.publish_scene_assignments(client_id, assignments).await,
        }
    }

    async fn get_scene(
        &self,
        scene_id: &str,
    ) -> Result<Option<(String, String, Option<String>, String, String, Vec<(i64, Option<String>)>)>, String> {
        match self {
            Self::Sqlite(db) => db.get_scene(scene_id),
            Self::Postgres(db) => db.get_scene(scene_id).await,
        }
    }

    async fn list_scene_ids(&self) -> Result<Vec<String>, String> {
        match self {
            Self::Sqlite(db) => db.list_scene_ids(),
            Self::Postgres(db) => db.list_scene_ids().await,
        }
    }

    async fn save_scene(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        timestamp: &str,
        assignments: &[(i64, Option<String>)],
        update: bool,
    ) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.save_scene(id, name, description, timestamp, assignments, update),
            Self::Postgres(db) => db
                .save_scene(id, name, description, timestamp, assignments, update)
                .await,
        }
    }

    async fn delete_scene(&self, scene_id: &str) -> Result<usize, String> {
        match self {
            Self::Sqlite(db) => db.delete_scene(scene_id),
            Self::Postgres(db) => db.delete_scene(scene_id).await,
        }
    }
}

pub fn router(runtime_state: RuntimeState) -> Router {
    let auth = crate::auth::AuthState::from_env();
    let auth_repository = runtime_state.auth_repository.clone();
    auth.load_from_repository(auth_repository.clone());
    let auth_app = crate::auth::AuthAppState {
        auth: auth.clone(),
        repository: auth_repository,
    };
    crate::middleware::spawn_gc(auth.clone());
    let files_limiter = std::sync::Arc::new(crate::rate_limit::RateLimiter::new());
    let rate_limit_state = RateLimitLayerState {
        limiter: files_limiter,
        runtime_state: runtime_state.clone(),
    };
    let metrics = runtime_state.metrics.clone();

    Router::new()
        .route("/api/layouts", get(list_layouts).post(create_layout))
        .route("/api/layout-folders", get(list_layout_folders).post(create_layout_folder))
        .route(
            "/api/layout-folders/{id}",
            put(update_layout_folder).delete(delete_layout_folder),
        )
        .route("/api/layouts/{id}/render", get(render_layout))
        .route("/api/layouts/{id}/publish", post(publish_layout))
        .route("/api/layouts/{id}/revisions", get(list_layout_revisions))
        .route("/api/layouts/{id}/propagation", get(layout_propagation_status))
        .route(
            "/api/layouts/{id}",
            get(get_layout).put(update_layout).delete(delete_layout),
        )
        .route("/api/displays", get(list_displays))
        .route(
            "/api/display-state",
            get(get_display_state).put(set_display_state),
        )
        .route(
            "/api/display-state/{display}",
            get(get_display_state_for_display).delete(delete_display_state),
        )
        .route("/api/system", get(system_status))
        .route("/api/addons", get(list_addons).post(install_addon))
        .route(
            "/api/addons/execution/approvals",
            get(list_addon_execution_approvals).post(create_addon_execution_approval),
        )
        .route(
            "/api/addons/execution/approvals/{id}",
            get(get_addon_execution_approval),
        )
        .route(
            "/api/addons/execution/approvals/{id}/capabilities",
            patch(update_addon_execution_capabilities),
        )
        .route(
            "/api/addons/execution/approvals/{id}/approve",
            post(approve_addon_execution_approval),
        )
        .route(
            "/api/addons/execution/approvals/{id}/revoke",
            post(revoke_addon_execution_approval),
        )
        .route(
            "/api/addons/execution/approvals/{id}/targets/{target}/approve",
            post(approve_addon_execution_target),
        )
        .route(
            "/api/addons/execution/approvals/{id}/targets/{target}/revoke",
            post(revoke_addon_execution_target),
        )
        .route("/api/addons/widget-types", get(list_addon_widget_types))
        .route(
            "/api/addons/package",
            post(upload_addon_package)
                .put(update_addon_package)
                .layer(DefaultBodyLimit::max(256 * 1024 * 1024)),
        )
        .route(
            "/api/addons/package/{id}/rollback",
            post(rollback_signed_client_package_handler),
        )
        .route(
            "/api/addons/trusted-service/stage",
            post(stage_trusted_service_manifest).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route(
            "/api/addons/trusted-service/staged",
            get(list_trusted_service_stages),
        )
        .route(
            "/api/addons/trusted-service/staged/{id}",
            delete(remove_trusted_service_stage),
        )
        .route(
            "/api/addons/trusted-service/staged/{id}/approve",
            post(approve_trusted_service),
        )
        .route(
            "/api/addons/trusted-service/runtime",
            get(list_trusted_service_runtime),
        )
        .route(
            "/api/addons/trusted-service/runtime/{id}/disable",
            post(disable_trusted_service),
        )
        .route(
            "/api/addons/{addon_id}/storage/{storage_key}",
            get(get_addon_storage)
                .put(set_addon_storage)
                .delete(delete_addon_storage)
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/api/addons/{id}/rollback",
            post(rollback_addon_handler),
        )
        .route("/api/addons/{id}/enable", post(enable_addon))
        .route("/api/addons/{id}/disable", post(disable_addon))
        .route("/api/addons/{id}", put(update_addon).delete(delete_addon))
        .route("/api/scenes", get(list_scenes).post(create_scene))
        .route(
            "/api/scenes/{id}",
            get(get_scene).put(update_scene).delete(delete_scene),
        )
        .route(
            "/api/scenes/{id}/publish/{client_id}",
            post(publish_scene_to_client),
        )
        .route("/api/clients", get(connected_clients))
        .route("/api/clients/register", post(register_client))
        .route("/api/clients/{id}/adopt", post(adopt_client))
        .route(
            "/api/clients/{id}/assignment",
            get(get_client_assignment).put(set_client_assignment),
        )
        .route("/api/clients/{id}/config", get(get_client_config))
        .route("/api/clients/{id}/addons", get(get_client_addons))
        .route(
            "/api/clients/{id}/addons/status",
            post(post_client_addon_status)
                .layer(DefaultBodyLimit::max(crate::addons::MAX_CLIENT_STATUS_BYTES)),
        )
        .route("/api/clients/{id}/addons/{addon_id}/package", get(get_client_addon_package))
        .route("/api/clients/{id}/update-check", get(get_client_update_check))
        .route("/api/clients/{id}/theme", patch(set_client_theme))
        .route(
            "/api/clients/{id}/display-count",
            post(set_client_display_count),
        )
        .route(
            "/api/clients/{id}/touch-binding",
            post(set_client_touch_binding),
        )
        .route("/api/clients/{id}/admin-users", get(get_client_admin_users))
        .route("/api/clients/assign-bulk", post(bulk_assign_clients))
        .route("/api/clients/command-bulk", post(bulk_command_clients))
        .route(
            "/api/clients/diagnostics",
            get(list_client_diagnostics)
                .post(save_client_diagnostics)
                .layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/api/clients/diagnostics/{filename}",
            get(download_client_diagnostics),
        )
        .route("/api/clients/{id}/nudge", post(nudge_client))
        .route("/api/clients/virtual", post(create_virtual_client))
        .route("/api/clients/{id}/virtual", get(get_virtual_client).patch(update_virtual_client).put(set_virtual_client_layout).delete(delete_virtual_client))
        .route("/api/clients/{id}", delete(delete_client))
        .route(
            "/api/clients/{id}/history",
            get(get_client_history).delete(delete_client_history),
        )
        .route(
            "/api/clients/{id}/command",
            get(get_client_command)
                .post(set_client_command)
                .delete(clear_client_command)
                .layer(DefaultBodyLimit::max(MAX_CLIENT_COMMAND_PAYLOAD_BYTES)),
        )
        .route(
            "/api/clients/{id}/command/ack",
            post(acknowledge_client_command),
        )
        .route("/api/clients/{id}/displays", get(list_client_displays))
        .route(
            "/api/clients/{id}/displays/bulk-assign",
            post(bulk_assign_displays),
        )
        .route(
            "/api/clients/{id}/displays/{index}/control",
            get(get_client_display_control)
                .patch(set_client_display_control_method),
        )
        .route(
            "/api/clients/{id}/displays/{index}/orientation",
            patch(set_client_display_orientation),
        )
        .route(
            "/api/clients/{id}/displays/{index}/config",
            get(get_client_display_config),
        )
        .route(
            "/api/clients/{id}/displays/{index}/layout",
            post(set_client_display_layout),
        )
        .route(
            "/api/clients/{id}/displays/{index}/command",
            post(set_client_display_command).delete(clear_client_display_command),
        )
        .route("/api/clients/search", get(search_connected_clients))
        .route("/api/refresh", get(get_refresh).post(bump_refresh))
        .route("/api/clear-cache", post(clear_cache))
        .route("/api/log", post(player_log))
        .route("/api/ics-proxy", get(ics_proxy))
        .route("/api/weather/lookup", get(weather_location_lookup))
        .route("/api/weather/forecast", get(weather_forecast))
        .route("/api/traffic/events", get(traffic_events))
        .route("/api/traffic/map.png", get(traffic_map_image))
        .route("/api/news/headlines", get(get_news_headlines))
        .route("/api/news/refresh", post(post_news_refresh))
        .route("/api/templates", get(list_templates).post(create_template))
        .route(
            "/api/templates/{id}",
            get(get_template).delete(delete_template),
        )
        .route(
            "/api/display-overrides",
            get(get_overrides).put(set_override),
        )
        .route(
            "/api/display-overrides/{display}",
            delete(clear_override),
        )
        .route("/api/files/{category}", get(list_files).post(upload_file).layer(DefaultBodyLimit::max(1024 * 1024 * 1024)))
        .route(
            "/api/files/{category}/{name}",
            delete(delete_file),
        )
        .route("/api/files/trash", get(list_trash))
        .route("/api/files/trash/{id}/restore", post(restore_trash))
        .route("/api/fonts/catalog", get(fonts_catalog_handler))
        .route("/api/fonts/preview-status", get(fonts_preview_status_handler))
        .route("/api/fonts/full-preview-css", get(fonts_full_preview_css_handler))
        .route("/api/fonts/installed", get(fonts_installed_handler))
        .route("/api/fonts/download", post(fonts_download_handler))
        .route(
            "/api/fonts/{filename}",
            delete(fonts_delete_handler),
        )
        .route(
            "/api/backups",
            get(backup_api::list_backups),
        )
        .route(
            "/api/backups",
            post(backup_api::create_backup),
        )
        .route(
            "/api/backups/{id}",
            delete(backup_api::delete_backup),
        )
        .route(
            "/api/backups/{id}",
            put(backup_api::restore_backup),
        )
        .route(
            "/api/backups/upload",
            put(backup_api::upload_backup),
        )
        .route(
            "/api/backups/download/{id}",
            get(backup_api::download_backup),
        )
        .route("/api/schedules", get(list_schedules).post(create_schedule))
        .route(
            "/api/schedules/{id}",
            get(get_schedule)
                .put(update_schedule)
                .delete(delete_schedule),
        )
        // ── Layout Sets ──────────────────────────────────────────
        .route(
            "/api/layout-sets",
            get(list_layout_sets).post(create_layout_set),
        )
        .route(
            "/api/layout-sets/{id}",
            get(get_layout_set)
                .put(update_layout_set)
                .delete(delete_layout_set),
        )
        .route("/api/layout-sets/{id}/entries", put(replace_layout_set_entries))
        // ── Widget Presets ──
        .route(
            "/api/widget-presets",
            get(list_widget_presets)
                .post(create_widget_preset)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/api/widget-presets/{id}",
            get(get_widget_preset)
                .put(update_widget_preset)
                .delete(delete_widget_preset)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/api/widget-presets/{id}/assets",
            post(add_widget_preset_asset),
        )
        .route(
            "/api/widget-presets/{id}/assets/{asset_id}",
            delete(remove_widget_preset_asset),
        )
        .route("/api/widget-presets/{id}/export", get(export_widget_preset))
        .route("/api/widget-presets/import", post(import_widget_preset))
        .route(
            "/api/widget-presets/reset-defaults",
            post(reset_default_widget_presets),
        )
        // Version manager
        .route(
            "/install/player-client-bundle-download.tar.gz",
            get(download_player_client_bundle_handler),
        )
        .route(
            "/install/player-client-bundle-bootstrap.sh",
            get(download_player_client_bootstrap_handler),
        )
        .route(
            "/install/screentra-cast-installers.tar.gz",
            get(download_screentra_cast_installers_handler),
        )
        .route("/api/version", get(get_version))
        .route("/api/version/export", get(export_state_bundle_handler))
        .route("/api/version/import", post(import_state_bundle_handler).layer(DefaultBodyLimit::max(1024 * 1024 * 1024)))
        .route("/api/version/restart", post(restart_service_handler))
        // Interactive submissions — POST is unauthenticated (player-side data collection)
        .route("/api/interactive/submit", post(post_interactive_submission_handler))
        .route(
            "/api/interactive/submissions",
            get(get_interactive_submissions_handler)
                .delete(delete_interactive_submissions_handler),
        )
        .route(
            "/api/interactive/submissions/stats",
            get(get_interactive_submissions_stats_handler),
        )
        .route(
            "/api/interactive/submissions/export.csv",
            get(export_interactive_submissions_csv_handler),
        )
        .route(
            "/api/interactive/submissions/sync-to-google",
            post(sync_interactive_submissions_to_google_handler),
        )
        .route(
            "/api/version/update/export",
            get(export_update_bundle_handler),
        )
        .route(
            "/api/version/update/import",
            post(import_update_bundle_handler).layer(DefaultBodyLimit::max(1024 * 1024 * 1024)),
        )
        .route(
            "/api/version/update/last-failure",
            get(update_last_failure_handler),
        )
        .route(
            "/api/version/update/github",
            get(github_update_status_handler),
        )
        .route(
            "/api/version/update/github/check",
            post(github_update_check_handler),
        )
        .route(
            "/api/version/update/github/settings",
            post(github_update_settings_handler),
        )
        .route(
            "/api/version/update/apply",
            post(apply_github_update_handler),
        )
        .route("/api/audit", get(list_audit_events_handler))
        .route("/api/version/update/preview", get(update_preview_handler))
        .route(
            "/api/version/player-client-bundle/build",
            post(build_player_client_bundle_handler),
        )
        .route(
            "/api/version/server-bundle/build",
            post(build_server_bundle_handler),
        )
        .route(
            "/api/version/player-client-bundle/download",
            get(download_player_client_bundle_handler),
        )
        .route(
            "/api/version/player-client-bundle/bootstrap",
            get(download_player_client_bootstrap_handler),
        )
        // System time
        .route("/api/system/time", get(get_system_time))
        .route("/api/system/time/timezone", post(set_timezone))
        .route("/api/system/time/clock", post(set_clock))
        .route("/api/system/time/sync-ntp", post(sync_ntp))
        .route("/api/system/maintenance/vacuum", post(vacuum_database))
        .route("/api/system/health", get(get_system_health))
        .route("/api/system/liveness", get(get_system_liveness))
        .route("/api/system/readiness", get(get_system_readiness))
        .route("/metrics", get(get_metrics))
        .route("/api/schedule-runs", get(list_schedule_runs))
        // User manager (admin-only handlers; still covered by global auth middleware)
        .nest(
            "/api/users",
            axum::Router::new()
                .route("/", get(list_users).post(create_user))
                .route("/{id}", get(get_user).put(update_user).delete(delete_user))
                .with_state(auth_app.clone()),
        )
        // Serve uploaded files (icons, images, videos, music, etc.)
        .route("/api/setup", post(create_initial_admin).with_state(auth_app.clone()))
        .route("/api/setup/", post(create_initial_admin).with_state(auth_app.clone()))
        .nest(
            "/api/setup",
            axum::Router::new()
                .route("/status", get(setup_status))
                .with_state(auth_app.clone()),
        )
        .nest(
            "/api/auth",
            axum::Router::new()
                .route("/login", post(crate::auth::login))
                .route("/step-up", post(crate::auth::step_up))
                .route("/step-up/clear", post(crate::auth::clear_step_up))
                .route("/logout", post(crate::auth::logout))
                .route("/status", get(crate::auth::status))
                .route(
                    "/google/login-url",
                    get(crate::auth::google_login_url),
                )
                .route(
                    "/google/callback",
                    get(crate::auth::google_callback),
                )
                .with_state(auth_app.clone()),
        )
        .nest(
            "/api/account",
            axum::Router::new()
                .route("/", get(crate::auth::account_status))
                .route("/password", post(crate::auth::change_password))
                .route("/google/bind-url", get(crate::auth::google_bind_url))
                .route("/google/unbind", post(crate::auth::google_unbind))
                .with_state(auth_app.clone()),
        )
        .nest(
            "/api/system/maintenance",
            axum::Router::new()
                .route("/purge-sessions", post(purge_sessions))
                .with_state(auth_app.clone()),
        )
        .route(
            "/clients/{client_id}/screenshot",
            get(crate::screenshot::get_screenshot)
                .post(crate::screenshot::post_screenshot)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/api/screenshots",
            get(crate::screenshot::list_screenshots),
        )
        .layer(axum::middleware::from_fn_with_state(
            rate_limit_state,
            crate::api::rate_limit_files_layer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            crate::middleware::require_auth,
        ))
        .layer(axum::Extension(auth.clone()))
        .route("/uploads", get(serve_upload))
        .route("/uploads/{*path}", get(serve_upload))
        .fallback_service(
            tower_http::services::ServeDir::new("static").append_index_html_on_directories(true),
        )
        .layer(axum::middleware::from_fn(crate::api::csp_layer))
        .layer(axum::middleware::from_fn_with_state(
            metrics,
            crate::observability::request_layer,
        ))
        .with_state(runtime_state)
}

// ── Layout handlers ──────────────────────────────────────────

async fn list_layout_folders(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<Vec<LayoutFolder>>, (StatusCode, String)> {
    repository
        .list_layout_folders()
        .await
        .map(AxumJson)
        .map_err(|_| internal_server_error())
}

async fn create_layout_folder(
    State(repository): State<ClientRepository>,
    AxumJson(mut folder): AxumJson<LayoutFolder>,
) -> Result<AxumJson<LayoutFolder>, (StatusCode, String)> {
    if folder.id.is_empty() {
        folder.id = Uuid::new_v4().to_string();
    }
    let now = Utc::now().to_rfc3339();
    folder.created_at = now.clone();
    folder.updated_at = now;
    folder.name = folder.name.trim().to_string();
    repository
        .save_layout_folder(&folder)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(AxumJson(folder))
}

async fn update_layout_folder(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    AxumJson(mut folder): AxumJson<LayoutFolder>,
) -> Result<AxumJson<LayoutFolder>, (StatusCode, String)> {
    let existing = repository
        .get_layout_folder(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::NOT_FOUND, "Folder not found".to_string()))?;
    folder.id = id;
    folder.created_at = existing.created_at;
    folder.updated_at = Utc::now().to_rfc3339();
    folder.name = folder.name.trim().to_string();
    repository
        .save_layout_folder(&folder)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(AxumJson(folder))
}

async fn delete_layout_folder(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    repository
        .delete_layout_folder(&id)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_layouts(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<Vec<Layout>>, (StatusCode, String)> {
    repository
        .list_layouts()
        .await
        .map(AxumJson)
        .map_err(|_| internal_server_error())
}

async fn get_layout(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<Layout>, (StatusCode, String)> {
    repository
        .get_layout(&id)
        .await
        .map_err(|_| internal_server_error())?
        .map(AxumJson)
        .ok_or((StatusCode::NOT_FOUND, "Layout not found".to_string()))
}

async fn create_layout(
    State(repository): State<ClientRepository>,
    AxumJson(mut layout): AxumJson<Layout>,
) -> Result<AxumJson<Layout>, (StatusCode, String)> {
    if layout.id.is_empty() {
        layout.id = Uuid::new_v4().to_string();
    }
    let now = Utc::now().to_rfc3339();
    if layout.page_type.is_empty() {
        layout.page_type = "single".to_string();
    }
    if layout.timing_mode.is_empty() {
        layout.timing_mode = "global".to_string();
    }
    if layout.duration < 1 {
        layout.duration = 15;
    }
    if layout.created_at.is_empty() {
        layout.created_at = now.clone();
    }
    layout.updated_at = now;

    repository
        .save_layout(&layout)
        .await
        .map_err(|_| internal_server_error())?;
    // If the layout contains any News widgets, make sure their feed URLs
    // are registered in the news_feeds cache so the next maintenance pass
    // starts fetching them. Errors here are non-fatal: the maintenance
    // loop will just not have anything to fetch until the user adds a
    // feed URL.
    for (_, url) in crate::news::collect_news_feed_urls(&[layout.clone()]) {
        if let Err(e) = repository.ensure_news_feed_registered(&url).await {
            log::warn!("could not register news feed {url}: {e}");
        }
    }
    Ok(AxumJson(layout))
}

async fn update_layout(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    AxumJson(mut layout): AxumJson<Layout>,
) -> Result<AxumJson<Layout>, (StatusCode, String)> {
    layout.id = id;
    if layout.page_type.is_empty() {
        layout.page_type = "single".to_string();
    }
    if layout.timing_mode.is_empty() {
        layout.timing_mode = "global".to_string();
    }
    if layout.duration < 1 {
        layout.duration = 15;
    }
    layout.updated_at = Utc::now().to_rfc3339();
    repository
        .save_layout(&layout)
        .await
        .map_err(|_| internal_server_error())?;
    for (_, url) in crate::news::collect_news_feed_urls(&[layout.clone()]) {
        if let Err(e) = repository.ensure_news_feed_registered(&url).await {
            log::warn!("could not register news feed {url}: {e}");
        }
    }
    Ok(AxumJson(layout))
}

async fn list_layout_revisions(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<Vec<crate::models::LayoutRevision>>, (StatusCode, String)> {
    let revisions = repository
        .list_layout_revisions(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(revisions))
}

async fn publish_layout(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let layout = repository
        .get_layout(&id)
        .await
        .map_err(|_| internal_server_error())?;
    let Some(layout) = layout else {
        return Err((StatusCode::NOT_FOUND, "Layout not found".to_string()));
    };
    let revision = repository
        .publish_layout_revision(&layout, None)
        .await
        .map_err(|_| internal_server_error())?;
    let epoch = repository
        .bump_refresh_epoch()
        .await
        .map_err(|_| internal_server_error())?;
    let mut reload_queued = 0usize;
    let reload_payload = serde_json::json!({
        "all_displays": true,
        "published_revision": revision.revision,
    });
    for client in repository
        .list_registered_clients()
        .await
        .map_err(|_| internal_server_error())?
    {
        let client_id = &client.0;
        let enrolled = client.14 != 0;
        let is_virtual = client.21 != 0;
        if !enrolled || is_virtual {
            continue;
        }
        let (_, client_layout_id, displays) = repository
            .list_client_displays(client_id)
            .await
            .map_err(|_| internal_server_error())?;
        let assigned = client_layout_id.as_deref() == Some(id.as_str())
            || displays
                .iter()
                .any(|display| display.assigned_layout_id.as_deref() == Some(id.as_str()));
        if assigned {
            queue_repository_command(
                &repository,
                client_id,
                "reload_layout",
                Some(&reload_payload),
                Some("system"),
                )
                .await
                .map_err(command_queue_http_error)?;
            reload_queued += 1;
        }
    }
    log::info!("layout published layout_id={} revision={} refresh_epoch={} reload_queued={}", id, revision.revision, epoch, reload_queued);
    Ok(AxumJson(serde_json::json!({
        "layout_id": id,
        "revision": revision.revision,
        "refresh_epoch": epoch,
        "reload_queued": reload_queued,
        "published_at": revision.created_at,
    })))
}

async fn delete_layout(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    repository
        .delete_layout(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Display handlers ─────────────────────────────────────────

async fn list_displays(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<Vec<DisplayInfo>>, (StatusCode, String)> {
    let mut displays = detect_displays();
    let overrides = repository
        .get_display_overrides()
        .await
        .unwrap_or_default();
    for display in &mut displays {
        if let Some((w, h)) = overrides.get(&display.connector) {
            display.override_width = Some(*w);
            display.override_height = Some(*h);
            display.current_mode = Some(DisplayMode {
                width: *w,
                height: *h,
                refresh_rate: display
                    .current_mode
                    .as_ref()
                    .map(|m| m.refresh_rate)
                    .unwrap_or(60.0),
            });
            display.label = format!("{} ({}x{} overridden)", display.connector, w, h);
        }
    }
    Ok(AxumJson(displays))
}

// ── Display State handlers ───────────────────────────────────

async fn get_display_state(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<DisplayState>, (StatusCode, String)> {
    repository
        .get_display_state()
        .await
        .map(AxumJson)
        .map_err(|_| internal_server_error())
}

async fn get_display_state_for_display(
    State(repository): State<ClientRepository>,
    Path(display): Path<String>,
) -> Result<AxumJson<DisplayState>, (StatusCode, String)> {
    if let Some(assignment) = repository
        .get_display_assignment(&display)
        .await
        .map_err(|_| internal_server_error())?
    {
        Ok(AxumJson(DisplayState {
            assignments: vec![assignment],
        }))
    } else {
        Ok(AxumJson(DisplayState {
            assignments: vec![],
        }))
    }
}

async fn set_display_state(
    State(repository): State<ClientRepository>,
    AxumJson(state): AxumJson<DisplayState>,
) -> Result<AxumJson<DisplayState>, (StatusCode, String)> {
    for assignment in &state.assignments {
        repository
            .set_display_assignment(&assignment.display, assignment.layout_id.as_deref())
            .await
            .map_err(|_| internal_server_error())?;
    }
    Ok(AxumJson(state))
}

/// DELETE /api/display-state/{display}
///
/// Remove a single display's assignment. Returns the number of rows
/// deleted (0 if the display had no assignment). Useful for cleaning
/// up orphaned state after a schedule is removed or a layout is
/// deleted.
async fn delete_display_state(
    State(repository): State<ClientRepository>,
    Path(display): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let rows = repository
        .delete_display_assignment(&display)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(
        serde_json::json!({"deleted": rows, "display": display}),
    ))
}

// ── System Status handler ────────────────────────────────────

fn addon_error_response(error: AddonError) -> (StatusCode, String) {
    match error {
        AddonError::Invalid(message) => (StatusCode::BAD_REQUEST, message),
        AddonError::AlreadyInstalled => (StatusCode::CONFLICT, error.to_string()),
        AddonError::NotFound => (StatusCode::NOT_FOUND, error.to_string()),
        AddonError::Io(message) => {
            log::error!("Addon manager storage failure: {message}");
            internal_server_error()
        }
    }
}

fn trusted_service_runtime_error_response(
    error: crate::trusted_service_runtime::RuntimeError,
) -> (StatusCode, String) {
    use crate::trusted_service_runtime::RuntimeError;
    match error {
        RuntimeError::Invalid(message) => (StatusCode::BAD_REQUEST, message),
        RuntimeError::Conflict(message) => (StatusCode::CONFLICT, message),
        RuntimeError::Blocked(message) => (StatusCode::CONFLICT, message),
        RuntimeError::NotFound => (StatusCode::NOT_FOUND, error.to_string()),
        RuntimeError::Io(message) => {
            log::error!("Trusted service runtime storage failure: {message}");
            internal_server_error()
        }
    }
}

fn require_addon_storage_admin(
    principal: &crate::middleware::AuthenticatedPrincipal,
) -> Result<(), (StatusCode, String)> {
    if principal.role == "admin" {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "administrator role required".to_string(),
        ))
    }
}

async fn audit_trusted_service_stage_event(
    state: &RuntimeState,
    principal: &crate::middleware::AuthenticatedPrincipal,
    event_type: &str,
    target: &str,
    outcome: &str,
    detail: Option<&str>,
) -> Result<(), String> {
    state
        .client_repository()
        .record_audit_event(
            Some(&principal.username),
            Some(&principal.role),
            event_type,
            Some(target),
            outcome,
            detail,
            &Utc::now().to_rfc3339(),
        )
        .await
        .map(|_| ())
        .map_err(|error| {
            log::error!("trusted service audit persistence failed: {error}");
            "trusted service audit persistence failed".to_string()
        })
}

async fn get_addon_storage(
    State(repository): State<AddonStorageRepository>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path((addon_id, storage_key)): Path<(String, String)>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    crate::addons::validate_addon_storage_namespace(&addon_id)
        .and(crate::addons::validate_addon_storage_key(&storage_key))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let value_json = repository
        .get(&addon_id, &storage_key)
        .await
        .map_err(|error| {
            log::error!("addon storage read failed: {error}");
            internal_server_error()
        })?
        .ok_or((StatusCode::NOT_FOUND, "Addon storage entry not found".to_string()))?;
    let value = serde_json::from_str(&value_json).map_err(|error| {
        log::error!("addon storage contains invalid JSON: {error}");
        internal_server_error()
    })?;
    Ok(AxumJson(value))
}

async fn set_addon_storage(
    State(repository): State<AddonStorageRepository>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path((addon_id, storage_key)): Path<(String, String)>,
    AxumJson(value): AxumJson<serde_json::Value>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    crate::addons::validate_addon_storage_namespace(&addon_id)
        .and(crate::addons::validate_addon_storage_key(&storage_key))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let value_json = serde_json::to_string(&value).map_err(|_| internal_server_error())?;
    crate::addons::validate_addon_storage_value(&value_json)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    repository
        .set(&addon_id, &storage_key, &value_json, &Utc::now().to_rfc3339())
        .await
        .map_err(|error| {
            log::error!("addon storage write failed: {error}");
            internal_server_error()
        })?;
    Ok(AxumJson(value))
}

async fn delete_addon_storage(
    State(repository): State<AddonStorageRepository>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path((addon_id, storage_key)): Path<(String, String)>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    crate::addons::validate_addon_storage_namespace(&addon_id)
        .and(crate::addons::validate_addon_storage_key(&storage_key))
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let deleted = repository.delete(&addon_id, &storage_key).await.map_err(|error| {
        log::error!("addon storage delete failed: {error}");
        internal_server_error()
    })?;
    Ok(AxumJson(serde_json::json!({ "deleted": deleted })))
}

async fn list_addons() -> Result<AxumJson<Vec<AddonRecord>>, (StatusCode, String)> {
    crate::addons::list()
        .map(AxumJson)
        .map_err(addon_error_response)
}

async fn list_addon_execution_approvals(
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
) -> Result<AxumJson<Vec<crate::addon_capabilities::AddonApprovalRecord>>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    crate::addon_capabilities::AddonApprovalStore::list()
        .map(AxumJson)
        .map_err(|error| {
            log::error!("addon approval listing failed: {error}");
            internal_server_error()
        })
}

async fn get_addon_execution_approval(
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<AxumJson<crate::addon_capabilities::AddonApprovalRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    crate::addon_capabilities::AddonApprovalStore::load(&id)
        .map(AxumJson)
        .map_err(|error| {
            if error == "Approval record not found" {
                (StatusCode::NOT_FOUND, error)
            } else {
                log::error!("addon approval read failed: {error}");
                internal_server_error()
            }
        })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateAddonExecutionApproval {
    addon_id: String,
    addon_version: String,
    artifact_digest: String,
    request: crate::addon_capabilities::AddonExecutionRequest,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityApprovalUpdate {
    approved: Vec<bool>,
}

async fn create_addon_execution_approval(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    AxumJson(payload): AxumJson<CreateAddonExecutionApproval>,
) -> Result<AxumJson<crate::addon_capabilities::AddonApprovalRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    if let Ok(existing) = crate::addon_capabilities::AddonApprovalStore::load(&payload.addon_id) {
        if existing.state != crate::addon_capabilities::ApprovalState::Pending {
            return Err((
                StatusCode::CONFLICT,
                "An approval already exists for this add-on".to_string(),
            ));
        }
        return Err((
            StatusCode::CONFLICT,
            "An approval is already pending for this add-on".to_string(),
        ));
    }
    let record = crate::addon_capabilities::AddonApprovalRecord::pending(
        &payload.addon_id,
        &payload.addon_version,
        &payload.artifact_digest,
        payload.request,
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, format!("Invalid approval request: {error:?}")))?;
    crate::addon_capabilities::AddonApprovalStore::save(&record)
        .map_err(|error| {
            log::error!("addon approval save failed: {error}");
            internal_server_error()
        })?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "addon_execution_approval",
        &record.addon_id,
        "pending",
        Some("execution approval staged for administrator review"),
    )
    .await
    .map_err(|_| internal_server_error())?;
    Ok(AxumJson(record))
}

async fn update_addon_execution_capabilities(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
    AxumJson(payload): AxumJson<CapabilityApprovalUpdate>,
) -> Result<AxumJson<crate::addon_capabilities::AddonApprovalRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let mut record = crate::addon_capabilities::AddonApprovalStore::load(&id)
        .map_err(|_| (StatusCode::NOT_FOUND, "Approval record not found".to_string()))?;
    if record.state != crate::addon_capabilities::ApprovalState::Pending {
        return Err((StatusCode::CONFLICT, "Approval is already finalized".to_string()));
    }
    if payload.approved.len() != record.request.capabilities.len() {
        return Err((StatusCode::BAD_REQUEST, "Capability approval count mismatch".to_string()));
    }
    for (index, approved) in payload.approved.into_iter().enumerate() {
        record
            .set_capability_approved(index, approved)
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid capability approval".to_string()))?;
    }
    crate::addon_capabilities::AddonApprovalStore::save(&record)
        .map_err(|_| internal_server_error())?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "addon_execution_approval",
        &record.addon_id,
        "capabilities_updated",
        Some("capability decisions updated"),
    )
    .await
    .map_err(|_| internal_server_error())?;
    Ok(AxumJson(record))
}

async fn approve_addon_execution_approval(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<AxumJson<crate::addon_capabilities::AddonApprovalRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let mut record = crate::addon_capabilities::AddonApprovalStore::load(&id)
        .map_err(|_| (StatusCode::NOT_FOUND, "Approval record not found".to_string()))?;
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    record
        .approve(&principal.username, &timestamp)
        .map_err(|error| (StatusCode::BAD_REQUEST, format!("Approval rejected: {error:?}")))?;
    crate::addon_capabilities::AddonApprovalStore::save(&record)
        .map_err(|_| internal_server_error())?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "addon_execution_approval",
        &record.addon_id,
        "approved",
        Some("administrator approved requested execution capabilities"),
    )
    .await
    .map_err(|_| internal_server_error())?;
    Ok(AxumJson(record))
}

async fn revoke_addon_execution_approval(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<AxumJson<crate::addon_capabilities::AddonApprovalRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let mut record = crate::addon_capabilities::AddonApprovalStore::load(&id)
        .map_err(|_| (StatusCode::NOT_FOUND, "Approval record not found".to_string()))?;
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    record
        .revoke(&principal.username, &timestamp)
        .map_err(|error| (StatusCode::BAD_REQUEST, format!("Revocation rejected: {error:?}")))?;
    crate::addon_capabilities::AddonApprovalStore::save(&record)
        .map_err(|_| internal_server_error())?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "addon_execution_approval",
        &record.addon_id,
        "revoked",
        Some("administrator revoked execution approval"),
    )
    .await
    .map_err(|_| internal_server_error())?;
    Ok(AxumJson(record))
}

fn parse_addon_execution_target(
    target: &str,
) -> Result<crate::addon_capabilities::ExecutionTarget, (StatusCode, String)> {
    match target {
        "server" => Ok(crate::addon_capabilities::ExecutionTarget::Server),
        "client" => Ok(crate::addon_capabilities::ExecutionTarget::Client),
        _ => Err((StatusCode::BAD_REQUEST, "Invalid execution target".to_string())),
    }
}

async fn approve_addon_execution_target(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path((id, target)): Path<(String, String)>,
) -> Result<AxumJson<crate::addon_capabilities::AddonApprovalRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let target = parse_addon_execution_target(&target)?;
    let mut record = crate::addon_capabilities::AddonApprovalStore::load(&id)
        .map_err(|_| (StatusCode::NOT_FOUND, "Approval record not found".to_string()))?;
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    record
        .approve_target(target, &principal.username, &timestamp)
        .map_err(|error| (StatusCode::BAD_REQUEST, format!("Target approval rejected: {error:?}")))?;
    crate::addon_capabilities::AddonApprovalStore::save(&record)
        .map_err(|_| internal_server_error())?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "addon_execution_approval",
        &record.addon_id,
        "target_approved",
        Some("administrator approved one execution target"),
    )
    .await
    .map_err(|_| internal_server_error())?;
    Ok(AxumJson(record))
}

async fn revoke_addon_execution_target(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path((id, target)): Path<(String, String)>,
) -> Result<AxumJson<crate::addon_capabilities::AddonApprovalRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let target = parse_addon_execution_target(&target)?;
    let mut record = crate::addon_capabilities::AddonApprovalStore::load(&id)
        .map_err(|_| (StatusCode::NOT_FOUND, "Approval record not found".to_string()))?;
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    record
        .revoke_target(target, &principal.username, &timestamp)
        .map_err(|error| (StatusCode::BAD_REQUEST, format!("Target revocation rejected: {error:?}")))?;
    crate::addon_capabilities::AddonApprovalStore::save(&record)
        .map_err(|_| internal_server_error())?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "addon_execution_approval",
        &record.addon_id,
        "target_revoked",
        Some("administrator revoked one execution target"),
    )
    .await
    .map_err(|_| internal_server_error())?;
    Ok(AxumJson(record))
}

async fn list_addon_widget_types() -> Result<AxumJson<Vec<AddonWidgetRegistration>>, (StatusCode, String)> {
    crate::addons::enabled_widget_registrations()
        .map(AxumJson)
        .map_err(addon_error_response)
}

async fn list_trusted_service_stages(
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
) -> Result<AxumJson<Vec<crate::addons::TrustedServiceStage>>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    crate::addons::list_trusted_service_stages()
        .map(AxumJson)
        .map_err(addon_error_response)
}

async fn list_trusted_service_runtime(
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
) -> Result<AxumJson<Vec<crate::trusted_service_runtime::TrustedServiceRuntimeRecord>>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    crate::trusted_service_runtime::list()
        .map(AxumJson)
        .map_err(trusted_service_runtime_error_response)
}

async fn approve_trusted_service(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<AxumJson<crate::trusted_service_runtime::TrustedServiceRuntimeRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let record = crate::trusted_service_runtime::approve(&id, &principal.username)
        .map_err(trusted_service_runtime_error_response)?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "trusted_service_approval",
        &id,
        "success",
        None,
    )
    .await
    .map_err(|error| {
        log::error!("trusted service audit persistence failed: {error}");
        internal_server_error()
    })?;
    Ok(AxumJson(record))
}

async fn disable_trusted_service(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<AxumJson<crate::trusted_service_runtime::TrustedServiceRuntimeRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let record = crate::trusted_service_runtime::disable(&id)
        .map_err(trusted_service_runtime_error_response)?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "trusted_service_disable",
        &id,
        "success",
        None,
    )
    .await
    .map_err(|error| {
        log::error!("trusted service audit persistence failed: {error}");
        internal_server_error()
    })?;
    Ok(AxumJson(record))
}

async fn remove_trusted_service_stage(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    crate::trusted_service_runtime::remove(&id)
        .map_err(trusted_service_runtime_error_response)?;
    crate::addons::remove_trusted_service_stage(&id).map_err(addon_error_response)?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "trusted_service_stage_remove",
        &id,
        "success",
        None,
    )
    .await
    .map_err(|error| {
        log::error!("trusted service audit persistence failed: {error}");
        internal_server_error()
    })?;
    Ok(StatusCode::NO_CONTENT)
}

fn reject_symlinked_trusted_service_upload_ancestors(path: &StdPath) -> std::io::Result<()> {
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "upload path contains a symlinked ancestor",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let Some(parent) = current.parent() else {
            return Ok(());
        };
        current = parent;
    }
}

fn create_private_trusted_service_upload_dir(path: &StdPath) -> std::io::Result<()> {
    reject_symlinked_trusted_service_upload_ancestors(path)?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "upload parent is missing")
    })?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "upload parent is not a real directory",
        ));
    }
    let mut directory_builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory_builder.mode(0o700);
    }
    directory_builder.create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        let _ = fs::remove_dir(path);
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "upload staging path is not a real directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        if let Err(error) = fs::set_permissions(path, permissions) {
            let _ = fs::remove_dir(path);
            return Err(error);
        }
    }
    Ok(())
}

fn create_private_trusted_service_upload_file(path: &StdPath) -> std::io::Result<fs::File> {
    reject_symlinked_trusted_service_upload_ancestors(path)?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "upload parent is missing")
    })?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "upload parent is not a real directory",
        ));
    }
    let mut file_options = fs::OpenOptions::new();
    file_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file_options.mode(0o600);
    }
    let file = file_options.open(path)?;
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            let _ = fs::remove_file(path);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "upload target is not a regular file",
            ));
        }
    }
    Ok(file)
}

async fn stage_trusted_service_manifest(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    mut multipart: Multipart,
) -> Result<(StatusCode, AxumJson<crate::addons::TrustedServiceStage>), (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let temp_dir = std::env::temp_dir().join(format!("screentra-trusted-service-{}", Uuid::new_v4()));
    create_private_trusted_service_upload_dir(&temp_dir)
        .map_err(|_| internal_server_error())?;
    let manifest_path = temp_dir.join("manifest.json");
    let signature_path = temp_dir.join("manifest.sig");
    let image_signature_path = temp_dir.join("image.sig");
    let mut seen_manifest = false;
    let mut seen_signature = false;
    let mut seen_image_signature = false;
    let mut result = Ok(());
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "Unable to read trusted service upload".to_string()))?
    {
        let Some(name) = field.name() else {
            result = Err((StatusCode::BAD_REQUEST, "Trusted service upload fields must be named".to_string()));
            break;
        };
        let (path, seen, limit) = match name {
            "manifest" => (&manifest_path, &mut seen_manifest, 1024 * 1024),
            "signature" => (&signature_path, &mut seen_signature, 64 * 1024),
            "image_signature" => (&image_signature_path, &mut seen_image_signature, 64 * 1024),
            _ => {
                result = Err((StatusCode::BAD_REQUEST, "Unknown trusted service upload field".to_string()));
                break;
            }
        };
        if *seen {
            result = Err((StatusCode::BAD_REQUEST, "Trusted service upload field repeated".to_string()));
            break;
        }
        *seen = true;
        let mut file = create_private_trusted_service_upload_file(path)
            .map_err(|_| internal_server_error())?;
        let mut total = 0usize;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "Unable to read trusted service upload".to_string()))?
        {
            total = total.saturating_add(chunk.len());
            if total > limit {
                result = Err((StatusCode::BAD_REQUEST, "Trusted service upload field is too large".to_string()));
                break;
            }
            file.write_all(&chunk).map_err(|_| internal_server_error())?;
        }
        if result.is_err() {
            break;
        }
        file.flush().map_err(|_| internal_server_error())?;
    }
    if result.is_ok() && (!seen_manifest || !seen_signature || !seen_image_signature) {
        result = Err((
            StatusCode::BAD_REQUEST,
            "Trusted service upload requires manifest, signature, and image_signature fields".to_string(),
        ));
    }
    let staged = if result.is_ok() {
        crate::addons::stage_trusted_service_manifest(
            &manifest_path,
            &signature_path,
            &image_signature_path,
        )
            .map_err(addon_error_response)
            .map(Some)
    } else {
        Ok(None)
    };
    let _ = fs::remove_dir_all(&temp_dir);
    result?;
    let stage = staged?.ok_or_else(internal_server_error)?;
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "trusted_service_stage_create",
        &stage.manifest.id,
        "success",
        None,
    )
    .await
    .map_err(|error| {
        log::error!("trusted service audit persistence failed: {error}");
        internal_server_error()
    })?;
    Ok((StatusCode::CREATED, AxumJson(stage)))
}

async fn upload_addon_package(
    multipart: Multipart,
) -> Result<(StatusCode, AxumJson<AddonRecord>), (StatusCode, String)> {
    upload_addon_package_with_mode(multipart, false).await
}

async fn update_addon_package(
    multipart: Multipart,
) -> Result<(StatusCode, AxumJson<AddonRecord>), (StatusCode, String)> {
    upload_addon_package_with_mode(multipart, true).await
}

async fn rollback_addon_handler(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<AxumJson<AddonRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let record = match crate::addons::rollback_addon(&id) {
        Ok(record) => record,
        Err(error) => {
            if let Err(audit_error) = audit_trusted_service_stage_event(
                &state,
                &principal,
                "addon_rollback",
                &id,
                "failure",
                Some("rollback rejected"),
            )
            .await
            {
                log::error!("addon rollback failure audit persistence failed: {audit_error}");
            }
            return Err(addon_error_response(error));
        }
    };
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "addon_rollback",
        &id,
        "success",
        None,
    )
    .await
    .map_err(|error| {
        log::error!("addon rollback audit persistence failed: {error}");
        internal_server_error()
    })?;
    Ok(AxumJson(record))
}

async fn rollback_signed_client_package_handler(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<AxumJson<AddonRecord>, (StatusCode, String)> {
    require_addon_storage_admin(&principal)?;
    let record = match crate::addons::rollback_signed_client_package(&id) {
        Ok(record) => record,
        Err(error) => {
            if let Err(audit_error) = audit_trusted_service_stage_event(
                &state,
                &principal,
                "signed_client_package_rollback",
                &id,
                "failure",
                Some("rollback rejected"),
            )
            .await
            {
                log::error!("signed client package rollback failure audit persistence failed: {audit_error}");
            }
            return Err(addon_error_response(error));
        }
    };
    audit_trusted_service_stage_event(
        &state,
        &principal,
        "signed_client_package_rollback",
        &id,
        "success",
        None,
    )
    .await
    .map_err(|error| {
        log::error!("signed client package rollback audit persistence failed: {error}");
        internal_server_error()
    })?;
    Ok(AxumJson(record))
}

async fn upload_addon_package_with_mode(
    mut multipart: Multipart,
    update: bool,
) -> Result<(StatusCode, AxumJson<AddonRecord>), (StatusCode, String)> {
    const MAX_ADDON_PACKAGE_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
    let package_path = std::env::temp_dir().join(format!("screentra-addon-{}.tar.gz", Uuid::new_v4()));
    let mut received = false;
    let mut file = fs::File::create(&package_path).map_err(|_| internal_server_error())?;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "Unable to read addon package upload".to_string()))?
    {
        if received {
            let _ = fs::remove_file(&package_path);
            return Err((
                StatusCode::BAD_REQUEST,
                "Addon package upload must contain exactly one file".to_string(),
            ));
        }
        received = true;
        let mut field = field;
        let mut total = 0usize;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "Unable to read addon package upload".to_string()))?
        {
            if total.saturating_add(chunk.len()) > MAX_ADDON_PACKAGE_UPLOAD_BYTES {
                let _ = fs::remove_file(&package_path);
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Addon package upload is too large".to_string(),
                ));
            }
            total = total.saturating_add(chunk.len());
            file.write_all(&chunk).map_err(|_| internal_server_error())?;
        }
    }
    file.flush().map_err(|_| internal_server_error())?;
    drop(file);
    if !received {
        let _ = fs::remove_file(&package_path);
        return Err((
            StatusCode::BAD_REQUEST,
            "Addon package upload is empty".to_string(),
        ));
    }
    let result = if update {
        crate::addons::update_signed_client_package(&package_path)
    } else {
        crate::addons::install_signed_client_package(&package_path)
    };
    let _ = fs::remove_file(&package_path);
    result
        .map(|record| {
            let status = if update {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (status, AxumJson(record))
        })
        .map_err(addon_error_response)
}

async fn install_addon(
    Json(manifest): Json<AddonManifest>,
) -> Result<(StatusCode, AxumJson<AddonRecord>), (StatusCode, String)> {
    crate::addons::install(manifest)
        .map(|record| (StatusCode::CREATED, AxumJson(record)))
        .map_err(addon_error_response)
}

async fn update_addon(
    Path(id): Path<String>,
    Json(manifest): Json<AddonManifest>,
) -> Result<AxumJson<AddonRecord>, (StatusCode, String)> {
    if id != manifest.id {
        return Err((
            StatusCode::BAD_REQUEST,
            "Addon path ID must match manifest ID".to_string(),
        ));
    }
    crate::addons::update(manifest)
        .map(AxumJson)
        .map_err(addon_error_response)
}

async fn enable_addon(
    Path(id): Path<String>,
) -> Result<AxumJson<AddonRecord>, (StatusCode, String)> {
    crate::addons::set_enabled(&id, true)
        .map(AxumJson)
        .map_err(addon_error_response)
}

async fn disable_addon(
    Path(id): Path<String>,
) -> Result<AxumJson<AddonRecord>, (StatusCode, String)> {
    crate::addons::set_enabled(&id, false)
        .map(AxumJson)
        .map_err(addon_error_response)
}

async fn delete_addon(
    State(storage): State<AddonStorageRepository>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let deleted_storage = storage.delete_namespace(&id).await.map_err(|error| {
        log::error!("addon storage cleanup failed before addon removal: {error}");
        internal_server_error()
    })?;
    crate::trusted_service_runtime::remove(&id).map_err(|error| {
        log::error!("trusted service runtime cleanup failed before addon removal: {error}");
        internal_server_error()
    })?;
    crate::addons::remove_trusted_service_stage(&id).map_err(|error| {
        log::error!("trusted service staging cleanup failed before addon removal: {error}");
        internal_server_error()
    })?;
    crate::addons::remove(&id).map_err(addon_error_response)?;
    log::info!("removed addon id={} storage_rows={deleted_storage}", id);
    Ok(StatusCode::NO_CONTENT)
}

async fn system_status() -> AxumJson<SystemStatus> {
    AxumJson(get_system_status())
}

// ── Refresh handlers ────────────────────────────────────────

async fn get_refresh(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let epoch = repository
        .get_refresh_epoch()
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({"epoch": epoch})))
}

async fn bump_refresh(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let epoch = repository
        .bump_refresh_epoch()
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({"epoch": epoch})))
}

// ── Clear Chrome Cache + Restart Browser ─────────────────────

async fn clear_cache() -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    use tokio::process::Command;

    // Kill Chrome processes
    let mut pkill = Command::new("pkill");
    pkill.args(["-9", "chrome"]);
    let _ = bounded_async_status(pkill, Duration::from_secs(30)).await;

    // Clear cache directories
    let cache_dirs = ["/tmp/chrome-cache-1", "/tmp/chrome-cache-2"];
    for dir in &cache_dirs {
        let _ = std::fs::remove_dir_all(dir);
    }

    // Clear Chrome Default Cache directories
    let _ = std::fs::remove_dir_all("/home/thor/.config/google-chrome-1/Default/Cache");
    let _ = std::fs::remove_dir_all("/home/thor/.config/google-chrome-2/Default/Cache");

    // Restart the OS-level Chromium kiosk-mode service
    let mut restart = Command::new("sudo");
    restart.args(["systemctl", "restart", "signage-kiosk.service"]); // OS-level Chromium kiosk mode — see signage-kiosk.service
    let _ = bounded_async_status(restart, Duration::from_secs(30)).await;

    Ok(AxumJson(
        serde_json::json!({"ok": true, "message": "Cache cleared and browser restarted"}),
    ))
}

// ── Player Log ──────────────────────────────────────────────

async fn player_log(AxumJson(body): AxumJson<serde_json::Value>) -> AxumJson<serde_json::Value> {
    let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("");
    log::info!("[PLAYER] {}", msg);
    AxumJson(serde_json::json!({"ok":true}))
}

// ── ICS Proxy ───────────────────────────────────────────────

fn disallowed_proxy_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback() || v4.is_private() || v4.is_link_local()
                || v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast()
                || o[0] == 0 || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 192 && o[1] == 0 && o[2] == 2)
                || (o[0] == 198 && o[1] == 51 && o[2] == 100)
                || (o[0] == 203 && o[1] == 0 && o[2] == 113)
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) || o[0] >= 240
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4() {
                return disallowed_proxy_ip(std::net::IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

fn validate_proxy_url(url: &str) -> Result<(String, u16, std::net::IpAddr), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid url: {}", e))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Disallowed url scheme: {}", other)),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "url has no host".to_string())?
        .to_string();
    let host_lower = host.to_lowercase();
    if host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".local")
    {
        return Err("Disallowed url host".to_string());
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::IpAddr> =
        std::net::ToSocketAddrs::to_socket_addrs(&(host_lower.as_str(), port))
            .map_err(|e| format!("dns resolve failed: {}", e))?
            .map(|sa| sa.ip())
            .collect();
    if addrs.is_empty() {
        return Err("dns resolve returned no addresses".to_string());
    }
    for ip in &addrs {
        if disallowed_proxy_ip(*ip) {
            return Err("Disallowed url host".to_string());
        }
    }
    // Pin the request to the address validated above. This prevents a DNS
    // rebinding between validation and reqwest's later resolver lookup.
    Ok((host_lower, port, addrs[0]))
}

#[derive(Serialize)]
struct WeatherLocationLookupResponse {
    results: Vec<serde_json::Value>,
}

async fn weather_location_lookup(
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Result<AxumJson<WeatherLocationLookupResponse>, (StatusCode, String)> {
    let query = params
        .get("q")
        .or_else(|| params.get("name"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or((StatusCode::BAD_REQUEST, "Missing q parameter".to_string()))?;

    if query.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Location query is too long".to_string(),
        ));
    }

    let count = params
        .get("count")
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(5)
        .clamp(1, 10);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(4))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("RipleySignage/1.0")
        .build()
        .map_err(|_| internal_server_error())?;

    let mut candidates = vec![query.to_string()];
    if !query.contains(',') {
        let parts: Vec<&str> = query.split_whitespace().collect();
        if parts.len() > 1 {
            let place = parts[..parts.len() - 1].join(" ");
            let region = parts[parts.len() - 1];
            if !place.is_empty() && !region.is_empty() {
                candidates.push(format!("{}, {}", place, region));
            }
        }
    }

    let mut last_error: Option<String> = None;
    for candidate in candidates {
        let count_str = count.to_string();
        let res = client
            .get("https://nominatim.openstreetmap.org/search")
            .query(&[
                ("q", candidate.as_str()),
                ("limit", count_str.as_str()),
                ("format", "jsonv2"),
                ("addressdetails", "1"),
            ])
            .send()
            .await
            .map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Location lookup failed: {}", e),
                )
            })?;

        let status = res.status();
        if !status.is_success() {
            last_error = Some(format!("Location lookup returned {}", status));
            continue;
        }

        let body: serde_json::Value = read_response_json_limited(
            res,
            1024 * 1024,
            "Invalid lookup response",
        )
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        let results = body
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|item| {
                let address = item.get("address").cloned().unwrap_or_default();
                serde_json::json!({
                    "name": item.get("name").or_else(|| item.get("display_name")).and_then(|v| v.as_str()).unwrap_or(candidate.as_str()),
                    "latitude": item.get("lat").and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok()),
                    "longitude": item.get("lon").and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok()),
                    "country": address.get("country").and_then(|v| v.as_str()).unwrap_or(""),
                    "admin1": address.get("state").and_then(|v| v.as_str()).unwrap_or(""),
                    "timezone": ""
                })
            })
            .collect::<Vec<_>>();
        if !results.is_empty() {
            return Ok(AxumJson(WeatherLocationLookupResponse { results }));
        }
    }

    if let Some(err) = last_error {
        return Err((StatusCode::BAD_GATEWAY, err));
    }

    Ok(AxumJson(WeatherLocationLookupResponse { results: vec![] }))
}

fn weather_code_from_nws(text: &str) -> i32 {
    // NWS uses phrases such as "Patchy Fog then Partly Sunny" for a
    // daytime period.  The old first-match parser reported fog for the
    // entire day.  For a daily icon, the final/prevailing clause is the
    // useful one; hourly data below provides the more exact day/night code.
    let t = text.to_ascii_lowercase();
    let prevailing = t.rsplit(" then ").next().unwrap_or(&t).trim();
    if prevailing.contains("thunder") { return 95; }
    if prevailing.contains("snow") || prevailing.contains("flurr") { return 71; }
    if prevailing.contains("sleet") || prevailing.contains("freezing") { return 66; }
    if prevailing.contains("drizzle") { return 53; }
    if prevailing.contains("rain") || prevailing.contains("shower") { return 63; }
    if prevailing.contains("fog") { return 45; }
    if prevailing.contains("partly") { return 2; }
    if prevailing.contains("mostly sunny") { return 1; }
    if prevailing.contains("sunny") || prevailing.contains("clear") { return 0; }
    if prevailing.contains("cloud") || prevailing.contains("overcast") { return 3; }
    0
}

fn nws_wind_speed(text: &str) -> f64 {
    text.split(|c: char| !c.is_ascii_digit() && c != '.')
        .filter_map(|part| part.parse::<f64>().ok())
        .fold(0.0, f64::max)
}

fn nws_period_date(period: &serde_json::Value) -> Option<&str> {
    period.get("startTime").and_then(|v| v.as_str()).and_then(|s| s.get(0..10))
}

fn nws_daily_precipitation_probability(
    forecast_periods: &[serde_json::Value],
    hourly_periods: &[serde_json::Value],
    date: &str,
) -> f64 {
    forecast_periods
        .iter()
        .chain(hourly_periods.iter())
        .filter(|period| nws_period_date(period) == Some(date))
        .filter_map(|period| period.pointer("/probabilityOfPrecipitation/value").and_then(|v| v.as_f64()))
        .fold(0.0, f64::max)
}

fn nws_hourly_min_temperature_for_date(hourly_periods: &[serde_json::Value], date: &str) -> Option<f64> {
    hourly_periods
        .iter()
        .filter(|period| nws_period_date(period) == Some(date))
        .filter_map(|period| period.get("temperature").and_then(|v| v.as_f64()))
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn nws_alerts_for_date(features: &[serde_json::Value], date: &str) -> Vec<String> {
    let mut alerts = Vec::new();
    for feature in features {
        let Some(properties) = feature.get("properties") else { continue };
        let Some(event) = properties.get("event").and_then(|v| v.as_str()).filter(|event| !event.trim().is_empty()) else { continue };
        let start_date = properties.get("onset").or_else(|| properties.get("effective"))
            .and_then(|v| v.as_str()).and_then(|value| value.get(0..10));
        let end_date = properties.get("ends").or_else(|| properties.get("expires"))
            .and_then(|v| v.as_str()).and_then(|value| value.get(0..10));
        let overlaps = match (start_date, end_date) {
            (Some(start), Some(end)) => start <= date && date <= end,
            (Some(start), None) => start == date,
            (None, Some(end)) => date <= end,
            (None, None) => false,
        };
        if overlaps && !alerts.iter().any(|existing| existing == event) {
            alerts.push(event.to_string());
        }
    }
    alerts
}

fn nws_period_hour(period: &serde_json::Value) -> Option<i32> {
    period.get("startTime")
        .and_then(|v| v.as_str())
        .and_then(|s| s.get(11..13))
        .and_then(|h| h.parse::<i32>().ok())
}

fn nws_morning_transition_for_date(
    periods: &[serde_json::Value],
    date: &str,
    day_code: i32,
) -> Option<(i32, String)> {
    let morning: Vec<&serde_json::Value> = periods.iter()
        .filter(|p| p.get("isDaytime").and_then(|v| v.as_bool()) == Some(true))
        .filter(|p| nws_period_date(p) == Some(date))
        .filter(|p| nws_period_hour(p).map(|hour| (5..12).contains(&hour)).unwrap_or(false))
        .collect();
    let first = morning.first().copied()?;
    let morning_code = weather_code_from_nws(first.get("shortForecast").and_then(|v| v.as_str()).unwrap_or(""));
    if morning_code == day_code {
        return None;
    }
    let until = morning.iter().skip(1)
        .find(|p| weather_code_from_nws(p.get("shortForecast").and_then(|v| v.as_str()).unwrap_or("")) != morning_code)
        .and_then(|p| p.get("startTime").and_then(|v| v.as_str()))?;
    Some((morning_code, until.to_string()))
}

fn nws_hourly_code_for_date(
    periods: &[serde_json::Value],
    date: &str,
    daytime: bool,
    target_hour: i32,
) -> Option<i32> {
    periods.iter()
        .filter(|p| p.get("isDaytime").and_then(|v| v.as_bool()) == Some(daytime))
        .filter(|p| nws_period_date(p) == Some(date))
        .filter_map(|p| Some((p, nws_period_hour(p)?)))
        .min_by_key(|(_, hour)| (hour - target_hour).abs())
        .map(|(p, _)| weather_code_from_nws(p.get("shortForecast").and_then(|v| v.as_str()).unwrap_or("")))
}

const WEATHER_CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone)]
struct WeatherCacheEntry {
    value: serde_json::Value,
    stored_at: Instant,
}

static WEATHER_CACHE: OnceLock<Mutex<HashMap<String, WeatherCacheEntry>>> = OnceLock::new();

fn weather_cache() -> &'static Mutex<HashMap<String, WeatherCacheEntry>> {
    WEATHER_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn weather_cache_key(lat: f64, lon: f64) -> String {
    format!("{lat:.5},{lon:.5}")
}

fn mark_weather_stale(mut value: serde_json::Value, reason: &str) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        if let Some(meta) = object.get_mut("weather_meta").and_then(|v| v.as_object_mut()) {
            meta.insert("stale".to_string(), serde_json::Value::Bool(true));
            meta.insert("stale_reason".to_string(), serde_json::Value::String(reason.to_string()));
        }
    }
    value
}

fn cache_weather_value(key: &str, value: serde_json::Value) {
    if let Ok(mut cache) = weather_cache().lock() {
        cache.insert(key.to_string(), WeatherCacheEntry { value, stored_at: Instant::now() });
        if cache.len() > 256 {
            cache.retain(|_, entry| entry.stored_at.elapsed() < WEATHER_CACHE_TTL * 12);
        }
    }
}

fn cached_weather_value(key: &str) -> Option<WeatherCacheEntry> {
    weather_cache().lock().ok()?.get(key).cloned()
}

fn weather_response_with_meta(mut value: serde_json::Value, fetched_at: &str) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        object.insert("weather_meta".to_string(), serde_json::json!({
            "fetched_at": fetched_at,
            "stale": false
        }));
    }
    value
}

fn curl_json(url: &str) -> Option<serde_json::Value> {
    const MAX_WEATHER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
    let mut command = Command::new("curl");
    command.args([
        "-fsSL",
        "--max-time",
        "12",
        "-A",
        "Screentra Cast/1.0 weather contact: admin@screentra-cast.local",
        url,
    ]);
    let (status, bytes) = bounded_command_output(
        command,
        MAX_WEATHER_RESPONSE_BYTES,
        Duration::from_secs(15),
    )
    .ok()?;
    if !status.success() {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

async fn weather_forecast(
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let lat = params.get("lat").and_then(|v| v.parse::<f64>().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing or invalid lat".to_string()))?;
    let lon = params.get("lon").and_then(|v| v.parse::<f64>().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing or invalid lon".to_string()))?;
    if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err((StatusCode::BAD_REQUEST, "Coordinates out of range".to_string()));
    }
    let cache_key = weather_cache_key(lat, lon);
    let cached = cached_weather_value(&cache_key);
    if let Some(entry) = cached.as_ref().filter(|entry| entry.stored_at.elapsed() < WEATHER_CACHE_TTL) {
        return Ok(AxumJson(entry.value.clone()));
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(5)).local_address(Some("0.0.0.0".parse().unwrap())).redirect(reqwest::redirect::Policy::none())
        .user_agent("Screentra Cast/1.0 weather contact: admin@screentra-cast.local").build()
        .map_err(|_| internal_server_error())?;

    // NWS is authoritative for U.S. points. The points response supplies the
    // correct forecast office/grid URL; never construct that URL ourselves.
    let points_url = format!("https://api.weather.gov/points/{:.5},{:.5}", lat, lon);
    if let Some(points) = curl_json(&points_url) {
        if let Some(url) = points.pointer("/properties/forecast").and_then(|v| v.as_str()) {
            if let Some(body) = curl_json(url) {
                let periods = body.pointer("/properties/periods").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let hourly_periods = points.pointer("/properties/forecastHourly")
                    .and_then(|v| v.as_str())
                    .and_then(curl_json)
                    .and_then(|v| v.pointer("/properties/periods").and_then(|p| p.as_array().cloned()))
                    .unwrap_or_default();
                let active_alerts = curl_json(&format!("https://api.weather.gov/alerts/active?point={:.5},{:.5}", lat, lon))
                    .and_then(|v| v.get("features").and_then(|features| features.as_array().cloned()))
                    .unwrap_or_default();
                let mut dates = Vec::new();
                let mut highs = Vec::new();
                let mut lows = Vec::new();
                let mut rain = Vec::new();
                let mut codes = Vec::new();
                let mut day_codes = Vec::new();
                let mut night_codes = Vec::new();
                let mut morning_codes: Vec<Option<i32>> = Vec::new();
                let mut morning_untils: Vec<Option<String>> = Vec::new();
                let mut winds = Vec::new();
                let mut alerts: Vec<Vec<String>> = Vec::new();
                for p in periods.iter().filter(|p| p.get("isDaytime").and_then(|v| v.as_bool()).unwrap_or(false)).take(7) {
                    let date = p.get("startTime").and_then(|v| v.as_str()).and_then(|s| s.get(0..10)).unwrap_or("").to_string();
                    dates.push(date.clone());
                    highs.push(p.get("temperature").and_then(|v| v.as_f64()).unwrap_or(0.0));
                    let summary_code = weather_code_from_nws(p.get("shortForecast").and_then(|v| v.as_str()).unwrap_or(""));
                    // Prefer the hourly condition nearest midday. This prevents
                    // a short morning-only condition such as "Patchy Fog"
                    // from becoming the icon for the whole daytime forecast.
                    let day_code = nws_hourly_code_for_date(&hourly_periods, &date, true, 12).unwrap_or(summary_code);
                    let morning = nws_morning_transition_for_date(&hourly_periods, &date, day_code);
                    let night_summary = periods.iter()
                        .find(|n| !n.get("isDaytime").and_then(|v| v.as_bool()).unwrap_or(true) && nws_period_date(n) == Some(date.as_str()))
                        .map(|n| weather_code_from_nws(n.get("shortForecast").and_then(|v| v.as_str()).unwrap_or("")))
                        .unwrap_or(summary_code);
                    let night_code = nws_hourly_code_for_date(&hourly_periods, &date, false, 22).unwrap_or(night_summary);
                    codes.push(day_code);
                    day_codes.push(day_code);
                    night_codes.push(night_code);
                    morning_codes.push(morning.as_ref().map(|(code, _)| *code));
                    morning_untils.push(morning.map(|(_, until)| until));
                    rain.push(nws_daily_precipitation_probability(&periods, &hourly_periods, &date));
                    winds.push(nws_wind_speed(p.get("windSpeed").and_then(|v| v.as_str()).unwrap_or("")));
                    alerts.push(nws_alerts_for_date(&active_alerts, &date));
                    let low = periods.iter()
                        .find(|n| !n.get("isDaytime").and_then(|v| v.as_bool()).unwrap_or(true) && nws_period_date(n) == Some(date.as_str()))
                        .and_then(|n| n.get("temperature"))
                        .and_then(|v| v.as_f64())
                        .or_else(|| nws_hourly_min_temperature_for_date(&hourly_periods, &date))
                        .unwrap_or_else(|| highs.last().copied().unwrap_or(0.0));
                    lows.push(low);
                }
                if !dates.is_empty() {
                    let now = Utc::now();
                    // The NWS 12-hour forecast period temperature is the
                    // period's high/low, not the temperature right now. Use
                    // the hourly forecast for the live current reading and
                    // only fall back to the broader period if hourly data is
                    // unavailable.
                    let current = hourly_periods.iter().find(|p| {
                        let start = p.get("startTime").and_then(|v| v.as_str()).and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
                        let end = p.get("endTime").and_then(|v| v.as_str()).and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
                        match (start, end) {
                            (Some(start), Some(end)) => now >= start.with_timezone(&Utc) && now < end.with_timezone(&Utc),
                            _ => false,
                        }
                    }).or_else(|| periods.iter().find(|p| {
                        let start = p.get("startTime").and_then(|v| v.as_str()).and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
                        let end = p.get("endTime").and_then(|v| v.as_str()).and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
                        match (start, end) {
                            (Some(start), Some(end)) => now >= start.with_timezone(&Utc) && now < end.with_timezone(&Utc),
                            _ => false,
                        }
                    })).or_else(|| periods.iter().find(|p| p.get("isDaytime").and_then(|v| v.as_bool()) == Some(true)));
                    let current_temp = current.and_then(|p| p.get("temperature")).and_then(|v| v.as_f64()).unwrap_or(highs[0]);
                    let current_code = current.map(|p| weather_code_from_nws(p.get("shortForecast").and_then(|v| v.as_str()).unwrap_or(""))).unwrap_or(codes[0]);
                    let current_wind = current.map(|p| nws_wind_speed(p.get("windSpeed").and_then(|v| v.as_str()).unwrap_or(""))).unwrap_or(winds[0]);
                    let current_is_daytime = current.and_then(|p| p.get("isDaytime")).and_then(|v| v.as_bool()).unwrap_or(true);
                    let fetched_at = Utc::now().to_rfc3339();
                    let response = weather_response_with_meta(serde_json::json!({
                        "source":"noaa_nws",
                        "timezone": points.pointer("/properties/timeZone").and_then(|v| v.as_str()).unwrap_or("America/New_York"),
                        "daily": {
                            "time": dates,
                            "temperature_2m_max": highs,
                            "temperature_2m_min": lows,
                            "precipitation_probability_max": rain,
                            "weather_code": codes,
                            "weather_code_day": day_codes,
                            "weather_code_night": night_codes,
                            "weather_code_morning": morning_codes,
                            "weather_morning_until": morning_untils,
                            "wind_speed": winds,
                            "weather_alerts": alerts
                        },
                        "current_weather": {
                            "temperature": current_temp,
                            "weathercode": current_code,
                            "windspeed": current_wind,
                            "is_daytime": current_is_daytime
                        }
                    }), &fetched_at);
                    cache_weather_value(&cache_key, response.clone());
                    return Ok(AxumJson(response));
                }
            }
        }
    }

    if let Some(entry) = cached {
        return Ok(AxumJson(mark_weather_stale(entry.value, "NOAA/NWS provider unavailable; showing cached weather")));
    }
    Err((StatusCode::BAD_GATEWAY, "NOAA/NWS weather provider unavailable".to_string()))
}


async fn traffic_map_image(
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let lat = params
        .get("lat")
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing or invalid lat".to_string()))?;
    let lon = params
        .get("lon")
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing or invalid lon".to_string()))?;
    if !lat.is_finite()
        || !lon.is_finite()
        || !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon)
    {
        return Err((StatusCode::BAD_REQUEST, "Coordinates out of range".to_string()));
    }
    let api_key = std::env::var("GOOGLE_MAPS_STATIC_API_KEY").unwrap_or_default();
    if api_key.trim().is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Google Static Maps is not configured; set server-side GOOGLE_MAPS_STATIC_API_KEY"
                .to_string(),
        ));
    }

    let category = params
        .get("category")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| "other".to_string());
    let (marker_color, marker_label) = match category.as_str() {
        "crash" => ("red", "C"),
        "closure" => ("orange", "R"),
        "delay" => ("yellow", "D"),
        _ => ("blue", "T"),
    };
    let coordinate = format!("{lat:.6},{lon:.6}");
    let marker = format!("color:{marker_color}|label:{marker_label}|{coordinate}");
    let mut map_url = url::Url::parse("https://maps.googleapis.com/maps/api/staticmap")
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    {
        let mut query = map_url.query_pairs_mut();
        query
            .append_pair("center", &coordinate)
            .append_pair("zoom", "14")
            .append_pair("size", "640x360")
            .append_pair("scale", "2")
            .append_pair("maptype", "roadmap")
            .append_pair("markers", &marker)
            .append_pair("key", api_key.trim());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Screentra Cast/1.0 traffic map snippet")
        .build()
        .map_err(|_| internal_server_error())?;
    let upstream = client
        .get(map_url)
        .send()
        .await
        .map_err(|_| (StatusCode::BAD_GATEWAY, "Traffic map provider unavailable".to_string()))?;
    if !upstream.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            "Traffic map provider unavailable".to_string(),
        ));
    }
    let content_type = upstream
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.starts_with("image/") {
        return Err((
            StatusCode::BAD_GATEWAY,
            "Google Static Maps returned a non-image response".to_string(),
        ));
    }
    if upstream
        .content_length()
        .is_some_and(|length| length > 8 * 1024 * 1024)
    {
        return Err((
            StatusCode::BAD_GATEWAY,
            "Traffic map image exceeded the server response limit".to_string(),
        ));
    }
    let mut stream = upstream.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            (StatusCode::BAD_GATEWAY, "Traffic map image read failed".to_string())
        })?;
        if body.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
            return Err((
                StatusCode::BAD_GATEWAY,
                "Traffic map image exceeded the server response limit".to_string(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let content_type = axum::http::HeaderValue::from_str(&content_type).map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            "Traffic map provider returned invalid content".to_string(),
        )
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, content_type);
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    Ok((headers, body).into_response())
}

async fn traffic_events(
    Query(params): Query<HashMap<String, String>>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let lat = params.get("lat").and_then(|v| v.parse::<f64>().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing or invalid lat".to_string()))?;
    let lon = params.get("lon").and_then(|v| v.parse::<f64>().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing or invalid lon".to_string()))?;
    let radius_miles = params.get("radius_miles").and_then(|v| v.parse::<f64>().ok()).unwrap_or(25.0).clamp(1.0, 100.0);
    if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err((StatusCode::BAD_REQUEST, "Coordinates out of range".to_string()));
    }
    let query = TrafficQuery { lat, lon, radius_miles };
    let username = std::env::var("PENNDOT_RCRS_USERNAME").ok();
    let password = std::env::var("PENNDOT_RCRS_PASSWORD").ok();
    let new_york_api_key = std::env::var("TRAFFIC_511NY_API_KEY").ok();
    let provider = TrafficProvider::select(&query, username.as_deref(), password.as_deref());
    if provider == TrafficProvider::NewYork511
        && new_york_api_key.as_deref().unwrap_or_default().trim().is_empty()
    {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": false,
            "available": false,
            "delayed_or_incomplete": true,
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": [],
            "message": "New York 511 provider requires server-side TRAFFIC_511NY_API_KEY"
        })));
    }
    if !provider.is_available() {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": false,
            "available": false,
            "delayed_or_incomplete": false,
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": [],
            "message": "No traffic provider is available for this location"
        })));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Screentra Cast/1.0 traffic widget")
        .build()
        .map_err(|_| internal_server_error())?;
    let events = provider
        .fetch(
            &client,
            &query,
            username.as_deref(),
            password.as_deref(),
            new_york_api_key.as_deref(),
        )
        .await
        .map_err(|error| {
            log::warn!("traffic provider {} failed: {}", provider.source(), error);
            (StatusCode::BAD_GATEWAY, "Traffic provider unavailable".to_string())
        })?;
    if provider == TrafficProvider::Public511Pa {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Public 511PA map feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::MarylandChart {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Maryland CHART public feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::NewYork511 {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "New York 511 feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::Manitoba511 {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Manitoba 511 public ArcGIS feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::Saskatchewan511 {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Saskatchewan 511 public feed; coverage may be incomplete or delayed"
        })));
    }
    if provider.is_ibi511() {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Public provincial 511 feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::TennesseeWazeSmartway {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Tennessee Smartway public ArcGIS feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::NorthwestTerritoriesDrivenwt {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "DriveNWT public feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::AlabamaAlgoTraffic
        || provider == TrafficProvider::ArkansasIdrive
        || provider == TrafficProvider::Nebraska511
        || provider == TrafficProvider::WestVirginia511 {
        let message = match provider {
            TrafficProvider::ArkansasIdrive =>
                "IDrive Arkansas public closure feed; coverage may be incomplete or delayed",
            TrafficProvider::Nebraska511 =>
                "Nebraska 511 public ArcGIS feed; coverage may be incomplete or delayed",
            TrafficProvider::WestVirginia511 =>
                "WV511 public KML feed; coverage may be incomplete or delayed",
            _ => "ALGO Traffic public feed; coverage may be incomplete or delayed",
        };
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": message
        })));
    }
    if provider == TrafficProvider::ConnecticutCtroads {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Connecticut CTroads public feed; coverage may be incomplete or delayed"
        })));
    }
    if matches!(
        provider,
        TrafficProvider::MontanaIteris511
            | TrafficProvider::SouthDakotaIteris511
            | TrafficProvider::SouthCarolinaIteris511
    ) {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Public state 511 GeoJSON feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::Wzdx {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "Public WZDx work-zone feed; coverage may be incomplete or delayed"
        })));
    }
    if provider == TrafficProvider::Open511BritishColumbia {
        return Ok(AxumJson(serde_json::json!({
            "source": provider.source(),
            "configured": true,
            "available": true,
            "delayed_or_incomplete": provider.delayed_or_incomplete(),
            "fetched_at": Utc::now().to_rfc3339(),
            "center": {"lat": query.lat, "lon": query.lon},
            "radius_miles": query.radius_miles,
            "events": events,
            "message": "BC Open511 feed; coverage may be incomplete or delayed"
        })));
    }
    Ok(AxumJson(serde_json::json!({
        "source": provider.source(),
        "configured": true,
        "fetched_at": Utc::now().to_rfc3339(),
        "center": {"lat": query.lat, "lon": query.lon},
        "radius_miles": query.radius_miles,
        "events": events
    })))
}

async fn ics_proxy(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<String, (StatusCode, String)> {
    let url = params
        .get("url")
        .ok_or((StatusCode::BAD_REQUEST, "Missing url parameter".to_string()))?;

    let (resolved_host, resolved_port, resolved_ip) = validate_proxy_url(url).map_err(|reason| {
        log::warn!("proxy URL rejected: {reason}");
        let status =
            if reason.starts_with("dns resolve") || reason == "dns resolve returned no addresses" {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::BAD_REQUEST
            };
        (status, "Proxy URL unavailable".to_string())
    })?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("DigitalSignage/1.0")
        .resolve(
            &resolved_host,
            std::net::SocketAddr::new(resolved_ip, resolved_port),
        )
        .build()
        .map_err(|_| internal_server_error())?;

    let res = client
        .get(url)
        .header("User-Agent", "DigitalSignage/1.0")
        .send()
        .await
        .map_err(|e| {
            log::warn!("proxy request failed: {e}");
            (StatusCode::BAD_GATEWAY, "Upstream unavailable".to_string())
        })?;

    let status = res.status();
    if !status.is_success() {
        log::warn!("proxy upstream returned status {status}");
        return Err((StatusCode::BAD_GATEWAY, "Upstream unavailable".to_string()));
    }

    // Stream the body so an upstream cannot force an unbounded allocation.
    const MAX_BYTES: usize = 4 * 1024 * 1024;
    if res.content_length().is_some_and(|n| n > MAX_BYTES as u64) {
        return Err((StatusCode::BAD_GATEWAY, "upstream too large".to_string()));
    }
    let mut stream = res.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| {
                log::warn!("proxy response read failed: {e}");
                (StatusCode::BAD_GATEWAY, "Upstream unavailable".to_string())
            })?;
        if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
            return Err((StatusCode::BAD_GATEWAY, "upstream too large".to_string()));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_| (StatusCode::BAD_GATEWAY, "upstream body was not valid UTF-8".to_string()))
}

// ---- News feed headlines ----
//
// Read the cached headlines for a feed URL. Used by the player and by
// /news-template.html when rendering a News widget that uses a
// template. The url is the feed URL; we look it up in the news_feeds
// cache and return whatever is currently there (could be empty if the
// maintenance loop has not fetched it yet).
//
// Always returns 200 with a JSON body. Missing/empty rows return
// {"headlines": [], "title": "", "last_fetched_at": ""} so the caller
// can render an "empty" state without special-casing 404s.
async fn get_news_headlines(
    State(repository): State<ClientRepository>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> AxumJson<serde_json::Value> {
    let url = params.get("url").cloned().unwrap_or_default();
    let empty = serde_json::json!({
        "url": url,
        "title": "",
        "headlines": [],
        "last_fetched_at": "",
        "last_error": null,
    });
    if url.is_empty() {
        return AxumJson(empty);
    }
    let row = match repository.get_news_feed(&url).await {
        Ok(Some(r)) => r,
        Ok(None) => return AxumJson(empty),
        Err(e) => {
            log::warn!("get_news_headlines({url}) failed: {e}");
            return AxumJson(empty);
        }
    };
    // headlines_json is always a JSON array (per the upsert path); if
    // it's empty or malformed, surface an empty array.
    let headlines: serde_json::Value = serde_json::from_str(&row.headlines_json)
        .unwrap_or_else(|_| serde_json::json!([]));
    AxumJson(serde_json::json!({
        "url": row.url,
        "title": row.title,
        "headlines": headlines,
        "last_fetched_at": row.last_fetched_at,
        "last_error": row.last_error,
    }))
}

// Admin-only manual refresh of all news feeds. Walks the news_feeds
// table and immediately re-fetches every entry, ignoring the 5-minute
// staleness window.
async fn post_news_refresh(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let feeds = repository
        .list_news_feeds()
        .await
        .map_err(|_| internal_server_error())?;
    if feeds.is_empty() {
        return Ok(AxumJson(serde_json::json!({
            "refreshed": 0,
            "errors": 0,
            "message": "no feeds registered",
        })));
    }
    let n = feeds.len();
    let fetches = tokio::task::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .user_agent("Screentra Cast/1.0 (news-ticker; +https://github.com/ripley-kiosk)")
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string());
        feeds
            .into_iter()
            .map(|feed| {
                let result = match &client {
                    Ok(client) => crate::news::fetch_one_update_with_client(client, &feed),
                    Err(error) => Err(error.clone()),
                };
                (feed, result)
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?;

    let mut refreshed = 0usize;
    let mut errors = 0usize;
    let mut error_details: Vec<serde_json::Value> = Vec::new();
    for (feed, result) in fetches {
        match result {
            Ok(update) => {
                let persist = if update.not_modified {
                    repository
                        .mark_news_feed_fetched(&feed.url, &chrono::Utc::now().to_rfc3339())
                        .await
                } else {
                    repository
                        .upsert_news_feed(
                            &feed.url,
                            &update.title,
                            &update.headlines_json,
                            update.new_etag.as_deref(),
                            update.new_last_modified.as_deref(),
                        )
                        .await
                };
                match persist {
                    Ok(()) => {
                        if !update.not_modified {
                            refreshed += 1;
                        }
                    }
                    Err(error) => {
                        errors += 1;
                        error_details.push(serde_json::json!({ "url": feed.url, "error": error }));
                    }
                }
            }
            Err(error) => {
                errors += 1;
                let _ = repository.record_news_feed_error(&feed.url, &error).await;
                error_details.push(serde_json::json!({ "url": feed.url, "error": error }));
            }
        }
    }
    Ok(AxumJson(serde_json::json!({
        "refreshed": refreshed,
        "errors": errors,
        "error_details": error_details,
        "feeds_total": n,
    })))
}

// ── Template handlers ───────────────────────────────────────

async fn list_templates(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<Vec<TemplateMeta>>, (StatusCode, String)> {
    repository.list_templates().await
        .map(AxumJson)
        .map_err(|_| internal_server_error())
}

async fn get_template(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let json: Option<String> = repository
        .get_template(&id)
        .await
        .map_err(|_| internal_server_error())?;
    match json.as_deref() {
        Some(j) => {
            let parsed: serde_json::Value =
                serde_json::from_str(j).unwrap_or(serde_json::Value::Null);
            Ok(AxumJson(parsed))
        }
        None => Err((StatusCode::NOT_FOUND, "Template not found".to_string())),
    }
}

async fn create_template(
    State(repository): State<ClientRepository>,
    AxumJson(body): AxumJson<serde_json::Value>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled")
        .to_string();
    let category = body
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("general")
        .to_string();
    let layout = body.get("layout").unwrap_or(&serde_json::Value::Null);
    let layout_json = serde_json::to_string(layout)
        .map_err(|_| internal_server_error())?;

    let final_id = if id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        id
    };
    let now = Utc::now().to_rfc3339();

    repository.save_template(&final_id, &name, &category, false, &layout_json, &now)
        .await
        .map_err(|_| internal_server_error())?;

    Ok(AxumJson(
        serde_json::json!({"id": final_id, "name": name, "category": category}),
    ))
}

async fn delete_template(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    repository.delete_template(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Display Override handlers ───────────────────────────────

async fn get_overrides(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let overrides = repository
        .get_display_overrides()
        .await
        .map_err(|_| internal_server_error())?;
    let map: serde_json::Map<String, serde_json::Value> = overrides
        .into_iter()
        .map(|(k, (w, h))| (k, serde_json::json!({"width": w, "height": h})))
        .collect();
    Ok(AxumJson(serde_json::Value::Object(map)))
}

async fn set_override(
    State(repository): State<ClientRepository>,
    AxumJson(body): AxumJson<serde_json::Value>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let display = body
        .get("display")
        .and_then(|v| v.as_str())
        .ok_or((StatusCode::BAD_REQUEST, "Missing display".to_string()))?;
    let width = body
        .get("width")
        .and_then(|v| v.as_u64())
        .ok_or((StatusCode::BAD_REQUEST, "Missing width".to_string()))? as u32;
    let height = body
        .get("height")
        .and_then(|v| v.as_u64())
        .ok_or((StatusCode::BAD_REQUEST, "Missing height".to_string()))? as u32;

    repository
        .set_display_override(display, width, height)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(
        serde_json::json!({"display": display, "width": width, "height": height}),
    ))
}

async fn clear_override(
    State(repository): State<ClientRepository>,
    Path(display): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    repository
        .clear_display_override(&display)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

// ── File Upload/List/Delete handlers ─────────────────────────

#[derive(Serialize)]
struct FileEntry {
    name: String,
    url: String,
    size: u64,
}

fn is_active_upload_name(name: &str) -> bool {
    name.rsplit_once('.')
        .map(|(_, extension)| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "html" | "htm" | "xhtml" | "js" | "mjs" | "cjs" | "css" | "svg" | "xml" | "wasm"
            )
        })
        .unwrap_or(false)
}

async fn serve_upload(request: axum::extract::Request) -> Response {
    let active = request
        .uri()
        .path()
        .rsplit('/')
        .next()
        .map(is_active_upload_name)
        .unwrap_or(false);
    let mut request = request;
    let path_and_query = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let stripped = path_and_query.strip_prefix("/uploads").unwrap_or("/");
    *request.uri_mut() = stripped.parse().unwrap_or_else(|_| axum::http::Uri::from_static("/"));
    let mut response = match tower_http::services::ServeDir::new("uploads")
        .oneshot(request)
        .await
    {
        Ok(response) => response.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // User uploads are public player assets. Prevent content sniffing for all
    // of them, and force potentially active formats to download instead of
    // being interpreted as same-origin documents/scripts.
    response.headers_mut().insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    if active {
        response.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/octet-stream"),
        );
        response.headers_mut().insert(
            "content-disposition",
            axum::http::HeaderValue::from_static("attachment"),
        );
    }
    response
}

fn uploads_dir(category: &str) -> Result<PathBuf, (StatusCode, String)> {
    // Allowlist categories and reject anything that could escape the uploads root.
    // Categories are part of the URL path and attacker-controlled without auth.
    const ALLOWED: &[&str] = &["images", "videos", "audio", "templates", "icons", "misc"];
    if !ALLOWED.contains(&category) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid category: {}", category),
        ));
    }
    if category.is_empty()
        || category == "."
        || category == ".."
        || category.contains('/')
        || category.contains('\\')
        || category.contains('\0')
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid category".to_string()));
    }
    let root = std::env::current_dir()
        .map_err(|_| internal_server_error())?
        .join("uploads");
    let dir = root.join(category);
    // Final canonical containment check.
    let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    if !canonical.starts_with(&canonical_root) {
        return Err((StatusCode::BAD_REQUEST, "Invalid category".to_string()));
    }
    Ok(dir)
}

fn safe_filename(name: &str) -> Result<String, (StatusCode, String)> {
    let last = name.rsplit('/').next().unwrap_or(name);
    let last = last.rsplit('\\').next().unwrap_or(last);
    if last.is_empty()
        || last == "."
        || last == ".."
        || last.contains('/')
        || last.contains('\\')
        || last.contains('\0')
        || last.chars().any(|c| c.is_control())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid filename: {}", name),
        ));
    }
    Ok(last.to_string())
}

fn sanitized_download_stem(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        }
    }
    let out = out.trim_matches(['.', '_', '-']).to_string();
    if out.is_empty() {
        "widget-preset".to_string()
    } else {
        out
    }
}

fn is_local_upload_url(url: &str) -> bool {
    let u = url.trim();
    if !(u.starts_with("/uploads/images/") || u.starts_with("/uploads/icons/")) {
        return false;
    }
    !u.contains("..") && !u.contains('\0') && !u.contains('\\')
}

/// Widget types that are simple enough to keep hardcoded defaults only.
/// Widget templates (presets) are not allowed for these because users
/// would just be re-creating the same hardcoded defaults the system
/// already applies when the widget is added to a layout.
// Widget types that are simple enough to keep hardcoded defaults only.
// Widget templates (presets) are not allowed for these because users
// would just be re-creating the same hardcoded defaults the system
// already applies when the widget is added to a layout.
//
// "weather", "calendar", "news", and "webpage" (the Embed widget) are
// intentionally NOT in this list - those are the "complex" widgets
// that have their own internal template_elements composition, and the
// Widget Templates editor is the place users build their layouts.
const RESTRICTED_PRESET_TYPES: &[&str] = &[
    "text",
    "image",
    "video",
    "clock",
    "date",
    "datetime",
    "countdown",
];

fn validate_preset_type(widget_type: &str) -> Result<(), (StatusCode, String)> {
    if RESTRICTED_PRESET_TYPES.contains(&widget_type) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Widget templates are not allowed for '{}' — this widget type uses hardcoded defaults.",
                widget_type
            ),
        ));
    }
    Ok(())
}

fn validate_widget_preset_asset(
    kind: &str,
    key: &str,
    url: &str,
) -> Result<(), (StatusCode, String)> {
    if kind != "weather_icon" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Unsupported widget preset asset kind".to_string(),
        ));
    }
    let code: u16 = key.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Weather icon key must be a WMO code".to_string(),
        )
    })?;
    if code > 999 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Weather icon key is out of range".to_string(),
        ));
    }
    if !is_local_upload_url(url) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Weather icon URL must be a local uploaded image/icon".to_string(),
        ));
    }
    Ok(())
}

fn image_extension_from_upload(
    filename: &str,
    data: &[u8],
) -> Result<&'static str, (StatusCode, String)> {
    let ext = StdPath::new(filename)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) && ext == "png" {
        return Ok("png");
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) && matches!(ext.as_str(), "jpg" | "jpeg") {
        return Ok("jpg");
    }
    if (data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) && ext == "gif" {
        return Ok("gif");
    }
    if data.starts_with(b"RIFF") && data.len() >= 12 && &data[8..12] == b"WEBP" && ext == "webp" {
        return Ok("webp");
    }
    Err((
        StatusCode::BAD_REQUEST,
        "Unsupported image type; use png, jpg, gif, or webp".to_string(),
    ))
}

async fn list_files(
    Path(category): Path<String>,
) -> Result<AxumJson<Vec<FileEntry>>, (StatusCode, String)> {
    const MAX_FILE_LIST_ENTRIES: usize = 10_000;
    let dir = uploads_dir(&category)?;
    if !dir.exists() {
        return Ok(AxumJson(vec![]));
    }
    let mut entries = Vec::new();
    let mut scanned_entries = 0usize;
    if let Ok(read_dir) = fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            scanned_entries = scanned_entries.saturating_add(1);
            if scanned_entries > MAX_FILE_LIST_ENTRIES || entries.len() >= MAX_FILE_LIST_ENTRIES {
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Too many uploaded files".to_string(),
                ));
            }
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let url = format!("/uploads/{}/{}", category, name);
                    entries.push(FileEntry {
                        name,
                        url,
                        size: meta.len(),
                    });
                }
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(AxumJson(entries))
}

async fn upload_file(
    Path(category): Path<String>,
    mut multipart: Multipart,
) -> Result<AxumJson<FileEntry>, (StatusCode, String)> {
    let dir = uploads_dir(&category)?;
    fs::create_dir_all(&dir).map_err(|_| internal_server_error())?;

    if let Ok(Some(field)) = multipart.next_field().await {
        let filename = field.file_name().unwrap_or("unnamed").to_string();
        let original_name = safe_filename(&filename)?;
        if is_active_upload_name(&original_name) {
            return Err((
                StatusCode::BAD_REQUEST,
                "active-content upload formats are not allowed".to_string(),
            ));
        }
        const MAX_UPLOAD_BYTES: u64 = 1024 * 1024 * 1024;
        let tmp_path = dir.join(format!(".upload-{}", Uuid::new_v4()));
        let mut tmp = fs::File::create(&tmp_path).map_err(|_| internal_server_error())?;
        let mut total = 0u64;
        let mut field = field;
        loop {
            let chunk = match field.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => {
                    let _ = fs::remove_file(&tmp_path);
                    return Err((StatusCode::BAD_REQUEST, "Unable to read upload".to_string()));
                }
            };
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_UPLOAD_BYTES {
                let _ = fs::remove_file(&tmp_path);
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Upload is too large".to_string(),
                ));
            }
            if tmp.write_all(&chunk).is_err() {
                let _ = fs::remove_file(&tmp_path);
                return Err(internal_server_error());
            }
        }
        if tmp.flush().is_err() {
            let _ = fs::remove_file(&tmp_path);
            return Err(internal_server_error());
        }
        drop(tmp);

        let mut image_data: Option<Vec<u8>> = None;
        let safe_name = if category == "images" || category == "icons" {
            const MAX_IMAGE_UPLOAD_BYTES: usize = 15 * 1024 * 1024;
            if total > MAX_IMAGE_UPLOAD_BYTES as u64 {
                let _ = fs::remove_file(&tmp_path);
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Image upload is too large".to_string(),
                ));
            }
            let data = match read_bounded_file(&tmp_path, MAX_IMAGE_UPLOAD_BYTES) {
                Ok(data) => data,
                Err(_) => {
                    let _ = fs::remove_file(&tmp_path);
                    return Err(internal_server_error());
                }
            };
            let ext = match image_extension_from_upload(&original_name, &data) {
                Ok(ext) => ext,
                Err(error) => {
                    let _ = fs::remove_file(&tmp_path);
                    return Err(error);
                }
            };
            if fs::remove_file(&tmp_path).is_err() {
                return Err(internal_server_error());
            }
            image_data = Some(data);
            format!("{}-{}.{}", Utc::now().timestamp(), Uuid::new_v4(), ext)
        } else {
            original_name
        };
        let dest = dir.join(&safe_name);

        if category == "images" || category == "icons" {
            fs::write(&dest, image_data.as_deref().unwrap_or_default())
                .map_err(|_| internal_server_error())?;
        } else {
            if fs::rename(&tmp_path, &dest).is_err() {
                let _ = fs::remove_file(&tmp_path);
                return Err(internal_server_error());
            }
        }

        let url = format!("/uploads/{}/{}", category, safe_name);
        return Ok(AxumJson(FileEntry {
            name: safe_name,
            url,
            size: total,
        }));
    }

    Err((StatusCode::BAD_REQUEST, "No file uploaded".to_string()))
}

async fn delete_file(
    Path((category, name)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let safe_name = safe_filename(&name)?;
    let path = uploads_dir(&category)?.join(&safe_name);
    if !path.exists() {
        return Err((StatusCode::NOT_FOUND, "File not found".to_string()));
    }
    // Move to trash instead of deleting
    let trash_dir = std::path::Path::new("trash");
    if !trash_dir.exists() {
        fs::create_dir_all(trash_dir).map_err(|_| internal_server_error())?;
    }
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S_%f").to_string();
    let ext = std::path::Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let trash_name = if ext.is_empty() {
        format!("{}_{}", ts, name)
    } else {
        let stem = &name[..name.len() - ext.len() - 1];
        format!("{}_{}.{}", ts, stem, ext)
    };
    let final_trash_path = trash_dir.join(&trash_name);
    fs::rename(&path, &final_trash_path).map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Trash handlers ──────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct TrashEntry {
    id: String,
    name: String,
    category: String,
    deleted_at: String,
    size_bytes: u64,
    original_path: String,
}

async fn list_trash() -> Result<Json<Vec<TrashEntry>>, (StatusCode, String)> {
    const MAX_TRASH_ENTRIES: usize = 10_000;
    let trash_dir = std::path::Path::new("trash");
    if !trash_dir.exists() {
        return Ok(Json(vec![]));
    }
    let mut entries = Vec::new();
    let mut scanned_entries = 0usize;
    for entry in fs::read_dir(trash_dir).map_err(|_| internal_server_error())? {
        scanned_entries = scanned_entries.saturating_add(1);
        if scanned_entries > MAX_TRASH_ENTRIES {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "Too many trash entries".to_string(),
            ));
        }
        let entry = entry.map_err(|_| internal_server_error())?;
        let path = entry.path();
        if !path.is_file() { continue; }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let deleted_at;
        let original_name;
        if name.len() > 15 && name.chars().nth(8) == Some('_') {
            let name_clone = name.clone();
            let ts_str = &name_clone[..15];
            deleted_at = chrono::NaiveDateTime::parse_from_str(ts_str, "%Y%m%d_%H%M%S")
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|_| ts_str.to_string());
            original_name = if name_clone.len() > 16 { name_clone[16..].to_string() } else { name_clone.clone() };
        } else {
            deleted_at = "unknown".to_string();
            original_name = name.clone();
        }
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let category: &'static str = if original_name.ends_with(".mp4") || original_name.ends_with(".webm") || original_name.ends_with(".mov") {
            "videos"
        } else if original_name.ends_with(".png") || original_name.ends_with(".jpg") || original_name.ends_with(".jpeg") || original_name.ends_with(".gif") || original_name.ends_with(".webp") {
            "images"
        } else if original_name.ends_with(".ttf") || original_name.ends_with(".otf") || original_name.ends_with(".woff") || original_name.ends_with(".woff2") {
            "fonts"
        } else {
            "other"
        };
        let cat_owned = category.to_string();
        entries.push(TrashEntry {
            id: name.clone(),
            name: original_name.clone(),
            category: cat_owned.clone(),
            deleted_at,
            size_bytes: size,
            original_path: format!("/uploads/{}/{}", cat_owned, original_name.clone()),
        });
    }
    entries.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    Ok(Json(entries))
}

async fn restore_trash(Path(id): Path<String>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if id.is_empty()
        || id.contains("..")
        || StdPath::new(&id).file_name().and_then(|name| name.to_str()) != Some(id.as_str())
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid trash entry".to_string()));
    }
    let trash_dir = std::path::Path::new("trash");
    let trash_path = trash_dir.join(&id);
    if !trash_path.exists() {
        return Err((StatusCode::NOT_FOUND, "File not found in trash".to_string()));
    }
    let original_name = id
        .splitn(4, '_')
        .nth(3)
        .unwrap_or(&id)
        .to_string();
    let original_name = safe_filename(&original_name)?;
    let category = if original_name.ends_with(".mp4") || original_name.ends_with(".webm") || original_name.ends_with(".mov") {
        "videos"
    } else if original_name.ends_with(".png") || original_name.ends_with(".jpg") || original_name.ends_with(".jpeg") || original_name.ends_with(".gif") || original_name.ends_with(".webp") {
        "images"
    } else if original_name.ends_with(".ttf") || original_name.ends_with(".otf") || original_name.ends_with(".woff") || original_name.ends_with(".woff2") {
        "fonts"
    } else {
        "other"
    };
    let uploads_dir = std::path::Path::new("uploads").join(category);
    fs::create_dir_all(&uploads_dir).map_err(|_| internal_server_error())?;
    let dest = uploads_dir.join(&original_name);
    fs::rename(&trash_path, &dest).map_err(|_| internal_server_error())?;
    let restored_json = serde_json::json!({"restored": true, "name": original_name.clone(), "category": category});
    Ok(Json(restored_json))
}

// ── Font handlers ───────────────────────
//
// Google Fonts catalog + download. The catalog is a public
// metadata API (no key) cached in-process for 24h. Downloads
// return a zip of woff2/ttf/woff/eot/svg duplicates per
// weight; we extract only the woff2 files into uploads/fonts/.
//
async fn fonts_catalog_handler() -> Result<AxumJson<Vec<crate::fonts::FontCatalogEntry>>, (StatusCode, String)> {
    let entries = crate::fonts::get_cached_catalog().await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(entries))
}

async fn fonts_installed_handler() -> Result<AxumJson<Vec<crate::fonts::InstalledFont>>, (StatusCode, String)> {
    let entries = crate::fonts::list_installed()
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(entries))
}

#[derive(serde::Deserialize)]
struct FontDownloadRequest {
    family: String,
    #[serde(default)]
    weights: Vec<u32>,
}

async fn fonts_download_handler(
    AxumJson(req): AxumJson<FontDownloadRequest>,
) -> Result<AxumJson<Vec<crate::fonts::InstalledFont>>, (StatusCode, String)> {
    let weights = if req.weights.is_empty() { vec![400, 700] } else { req.weights };
    let installed = crate::fonts::download_family(&req.family, &weights).await
        .map_err(|_| (StatusCode::BAD_GATEWAY, "Font download unavailable".to_string()))?;
    Ok(AxumJson(installed))
}

async fn fonts_delete_handler(
    Path(filename): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    crate::fonts::delete_font(&filename)
        .map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

// GET /api/fonts/preview-status?families=roboto,lato,open-sans
//
// Returns `{ "<family>": "<url>" }` for every family that already
// has a cached preview woff2. Uncached families are omitted; the
// picker falls back to sans-serif for those. The endpoint is
// public (no auth) because the font picker iframe is unauthenticated
// and the URLs it returns are same-origin.
const MAX_PREVIEW_STATUS_FAMILIES: usize = 4096;

#[derive(serde::Serialize)]
struct PreviewStatusResponse {
    /// Cached family -> preview woff2 URL.
    cached: std::collections::HashMap<String, String>,
    /// How many families are in the cache right now (for the
    /// progress indicator in the picker).
    total_cached: usize,
    /// Total on-disk size of the cache in bytes.
    cache_bytes: u64,
}

async fn fonts_preview_status_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<AxumJson<PreviewStatusResponse>, (StatusCode, String)> {
    let families: Vec<String> = params
        .get("families")
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .take(MAX_PREVIEW_STATUS_FAMILIES)
                .collect()
        })
        .unwrap_or_default();
    let pairs = crate::fonts::preview_status(&families);
    let mut cached_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (k, v) in pairs {
        cached_map.insert(k, v);
    }
    let (total_cached, cache_bytes) = crate::fonts::preview_cache_stats();
    Ok(AxumJson(PreviewStatusResponse {
        cached: cached_map,
        total_cached,
        cache_bytes,
    }))
}

/// Response for `/api/fonts/full-preview-css`.
#[derive(serde::Serialize)]
struct FullPreviewCssResponse {
    /// The Google Fonts CSS with src: url(...) rewritten to point
    /// at our local server. Empty string if the family isn't cached
    /// comprehensively.
    css: String,
    /// List of subset names that have files on disk. Useful for
    /// the runtime to know which subsets it can rely on.
    subsets: Vec<String>,
    /// The base URL (host:port) so the client can reconstruct
    /// absolute URLs if needed. Most callers can use the relative
    /// paths in `css` directly.
    base_url: String,
}

/// Return the full Google Fonts CSS for a family with all
/// src: url(...) values rewritten to point at our local server.
///
/// Query params:
///   family  - family id (lowercase slug, e.g. "roboto", "open-sans")
///   weight  - numeric weight (100..900, default 400)
///
/// Returns 404 if no subsets are cached for that family. The
/// runtime uses this to inject @font-face for installed fonts
/// that may contain non-Latin characters (e.g. Cyrillic widget
/// text in a Roboto-styled text widget).
async fn fonts_full_preview_css_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<AxumJson<FullPreviewCssResponse>, (StatusCode, String)> {
    let family_id = params
        .get("family")
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "missing family".to_string()))?;
    let weight: u32 = params
        .get("weight")
        .and_then(|w| w.parse().ok())
        .unwrap_or(400);
    let subsets = crate::fonts::list_cached_subsets(family_id, weight);
    if subsets.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no cached subsets for {} {}", family_id, weight),
        ));
    }
    // Build a CSS string with one @font-face per cached subset.
    // The browser uses unicode-range to pick the right subset
    // automatically, so we don't need to know which subset the
    // widget text will use.
    let mut css = String::new();
    for subset in &subsets {
        let url = crate::fonts::preview_subset_url(family_id, weight, subset);
        // The picker will inject this CSS as a <style> block. We
        // use the family id as the font-family name (matching
        // what the picker stores as the cssValue).
        css.push_str(&format!(
            "@font-face{{font-family:'{}';font-style:normal;font-weight:{};src:url('{}') format('woff2');font-display:swap;}}\n",
            family_id, weight, url
        ));
    }
    Ok(AxumJson(FullPreviewCssResponse {
        css,
        subsets,
        base_url: String::new(),
    }))
}


// ── Schedule handlers ───────────────────────────────────────

fn validate_schedule_entry(entry: &ScheduleEntry) -> Result<(), (StatusCode, String)> {
    let bad = |message: &'static str| (StatusCode::BAD_REQUEST, message.to_string());
    if entry.display.len() > 256
        || (entry.action == "switch_layout" && entry.display.trim().is_empty())
    {
        return Err(bad("Invalid schedule display"));
    }
    if entry.day_of_week != crate::schedule::DAY_EVERY
        && (entry.day_of_week < 0 || entry.day_of_week > 127)
    {
        return Err(bad("Invalid schedule day mask"));
    }
    for value in [&entry.start_time, &entry.end_time] {
        if chrono::NaiveTime::parse_from_str(value, "%H:%M").is_err() {
            return Err(bad("Schedule times must use HH:MM format"));
        }
    }
    match entry.action.as_str() {
        "switch_layout" => {
            let has_layout = !entry.layout_id.trim().is_empty();
            let has_set = entry
                .layout_set_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
            if has_layout == has_set {
                return Err(bad("A schedule must select exactly one layout or layout set"));
            }
        }
        "power_on" | "power_off" | "display_sleep" => {
            if entry
                .target_client
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(bad("Power schedules require a target client"));
            }
            if let Some(state) = entry.power_state.as_deref() {
                let valid = match entry.action.as_str() {
                    "display_sleep" => state == "sleep",
                    "power_on" | "power_off" => state == "on" || state == "off",
                    _ => false,
                };
                if !valid {
                    return Err(bad("Invalid power state"));
                }
            }
        }
        "display_settings" => {
            if entry
                .target_client
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(bad("Display settings schedules require a target client"));
            }
            if entry.control_method.as_deref() != Some("ddc") {
                return Err(bad("Display settings schedules require DDC/CI control"));
            }
            if entry.brightness.is_none() && entry.contrast.is_none() {
                return Err(bad("Display settings schedules require brightness or contrast"));
            }
            if entry.brightness.is_some_and(|value| !(0..=100).contains(&value))
                || entry.contrast.is_some_and(|value| !(0..=100).contains(&value))
            {
                return Err(bad("Display settings values must be between 0 and 100"));
            }
        }
        _ => return Err(bad("Invalid schedule action")),
    }
    match entry.end_action.as_str() {
        "none" => {}
        "power_on" => {
            if !matches!(entry.action.as_str(), "power_off" | "display_sleep") {
                return Err(bad("Schedule end power-on requires a power-off or sleep start action"));
            }
            if entry
                .target_client
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(bad("Schedule end power-on requires a target client"));
            }
        }
        _ => return Err(bad("Invalid schedule end action")),
    }
    if let Some(method) = entry.control_method.as_deref() {
        if !matches!(method, "auto" | "ddc" | "cec" | "dpms" | "ir") {
            return Err(bad("Invalid schedule control method"));
        }
    }
    if entry.display_index.is_some_and(|index| !(0..=63).contains(&index)) {
        return Err(bad("Invalid schedule display index"));
    }
    Ok(())
}

const MAX_LAYOUT_SET_ITEMS: usize = 256;

fn validate_layout_set_item_count(count: usize) -> Result<(), StatusCode> {
    if count > MAX_LAYOUT_SET_ITEMS {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn validate_layout_set_values(set: &LayoutSetWithItems) -> Result<(), StatusCode> {
    if set.id.trim().is_empty() || set.id.len() > 128 || set.name.trim().is_empty() || set.name.len() > 256 {
        return Err(StatusCode::BAD_REQUEST);
    }
    if set.dwell_secs == 0 || set.dwell_secs > 86_400 || set.items.len() > MAX_LAYOUT_SET_ITEMS {
        return Err(StatusCode::BAD_REQUEST);
    }
    for item in &set.items {
        if item.layout_id.trim().is_empty()
            || item.layout_id.len() > 128
            || item.interval_secs.is_some_and(|seconds| seconds == 0 || seconds > 86_400)
        {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}

async fn list_schedules(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<Vec<ScheduleEntry>>, (StatusCode, String)> {
    repository
        .list_schedules()
        .await
        .map(AxumJson)
        .map_err(|_| internal_server_error())
}

async fn validate_schedule_references(
    repository: &ClientRepository,
    entry: &ScheduleEntry,
) -> Result<(), (StatusCode, String)> {
    if entry.action != "switch_layout" {
        return Ok(());
    }
    if !entry.layout_id.trim().is_empty() {
        let exists = repository
            .get_layout(&entry.layout_id)
            .await
            .map_err(|_| internal_server_error())?
            .is_some();
        if !exists {
            return Err((StatusCode::BAD_REQUEST, "Scheduled layout does not exist".to_string()));
        }
    } else if let Some(layout_set_id) = entry.layout_set_id.as_deref() {
        let exists = repository
            .get_layout_set(layout_set_id)
            .await
            .map_err(|_| internal_server_error())?
            .is_some();
        if !exists {
            return Err((StatusCode::BAD_REQUEST, "Scheduled layout set does not exist".to_string()));
        }
    }
    Ok(())
}

async fn create_schedule(
    State(repository): State<ClientRepository>,
    AxumJson(mut entry): AxumJson<ScheduleEntry>,
) -> Result<AxumJson<ScheduleEntry>, (StatusCode, String)> {
    if entry.id.is_empty() {
        entry.id = Uuid::new_v4().to_string();
    }
    if entry.created_at.is_empty() {
        entry.created_at = chrono::Utc::now().to_rfc3339();
    }
    validate_schedule_entry(&entry)?;
    validate_schedule_references(&repository, &entry).await?;
    repository
        .create_schedule(&entry)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(entry))
}

async fn get_schedule(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<ScheduleEntry>, (StatusCode, String)> {
    repository
        .get_schedule(&id)
        .await
        .map_err(|_| internal_server_error())?
        .map(AxumJson)
        .ok_or((StatusCode::NOT_FOUND, "Schedule not found".to_string()))
}

async fn update_schedule(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    AxumJson(mut entry): AxumJson<ScheduleEntry>,
) -> Result<AxumJson<ScheduleEntry>, (StatusCode, String)> {
    entry.id = id;
    validate_schedule_entry(&entry)?;
    validate_schedule_references(&repository, &entry).await?;
    repository
        .update_schedule(&entry)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(entry))
}

async fn delete_schedule(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    repository
        .delete_schedule(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}
// ── Layout Set handlers ─────────────────────────────────────

async fn list_layout_sets(
    State(repository): State<ClientRepository>,
) -> Result<Json<Vec<LayoutSetSummary>>, StatusCode> {
    repository
        .list_layout_sets()
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn create_layout_set(
    State(repository): State<ClientRepository>,
    Json(payload): Json<CreateLayoutSet>,
) -> Result<Json<LayoutSetWithItems>, StatusCode> {
    validate_layout_set_item_count(payload.items.len())?;
    let now = chrono::Utc::now().to_rfc3339();
    let items: Vec<LayoutSetEntry> = payload
        .items
        .into_iter()
        .map(|li| LayoutSetEntry {
            id: uuid::Uuid::new_v4().to_string(),
            layout_set_id: payload.id.clone(),
            layout_id: li.layout_id,
            position: li.position,
            interval_secs: li.interval_secs,
            transition: None,
        })
        .collect();
    let set = LayoutSetWithItems {
        id: payload.id.clone(),
        name: payload.name.clone(),
        shuffle: payload.shuffle,
        dwell_secs: payload.dwell_secs,
        created_at: now.clone(),
        updated_at: now,
        items,
    };
    validate_layout_set_values(&set)?;
    repository
        .create_layout_set(&set)
        .await
        .map_err(|e| {
            eprintln!("create_layout_set error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    match repository.get_layout_set(&payload.id).await {
        Ok(Some(s)) => Ok(Json(s)),
        Ok(None) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Err(e) => {
            eprintln!("get_layout_set error: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_layout_set(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<Json<LayoutSetWithItems>, StatusCode> {
    match repository.get_layout_set(&id).await {
        Ok(Some(s)) => Ok(Json(s)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            eprintln!("get_layout_set error: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn update_layout_set(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateLayoutSet>,
) -> Result<Json<LayoutSetWithItems>, StatusCode> {
    let existing = match repository.get_layout_set(&id).await {
        Ok(Some(s)) => s,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(e) => {
            eprintln!("get_layout_set error: {e}");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let updated = payload.apply_to(existing);
    validate_layout_set_values(&updated)?;
    repository
        .update_layout_set(&updated)
        .await
        .map_err(|e| {
            eprintln!("update_layout_set error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    match repository.get_layout_set(&id).await {
        Ok(Some(s)) => Ok(Json(s)),
        Ok(None) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Err(e) => {
            eprintln!("get_layout_set error: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn delete_layout_set(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> StatusCode {
    if repository.delete_layout_set(&id).await.is_ok() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn add_layout_set_entry(
    State(repository): State<ClientRepository>,
    Path(layout_set_id): Path<String>,
    Json(payload): Json<AddLayoutSetEntry>,
) -> Result<Json<LayoutSetWithItems>, StatusCode> {
    if layout_set_id.trim().is_empty() || payload.layout_id.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if payload.interval_secs.is_some_and(|seconds| seconds == 0 || seconds > 86_400) {
        return Err(StatusCode::BAD_REQUEST);
    }
    repository
        .add_layout_set_entry(&layout_set_id, &payload)
        .await
        .map_err(|e| {
            eprintln!("add_layout_set_entry error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    match repository.get_layout_set(&layout_set_id).await {
        Ok(Some(s)) => Ok(Json(s)),
        Ok(None) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Err(e) => {
            eprintln!("get_layout_set error: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn update_layout_set_entry(
    State(repository): State<ClientRepository>,
    Path((_layout_set_id, entry_id)): Path<(String, String)>,
    Json(payload): Json<UpdateLayoutSetEntry>,
) -> StatusCode {
    match repository.update_layout_set_entry(&entry_id, &payload).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(e) => {
            eprintln!("update_layout_set_entry error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn delete_layout_set_entry(
    State(repository): State<ClientRepository>,
    Path((_layout_set_id, entry_id)): Path<(String, String)>,
) -> StatusCode {
    if repository.delete_layout_set_entry(&entry_id).await.is_ok() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn replace_layout_set_entries(
    State(repository): State<ClientRepository>,
    Path(layout_set_id): Path<String>,
    Json(entries): Json<Vec<CreateLayoutSetItem>>,
) -> Result<Json<LayoutSetWithItems>, StatusCode> {
    if layout_set_id.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    validate_layout_set_item_count(entries.len())?;
    let entries: Vec<LayoutSetEntry> = entries
        .into_iter()
        .map(|e| {
            if e.layout_id.trim().is_empty()
                || e.layout_id.len() > 128
                || e.interval_secs.is_some_and(|seconds| seconds == 0 || seconds > 86_400)
            {
                return Err(StatusCode::BAD_REQUEST);
            }
            Ok(LayoutSetEntry {
            id: uuid::Uuid::new_v4().to_string(),
            layout_set_id: layout_set_id.clone(),
            layout_id: e.layout_id,
            position: e.position,
            interval_secs: e.interval_secs,
            transition: None,
            })
        })
        .collect::<Result<_, _>>()?;
    repository
        .replace_layout_set_entries(&layout_set_id, entries)
        .await
        .map_err(|e| {
            eprintln!("replace_layout_set_entries error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    match repository.get_layout_set(&layout_set_id).await {
        Ok(Some(s)) => Ok(Json(s)),
        Ok(None) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Err(e) => {
            eprintln!("get_layout_set error: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// ── Widget Preset handlers ──────────────────────────────────

#[derive(serde::Deserialize)]
struct CreateWidgetPresetBody {
    name: String,
    widget_type: String,
    props: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct UpdateWidgetPresetBody {
    name: String,
    widget_type: String,
    props: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct WidgetPresetAssetBody {
    kind: String,
    key: String,
    url: String,
}

async fn list_widget_presets(
    State(repository): State<ClientRepository>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<AxumJson<Vec<WidgetPresetMeta>>, (StatusCode, String)> {
    let widget_type = params.get("widget_type").map(|s| s.as_str());
    repository
        .list_widget_presets(widget_type)
        .await
        .map(AxumJson)
        .map_err(|_| internal_server_error())
}

async fn get_widget_preset(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<WidgetPreset>, (StatusCode, String)> {
    repository
        .get_widget_preset(&id)
        .await
        .map_err(|_| internal_server_error())?
        .map(AxumJson)
        .ok_or((StatusCode::NOT_FOUND, "Widget preset not found".to_string()))
}

async fn create_widget_preset(
    State(repository): State<ClientRepository>,
    Json(body): Json<CreateWidgetPresetBody>,
) -> Result<AxumJson<WidgetPreset>, (StatusCode, String)> {
    validate_preset_type(&body.widget_type)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let props_json = serde_json::to_string(&body.props).unwrap_or_else(|_| "{}".to_string());
    repository
        .create_widget_preset(&id, &body.name, &body.widget_type, &props_json, &now)
        .await
        .map_err(|_| internal_server_error())?;
    repository
        .get_widget_preset(&id)
        .await
        .map_err(|_| internal_server_error())?
        .map(AxumJson)
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load created preset".to_string(),
        ))
}

async fn update_widget_preset(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(body): Json<UpdateWidgetPresetBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    validate_preset_type(&body.widget_type)?;
    let now = chrono::Utc::now().to_rfc3339();
    let props_json = serde_json::to_string(&body.props).unwrap_or_else(|_| "{}".to_string());
    repository
        .update_widget_preset(&id, &body.name, &body.widget_type, &props_json, &now)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_widget_preset(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    repository
        .delete_widget_preset(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_widget_preset_asset(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(body): Json<WidgetPresetAssetBody>,
) -> Result<AxumJson<WidgetPresetAsset>, (StatusCode, String)> {
    validate_widget_preset_asset(&body.kind, &body.key, &body.url)?;
    let asset = WidgetPresetAsset {
        id: uuid::Uuid::new_v4().to_string(),
        preset_id: id,
        kind: body.kind,
        key: body.key,
        url: body.url,
    };
    repository
        .upsert_widget_preset_asset(&asset)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(asset))
}

async fn remove_widget_preset_asset(
    State(repository): State<ClientRepository>,
    Path((preset_id, asset_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let deleted = repository
        .delete_widget_preset_asset(&preset_id, &asset_id)
        .await
        .map_err(|_| internal_server_error())?;
    if !deleted {
        log::debug!(
            "widget preset asset delete requested for missing asset preset={} asset={}",
            preset_id,
            asset_id
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn export_widget_preset(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<
    (
        [(axum::http::header::HeaderName, axum::http::HeaderValue); 1],
        String,
    ),
    (StatusCode, String),
> {
    let preset = repository
        .get_widget_preset(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::NOT_FOUND, "Widget preset not found".to_string()))?;
    let props =
        normalize_widget_preset_props(Some(serde_json::Value::String(preset.props_json.clone())))?;
    let export_assets = widget_preset_assets_from_export(
        Some(
            serde_json::to_value(&preset.assets)
                .map_err(|_| internal_server_error())?,
        ),
        &props,
    );
    let preset_export = serde_json::json!({
        "name": preset.name,
        "widget_type": preset.widget_type,
        "props": props,
        "assets": export_assets,
    });
    let export = serde_json::json!({
        "version": 1,
        "kind": "widget_preset",
        "preset": preset_export,
    });
    let filename = format!(
        "attachment; filename=\"{}-{}.widget-preset.json\"",
        sanitized_download_stem(&preset.widget_type),
        sanitized_download_stem(&preset.name)
    );
    let disposition = axum::http::HeaderValue::from_str(&filename)
        .map_err(|_| internal_server_error())?;
    let header = [(axum::http::header::CONTENT_DISPOSITION, disposition)];
    let body = serde_json::to_string_pretty(&export).unwrap_or_else(|_| "{}".to_string());
    Ok((header, body))
}

fn normalize_widget_preset_props(
    value: Option<serde_json::Value>,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let raw = value.unwrap_or_else(|| serde_json::json!({}));
    let parsed = match raw {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str::<serde_json::Value>(trimmed).map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("Invalid widget preset props_json: {e}"),
                    )
                })?
            }
        }
        other => other,
    };
    if parsed.is_object() {
        Ok(parsed)
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "Widget preset props must be a JSON object".to_string(),
        ))
    }
}

fn widget_preset_assets_from_export(
    assets_value: Option<serde_json::Value>,
    props: &serde_json::Value,
) -> serde_json::Value {
    let mut out = Vec::new();
    if let Some(serde_json::Value::Array(arr)) = assets_value {
        out.extend(arr.into_iter().filter(|a| {
            a.get("kind").and_then(|v| v.as_str()).is_some()
                && a.get("key").and_then(|v| v.as_str()).is_some()
                && a.get("url").and_then(|v| v.as_str()).is_some()
        }));
    }
    // If the export did not carry asset rows, synthesize them from props.custom_icons.
    // The editor and DB use asset rows as the source of truth for custom weather icons.
    if out.is_empty() {
        if let Some(custom_icons) = props.get("custom_icons").and_then(|v| v.as_object()) {
            for (key, url) in custom_icons {
                if let Some(url) = url.as_str() {
                    if !key.trim().is_empty() && !url.trim().is_empty() {
                        out.push(serde_json::json!({
                            "kind": "weather_icon",
                            "key": key,
                            "url": url,
                        }));
                    }
                }
            }
        }
    }
    serde_json::Value::Array(out)
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct ImportWidgetPresetBody {
    /// The full export JSON OR just the inner preset object.
    #[serde(default)]
    preset: Option<serde_json::Value>,
    /// Or a flat form: { name, widget_type, props, assets }.
    name: Option<String>,
    widget_type: Option<String>,
    props: Option<serde_json::Value>,
    assets: Option<Vec<WidgetPresetAsset>>,
}

async fn import_widget_preset(
    State(repository): State<ClientRepository>,
    Json(body): Json<serde_json::Value>,
) -> Result<AxumJson<WidgetPreset>, (StatusCode, String)> {
    // Accept either { kind: "widget_preset", preset: {...} } or a flat preset object.
    let preset_obj = if let Some(p) = body.get("preset") {
        p.clone()
    } else {
        body.clone()
    };
    let name = preset_obj
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Imported")
        .to_string();
    let widget_type = preset_obj
        .get("widget_type")
        .and_then(|v| v.as_str())
        .unwrap_or("text")
        .to_string();
    let props = normalize_widget_preset_props(
        preset_obj
            .get("props")
            .cloned()
            .or_else(|| preset_obj.get("props_json").cloned()),
    )?;
    let assets = widget_preset_assets_from_export(preset_obj.get("assets").cloned(), &props);

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let props_json = serde_json::to_string(&props).unwrap_or_else(|_| "{}".to_string());
    repository
        .create_widget_preset(&id, &name, &widget_type, &props_json, &now)
        .await
        .map_err(|_| internal_server_error())?;
    if let Some(arr) = assets.as_array() {
        for asset in arr {
            let kind = asset
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("weather_icon")
                .to_string();
            let key = asset
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .to_string();
            let url = asset
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            validate_widget_preset_asset(&kind, &key, &url)?;
            let asset_id = uuid::Uuid::new_v4().to_string();
            repository
                .upsert_widget_preset_asset(&WidgetPresetAsset {
                    id: asset_id,
                    preset_id: id.clone(),
                    kind,
                    key,
                    url,
                })
                .await
                .map_err(|_| internal_server_error())?;
        }
    }
    repository
        .get_widget_preset(&id)
        .await
        .map_err(|_| internal_server_error())?
        .map(AxumJson)
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load imported preset".to_string(),
        ))
}

// (Installer Changelog endpoints removed; UI was removed in commit 42f74e3.)
// Original /api/installer-changelog handler code is gone. The orphan data file
// installer_changelog.json (if it exists on disk) is harmless; it's in .gitignore.

// ── Version Manager handlers ──────────────────────────────

async fn get_version() -> Result<AxumJson<crate::version::VersionInfo>, (StatusCode, String)> {
    use crate::version::{VersionInfo, VERSION, PKG_NAME, BUILD_TIME, GIT_COMMIT};
    use std::path::PathBuf;

    let exe = std::env::current_exe().unwrap_or_default();
    let data_dir = std::env::var("SIGNAGE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/board/data"));
    let static_dir = std::env::var("SIGNAGE_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| crate::version::static_dir_path());
    let backups_dir = data_dir.join("backups");
    let uploads_dir = data_dir.join("uploads");
    let db_path = data_dir.join("signage.db");
    let install_dir = static_dir.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/home/board/server"));

    Ok(AxumJson(VersionInfo {
        version: VERSION.to_string(),
        pkg_name: PKG_NAME.to_string(),
        build_time: BUILD_TIME.to_string(),
        git_commit: GIT_COMMIT.to_string(),
        uptime_seconds: crate::version::uptime_seconds(),
        binary_path: exe.to_string_lossy().to_string(),
        install_dir: install_dir.to_string_lossy().to_string(),
        database_path: db_path.to_string_lossy().to_string(),
        static_dir: static_dir.to_string_lossy().to_string(),
        backups_dir: backups_dir.to_string_lossy().to_string(),
        uploads_dir: uploads_dir.to_string_lossy().to_string(),
    }))
}

async fn export_state_bundle_handler(
    State(runtime_state): State<RuntimeState>,
) -> Result<
    (
        [(axum::http::header::HeaderName, axum::http::HeaderValue); 1],
        Vec<u8>,
    ),
    (StatusCode, String),
> {
    if matches!(runtime_state.client_repository(), ClientRepository::Postgres(_)) {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "State bundles currently support only the SQLite backend".to_string(),
        ));
    }
    // Drop the lock before doing file I/O so we don't hold the DB open during the tar.
    drop(runtime_state);
    // Use the install dir for the bundle — /tmp may be a small tmpfs on the player device.
    let dest = std::path::PathBuf::from(crate::version::install_dir_for_static()).join(format!(
        ".signage-export-{}.tar.gz",
        chrono::Utc::now().timestamp()
    ));
    crate::version::export_state_bundle(&dest)
        .map_err(|_| internal_server_error())?;
    let bundle_size = tokio::fs::metadata(&dest)
        .await
        .map_err(|_| internal_server_error())?
        .len();
    if bundle_size > MAX_STATE_BUNDLE_BYTES {
        let _ = tokio::fs::remove_file(&dest).await;
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Generated state bundle is too large".to_string(),
        ));
    }
    let bytes = read_bounded_async_file(&dest, MAX_STATE_BUNDLE_BYTES)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("read bundle: {}", e)))?;
    let _ = tokio::fs::remove_file(&dest).await;
    let header = [(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_static(
            "attachment; filename=\"signage-state-bundle.tar.gz\"",
        ),
    )];
    Ok((header, bytes))
}

const MAX_STATE_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;

async fn stage_multipart_bundle(
    mut field: axum::extract::multipart::Field<'_>,
    prefix: &str,
) -> Result<PathBuf, (StatusCode, String)> {
    let dest = PathBuf::from(crate::version::install_dir_for_static())
        .join(format!(".{prefix}-{}.tar.gz", Uuid::new_v4()));
    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|_| internal_server_error())?;
    let mut total = 0u64;
    loop {
        let next = match field.chunk().await {
            Ok(next) => next,
            Err(_) => {
                let _ = tokio::fs::remove_file(&dest).await;
                return Err((StatusCode::BAD_REQUEST, "Unable to read bundle".to_string()));
            }
        };
        let Some(chunk) = next else { break };
        total = total.saturating_add(chunk.len() as u64);
        if total > MAX_STATE_BUNDLE_BYTES {
            let _ = tokio::fs::remove_file(&dest).await;
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "Bundle is too large".to_string(),
            ));
        }
        if file.write_all(&chunk).await.is_err() {
            let _ = tokio::fs::remove_file(&dest).await;
            return Err(internal_server_error());
        }
    }
    if file.flush().await.is_err() {
        let _ = tokio::fs::remove_file(&dest).await;
        return Err(internal_server_error());
    }
    Ok(dest)
}

async fn import_state_bundle_handler(
    State(runtime_state): State<RuntimeState>,
    mut multipart: Multipart,
) -> Result<StatusCode, (StatusCode, String)> {
    if matches!(runtime_state.client_repository(), ClientRepository::Postgres(_)) {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "State bundles currently support only the SQLite backend".to_string(),
        ));
    }
    // The DB needs to be closed before we replace the file underneath it. SQLite holds
    // a file handle; if we overwrite the file, existing connections become stale.
    // We work around this by closing all prepared statements via a no-op query, but
    // in practice the caller must restart the service after import. The handler will
    // close the DB lock so we don't hold the file open during the copy.
    drop(runtime_state);
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("multipart: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "bundle" || name == "file" {
            let dest = stage_multipart_bundle(field, "signage-import").await?;
            // Synchronous import (it's not that slow)
            let import_dest = dest.clone();
            let result = tokio::task::spawn_blocking(move || crate::version::import_state_bundle(&import_dest))
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {}", e)))?
                .map_err(|_| internal_server_error());
            let _ = tokio::fs::remove_file(&dest).await;
            result?;
            // Restart the configured service so the new DB is loaded. The
            // helper prefers explicit service configuration, then detects the
            // current user/system unit instead of assuming a legacy unit.
            let _ = tokio::task::spawn_blocking(crate::version::restart_configured_service).await;
            return Ok(StatusCode::NO_CONTENT);
        }
    }
    Err((
        StatusCode::BAD_REQUEST,
        "No bundle file in upload".to_string(),
    ))
}

async fn update_preview_handler(
) -> Result<AxumJson<crate::version::UpdateManifest>, (StatusCode, String)> {
    // Build an in-memory manifest of what WOULD be exported, without writing a tarball.
    // Useful for the UI to show "this is what's in your update bundle" before exporting.
    let exe = std::env::current_exe().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("current_exe: {}", e),
        )
    })?;
    let install_dir = crate::version::install_dir_for_static();
    let static_dir = crate::version::static_dir_path();
    let binary_sha256 =
        crate::version::sha256_of_file(&exe).unwrap_or_else(|_| "unknown".to_string());
    let binary_size = crate::version::file_size(&exe).unwrap_or(0);
    let has_changelog = std::path::Path::new(&format!("{}/CHANGELOG.md", install_dir)).exists();
    let has_static = static_dir.exists();
    let runtime_dependencies = crate::version::detect_runtime_deps();
    Ok(AxumJson(crate::version::UpdateManifest {
        version: crate::version::VERSION.to_string(),
        pkg_name: crate::version::PKG_NAME.to_string(),
        build_time: crate::version::BUILD_TIME.to_string(),
        git_commit: crate::version::GIT_COMMIT.to_string(),
        source_binary_path: exe.to_string_lossy().to_string(),
        source_install_dir: install_dir,
        binary_sha256,
        binary_size_bytes: binary_size,
        static_sha256: crate::version::sha256_of_directory(&static_dir)
            .unwrap_or_else(|_| "unknown".to_string()),
        has_binary: true,
        has_static,
        has_changelog,
        runtime_dependencies,
        exported_at: chrono::Utc::now().to_rfc3339(),
    }))
}

async fn audit_update_import_failure(
    state: &RuntimeState,
    principal: &crate::middleware::AuthenticatedPrincipal,
    detail: &'static str,
) {
    let _ = state
        .client_repository()
        .record_audit_event(
            Some(&principal.username),
            Some(&principal.role),
            "server_update_import",
            Some("server-update-bundle"),
            "failure",
            Some(detail),
            &Utc::now().to_rfc3339(),
        )
        .await;
}

async fn audit_update_export_failure(
    state: &RuntimeState,
    principal: &crate::middleware::AuthenticatedPrincipal,
    detail: &'static str,
) {
    let _ = state
        .client_repository()
        .record_audit_event(
            Some(&principal.username),
            Some(&principal.role),
            "server_update_export",
            Some("server-update-bundle"),
            "failure",
            Some(detail),
            &Utc::now().to_rfc3339(),
        )
        .await;
}

async fn export_update_bundle_handler(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
) -> Result<
    (
        [(axum::http::header::HeaderName, axum::http::HeaderValue); 1],
        Vec<u8>,
    ),
    (StatusCode, String),
> {
    let dest = std::path::PathBuf::from(crate::version::install_dir_for_static()).join(format!(
        ".signage-update-export-{}.tar.gz",
        chrono::Utc::now().timestamp()
    ));
    let _manifest = match crate::version::export_update_bundle(&dest) {
        Ok(manifest) => manifest,
        Err(error) => {
            audit_update_export_failure(&state, &principal, "bundle export failed").await;
            return Err((StatusCode::INTERNAL_SERVER_ERROR, error));
        }
    };
    let bundle_size = match tokio::fs::metadata(&dest).await {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            audit_update_export_failure(&state, &principal, "bundle metadata failed").await;
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("inspect bundle: {}", error),
            ));
        }
    };
    if bundle_size > MAX_STATE_BUNDLE_BYTES {
        let _ = tokio::fs::remove_file(&dest).await;
        audit_update_export_failure(&state, &principal, "bundle too large").await;
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Generated update bundle is too large".to_string(),
        ));
    }
    let bytes = match read_bounded_async_file(&dest, MAX_STATE_BUNDLE_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            audit_update_export_failure(&state, &principal, "bundle read failed").await;
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read bundle: {}", error),
            ));
        }
    };
    let _ = tokio::fs::remove_file(&dest).await;
    let header = [(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_static(
            "attachment; filename=\"signage-update-bundle.tar.gz\"",
        ),
    )];
    let _ = state
        .client_repository()
        .record_audit_event(
            Some(&principal.username),
            Some(&principal.role),
            "server_update_export",
            Some("server-update-bundle"),
            "success",
            Some("bundle exported"),
            &Utc::now().to_rfc3339(),
        )
        .await;
    Ok((header, bytes))
}

fn update_failure_stage(error: &str) -> (&'static str, &'static str) {
    if error.starts_with("update bundle signature rejected:")
        || error.starts_with("manifest")
        || error.starts_with("parse manifest:")
        || error.starts_with("static SHA256 mismatch")
        || error.starts_with("binary SHA256 mismatch")
        || error.starts_with("bundle missing")
        || error.starts_with("required runtime dependency missing:")
        || error.starts_with("new binary")
        || error.starts_with("could not execute new binary")
    {
        ("server_update_verify", "bundle verification rejected")
    } else if error.starts_with("unsafe archive")
        || error.starts_with("archive contains")
        || error.starts_with("could not list update archive")
        || error.starts_with("could not inspect update archive entry types")
        || error.starts_with("inspect update archive")
    {
        ("server_update_verify", "archive validation rejected")
    } else if error.contains("auto-rolled back") {
        ("server_update_rollback", "automatic rollback completed")
    } else if error.starts_with("failed to swap binary")
        || error.starts_with("static swap failed")
    {
        ("server_update_swap", "atomic application swap failed")
    } else {
        ("server_update_apply", "application update stage failed")
    }
}

async fn import_update_bundle_handler(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    mut multipart: Multipart,
) -> Result<AxumJson<crate::version::UpdateResult>, (StatusCode, String)> {
    let force_skip_rollback = params
        .get("force")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    let mut bundle_path: Option<PathBuf> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => {
                audit_update_import_failure(&state, &principal, "multipart rejected").await;
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("multipart: {}", error),
                ));
            }
        };
        let name = field.name().unwrap_or("").to_string();
        if name == "bundle" || name == "file" {
            if let Some(previous_path) = bundle_path.take() {
                let _ = tokio::fs::remove_file(&previous_path).await;
                audit_update_import_failure(&state, &principal, "duplicate bundle fields").await;
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Update upload must contain exactly one bundle file".to_string(),
                ));
            }
            match stage_multipart_bundle(field, "signage-update-import").await {
                Ok(path) => bundle_path = Some(path),
                Err(error) => {
                    audit_update_import_failure(&state, &principal, "bundle field read failed").await;
                    return Err(error);
                }
            }
        }
    }
    let dest = match bundle_path {
        Some(path) => path,
        None => {
            audit_update_import_failure(&state, &principal, "bundle file missing").await;
            return Err((
                StatusCode::BAD_REQUEST,
                "No bundle file in upload".to_string(),
            ));
        }
    };
    let import_dest = dest.clone();
    let worker_result = tokio::task::spawn_blocking(move || {
        crate::version::import_update_bundle(&import_dest, force_skip_rollback)
    })
    .await;
    let _ = tokio::fs::remove_file(&dest).await;
    let result = match worker_result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let status = if error.starts_with("update bundle signature rejected:")
                || error.starts_with("unsafe archive")
                || error.starts_with("archive contains")
                || error.starts_with("manifest")
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            let (stage_event, stage_detail) = update_failure_stage(&error);
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    stage_event,
                    Some("server-update-bundle"),
                    "failure",
                    Some(stage_detail),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "server_update_import",
                    Some("server-update-bundle"),
                    "failure",
                    Some("bundle verification or application rejected"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            return Err((status, error));
        }
        Err(error) => {
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "server_update_import",
                    Some("server-update-bundle"),
                    "failure",
                    Some("update worker failed"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("join: {error}"),
            ));
        }
    };
    let (configured_service, _) = crate::version::configured_service();
    let restart = tokio::task::spawn_blocking(crate::version::restart_configured_service).await;
    match restart {
        Ok(Ok(())) => {
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "server_update_restart",
                    Some(&configured_service),
                    "success",
                    Some("service restart returned success"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
        }
        Ok(Err(error)) => {
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "server_update_restart",
                    Some(&configured_service),
                    "failure",
                    Some("service restart returned failure"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "server_update_import",
                    Some("server-update-bundle"),
                    "failure",
                    Some("bundle applied but service restart returned failure"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "update applied but {configured_service} restart failed: {error}"
                ),
            ));
        }
        Err(e) => {
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "server_update_restart",
                    Some(&configured_service),
                    "failure",
                    Some("service restart could not be started"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "server_update_import",
                    Some("server-update-bundle"),
                    "failure",
                    Some("bundle applied but service restart could not be started"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("update applied but could not restart {configured_service}: {e}"),
            ));
        }
    }
    let _ = state
        .client_repository()
        .record_audit_event(
            Some(&principal.username),
            Some(&principal.role),
            "server_update_swap",
            Some("server-update-bundle"),
            "success",
            Some("binary and static assets swapped atomically"),
            &Utc::now().to_rfc3339(),
        )
        .await;
    let _ = state
        .client_repository()
        .record_audit_event(
            Some(&principal.username),
            Some(&principal.role),
            "server_update_import",
            Some("server-update-bundle"),
            "success",
            Some("bundle verified, applied, and service restarted"),
            &Utc::now().to_rfc3339(),
        )
        .await;
    Ok(AxumJson(result))
}

#[derive(Serialize)]
struct UpdateLastFailure {
    failed: bool,
    timestamp: Option<String>,
    git_commit: Option<String>,
    version: Option<String>,
    error: Option<String>,
    backup_binary: Option<String>,
    backup_static: Option<String>,
}

async fn list_audit_events_handler(
    State(state): State<RuntimeState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<AxumJson<Vec<crate::models::AuditEvent>>, (StatusCode, String)> {
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    state
        .client_repository()
        .list_audit_events(limit)
        .await
        .map(AxumJson)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))
}
async fn update_last_failure_handler() -> AxumJson<UpdateLastFailure> {
    let install = crate::version::install_dir_for_static();
    match crate::version::read_last_update_failed(&install) {
        Some(v) => AxumJson(UpdateLastFailure {
            failed: true,
            timestamp: v
                .get("timestamp")
                .and_then(|x| x.as_str())
                .map(String::from),
            git_commit: v
                .get("git_commit")
                .and_then(|x| x.as_str())
                .map(String::from),
            version: v.get("version").and_then(|x| x.as_str()).map(String::from),
            error: v.get("error").and_then(|x| x.as_str()).map(String::from),
            backup_binary: v
                .get("backup_binary")
                .and_then(|x| x.as_str())
                .map(String::from),
            backup_static: v
                .get("backup_static")
                .and_then(|x| x.as_str())
                .map(String::from),
        }),
        None => AxumJson(UpdateLastFailure {
            failed: false,
            timestamp: None,
            git_commit: None,
            version: None,
            error: None,
            backup_binary: None,
            backup_static: None,
        }),
    }
}

#[derive(Deserialize)]
struct GithubUpdateSettingsPayload {
    auto_update: bool,
}

async fn github_update_status_handler() -> AxumJson<crate::github_update::UpdateStatus> {
    AxumJson(crate::github_update::read_status())
}

async fn github_update_check_handler() -> Result<AxumJson<crate::github_update::UpdateStatus>, (StatusCode, String)> {
    crate::github_update::check_latest()
        .await
        .map(AxumJson)
        .map_err(|error| (StatusCode::BAD_GATEWAY, error))
}

async fn github_update_settings_handler(
    AxumJson(payload): AxumJson<GithubUpdateSettingsPayload>,
) -> Result<AxumJson<crate::github_update::UpdateStatus>, (StatusCode, String)> {
    crate::github_update::save_settings(&crate::github_update::UpdateSettings {
        auto_update: payload.auto_update,
    })
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(AxumJson(crate::github_update::read_status()))
}

async fn apply_github_update_handler(
    State(state): State<RuntimeState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
) -> Result<AxumJson<crate::version::UpdateResult>, (StatusCode, String)> {
    let result = match crate::github_update::apply_latest().await {
        Ok(Some(result)) => result,
        Ok(None) => {
            return Err((
                StatusCode::CONFLICT,
                "No newer stable GitHub release is available".to_string(),
            ));
        }
        Err(error) => {
            let _ = state
                .client_repository()
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "server_update_import",
                    Some("github-latest"),
                    "failure",
                    Some("signed GitHub release rejected or failed"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            return Err((StatusCode::BAD_GATEWAY, error));
        }
    };
    let (configured_service, _) = crate::version::configured_service();
    let _ = state
        .client_repository()
        .record_audit_event(
            Some(&principal.username),
            Some(&principal.role),
            "server_update_import",
            Some("github-latest"),
            "success",
            Some("signed GitHub release applied; restarting service"),
            &Utc::now().to_rfc3339(),
        )
        .await;
    match tokio::task::spawn_blocking(crate::version::restart_configured_service).await {
        Ok(Ok(())) => Ok(AxumJson(result)),
        Ok(Err(error)) => {
            let message = format!("update applied but {configured_service} restart failed: {error}");
            let _ = crate::github_update::mark_error(&message);
            Err((StatusCode::INTERNAL_SERVER_ERROR, message))
        }
        Err(error) => {
            let message = format!("update applied but restart task failed: {error}");
            let _ = crate::github_update::mark_error(&message);
            Err((StatusCode::INTERNAL_SERVER_ERROR, message))
        }
    }
}

#[derive(Serialize)]
struct PlayerClientBundleBuildResult {
    ok: bool,
    path: String,
    url: String,
    bootstrap_url: String,
    size_bytes: u64,
    sha256: String,
    stdout: String,
}

async fn build_player_client_bundle_handler(
) -> Result<AxumJson<PlayerClientBundleBuildResult>, (StatusCode, String)> {
    let script = std::path::PathBuf::from(crate::version::install_dir_for_static())
        .join("tools/build-player-client-bundle.py");
    if !script.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("bundle builder missing: {}", script.display()),
        ));
    }
    let output = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new("python3");
        command
            .arg(&script)
            .current_dir(crate::version::install_dir_for_static());
        bounded_command_output(command, 1024 * 1024, Duration::from_secs(30))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {}", e)))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("run builder: {}", e)))?;
    let (status, stdout_bytes) = output;
    let stdout = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
    if !status.success() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("bundle builder failed: {}", status),
        ));
    }
    let json_line = stdout.lines().last().unwrap_or("{}");
    #[derive(Deserialize)]
    struct RawBundleResult {
        ok: bool,
        path: String,
        url: String,
        size_bytes: u64,
        sha256: String,
    }
    let raw: RawBundleResult = serde_json::from_str(json_line).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse builder output: {} :: {}", e, stdout),
        )
    })?;
    Ok(AxumJson(PlayerClientBundleBuildResult {
        ok: raw.ok,
        path: raw.path,
        url: raw.url,
        bootstrap_url: "/install/player-client-bundle-install.sh".to_string(),
        size_bytes: raw.size_bytes,
        sha256: raw.sha256,
        stdout,
    }))
}

#[derive(Serialize)]
struct ServerBundleBuildResult {
    ok: bool,
    path: String,
    url: String,
    bootstrap_url: String,
    size_bytes: u64,
    sha256: String,
    stdout: String,
}

async fn build_server_bundle_handler(
) -> Result<AxumJson<ServerBundleBuildResult>, (StatusCode, String)> {
    let script = std::path::PathBuf::from(crate::version::install_dir_for_static())
        .join("tools/build-server-bundle.py");
    if !script.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("server bundle builder missing: {}", script.display()),
        ));
    }
    let output = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new("python3");
        command
            .arg(&script)
            .current_dir(crate::version::install_dir_for_static());
        bounded_command_output(command, 1024 * 1024, Duration::from_secs(30))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {}", e)))?
    .map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("run server bundle builder: {}", e),
    ))?;
    let (status, stdout_bytes) = output;
    let stdout = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
    if !status.success() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("server bundle builder failed: {}", status),
        ));
    }
    let json_line = stdout.lines().last().unwrap_or("{}");
    #[derive(Deserialize)]
    struct RawServerBundleResult {
        ok: bool,
        path: String,
        url: String,
        bootstrap_url: String,
        size_bytes: u64,
        sha256: String,
    }
    let raw: RawServerBundleResult = serde_json::from_str(json_line).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse server bundle builder output: {} :: {}", e, stdout),
        )
    })?;
    Ok(AxumJson(ServerBundleBuildResult {
        ok: raw.ok,
        path: raw.path,
        url: raw.url,
        bootstrap_url: raw.bootstrap_url,
        size_bytes: raw.size_bytes,
        sha256: raw.sha256,
        stdout,
    }))
}

fn attachment_headers(
    filename: &'static str,
    content_type: &'static str,
) -> [(axum::http::header::HeaderName, axum::http::HeaderValue); 2] {
    [
        (
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static(content_type),
        ),
        (
            axum::http::header::CONTENT_DISPOSITION,
            axum::http::HeaderValue::from_static(match filename {
                "ripley-player-client-bundle.tar.gz" => {
                    "attachment; filename=\"ripley-player-client-bundle.tar.gz\""
                }
                "ripley-player-client-bundle-install.sh" => {
                    "attachment; filename=\"ripley-player-client-bundle-install.sh\""
                }
                "screentra-cast-installers.tar.gz" => {
                    "attachment; filename=\"screentra-cast-installers.tar.gz\""
                }
                _ => "attachment",
            }),
        ),
    ]
}

async fn download_static_install_file(
    filename: &'static str,
    content_type: &'static str,
) -> Result<
    (
        [(axum::http::header::HeaderName, axum::http::HeaderValue); 2],
        Vec<u8>,
    ),
    (StatusCode, String),
> {
    let path = crate::version::static_dir_path()
        .join("install")
        .join(filename);
    let bundle_size = tokio::fs::metadata(&path).await.map_err(|error| {
        (
            StatusCode::NOT_FOUND,
            format!("download file not available: {} ({})", path.display(), error),
        )
    })?.len();
    if bundle_size > MAX_STATE_BUNDLE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "download file is too large".to_string(),
        ));
    }
    let bytes = read_bounded_async_file(&path, MAX_STATE_BUNDLE_BYTES)
        .await
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                format!("download file not available: {} ({})", path.display(), e),
            )
        })?;
    Ok((attachment_headers(filename, content_type), bytes))
}

async fn download_player_client_bundle_handler() -> Result<
    (
        [(axum::http::header::HeaderName, axum::http::HeaderValue); 2],
        Vec<u8>,
    ),
    (StatusCode, String),
> {
    download_static_install_file("ripley-player-client-bundle.tar.gz", "application/gzip").await
}

async fn download_player_client_bootstrap_handler() -> Result<
    (
        [(axum::http::header::HeaderName, axum::http::HeaderValue); 2],
        Vec<u8>,
    ),
    (StatusCode, String),
> {
    download_static_install_file(
        "ripley-player-client-bundle-install.sh",
        "text/x-shellscript; charset=utf-8",
    )
    .await
}

async fn download_screentra_cast_installers_handler() -> Result<
    (
        [(axum::http::header::HeaderName, axum::http::HeaderValue); 2],
        Vec<u8>,
    ),
    (StatusCode, String),
> {
    download_static_install_file(
        "screentra-cast-installers.tar.gz",
        "application/gzip",
    )
    .await
}

async fn restart_service_handler() -> Result<StatusCode, (StatusCode, String)> {
    tokio::task::spawn_blocking(crate::version::restart_configured_service)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("restart worker: {error}")))?
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))
}

// ── System Time / Timezone handlers ───────────────────────

#[derive(serde::Serialize)]
struct SystemTimeInfo {
    /// Local time in RFC3339 format (e.g. "2026-07-23T09:48:59-04:00")
    local_time: String,
    /// Local time formatted for display
    local_time_human: String,
    /// UTC time in RFC3339 format
    utc_time: String,
    /// Current IANA timezone (e.g. "America/New_York")
    timezone: String,
    /// Timezone abbreviation (e.g. "EDT")
    timezone_abbr: String,
    /// UTC offset in seconds
    utc_offset_seconds: i32,
    /// Whether NTP is active and synchronized
    ntp_active: bool,
    ntp_synchronized: bool,
    /// Whether the system clock is set
    rtc_time: String,
}

#[derive(serde::Deserialize)]
struct SetTimezoneBody {
    timezone: String,
}

#[derive(serde::Deserialize)]
struct SetClockBody {
    /// ISO 8601 timestamp, e.g. "2026-07-23T10:30:00" (treated as local time)
    local_datetime: String,
}

fn read_bounded_file(path: &StdPath, maximum_bytes: usize) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("file is not regular".to_string());
    }
    if metadata.len() > maximum_bytes as u64 {
        return Err("file exceeds its bound".to_string());
    }
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take((maximum_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > maximum_bytes {
        return Err("file exceeds its bound".to_string());
    }
    Ok(bytes)
}

async fn read_response_json_limited<T: DeserializeOwned>(
    response: reqwest::Response,
    maximum_bytes: usize,
    label: &str,
) -> Result<T, String> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        return Err(format!("{label} exceeds response size limit"));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("{label}: {error}"))?;
        if bytes.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(format!("{label} exceeds response size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("{label}: {error}"))
}

async fn read_bounded_async_file(path: &StdPath, maximum_bytes: u64) -> Result<Vec<u8>, String> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("file is not regular".to_string());
    }
    if metadata.len() > maximum_bytes {
        return Err("file exceeds its bound".to_string());
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(metadata.len().min(maximum_bytes) as usize);
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum_bytes {
        return Err("file exceeds its bound".to_string());
    }
    Ok(bytes)
}

fn bounded_command_output(
    mut command: Command,
    maximum: usize,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn command: {}", e))?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("command stdout unavailable".to_string());
        }
    };
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take((maximum + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("failed to read command output: {}", e))?;
        Ok::<Vec<u8>, String>(bytes)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("command exceeded its timeout".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(format!("failed to wait for command: {}", error));
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "command output reader panicked".to_string())??;
    if bytes.len() > maximum {
        return Err("command output exceeds its bound".to_string());
    }
    Ok((status, bytes))
}

async fn terminate_bounded_child(
    child: &mut tokio::process::Child,
    write_task: &tokio::task::JoinHandle<Result<(), String>>,
    message: String,
) -> Result<std::process::Output, String> {
    let _ = child.kill().await;
    let _ = child.wait().await;
    write_task.abort();
    Err(message)
}

async fn bounded_async_status(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> Result<std::process::ExitStatus, String> {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn command: {error}"))?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => Err(format!("wait command: {error}")),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err("command exceeded its timeout".to_string())
        }
    }
}

async fn bounded_async_command_output(
    mut command: tokio::process::Command,
    input: Vec<u8>,
    maximum: usize,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn command: {error}"))?;
    let stdin = child.stdin.take();
    let write_task = tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            stdin
                .write_all(&input)
                .await
                .map_err(|error| error.to_string())?;
            stdin.shutdown().await.map_err(|error| error.to_string())?;
        }
        Ok::<(), String>(())
    });
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "command stdout unavailable".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "command stderr unavailable".to_string())?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut stdout_chunk = [0_u8; 8192];
    let mut stderr_chunk = [0_u8; 8192];
    let started = Instant::now();
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    while !stdout_done || !stderr_done {
        tokio::select! {
            result = stdout.read(&mut stdout_chunk), if !stdout_done => {
                match result {
                    Ok(0) => stdout_done = true,
                    Ok(count) => {
                        stdout_bytes.extend_from_slice(&stdout_chunk[..count]);
                        if stdout_bytes.len() > maximum {
                            return terminate_bounded_child(&mut child, &write_task, "command stdout exceeds its bound".to_string()).await;
                        }
                    }
                    Err(error) => return terminate_bounded_child(&mut child, &write_task, format!("read command stdout: {error}")).await,
                }
            }
            result = stderr.read(&mut stderr_chunk), if !stderr_done => {
                match result {
                    Ok(0) => stderr_done = true,
                    Ok(count) => {
                        stderr_bytes.extend_from_slice(&stderr_chunk[..count]);
                        if stderr_bytes.len() > maximum {
                            return terminate_bounded_child(&mut child, &write_task, "command stderr exceeds its bound".to_string()).await;
                        }
                    }
                    Err(error) => return terminate_bounded_child(&mut child, &write_task, format!("read command stderr: {error}")).await,
                }
            }
            _ = &mut deadline => {
                return terminate_bounded_child(&mut child, &write_task, "command exceeded its timeout".to_string()).await;
            }
        }
    }

    let remaining = timeout.saturating_sub(started.elapsed());
    let status = match tokio::time::timeout(remaining, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return terminate_bounded_child(&mut child, &write_task, format!("wait for command: {error}")).await,
        Err(_) => return terminate_bounded_child(&mut child, &write_task, "command exceeded its timeout".to_string()).await,
    };
    let _ = write_task.await;
    Ok(std::process::Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}

fn run_sudo(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("sudo");
    command.arg("-n").args(args);
    let (status, bytes) = bounded_command_output(command, 64 * 1024, Duration::from_secs(30))?;
    if !status.success() {
        return Err(format!(
            "sudo {} failed (exit {:?})",
            args.join(" "),
            status.code(),
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

async fn get_system_time() -> Result<AxumJson<SystemTimeInfo>, (StatusCode, String)> {
    // Use timedatectl to get authoritative status
    let mut timedatectl = Command::new("timedatectl");
    timedatectl.arg("show");
    let (status, stdout_bytes) = bounded_command_output(
        timedatectl,
        64 * 1024,
        Duration::from_secs(30),
    ).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("timedatectl: {}", e),
        )
    })?;
    if !status.success() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("timedatectl show failed: {:?}", status.code()),
        ));
    }
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let mut timezone = String::new();
    let mut ntpsynchronized = false;
    let mut ntp = false;
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("Timezone=") {
            timezone = v.to_string();
        } else if let Some(v) = line.strip_prefix("NTPSynchronized=") {
            ntpsynchronized = v == "yes";
        } else if let Some(v) = line.strip_prefix("NTP=") {
            ntp = v == "yes";
        }
    }

    // Use chrono::Local for the local time, formatted with offset
    let now_local = chrono::Local::now();
    let now_utc = chrono::Utc::now();
    let offset = now_local.offset();
    let offset_seconds = offset.local_minus_utc();
    let offset_hours = offset_seconds / 3600;
    let offset_minutes = (offset_seconds.abs() % 3600) / 60;
    let offset_str = format!(
        "{}{:02}:{:02}",
        if offset_seconds >= 0 { "+" } else { "-" },
        offset_hours.abs(),
        offset_minutes
    );

    // Format local time with offset for RFC3339
    let local_rfc3339 = format!("{}{}", now_local.format("%Y-%m-%dT%H:%M:%S"), offset_str);

    // Get timezone abbreviation by parsing the date output
    let mut date_command = Command::new("date");
    date_command.arg("+%Z").env("TZ", &timezone);
    let abbr_output = bounded_command_output(date_command, 4096, Duration::from_secs(30))
        .ok()
        .filter(|(status, _)| status.success())
        .map(|(_, bytes)| String::from_utf8_lossy(&bytes).trim().to_string())
        .unwrap_or_else(|| timezone.split('/').next_back().unwrap_or("").to_string());

    Ok(AxumJson(SystemTimeInfo {
        local_time: local_rfc3339,
        local_time_human: now_local.format("%Y-%m-%d %I:%M:%S %p").to_string(),
        utc_time: now_utc.to_rfc3339(),
        timezone,
        timezone_abbr: abbr_output,
        utc_offset_seconds: offset_seconds,
        ntp_active: ntp,
        ntp_synchronized: ntpsynchronized,
        rtc_time: String::new(), // Could query, but not critical
    }))
}

async fn set_timezone(
    AxumJson(body): AxumJson<SetTimezoneBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let tz = body.timezone.trim();
    if tz.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "timezone cannot be empty".to_string(),
        ));
    }
    // Verify the timezone exists in the zoneinfo database
    let zoneinfo_path = format!("/usr/share/zoneinfo/{}", tz);
    if !std::path::Path::new(&zoneinfo_path).exists() {
        return Err((StatusCode::BAD_REQUEST, format!("Unknown timezone: {}", tz)));
    }
    log::info!("Setting timezone to {}", tz);
    run_sudo(&["timedatectl", "set-timezone", tz])
        .map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_clock(
    AxumJson(body): AxumJson<SetClockBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    // The body.local_datetime is interpreted in the CURRENT system timezone.
    // We use timedatectl set-time which takes "YYYY-MM-DD HH:MM:SS" and interprets
    // it in the system timezone. Because NTP may be enabled (which prevents manual
    // time changes), we temporarily disable NTP, set the time, then re-enable NTP.
    let s = body.local_datetime.trim();
    if s.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "local_datetime cannot be empty".to_string(),
        ));
    }
    log::info!("Setting system clock to {}", s);
    // Disable NTP
    let _ = run_sudo(&["timedatectl", "set-ntp", "false"]);
    // Set the time
    let res = run_sudo(&["timedatectl", "set-time", s]);
    // Always try to re-enable NTP, regardless of whether the time-set succeeded
    let _ = run_sudo(&["timedatectl", "set-ntp", "true"]);
    res.map_err(|_| internal_server_error())?;
    Ok(StatusCode::NO_CONTENT)
}

async fn sync_ntp() -> Result<StatusCode, (StatusCode, String)> {
    log::info!("Triggering NTP sync");
    // Just restart systemd-timesyncd to force a re-sync from network
    // Try chrony first (most common on modern Ubuntu), fall back to systemd-timesyncd.
    let chrony_res = run_sudo(&["chronyc", "tracking"]);
    if chrony_res.is_ok() {
        run_sudo(&["chronyc", "makestep"]).map_err(|_| internal_server_error())?;
    } else {
        run_sudo(&["systemctl", "restart", "systemd-timesyncd"])
            .map_err(|_| internal_server_error())?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn vacuum_database(
    State(state): State<RuntimeState>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let db = state.legacy_db.clone().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "VACUUM is only supported for SQLite installations".to_string(),
    ))?;
    // Reclaim disk space from dropped tables / deleted rows by rebuilding the
    // SQLite file. VACUUM requires no other connections holding a write lock
    // and rewrites the file in place, so the file size can drop significantly
    // after large DROPs or bulk deletes.
    //
    // Returns the file size before and after so operators can see the effect.
    use std::path::PathBuf;
    let path: PathBuf = std::env::var("SIGNAGE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/board/data"))
        .join("signage.db");
    let size_before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    db.vacuum().map_err(|_| internal_server_error())?;
    let size_after = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    log::info!(
        "VACUUM complete: {} bytes -> {} bytes (saved {})",
        size_before,
        size_after,
        size_before.saturating_sub(size_after)
    );
    Ok(AxumJson(serde_json::json!({
        "size_before": size_before,
        "size_after": size_after,
        "reclaimed": size_before.saturating_sub(size_after),
    })))
}

/// POST /api/system/maintenance/purge-sessions
///
/// Delete all `sessions` rows whose `expires_at` is in the past.
/// Sessions are written on every login, so this table grows without
/// bound if never cleaned. Idempotent. The caller may want to VACUUM
/// afterwards to reclaim disk space.
async fn purge_sessions(
    State(state): State<crate::auth::AuthAppState>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let rows = state
        .repository
        .purge_expired_sessions()
        .await
        .map_err(|_| internal_server_error())?;
    log::info!("purged {rows} expired sessions");
    Ok(AxumJson(serde_json::json!({"deleted": rows})))
}

/// GET /api/system/health
///
/// Lightweight health probe for monitoring (Uptime Kuma, Prometheus
/// blackbox, etc.). Returns:
///   * `db_ok` - true (always, if the request returned at all)
///   * `stale_clients` - count of clients whose last_seen is more
///     than 5 minutes ago (and not virtual)
///   * `server_time` - RFC3339 timestamp
///   * `version` - server binary version string
/// Does NOT require auth (so a load balancer can probe it), but the
/// endpoint is cheap and leaks only counts / time.
async fn get_system_health(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let stale = repository
        .count_stale_clients(5 * 60)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({
        "db_ok": true,
        "stale_clients": stale,
        "server_time": chrono::Utc::now().to_rfc3339(),
        "version": crate::version::VERSION,
    })))
}

/// Process liveness probe. This intentionally does not touch the database.
async fn get_system_liveness() -> AxumJson<serde_json::Value> {
    AxumJson(serde_json::json!({
        "status": "alive",
        "server_time": chrono::Utc::now().to_rfc3339(),
        "version": crate::version::VERSION,
    }))
}

/// Readiness probe. A database query is required before reporting ready.
async fn get_system_readiness(
    State(state): State<RuntimeState>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, AxumJson<serde_json::Value>)> {
    match state.client_repository.count_stale_clients(5 * 60).await {
        Ok(_) => {
            state.metrics.record_readiness(true);
            Ok(AxumJson(serde_json::json!({
                "status": "ready",
                "db_ok": true,
                "server_time": chrono::Utc::now().to_rfc3339(),
                "version": crate::version::VERSION,
            })))
        }
        Err(_) => {
            state.metrics.record_readiness(false);
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                AxumJson(serde_json::json!({
                    "status": "not_ready",
                    "db_ok": false,
                    "server_time": chrono::Utc::now().to_rfc3339(),
                })),
            ))
        }
    }
}

async fn get_metrics(State(state): State<RuntimeState>) -> axum::response::Response {
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
        .body(axum::body::Body::from(state.metrics.render_prometheus()))
        .expect("metrics response builder")
}

/// GET /api/schedule-runs?limit=N
///
/// Return the most recent `schedule_runs` audit log rows, newest
/// first. Default limit 50, capped at 500. Each row records one
/// schedule fire: which schedule, what action, what target, whether
/// the dispatch succeeded, and a free-form message (e.g. the
/// reason a power_on schedule was skipped).
async fn list_schedule_runs(
    State(repository): State<ClientRepository>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<AxumJson<Vec<crate::models::ScheduleRun>>, (StatusCode, String)> {
    let limit: i64 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let rows = repository
        .list_schedule_runs(limit)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(rows))
}

// ── Reset default widget presets ────────────────────────────
// Reads the bundled default-templates.json, deletes any existing presets whose
// ID starts with "default-", and re-creates the defaults fresh.
async fn reset_default_widget_presets(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    // Read the default templates from the static file
    let path = std::path::Path::new("static/default-templates.json");
    let raw = String::from_utf8(read_bounded_file(path, 1024 * 1024).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("read default templates: {}", e),
        )
    })?)
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("read default templates: {}", e),
        )
    })?;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&raw).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse default templates: {}", e),
        )
    })?;

    // Delete existing defaults (everything with id starting with "default-")
    let existing = repository
        .list_widget_presets(None)
        .await
        .map_err(|_| internal_server_error())?;
    let mut deleted = 0;
    for p in existing {
        if p.id.starts_with("default-") {
            repository
                .delete_widget_preset(&p.id)
                .await
                .map_err(|_| internal_server_error())?;
            deleted += 1;
        }
    }

    // Re-create from the file
    let now = chrono::Utc::now().to_rfc3339();
    let mut created = 0;
    for item in &arr {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Default")
            .to_string();
        let widget_type = item
            .get("widget_type")
            .and_then(|v| v.as_str())
            .unwrap_or("text")
            .to_string();
        let props = item.get("props").cloned().unwrap_or(serde_json::json!({}));
        let assets = item.get("assets").cloned().unwrap_or(serde_json::json!([]));
        if id.is_empty() {
            continue;
        }
        let props_json = serde_json::to_string(&props).unwrap_or_else(|_| "{}".to_string());
        repository
            .create_widget_preset(&id, &name, &widget_type, &props_json, &now)
            .await
            .map_err(|_| internal_server_error())?;
        if let Some(a) = assets.as_array() {
            for asset in a {
                let kind = asset
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("weather_icon")
                    .to_string();
                let key = asset
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .to_string();
                let url = asset
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                validate_widget_preset_asset(&kind, &key, &url)?;
                let asset_id = uuid::Uuid::new_v4().to_string();
                repository
                    .upsert_widget_preset_asset(&WidgetPresetAsset {
                        id: asset_id,
                        preset_id: id.clone(),
                        kind,
                        key,
                        url,
                    })
                    .await
                    .map_err(|_| internal_server_error())?;
            }
        }
        created += 1;
    }

    Ok(AxumJson(serde_json::json!({
        "deleted": deleted,
        "created": created,
        "message": format!("Reset {} default template(s) and created {} fresh", deleted, created),
    })))
}

#[derive(Debug, Deserialize)]
struct InitialAdminRequest {
    username: String,
    #[serde(default)]
    display_name: Option<String>,
    password: String,
}

async fn setup_status(
    State(state): State<crate::auth::AuthAppState>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let active_admins = state
        .repository
        .count_active_admins()
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({
        "setup_required": active_admins == 0,
    })))
}

async fn create_initial_admin(
    State(state): State<crate::auth::AuthAppState>,
    Json(payload): Json<InitialAdminRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let username = normalize_username(&payload.username)?;
    if username == "admin" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Choose a username other than the built-in admin name".to_string(),
        ));
    }
    if payload.password.len() < 12 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Initial admin password must be at least 12 characters".to_string(),
        ));
    }
    let display_name = payload
        .display_name
        .unwrap_or_else(|| username.clone())
        .trim()
        .to_string();
    let now = Utc::now().to_rfc3339();
    let id = Uuid::new_v4().to_string();
    let hash = crate::auth::hash_password(&payload.password);
    let created = state
        .repository
        .create_initial_admin(&id, &username, &display_name, &hash, &now)
        .await
        .map_err(|e| {
            if e.to_ascii_lowercase().contains("unique") {
                (StatusCode::CONFLICT, "Username already exists".to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e)
            }
        })?;
    if !created {
        return Err((
            StatusCode::CONFLICT,
            "Initial setup is already complete".to_string(),
        ));
    }
    let token = state.auth.issue_for(&username, "admin");
    let expires_at = Utc::now().timestamp() + state.auth.ttl_seconds() as i64;
    state
        .repository
        .create_session(&token, &username, expires_at)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({
        "token": token,
        "expires_in": state.auth.ttl_seconds(),
        "username": username,
        "role": "admin",
        "setup_required": false,
    })))
}

#[derive(Debug, Serialize)]
struct UserInfo {
    id: String,
    username: String,
    display_name: String,
    role: String,
    active: bool,
    created_at: String,
    updated_at: String,
    last_login_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateUserRequest {
    username: String,
    #[serde(default)]
    display_name: Option<String>,
    role: String,
    password: String,
    #[serde(default = "default_true")]
    active: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateUserRequest {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

fn default_true() -> bool {
    true
}

fn normalize_username(username: &str) -> Result<String, (StatusCode, String)> {
    let u = username.trim().to_lowercase();
    if u.len() < 2 || u.len() > 48 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Username must be 2-48 characters".to_string(),
        ));
    }
    if !u
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Username may only contain letters, numbers, dot, dash, and underscore".to_string(),
        ));
    }
    Ok(u)
}

fn normalize_role(role: &str) -> Result<String, (StatusCode, String)> {
    let r = role.trim().to_lowercase();
    match r.as_str() {
        "admin" | "layout-manager" | "operator" | "viewer" => Ok(r),
        _ => Err((
            StatusCode::BAD_REQUEST,
            "Role must be admin, layout-manager, operator, or viewer".to_string(),
        )),
    }
}

fn user_from_record(record: crate::db::UserRecord) -> UserInfo {
    UserInfo {
        id: record.id,
        username: record.username,
        display_name: record.display_name,
        role: record.role,
        active: record.active,
        created_at: record.created_at,
        updated_at: record.updated_at,
        last_login_at: record.last_login_at,
    }
}

async fn require_admin_user_manager(
    auth: &crate::auth::AuthState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, String)> {
    let payload = match crate::auth::extract_token(headers).as_deref() {
        Some(token) => auth.token_payload(token).await,
        None => None,
    };
    let role = payload
        .as_ref()
        .and_then(|p| p.get("role"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if role == "admin" {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "Admin role required".to_string()))
    }
}

async fn list_users(
    State(state): State<crate::auth::AuthAppState>,
    headers: HeaderMap,
) -> Result<AxumJson<Vec<UserInfo>>, (StatusCode, String)> {
    require_admin_user_manager(&state.auth, &headers).await?;
    let users = state
        .repository
        .list_users()
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(users.into_iter().map(user_from_record).collect()))
}

async fn get_user(
    State(state): State<crate::auth::AuthAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<AxumJson<UserInfo>, (StatusCode, String)> {
    require_admin_user_manager(&state.auth, &headers).await?;
    let user = state
        .repository
        .get_user(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::NOT_FOUND, "User not found".to_string()))?;
    Ok(AxumJson(user_from_record(user)))
}

async fn create_user(
    State(state): State<crate::auth::AuthAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateUserRequest>,
) -> Result<AxumJson<UserInfo>, (StatusCode, String)> {
    require_admin_user_manager(&state.auth, &headers).await?;
    let username = normalize_username(&payload.username)?;
    let role = normalize_role(&payload.role)?;
    if payload.password.len() < 8 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Password must be at least 8 characters".to_string(),
        ));
    }
    let display_name = payload
        .display_name
        .unwrap_or_else(|| username.clone())
        .trim()
        .to_string();
    let now = Utc::now().to_rfc3339();
    let id = Uuid::new_v4().to_string();
    let hash = crate::auth::hash_password(&payload.password);
    state
        .repository
        .create_user(
            &id,
            &username,
            &display_name,
            &role,
            &hash,
            payload.active,
            &now,
        )
        .await
        .map_err(|e| {
            if e.contains("UNIQUE") {
                (StatusCode::CONFLICT, "Username already exists".to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e)
            }
        })?;
    let user = state
        .repository
        .get_user(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "User insert failed".to_string()))?;
    Ok(AxumJson(user_from_record(user)))
}

async fn update_user(
    State(state): State<crate::auth::AuthAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<UpdateUserRequest>,
) -> Result<AxumJson<UserInfo>, (StatusCode, String)> {
    require_admin_user_manager(&state.auth, &headers).await?;
    let current = state
        .repository
        .get_user(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::NOT_FOUND, "User not found".to_string()))?;
    let username = match payload.username {
        Some(v) => normalize_username(&v)?,
        None => current.username.clone(),
    };
    let display_name = payload
        .display_name
        .unwrap_or_else(|| current.display_name.clone())
        .trim()
        .to_string();
    let role = match payload.role {
        Some(v) => normalize_role(&v)?,
        None => current.role.clone(),
    };
    let active = payload.active.unwrap_or(current.active);
    if current.role == "admin"
        && current.active
        && (role != "admin" || !active)
        && state
            .repository
            .count_active_admins()
            .await
            .unwrap_or(0)
            <= 1
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot demote or disable the last active admin user".to_string(),
        ));
    }
    let now = Utc::now().to_rfc3339();
    let password_hash = if let Some(password) = payload.password {
        if password.len() < 8 {
            return Err((StatusCode::BAD_REQUEST, "Password must be at least 8 characters".to_string()));
        }
        Some(crate::auth::hash_password(&password))
    } else {
        None
    };
    let updated = state
        .repository
        .update_user(
            &id,
            &username,
            &display_name,
            &role,
            password_hash.as_deref(),
            active,
            &now,
        )
        .await
        .map_err(|e| {
            if e.contains("UNIQUE") {
                (StatusCode::CONFLICT, "Username already exists".to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e)
            }
        })?;
    if !updated {
        return Err((StatusCode::NOT_FOUND, "User not found".to_string()));
    }
    let user = state
        .repository
        .get_user(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "User update failed".to_string()))?;
    Ok(AxumJson(user_from_record(user)))
}

async fn delete_user(
    State(state): State<crate::auth::AuthAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    require_admin_user_manager(&state.auth, &headers).await?;
    let current = state
        .repository
        .get_user(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::NOT_FOUND, "User not found".to_string()))?;
    if current.role == "admin"
        && current.active
        && state
            .repository
            .count_active_admins()
            .await
            .unwrap_or(0)
            <= 1
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot delete the last active admin user".to_string(),
        ));
    }
    let deleted = state
        .repository
        .delete_user_record(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(
        serde_json::json!({"deleted": deleted, "username": current.username}),
    ))
}

#[derive(Debug, Serialize)]
struct ConnectedClientInfo {
    id: String,
    pairing_code: String,
    name: String,
    address: String,
    role: String,
    status: String,
    enrolled: bool,
    version: Option<String>,
    uptime_seconds: Option<u64>,
    cpu_usage: Option<f32>,
    memory_used: Option<u64>,
    memory_total: Option<u64>,
    disk_used: Option<u64>,
    disk_total: Option<u64>,
    active_layout_id: Option<String>,
    assigned_layout_id: Option<String>,
    last_seen: String,
    last_command_result: Option<serde_json::Value>,
    last_command_at: Option<String>,
    max_displays: Option<i64>,
    installer_detected_max_displays: Option<i64>,
    touch_binding: Option<serde_json::Value>,
    theme: Option<String>,
    is_virtual: bool,
}

/// Short physical-confirmation code shared by the server card and the client
/// display. It is a correlation value, not a credential.
fn client_pairing_code(client_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"screentra-cast-pairing-v1\0");
    hasher.update(client_id.as_bytes());
    let digest = hasher.finalize();
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{value:06}")
}

#[derive(Debug, Deserialize)]
struct CreateVirtualClientRequest {
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct VirtualClientResponse {
    id: String,
    name: String,
    url: String,
}
#[derive(Debug, Deserialize)]struct SetVirtualClientLayoutRequest {    layout_id: Option<String>,}

#[derive(Debug, Deserialize)]
struct ClientRegisterRequest {
    client_id: Option<String>,
    hostname: Option<String>,
    address: Option<String>,
    role: Option<String>,
    version: Option<String>,
    uptime_seconds: Option<u64>,
    cpu_usage: Option<f32>,
    memory_used: Option<u64>,
    memory_total: Option<u64>,
    disk_used: Option<u64>,
    disk_total: Option<u64>,
    active_layout_id: Option<String>,
    #[serde(default)]
    active_layout_revision: Option<i64>,
    #[serde(default)]
    last_command_result: Option<serde_json::Value>,
    #[serde(default)]
    last_command_at: Option<String>,
    #[serde(default)]
    displays: Vec<DisplayReport>,
    /// True when this payload came from the installed Ripley player client.
    /// This keeps the client protocol explicit and distinguishes real players
    /// from generic LAN devices found by network discovery.
    #[serde(default)]
    client_agent: bool,
    /// Human-readable identity source, e.g. "persistent_client_id". The server
    /// keys persistence on client_id, so address changes update the same row.
    #[serde(default)]
    identity_source: Option<String>,
    /// Number of displays the player's installer detected at install time
    /// (count of DRM connectors with `status=connected`).
    /// Stored verbatim as `clients.installer_detected_max_displays`.
    #[serde(default)]
    installer_detected_max_displays: Option<i64>,
    /// Operator-set cap (NULL means no override, follow installer_detected).
    /// Stored verbatim as `clients.max_displays`.
    #[serde(default)]
    max_displays: Option<i64>,
    #[serde(default)]
    touch_binding: Option<serde_json::Value>,
    #[serde(default)]
    theme: Option<String>,
    /// Raw player credential supplied only over the verified TLS connection;
    /// the server hashes it immediately and never persists or logs it.
    #[serde(default)]
    client_auth_token: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct DisplayReport {
    display_index: i64,
    #[serde(default)]
    connector: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    refresh_rate: Option<f32>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    active_layout_id: Option<String>,
    #[serde(default)]
    active_layout_revision: Option<i64>,
    #[serde(default)]
    last_error: Option<String>,
}

async fn adopt_client(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    if id.trim().is_empty() || id.len() > 200 {
        return Err((StatusCode::BAD_REQUEST, "Invalid client id".to_string()));
    }
    let changed = repository
        .set_client_enrolled(&id)
        .await
        .map_err(|_| internal_server_error())?;
    if !changed {
        let exists = repository
            .get_client_virtual_flag(&id)
            .await
            .map_err(|_| internal_server_error())?;
        return Err(match exists {
            Some(true) => (
                StatusCode::BAD_REQUEST,
                "Virtual clients do not require adoption".to_string(),
            ),
            Some(false) => (StatusCode::CONFLICT, "Client is already adopted".to_string()),
            None => (StatusCode::NOT_FOUND, "Client not found".to_string()),
        });
    }
    Ok(AxumJson(serde_json::json!({"id": id, "enrolled": true})))
}

async fn connected_clients(State(repository): State<ClientRepository>) -> AxumJson<Vec<ConnectedClientInfo>> {
    let rows = repository.list_registered_clients().await.unwrap_or_default();
    AxumJson(collect_connected_clients_from_rows(rows, false))
}

async fn search_connected_clients(
    State(repository): State<ClientRepository>,
) -> AxumJson<Vec<ConnectedClientInfo>> {
    let rows = repository.list_registered_clients().await.unwrap_or_default();
    AxumJson(collect_connected_clients_from_rows(rows, true))
}

#[derive(Debug, Deserialize)]
struct ClientAssignmentRequest {
    layout_id: Option<String>,
}

async fn get_client_assignment(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let layout_id = repository
        .get_client_assignment(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(
        serde_json::json!({ "client_id": id, "layout_id": layout_id }),
    ))
}

async fn set_client_assignment(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(payload): Json<ClientAssignmentRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    repository
        .set_client_assignment(&id, payload.layout_id.as_deref())
        .await
        .map_err(|_| internal_server_error())?;
    let layout_id = repository
        .get_client_assignment(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(
        serde_json::json!({ "client_id": id, "layout_id": layout_id }),
    ))
}

#[derive(Debug, Deserialize)]
struct ClientDisplayCountRequest {
    max_displays: Option<i64>,
}

async fn set_client_display_count(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(payload): Json<ClientDisplayCountRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let Some((installer, active)) = repository
        .set_client_display_count(&id, payload.max_displays)
        .await
        .map_err(|_| internal_server_error())?
    else {
        return Err((StatusCode::NOT_FOUND, "client not found".to_string()));
    };
    let command_payload = serde_json::json!({"max_displays": active});
    let queued = queue_repository_command(
        &repository,
        &id,
        "set_display_count",
        Some(&command_payload),
        Some("web-ui"),
    )
    .await
    .map_err(command_queue_http_error)?;
    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "installer_detected_max_displays": installer,
        "max_displays": active,
        "active_max_displays": active,
        "queued_command": queued,
    })))
}

#[derive(Debug, Deserialize)]
struct ClientThemeRequest {
    theme: String,
}

#[derive(Debug, Deserialize)]
struct ClientTouchBindingRequest {
    #[serde(default)]
    device_path: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    target_display_indexes: Vec<i64>,
    #[serde(default)]
    allow_joined_displays: bool,
}

async fn set_client_touch_binding(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(payload): Json<ClientTouchBindingRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let mut indexes: Vec<i64> = payload
        .target_display_indexes
        .iter()
        .filter_map(|v| if (0..=3).contains(v) { Some(*v) } else { None })
        .collect();
    indexes.sort_unstable();
    indexes.dedup();
    if indexes.is_empty() {
        indexes.push(0);
    }
    if payload.allow_joined_displays {
        indexes.truncate(2);
        if indexes.len() != 2 {
            return Err((
                StatusCode::BAD_REQUEST,
                "joined/tiled touch mode requires two distinct display indexes".to_string(),
            ));
        }
    } else {
        indexes.truncate(1);
    }
    if !repository
        .client_exists(&id)
        .await
        .map_err(|_| internal_server_error())?
    {
        return Err((StatusCode::NOT_FOUND, "client not found".to_string()));
    }
    let command_payload = serde_json::json!({
        "device_path": payload.device_path,
        "device_name": payload.device_name,
        "target_display_indexes": indexes,
        "allow_joined_displays": payload.allow_joined_displays,
        "requested_by": "web-ui",
    });
    let binding_json = serde_json::to_string(&command_payload).unwrap_or_default();
    repository
        .set_client_touch_binding(&id, &binding_json)
        .await
        .map_err(|_| internal_server_error())?;
    let queued = queue_repository_command(
        &repository,
        &id,
        "set_touch_binding",
        Some(&command_payload),
        Some("web-ui"),
    )
    .await
    .map_err(command_queue_http_error)?;
    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "touch_binding": command_payload,
        "queued_command": queued,
    })))
}


async fn set_client_theme(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(payload): Json<ClientThemeRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let updated = repository
        .set_client_theme(&id, &payload.theme)
        .await
        .map_err(|_| internal_server_error())?;
    if !updated {
        return Err((StatusCode::NOT_FOUND, "client not found".to_string()));
    }
    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "theme": payload.theme,
    })))
}

async fn get_client_admin_users(
    State(repository): State<ClientRepository>,
    Extension(auth): Extension<crate::auth::AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    // This route remains reachable by the player heartbeat, but it is not
    // public: an enrolled client credential or an authenticated admin session
    // is required. The repository intentionally returns metadata only.
    let authenticated_role = if let Some(token) = crate::auth::extract_token(&headers) {
        auth.token_payload(&token)
            .await
            .and_then(|payload| payload.get("role").and_then(|role| role.as_str()).map(str::to_string))
    } else {
        None
    };
    if let Some(role) = authenticated_role {
        if role != "admin" {
            return Err((StatusCode::FORBIDDEN, "admin authority required".to_string()));
        }
    } else {
        require_client_command_token(&repository, &id, &headers).await?;
    }
    let Some((enrolled, users)) = repository
        .list_client_admin_users(&id)
        .await
        .map_err(|_| internal_server_error())?
    else {
        return Err((StatusCode::NOT_FOUND, "client not enrolled".to_string()));
    };
    if !enrolled {
        return Err((StatusCode::FORBIDDEN, "client not enrolled".to_string()));
    }
    let users: Vec<serde_json::Value> = users
        .into_iter()
        .map(|user| serde_json::json!({
            "username": user.username,
            "display_name": user.display_name,
            "role": user.role,
        }))
        .collect();
    Ok(AxumJson(serde_json::json!({
        "synced_at": Utc::now().to_rfc3339(),
        "source": "server_users",
        "users": users,
    })))
}

async fn get_client_addons(
    State(repository): State<ClientRepository>,
    Extension(auth): Extension<crate::auth::AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let authenticated_role = if let Some(token) = crate::auth::extract_token(&headers) {
        auth.token_payload(&token)
            .await
            .and_then(|payload| payload.get("role").and_then(|role| role.as_str()).map(str::to_string))
    } else {
        None
    };
    if let Some(role) = authenticated_role {
        if role != "admin" {
            return Err((StatusCode::FORBIDDEN, "admin authority required".to_string()));
        }
    } else {
        require_client_command_token(&repository, &id, &headers).await?;
    }
    let addons = crate::addons::enabled_client_extensions().map_err(addon_error_response)?;
    let status = crate::addons::load_client_status(&id).map_err(addon_error_response)?;
    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "client_api_version": "1",
        "addons": addons,
        "status": status,
    })))
}

async fn post_client_addon_status(
    State(_repository): State<ClientRepository>,
    Extension(auth): Extension<crate::auth::AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(report): Json<ClientAddonStatusReport>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let authenticated_role = if let Some(token) = crate::auth::extract_token(&headers) {
        auth.token_payload(&token)
            .await
            .and_then(|payload| payload.get("role").and_then(|role| role.as_str()).map(str::to_string))
    } else {
        None
    };
    if let Some(role) = authenticated_role {
        if role != "admin" {
            return Err((StatusCode::FORBIDDEN, "admin authority required".to_string()));
        }
    } else {
        require_client_command_token(&_repository, &id, &headers).await?;
    }
    crate::addons::save_client_status(&id, report)
        .map_err(addon_error_response)?;
    Ok(AxumJson(serde_json::json!({"client_id": id, "saved": true})))
}

async fn get_client_addon_package(
    State(repository): State<ClientRepository>,
    Extension(auth): Extension<crate::auth::AuthState>,
    headers: HeaderMap,
    Path((id, addon_id)): Path<(String, String)>,
) -> Result<Response, (StatusCode, String)> {
    let authenticated_role = if let Some(token) = crate::auth::extract_token(&headers) {
        auth.token_payload(&token)
            .await
            .and_then(|payload| payload.get("role").and_then(|role| role.as_str()).map(str::to_string))
    } else {
        None
    };
    if let Some(role) = authenticated_role {
        if role != "admin" {
            return Err((StatusCode::FORBIDDEN, "admin authority required".to_string()));
        }
    } else {
        require_client_command_token(&repository, &id, &headers).await?;
    }
    let package = crate::addons::client_package(&addon_id).map_err(addon_error_response)?;
    let mut response = Response::new(package.into());
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/gzip"),
    );
    response.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_static("attachment; filename=addon.tar.gz"),
    );
    Ok(response)
}

async fn get_client_config(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let layout_id = repository
        .get_client_assignment(&id)
        .await
        .map_err(|_| internal_server_error())?;
    let theme = repository
        .get_client_theme(&id)
        .await
        .map_err(|_| internal_server_error())?;
    let layout = if let Some(ref lid) = layout_id {
        repository
            .get_layout(lid)
            .await
            .map_err(|_| internal_server_error())?
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| internal_server_error())?
    } else {
        None
    };
    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "assigned_layout_id": layout_id,
        "theme": theme,
        "layout": layout,
        "sync_interval_seconds": 30,
        "server_time": Utc::now().to_rfc3339(),
    })))
}

async fn get_client_update_check(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Query(query): Query<ClientUpdateCheckQuery>,
    headers: HeaderMap,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    require_client_command_token(&repository, &id, &headers).await?;
    let current = query.current_sha256.as_deref().unwrap_or("");
    if !current.is_empty()
        && (current.len() != 64
            || !current
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err((StatusCode::BAD_REQUEST, "current_sha256 must be a 64-character hexadecimal digest".to_string()));
    }

    let static_dir = crate::version::static_dir_path();
    let installer = static_dir.join("install/player-client.sh");
    let installer_size = tokio::fs::metadata(&installer).await.map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "client update artifact is unavailable".to_string(),
        )
    })?.len();
    if installer_size > MAX_STATE_BUNDLE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "client update artifact is too large".to_string(),
        ));
    }
    let bytes = read_bounded_async_file(&installer, MAX_STATE_BUNDLE_BYTES)
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "client update artifact is unavailable".to_string(),
            )
        })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = format!("{:x}", hasher.finalize());
    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "update_required": current != digest,
        "version": crate::version::VERSION,
        "sha256": digest,
        "artifact_url": "/install/player-client.sh",
        "signature_url": "/install/player-client.sh.sig",
    })))
}

#[derive(Debug, Deserialize)]
struct BulkAssignRequest {
    client_ids: Vec<String>,
    layout_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct BulkAssignResult {
    updated: usize,
    client_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct NudgeResponse {
    client_id: String,
    requested_at: String,
    layout_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ObservedLayout {
    layout_id: String,
    revision: i64,
    observed_at: String,
}

#[derive(Debug, Default)]
struct NudgeRegistry {
    last_nudge: Mutex<HashMap<String, (String, Option<String>)>>,
    observed_layouts: Mutex<HashMap<String, ObservedLayout>>,
}

static NUDGE_REGISTRY: std::sync::OnceLock<NudgeRegistry> = std::sync::OnceLock::new();


fn nudge_registry() -> &'static NudgeRegistry {
    NUDGE_REGISTRY.get_or_init(NudgeRegistry::default)
}

async fn bulk_assign_clients(
    State(repository): State<ClientRepository>,
    Json(payload): Json<BulkAssignRequest>,
) -> Result<AxumJson<BulkAssignResult>, (StatusCode, String)> {
    let updated = repository
        .bulk_set_client_assignment(&payload.client_ids, payload.layout_id.as_deref())
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(BulkAssignResult {
        updated,
        client_ids: payload.client_ids,
    }))
}

#[derive(Debug, Serialize)]
struct DeleteClientResponse {
    deleted: bool,
    client_id: String,
}

#[derive(Debug, Serialize)]
struct HeartbeatRecord {
    recorded_at: String,
    cpu_usage: Option<f64>,
    memory_used: Option<i64>,
    memory_total: Option<i64>,
    disk_used: Option<i64>,
    disk_total: Option<i64>,
    uptime_seconds: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
struct ClientHistoryQuery {
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CommandRequest {
    action: String,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    requested_by: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct CommandRecord {
    client_id: String,
    action: String,
    payload: Option<serde_json::Value>,
    requested_at: String,
    requested_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClientUpdateCheckQuery {
    current_sha256: Option<String>,
}

const CLIENT_COMMAND_TTL_SECS: i64 = 180;
const MAX_CLIENT_COMMAND_PAYLOAD_BYTES: usize = 64 * 1024;

const ALLOWED_CLIENT_COMMAND_ACTIONS: &[&str] = &[
    "restart_renderer",
    "restart_kiosk",
    "restart_kiosk_display",
    "reload_layout",
    "refresh_cache",
    "update_client",
    "set_display_count",
    "set_touch_binding",
    "bluetooth_scan",
    "bluetooth_connect",
    "audio_list",
    "audio_set_default",
    "audio_set_volume",
    "restart_client",
    "start_display_preview",
    "stop_display_preview",
    "ddc_set",
    "display_settings",
    "display_power",
    "cec_send",
    "ir_send",
    "client_service_status",
    "client_service_rollback_request",
    "client_service_transcribe_request",
];

const CLIENT_SERVICE_IDENTIFIER_MAX_BYTES: usize = 128;
const CLIENT_SERVICE_STATUS_IDENTIFIER_MAX_BYTES: usize = 64;
const CLIENT_SERVICE_STATUS_REQUEST_MAX_BYTES: usize = 80;
const CLIENT_SERVICE_TRANSCRIBE_IDENTIFIER_MAX_BYTES: usize = 64;
const CLIENT_SERVICE_TRANSCRIBE_UNSAFE_MARKERS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "credential",
    "bearer",
    "api-key",
    "api_key",
    "apikey",
];
const CLIENT_SERVICE_REQUEST_ID_MAX_BYTES: usize = 128;
const CLIENT_SERVICE_TRANSCRIBE_ENVELOPE_MAX_BYTES: usize = 512;
const CLIENT_SERVICE_REASON_MAX_BYTES: usize = 256;

fn validate_client_service_identifier(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > CLIENT_SERVICE_IDENTIFIER_MAX_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(format!("{field} is invalid or exceeds its bound"));
    }
    Ok(())
}

fn transcription_identifier_contains_unsafe_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    CLIENT_SERVICE_TRANSCRIBE_UNSAFE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn validate_client_service_transcription_identifier(
    value: &str,
    field: &str,
) -> Result<(), String> {
    validate_client_service_identifier(value, field)?;
    if value.len() > CLIENT_SERVICE_TRANSCRIBE_IDENTIFIER_MAX_BYTES
        || transcription_identifier_contains_unsafe_marker(value)
    {
        return Err(format!("{field} is invalid or exceeds its bound"));
    }
    Ok(())
}

fn validate_client_service_status_identifier(value: &str, field: &str) -> Result<(), String> {
    validate_client_service_identifier(value, field)?;
    let valid_shape = value.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
    });
    if value.len() > CLIENT_SERVICE_STATUS_IDENTIFIER_MAX_BYTES
        || transcription_identifier_contains_unsafe_marker(value)
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !valid_shape
    {
        return Err(format!("{field} is invalid or exceeds its bound"));
    }
    Ok(())
}

fn validate_client_service_payload(
    action: &str,
    payload: Option<&serde_json::Value>,
) -> Result<(), String> {
    let Some(value) = payload else {
        if matches!(action, "client_service_status" | "client_service_rollback_request" | "client_service_transcribe_request") {
            return Err("client service request payload is required".to_string());
        }
        return Ok(());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "client service payload must be a JSON object".to_string())?;
    let allowed = match action {
        "client_service_status" => &["service_id"][..],
        "client_service_rollback_request" => {
            &["service_id", "activation_id", "target_version", "reason"][..]
        }
        "client_service_transcribe_request" => {
            &["service_id", "request_id", "push_to_talk"][..]
        }
        _ => return Ok(()),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("client service payload contains an unsupported field".to_string());
    }
    if action == "client_service_status"
        && serde_json::to_vec(value)
            .map_err(|_| "client service status request could not be serialized".to_string())?
            .len()
            > CLIENT_SERVICE_STATUS_REQUEST_MAX_BYTES
    {
        return Err(format!(
            "client service status request exceeds {} bytes",
            CLIENT_SERVICE_STATUS_REQUEST_MAX_BYTES
        ));
    }
    let required = if action == "client_service_status" {
        &["service_id"][..]
    } else if action == "client_service_rollback_request" {
        &["service_id", "activation_id"][..]
    } else if action == "client_service_transcribe_request" {
        &["service_id", "request_id", "push_to_talk"][..]
    } else {
        &[][..]
    };
    for field in required {
        if !object.contains_key(*field) {
            return Err(format!("client service payload is missing {field}"));
        }
    }
    for field in ["service_id", "activation_id", "target_version", "request_id"] {
        if let Some(value) = object.get(field) {
            let value = value
                .as_str()
                .ok_or_else(|| format!("client service {field} must be a string"))?;
            if field == "request_id" {
                if value.is_empty()
                    || value.len() > CLIENT_SERVICE_REQUEST_ID_MAX_BYTES
                    || !value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
                    || transcription_identifier_contains_unsafe_marker(value)
                {
                    return Err(format!("{field} is invalid or exceeds its bound"));
                }
            } else if action == "client_service_status" && field == "service_id" {
                validate_client_service_status_identifier(value, field)?;
            } else if action == "client_service_transcribe_request" && field == "service_id" {
                validate_client_service_transcription_identifier(value, field)?;
            } else {
                validate_client_service_identifier(value, field)?;
            }
        }
    }
    if action == "client_service_transcribe_request"
        && !object
            .get("push_to_talk")
            .is_some_and(|value| value == &serde_json::Value::Bool(true))
    {
        return Err("client service push_to_talk must be true".to_string());
    }
    if action == "client_service_transcribe_request"
        && serde_json::to_vec(value)
            .map_err(|_| "client service payload could not be serialized".to_string())?
            .len()
            > CLIENT_SERVICE_TRANSCRIBE_ENVELOPE_MAX_BYTES
    {
        return Err(format!(
            "client service transcribe request exceeds {} bytes",
            CLIENT_SERVICE_TRANSCRIBE_ENVELOPE_MAX_BYTES
        ));
    }
    if let Some(reason) = object.get("reason") {
        let reason = reason
            .as_str()
            .ok_or_else(|| "client service reason must be a string".to_string())?;
        if reason.is_empty()
            || reason.len() > CLIENT_SERVICE_REASON_MAX_BYTES
            || reason.chars().any(|character| character.is_control())
        {
            return Err("client service reason is invalid or exceeds its bound".to_string());
        }
    }
    Ok(())
}

fn validate_client_command_for_client(
    client_id: &str,
    action: &str,
    payload: Option<&serde_json::Value>,
) -> Result<(), String> {
    if !ALLOWED_CLIENT_COMMAND_ACTIONS.contains(&action) {
        return Err(format!("unsupported client command action: {action}"));
    }
    if client_id.is_empty() || client_id.len() > 200 || client_id.chars().any(|character| character.is_control()) {
        return Err("client command client identity is invalid".to_string());
    }
    if let Some(value) = payload {
        if !value.is_object() {
            return Err("client command payload must be a JSON object".to_string());
        }
        let size = serde_json::to_vec(value)
            .map_err(|_| "client command payload could not be serialized".to_string())?
            .len();
        if size > MAX_CLIENT_COMMAND_PAYLOAD_BYTES {
            return Err(format!(
                "client command payload exceeds {} bytes",
                MAX_CLIENT_COMMAND_PAYLOAD_BYTES
            ));
        }
        if value.get("client_id").is_some() {
            return Err("client identity is bound to the command envelope".to_string());
        }
    }
    validate_client_service_payload(action, payload)
}

fn validate_client_command(
    action: &str,
    payload: Option<&serde_json::Value>,
) -> Result<(), String> {
    if !ALLOWED_CLIENT_COMMAND_ACTIONS.contains(&action) {
        return Err(format!("unsupported client command action: {action}"));
    }
    if let Some(value) = payload {
        if !value.is_object() {
            return Err("client command payload must be a JSON object".to_string());
        }
        let size = serde_json::to_vec(value)
            .map_err(|_| "client command payload could not be serialized".to_string())?
            .len();
        if size > MAX_CLIENT_COMMAND_PAYLOAD_BYTES {
            return Err(format!(
                "client command payload exceeds {} bytes",
                MAX_CLIENT_COMMAND_PAYLOAD_BYTES
            ));
        }
    }
    validate_client_service_payload(action, payload)
}

fn command_is_stale(requested_at: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(requested_at)
        .map(|ts| {
            Utc::now()
                .signed_duration_since(ts.with_timezone(&Utc))
                .num_seconds()
                > CLIENT_COMMAND_TTL_SECS
        })
        .unwrap_or(false)
}

pub(crate) fn queue_command(
    db: &Database,
    client_id: &str,
    action: &str,
    payload: Option<&serde_json::Value>,
    requested_by: Option<&str>,
) -> Result<CommandRecord, String> {
    validate_client_command_for_client(client_id, action, payload)?;
    let now = Utc::now().to_rfc3339();
    let payload_str = payload.and_then(|v| serde_json::to_string(v).ok());
    db.queue_client_command(
        client_id,
        action,
        payload_str.as_deref(),
        &now,
        requested_by,
    )?;
    Ok(CommandRecord {
        client_id: client_id.to_string(),
        action: action.to_string(),
        payload: payload.cloned(),
        requested_at: now,
        requested_by: requested_by.map(|s| s.to_string()),
    })
}

async fn queue_repository_command(
    repository: &ClientRepository,
    client_id: &str,
    action: &str,
    payload: Option<&serde_json::Value>,
    requested_by: Option<&str>,
) -> Result<CommandRecord, String> {
    let now = Utc::now().to_rfc3339();
    if let Err(error) = validate_client_command_for_client(client_id, action, payload) {
        let _ = repository
            .record_audit_event(
                requested_by,
                None,
                "client_command_create",
                Some(client_id),
                "failure",
                Some("command rejected by server policy"),
                &now,
            )
            .await;
        return Err(error);
    }
    let payload_str = payload.and_then(|v| serde_json::to_string(v).ok());
    if let Err(error) = repository
        .queue_client_command(
            client_id,
            action,
            payload_str.as_deref(),
            &now,
            requested_by,
        )
        .await
    {
        if error.strip_prefix("cooldown:").is_some() {
            let _ = repository
                .record_audit_event(
                    requested_by,
                    None,
                    "client_command_cooldown",
                    Some(client_id),
                    "failure",
                    Some("command cooldown active"),
                    &now,
                )
                .await;
        } else {
            let _ = repository
                .record_audit_event(
                    requested_by,
                    None,
                    "client_command_persist",
                    Some(client_id),
                    "failure",
                    Some("command persistence failed"),
                    &now,
                )
                .await;
        }
        return Err(error);
    }
    if action == "start_display_preview" {
        let display = payload
            .and_then(|value| value.get("display_index"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as u32;
        crate::screenshot::clear_screenshot(client_id, display).await;
    }
    let detail = format!("action={action}");
    let _ = repository
        .record_audit_event(
            requested_by,
            None,
            "client_command_persist",
            Some(client_id),
            "success",
            Some(&detail),
            &now,
        )
        .await;
    let _ = repository
        .record_audit_event(
            requested_by,
            None,
            "client_command_create",
            Some(client_id),
            "success",
            Some(&detail),
            &now,
        )
        .await;
    Ok(CommandRecord {
        client_id: client_id.to_string(),
        action: action.to_string(),
        payload: payload.cloned(),
        requested_at: now,
        requested_by: requested_by.map(|s| s.to_string()),
    })
}

fn command_queue_http_error(error: String) -> (StatusCode, String) {
    if error == "command_pending" {
        return (StatusCode::CONFLICT, "A client command is already pending".to_string());
    }
    if let Some(raw) = error.strip_prefix("cooldown:") {
        let retry_after = raw.parse::<u64>().unwrap_or(1).clamp(1, 86_400);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            format!("client command cooldown active; retry after {retry_after} seconds"),
        );
    }
    internal_server_error()
}

fn hash_client_auth_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn client_auth_token_matches(expected_hash: &str, supplied: &str) -> bool {
    hash_client_auth_token(supplied)
        .as_bytes()
        .ct_eq(expected_hash.as_bytes())
        .unwrap_u8()
        == 1
}

pub(crate) async fn require_client_command_token(
    repository: &ClientRepository,
    client_id: &str,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, String)> {
    let expected_hash = repository
        .get_client_auth_token_hash(client_id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "client authentication lookup failed".to_string()))?;
    let supplied = headers
        .get("X-SCREENTRA-CLIENT-TOKEN")
        .and_then(|value| value.to_str().ok())
        .filter(|value| (32..=256).contains(&value.len()) && value.is_ascii())
        .ok_or((StatusCode::UNAUTHORIZED, "client authentication token required".to_string()))?;
    let expected_hash = expected_hash
        .ok_or((StatusCode::UNAUTHORIZED, "client authentication token required".to_string()))?;
    if !client_auth_token_matches(&expected_hash, supplied) {
        return Err((StatusCode::UNAUTHORIZED, "invalid client authentication token".to_string()));
    }
    Ok(())
}

async fn set_client_command(
    State(repository): State<ClientRepository>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
    Json(payload): Json<CommandRequest>,
) -> Result<AxumJson<CommandRecord>, (StatusCode, String)> {
    let rec = queue_repository_command(
        &repository,
        &id,
        &payload.action,
        payload.payload.as_ref(),
        Some(&principal.username),
    )
    .await
    .map_err(command_queue_http_error)?;
    Ok(AxumJson(rec))
}

async fn get_client_command(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<AxumJson<Option<CommandRecord>>, (StatusCode, String)> {
    require_client_command_token(
        &repository,
        &id,
        &headers,
    )
    .await?;
    let row = repository
        .get_client_command(&id)
        .await
        .map_err(|_| internal_server_error())?;

    let record = row.map(|(client_id, action, payload_str, requested_at, requested_by)| {
        let payload = payload_str
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        CommandRecord {
            client_id,
            action,
            payload,
            requested_at,
            requested_by,
        }
    });

    if let Some(record) = record {
        let poll_detail = format!("action={}", record.action);
        let _ = repository
            .record_audit_event(
                Some(&record.client_id),
                Some("client"),
                "client_command_poll",
                Some(&record.client_id),
                "success",
                Some(&poll_detail),
                &Utc::now().to_rfc3339(),
            )
            .await;
        if command_is_stale(&record.requested_at) {
            if let Err(error) = repository
                .delete_client_command_if_requested_at(&record.client_id, &record.requested_at)
                .await
            {
                let _ = repository
                    .record_audit_event(
                        Some(&record.client_id),
                        Some("client"),
                        "client_command_expire",
                        Some(&record.client_id),
                        "failure",
                        Some("expired command could not be cleared"),
                        &Utc::now().to_rfc3339(),
                    )
                    .await;
                return Err((StatusCode::INTERNAL_SERVER_ERROR, error));
            }
            let _ = repository
                .record_audit_event(
                    Some(&record.client_id),
                    Some("client"),
                    "client_command_expire",
                    Some(&record.client_id),
                    "success",
                    Some(&format!("action={}", record.action)),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            return Ok(AxumJson(None));
        }
        let _ = repository
            .record_audit_event(
                Some(&record.client_id),
                Some("client"),
                "client_command_claim",
                Some(&record.client_id),
                "success",
                Some(&poll_detail),
                &Utc::now().to_rfc3339(),
            )
            .await;
        Ok(AxumJson(Some(record)))
    } else {
        Ok(AxumJson(None))
    }
}

async fn clear_client_command(
    State(repository): State<ClientRepository>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let rows = match repository.clear_client_command(&id).await {
        Ok(rows) => rows,
        Err(error) => {
            let _ = repository
                .record_audit_event(
                    Some(&principal.username),
                    Some(&principal.role),
                    "client_command_clear",
                    Some(&id),
                    "failure",
                    Some("command clear failed"),
                    &Utc::now().to_rfc3339(),
                )
                .await;
            return Err((StatusCode::INTERNAL_SERVER_ERROR, error));
        }
    };
    let _ = repository
        .record_audit_event(
            Some(&principal.username),
            Some(&principal.role),
            "client_command_clear",
            Some(&id),
            "success",
            Some(&format!("cleared={rows}")),
            &Utc::now().to_rfc3339(),
        )
        .await;
    Ok(AxumJson(
        serde_json::json!({"cleared": rows, "client_id": id}),
    ))
}

async fn acknowledge_client_command(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    require_client_command_token(&repository, &id, &headers).await?;
    let now = Utc::now().to_rfc3339();
    let rows = match repository.clear_client_command(&id).await {
        Ok(rows) => rows,
        Err(error) => {
            let _ = repository
                .record_audit_event(
                    Some(&id),
                    Some("client"),
                    "client_command_ack",
                    Some(&id),
                    "failure",
                    Some("command acknowledgement failed"),
                    &now,
                )
                .await;
            return Err((StatusCode::INTERNAL_SERVER_ERROR, error));
        }
    };
    let _ = repository
        .record_audit_event(
            Some(&id),
            Some("client"),
            "client_command_ack",
            Some(&id),
            "success",
            Some(&format!("cleared={rows}")),
            &now,
        )
        .await;
    Ok(AxumJson(
        serde_json::json!({"cleared": rows, "client_id": id}),
    ))
}

#[derive(Debug, Deserialize)]
struct BulkCommandRequest {
    client_ids: Vec<String>,
    action: String,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    requested_by: Option<String>,
}

const MAX_BULK_COMMAND_CLIENT_IDS: usize = 10_000;

fn validate_bulk_command_client_ids(client_ids: &[String]) -> Result<(), (StatusCode, String)> {
    if client_ids.len() > MAX_BULK_COMMAND_CLIENT_IDS {
        return Err((
            StatusCode::BAD_REQUEST,
            "too many client ids".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct BulkCommandResult {
    queued: usize,
    results: Vec<serde_json::Value>,
}

async fn bulk_command_clients(
    State(repository): State<ClientRepository>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Json(payload): Json<BulkCommandRequest>,
) -> Result<AxumJson<BulkCommandResult>, (StatusCode, String)> {
    validate_bulk_command_client_ids(&payload.client_ids)?;
    let mut results = Vec::with_capacity(payload.client_ids.len());
    let mut queued = 0usize;
    for cid in &payload.client_ids {
        match queue_repository_command(
            &repository,
            cid,
            &payload.action,
            payload.payload.as_ref(),
            Some(&principal.username),
        )
        .await
        {
            Ok(rec) => {
                queued += 1;
                results.push(serde_json::json!({"client_id": cid, "ok": true, "action": rec.action, "requested_at": rec.requested_at}));
            }
            Err(e) => {
                results.push(serde_json::json!({"client_id": cid, "ok": false, "error": e}));
            }
        }
    }
    Ok(AxumJson(BulkCommandResult { queued, results }))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct SceneDisplayAssignment {
    display_index: i64,
    layout_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct SceneRecord {
    id: String,
    name: String,
    description: Option<String>,
    created_at: String,
    updated_at: String,
    assignments: Vec<SceneDisplayAssignment>,
}

#[derive(Debug, Deserialize)]
struct SceneUpsertRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    assignments: Vec<SceneDisplayAssignment>,
}

async fn load_scene(repository: &ClientRepository, scene_id: &str) -> Result<Option<SceneRecord>, String> {
    let Some((id, name, description, created_at, updated_at, rows)) = repository.get_scene(scene_id).await? else {
        return Ok(None);
    };
    Ok(Some(SceneRecord {
        id,
        name,
        description,
        created_at,
        updated_at,
        assignments: rows
            .into_iter()
            .map(|(display_index, layout_id)| SceneDisplayAssignment { display_index, layout_id })
            .collect(),
    }))
}

async fn list_scenes(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<Vec<SceneRecord>>, (StatusCode, String)> {
    let ids = repository
        .list_scene_ids()
        .await
        .map_err(|_| internal_server_error())?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(scene) = load_scene(&repository, &id)
            .await
            .map_err(|_| internal_server_error())?
        {
            out.push(scene);
        }
    }
    Ok(AxumJson(out))
}

async fn get_scene(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<SceneRecord>, (StatusCode, String)> {
    load_scene(&repository, &id)
        .await
        .map_err(|_| internal_server_error())?
        .map(AxumJson)
        .ok_or((StatusCode::NOT_FOUND, "scene not found".to_string()))
}

async fn create_scene(
    State(repository): State<ClientRepository>,
    Json(payload): Json<SceneUpsertRequest>,
) -> Result<AxumJson<SceneRecord>, (StatusCode, String)> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let assignments = payload
        .assignments
        .iter()
        .map(|a| (a.display_index, a.layout_id.clone()))
        .collect::<Vec<_>>();
    if !repository
        .save_scene(&id, &payload.name, payload.description.as_deref(), &now, &assignments, false)
        .await
        .map_err(|_| internal_server_error())?
    {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "scene insert failed".to_string()));
    }
    Ok(AxumJson(
        load_scene(&repository, &id)
            .await
            .map_err(|_| internal_server_error())?
            .unwrap(),
    ))
}

async fn update_scene(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(payload): Json<SceneUpsertRequest>,
) -> Result<AxumJson<SceneRecord>, (StatusCode, String)> {
    let now = Utc::now().to_rfc3339();
    let assignments = payload
        .assignments
        .iter()
        .map(|a| (a.display_index, a.layout_id.clone()))
        .collect::<Vec<_>>();
    let updated = repository
        .save_scene(&id, &payload.name, payload.description.as_deref(), &now, &assignments, true)
        .await
        .map_err(|_| internal_server_error())?;
    if !updated {
        return Err((StatusCode::NOT_FOUND, "scene not found".to_string()));
    }
    Ok(AxumJson(
        load_scene(&repository, &id)
            .await
            .map_err(|_| internal_server_error())?
            .unwrap(),
    ))
}

async fn delete_scene(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let deleted = repository
        .delete_scene(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({"deleted": deleted, "id": id})))
}

async fn publish_scene_to_client(
    State(repository): State<ClientRepository>,
    Path((id, client_id)): Path<(String, String)>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let scene = load_scene(&repository, &id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::NOT_FOUND, "scene not found".to_string()))?;
    let assignment_rows = scene
        .assignments
        .iter()
        .map(|a| (a.display_index, a.layout_id.clone()))
        .collect::<Vec<_>>();
    let updated = repository
        .publish_scene_assignments(&client_id, &assignment_rows)
        .await
        .map_err(|_| internal_server_error())?;
    let indexes: Vec<i64> = scene.assignments.iter().map(|a| a.display_index).collect();
    let command_payload = serde_json::json!({
        "scene_id": id,
        "scene_name": scene.name,
        "all_displays": true,
        "display_indexes": indexes,
    });
    let queued = queue_repository_command(
        &repository,
        &client_id,
        "reload_layout",
        Some(&command_payload),
        Some("scene-publish"),
    )
    .await
    .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({
        "scene_id": id,
        "client_id": client_id,
        "updated": updated,
        "assignments": scene.assignments,
        "queued_command": queued,
    })))
}

#[derive(Debug, Serialize, Clone)]
struct ClientDisplayInfo {
    client_id: String,
    display_index: i64,
    connector: Option<String>,
    label: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    refresh_rate: Option<f32>,
    enabled: bool,
    assigned_layout_id: Option<String>,
    active_layout_id: Option<String>,
    last_seen: String,
    last_error: Option<String>,
    status: String,
    port: i64,
    orientation: String,
    control_method: String,
    /// Present when the assigned layout is a set page
    #[serde(skip_serializing_if = "Option::is_none")]
    layout_set: Option<LayoutSetWithItems>,
}

fn client_display_status(last_seen: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(last_seen) {
        let dt = dt.with_timezone(&Utc);
        let age = (Utc::now() - dt).num_seconds();
        if age <= 90 {
            return "online".to_string();
        }
        if age <= 300 {
            return "stale".to_string();
        }
        return "offline".to_string();
    }
    "unknown".to_string()
}

/// Virtual clients are server-rendered preview targets. They do not emit a
/// physical-player heartbeat, so their creation timestamp must not make them
/// appear stale or offline. A virtual target is considered to observe the
/// current published revision because its config endpoint reads that immutable
/// revision directly from the server.
fn effective_client_display_status(is_virtual: bool, enabled: bool, last_seen: &str) -> String {
    if !enabled {
        "disconnected".to_string()
    } else if is_virtual {
        "online".to_string()
    } else {
        client_display_status(last_seen)
    }
}

fn effective_observed_revision(
    is_virtual: bool,
    target_revision: i64,
    observed_revision: Option<i64>,
) -> Option<i64> {
    if is_virtual && target_revision > 0 {
        Some(target_revision)
    } else {
        observed_revision
    }
}

async fn layout_propagation_status(
    State(repository): State<ClientRepository>,
    Path(layout_id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let target_revision = repository
        .list_layout_revisions(&layout_id)
        .await
        .map_err(|_| internal_server_error())?
        .into_iter()
        .map(|revision| revision.revision)
        .max()
        .unwrap_or(0);
    let clients = load_registered_clients_from_rows(
        repository
            .list_registered_clients()
            .await
            .map_err(|_| internal_server_error())?,
    );
    let observations = nudge_registry().observed_layouts.lock().unwrap().clone();
    let mut targets = Vec::new();
    let mut acknowledged = 0usize;
    let mut pending = 0usize;
    let mut stale = 0usize;
    let mut offline = 0usize;

    for client in clients {
        let (_, client_layout_id, records) = repository
            .list_client_displays(&client.id)
            .await
            .map_err(|_| internal_server_error())?;
        let mut matched = false;
        for record in records {
            if record.assigned_layout_id.as_deref() != Some(layout_id.as_str()) {
                continue;
            }
            matched = true;
            let display_status = effective_client_display_status(
                client.is_virtual,
                record.enabled,
                &record.last_seen,
            );
            let observation = observations.get(&format!("{}:{}", client.id, record.display_index));
            let observed_revision = effective_observed_revision(
                client.is_virtual,
                target_revision,
                observation
                .filter(|observed| observed.layout_id == layout_id)
                .map(|observed| observed.revision),
            );
            let state = if display_status != "online" {
                offline += 1;
                display_status.clone()
            } else if observed_revision.unwrap_or(0) >= target_revision && target_revision > 0 {
                acknowledged += 1;
                "acknowledged".to_string()
            } else {
                pending += 1;
                "pending".to_string()
            };
            if observed_revision.is_some() && observed_revision.unwrap_or(0) < target_revision {
                stale += 1;
            }
            targets.push(serde_json::json!({
                "client_id": client.id,
                "client_name": client.name,
                "display_index": record.display_index,
                "connector": record.connector,
                "status": state,
                "connection_status": display_status,
                "assigned_layout_id": record.assigned_layout_id,
                "active_layout_id": record.active_layout_id,
                "target_revision": target_revision,
                "observed_revision": observed_revision,
                "observed_at": observation.map(|observed| observed.observed_at.clone()),
                "last_seen": record.last_seen,
            }));
        }
        if !matched && client_layout_id.as_deref() == Some(layout_id.as_str()) {
            let observation = observations.get(&client.id);
            let observed_revision = effective_observed_revision(
                client.is_virtual,
                target_revision,
                observation
                .filter(|observed| observed.layout_id == layout_id)
                .map(|observed| observed.revision),
            );
            let state = if client.status != "online" {
                offline += 1;
                client.status.clone()
            } else if observed_revision.unwrap_or(0) >= target_revision && target_revision > 0 {
                acknowledged += 1;
                "acknowledged".to_string()
            } else {
                pending += 1;
                "pending".to_string()
            };
            targets.push(serde_json::json!({
                "client_id": client.id,
                "client_name": client.name,
                "display_index": 0,
                "connector": null,
                "status": state,
                "connection_status": client.status,
                "assigned_layout_id": client_layout_id,
                "active_layout_id": client.active_layout_id,
                "target_revision": target_revision,
                "observed_revision": observed_revision,
                "observed_at": observation.map(|observed| observed.observed_at.clone()),
                "last_seen": client.last_seen,
            }));
        }
    }
    Ok(AxumJson(serde_json::json!({
        "layout_id": layout_id,
        "target_revision": target_revision,
        "target_count": targets.len(),
        "acknowledged_count": acknowledged,
        "pending_count": pending,
        "stale_count": stale,
        "offline_count": offline,
        "targets": targets,
    })))
}

async fn list_client_displays(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<Vec<ClientDisplayInfo>>, (StatusCode, String)> {
    let (is_virtual, _client_layout_id, records) = repository
        .list_client_displays(&id)
        .await
        .map_err(|_| internal_server_error())?;
    let mut out = Vec::<ClientDisplayInfo>::new();
    for record in records {
        let display_index = record.display_index;
        let enabled = record.enabled;
        let last_seen = record.last_seen.clone();
        let assigned_layout_id = record.assigned_layout_id.clone();
        out.push(ClientDisplayInfo {
            client_id: record.client_id,
            display_index,
            connector: record.connector,
            label: record.label,
            width: record.width,
            height: record.height,
            refresh_rate: record.refresh_rate,
            enabled,
            assigned_layout_id,
            active_layout_id: record.active_layout_id,
            last_seen: last_seen.clone(),
            last_error: record.last_error,
            status: effective_client_display_status(is_virtual, enabled, &last_seen),
            port: 3900 + display_index,
            orientation: record.orientation,
            control_method: record.control_method,
            layout_set: None,
        });
    }

    // Attach layout sets for set-page layouts
    if !out.is_empty() {
        let layout_ids: Vec<String> = out.iter()
            .filter_map(|d| d.assigned_layout_id.clone())
            .collect();
        if !layout_ids.is_empty() {
            let sets_map = repository.get_layout_sets_for_layouts(&layout_ids)
            .await
            .map_err(|_| internal_server_error())?;
            for disp in out.iter_mut() {
                if let Some(ref lid) = disp.assigned_layout_id {
                    disp.layout_set = sets_map.get(lid).cloned();
                }
            }
        }
    }

    Ok(AxumJson(out))
}

#[derive(Debug, Deserialize)]
struct BulkAssignDisplaysRequest {
    layout_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct BulkAssignDisplaysResult {
    updated: usize,
    client_id: String,
}

async fn bulk_assign_displays(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(payload): Json<BulkAssignDisplaysRequest>,
) -> Result<AxumJson<BulkAssignDisplaysResult>, (StatusCode, String)> {
    let updated = repository
        .bulk_set_display_layout(&id, payload.layout_id.as_deref())
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(BulkAssignDisplaysResult {
        updated,
        client_id: id,
    }))
}

async fn get_client_display_config(
    State(repository): State<ClientRepository>,
    Path((id, index)): Path<(String, i64)>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let (layout_id, orientation) = repository
        .get_display_config(&id, index)
        .await
        .map_err(|_| internal_server_error())?;
    let layout = if let Some(ref lid) = layout_id {
        repository
            .get_published_layout(lid)
            .await
            .map_err(|_| internal_server_error())?
            .or(repository
                .get_layout(lid)
                .await
                .map_err(|_| internal_server_error())?)
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| internal_server_error())?
    } else {
        None
    };
    let published_revision = if let Some(ref lid) = layout_id {
        repository
            .list_layout_revisions(lid)
            .await
            .map_err(|_| internal_server_error())?
            .into_iter()
            .map(|r| r.revision)
            .max()
    } else {
        None
    };
    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "display_index": index,
        "assigned_layout_id": layout_id,
        "active_layout_revision": published_revision,
        "layout": layout,
        "port": 3900 + index,
        "server_time": Utc::now().to_rfc3339(),
        "sync_interval_seconds": 30,
        "orientation": orientation,
    })))
}

#[derive(Debug, Deserialize)]
struct DisplayCommandRequest {
    action: String,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    requested_by: Option<String>,
}

async fn set_client_display_command(
    State(repository): State<ClientRepository>,
    Path((id, index)): Path<(String, i64)>,
    Json(payload): Json<DisplayCommandRequest>,
) -> Result<AxumJson<CommandRecord>, (StatusCode, String)> {
    let mut command_payload = payload
        .payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    if !command_payload.is_object() {
        command_payload = serde_json::json!({ "value": command_payload });
    }
    if let Some(obj) = command_payload.as_object_mut() {
        obj.insert(
            "display_index".to_string(),
            serde_json::Value::Number(serde_json::Number::from(index)),
        );
        // Add has_layout: true if this display has a layout assigned OR
        // was seen recently (within 3 heartbeat cycles / ~90s). Displays
        // that were seen before but have gone offline (last_seen stale) will
        // not trigger a retry — avoids hammering a display that is gone.
        if payload.action == "ddc_set" {
            let has_layout = {
                let row = repository
                    .get_display_assignment_and_last_seen(&id, index)
                    .await
                    .map_err(|_| internal_server_error())?;
                match row {
                    Some((Some(_), _)) => true,
                    Some((None, last_seen)) => {
                        if let Ok(last_dt) = chrono::DateTime::parse_from_rfc3339(&last_seen) {
                            let age_secs = Utc::now()
                                .signed_duration_since(last_dt.with_timezone(&Utc))
                                .num_seconds();
                            age_secs <= 90
                        } else {
                            false
                        }
                    }
                    None => false,
                }
            };
            obj.insert("has_layout".to_string(), serde_json::Value::Bool(has_layout));
        }
    }
    let rec = queue_repository_command(
        &repository,
        &id,
        &payload.action,
        Some(&command_payload),
        payload.requested_by.as_deref(),
    )
    .await
    .map_err(command_queue_http_error)?;
    Ok(AxumJson(rec))
}

#[derive(Debug, Deserialize)]
struct DisplayLayoutRequest {
    layout_id: Option<String>,
}

async fn set_client_display_layout(
    State(repository): State<ClientRepository>,
    Path((id, index)): Path<(String, i64)>,
    Json(payload): Json<DisplayLayoutRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let updated = repository
        .set_display_layout(&id, index, payload.layout_id.as_deref())
        .await
        .map_err(|_| internal_server_error())?;
    let command_payload = serde_json::json!({
        "display_index": index,
        "assigned_layout_id": payload.layout_id,
    });
    let queued = queue_repository_command(
        &repository,
        &id,
        "reload_layout",
        Some(&command_payload),
        Some("web-ui"),
    )
    .await
    .map_err(command_queue_http_error)?;
    Ok(AxumJson(serde_json::json!({
        "updated": updated,
        "client_id": id,
        "display_index": index,
        "assigned_layout_id": payload.layout_id,
        "queued_command": queued,
    })))
}

#[derive(Debug, Deserialize)]
struct DisplayOrientationRequest {
    orientation: String,
}

#[derive(Debug, Deserialize)]
struct DisplayControlMethodRequest {
    control_method: String,
}

async fn get_client_display_control(
    State(repository): State<ClientRepository>,
    Path((id, index)): Path<(String, i64)>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let row = repository
        .get_display_control(&id, index)
        .await
        .map_err(|_| internal_server_error())?;
    let Some(row) = row else {
        return Err((StatusCode::NOT_FOUND, "Display not found".to_string()));
    };

    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "display_index": index,
        "connector": row.0,
        "label": row.1,
        "width": row.2,
        "height": row.3,
        "refresh_rate": row.4,
        "enabled": row.5,
        "orientation": row.6,
        "control_method": row.7,
    })))
}

async fn set_client_display_control_method(
    State(repository): State<ClientRepository>,
    Path((id, index)): Path<(String, i64)>,
    Json(payload): Json<DisplayControlMethodRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let method = match payload.control_method.as_str() {
        "ddc" | "cec" | "ir" | "dpms" | "auto" => payload.control_method.clone(),
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid control_method '{}'. Valid: ddc, cec, ir, dpms, auto", other),
            ));
        }
    };
    let updated = repository
        .set_display_control_method(&id, index, &method)
        .await
        .map_err(|_| internal_server_error())?;
    if updated == 0 {
        return Err((StatusCode::NOT_FOUND, "Display not found".to_string()));
    }
    Ok(AxumJson(serde_json::json!({
        "updated": updated,
        "client_id": id,
        "display_index": index,
        "control_method": method,
    })))
}

async fn set_client_display_orientation(
    State(repository): State<ClientRepository>,
    Path((id, index)): Path<(String, i64)>,
    Json(payload): Json<DisplayOrientationRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let orientation = match payload.orientation.as_str() {
        "normal" | "flipped" | "rotated_left" | "rotated_right" => payload.orientation.clone(),
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Invalid orientation '{}'. Valid values: normal, flipped, rotated_left, rotated_right",
                    other
                ),
            ))
        }
    };
    let updated = repository
        .set_display_orientation(&id, index, &orientation)
        .await
        .map_err(|_| internal_server_error())?;
    if updated == 0 {
        return Err((StatusCode::NOT_FOUND, "Display not found".to_string()));
    }
    let command_payload = serde_json::json!({
        "display_index": index,
        "orientation": orientation,
    });
    let queued = queue_repository_command(&repository, &id, "reload_layout", Some(&command_payload), Some("web-ui"))
        .await
        .map_err(command_queue_http_error)?;
    Ok(AxumJson(serde_json::json!({
        "updated": updated,
        "client_id": id,
        "display_index": index,
        "orientation": orientation,
        "queued_command": queued,
    })))
}

async fn clear_client_display_command(
    State(repository): State<ClientRepository>,
    Path((id, index)): Path<(String, i64)>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    // Real clearing: delete only when the queued command targets this display_index,
    // otherwise fall back to deleting the client-wide command so callers can be sure
    // a stale command is not blocking their next queued action.
    let deleted = repository
        .clear_client_display_command(&id, index)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({
        "cleared": deleted,
        "client_id": id,
        "display_index": index,
    })))
}

async fn delete_client_history(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let rows = repository
        .delete_client_history(&id)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(
        serde_json::json!({"deleted": rows, "client_id": id}),
    ))
}

async fn get_client_history(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ClientHistoryQuery>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(50).clamp(1, 1000);
    let mut records = repository
        .list_client_heartbeats(&id, limit as i64)
        .await
        .map_err(|_| internal_server_error())?
        .into_iter()
        .map(|(recorded_at, cpu_usage, memory_used, memory_total, disk_used, disk_total, uptime_seconds)| {
            HeartbeatRecord {
                recorded_at,
                cpu_usage,
                memory_used,
                memory_total,
                disk_used,
                disk_total,
                uptime_seconds,
            }
        })
        .collect::<Vec<_>>();
    records.reverse();
    Ok(AxumJson(
        serde_json::json!({"client_id": id, "count": records.len(), "heartbeats": records}),
    ))
}

async fn delete_virtual_client(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<DeleteClientResponse>, (StatusCode, String)> {
    let deleted = repository
        .delete_virtual_client(&id)
        .await
        .map_err(|_| internal_server_error())?;
    if !deleted {
        return Err((StatusCode::NOT_FOUND, "Not a virtual client".to_string()));
    }
    nudge_registry().last_nudge.lock().unwrap().remove(&id);
    nudge_registry().observed_layouts.lock().unwrap().retain(|key, _| key != &id && !key.starts_with(&format!("{}:", id)));
    Ok(AxumJson(DeleteClientResponse { deleted, client_id: id }))
}

async fn delete_client(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<DeleteClientResponse>, (StatusCode, String)> {
    let deleted = repository
        .delete_client(&id)
        .await
        .map_err(|_| internal_server_error())?;
    nudge_registry().last_nudge.lock().unwrap().remove(&id);
    nudge_registry().observed_layouts.lock().unwrap().retain(|key, _| key != &id && !key.starts_with(&format!("{}:", id)));
    Ok(AxumJson(DeleteClientResponse {
        deleted,
        client_id: id,
    }))
}

async fn nudge_client(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<NudgeResponse>, (StatusCode, String)> {
    let layout_id = repository
        .get_client_assignment(&id)
        .await
        .map_err(|_| internal_server_error())?;
    let now = Utc::now().to_rfc3339();
    nudge_registry()
        .last_nudge
        .lock()
        .unwrap()
        .insert(id.clone(), (now.clone(), layout_id.clone()));
    Ok(AxumJson(NudgeResponse {
        client_id: id,
        requested_at: now,
        layout_id,
    }))
}

#[derive(Debug, Serialize)]
struct LayoutRender {
    id: String,
    name: String,
    widgets: Vec<serde_json::Value>,
    background: String,
    width: u32,
    height: u32,
    canvas_orientation: String,
}

async fn render_layout(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<LayoutRender>, (StatusCode, String)> {
    let layout = repository
        .get_layout(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::NOT_FOUND, format!("Layout {} not found", id)))?;
    let widgets_json = serde_json::to_value(&layout.widgets)
        .map_err(|_| internal_server_error())?;
    let widgets_arr = widgets_json.as_array().cloned().unwrap_or_default();
    Ok(AxumJson(LayoutRender {
        id: layout.id,
        name: layout.name,
        widgets: widgets_arr,
        background: String::new(),
        width: if layout.canvas_orientation == "portrait" { 1080 } else { 1920 },
        height: if layout.canvas_orientation == "portrait" { 1920 } else { 1080 },
        canvas_orientation: layout.canvas_orientation,
    }))
}

fn command_result_metadata(value: &serde_json::Value) -> (String, bool) {
    let action = value
        .get("action")
        .and_then(serde_json::Value::as_str)
        .filter(|action| !action.is_empty())
        .unwrap_or("unknown")
        .to_string();
    let ok = value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    (action, ok)
}

fn sanitized_command_result(value: &serde_json::Value) -> serde_json::Value {
    let mut result = serde_json::Map::new();
    for key in ["action", "display_index", "ok", "started_at", "finished_at", "requested_at"] {
        if let Some(field) = value.get(key) {
            match field {
                serde_json::Value::String(text) => {
                    let bounded = text.chars().take(256).collect::<String>();
                    result.insert(key.to_string(), serde_json::Value::String(bounded));
                }
                serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
                    result.insert(key.to_string(), field.clone());
                }
                _ => {}
            }
        }
    }
    if let Some(message) = value.get("message").and_then(serde_json::Value::as_str) {
        result.insert(
            "message".to_string(),
            serde_json::Value::String(message.chars().take(256).collect()),
        );
    }
    serde_json::Value::Object(result)
}

const MAX_CLIENT_REGISTRATION_DISPLAYS: usize = 64;

fn validate_client_registration_display_count(count: usize) -> Result<(), (StatusCode, String)> {
    if count > MAX_CLIENT_REGISTRATION_DISPLAYS {
        return Err((
            StatusCode::BAD_REQUEST,
            "too many client displays".to_string(),
        ));
    }
    Ok(())
}

async fn register_client(
    State(repository): State<ClientRepository>,
    Json(payload): Json<ClientRegisterRequest>,
) -> Result<AxumJson<ConnectedClientInfo>, (StatusCode, String)> {
    validate_client_registration_display_count(payload.displays.len())?;
    let client_auth_token = payload.client_auth_token.as_deref();
    if let Some(token) = client_auth_token {
        if !(32..=256).contains(&token.len()) || !token.is_ascii() {
            return Err((
                StatusCode::BAD_REQUEST,
                "invalid client authentication token".to_string(),
            ));
        }
    }
    let now = Utc::now().to_rfc3339();
    let address = payload
        .address
        .or_else(request_peer_ip)
        .unwrap_or_else(|| "unknown".to_string());
    let name = payload
        .hostname
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format!("Player {}", address));
    let id = payload
        .client_id
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format!("player-{}", address.replace('.', "-")));
    let stored_hash = repository
        .get_client_auth_token_hash(&id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "client authentication lookup failed".to_string()))?;
    let existing_client = repository
        .list_registered_clients()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "client registration lookup failed".to_string()))?
        .into_iter()
        .find(|row| row.0 == id);
    let was_enrolled = existing_client.as_ref().map(|row| row.14 != 0).unwrap_or(false);
    let client_auth_token_hash = client_auth_token.map(hash_client_auth_token);
    if existing_client.is_some() && stored_hash.is_none() {
        // Existing legacy rows without a token must be claimed through the
        // authenticated admin enrollment flow. Otherwise anyone who learns a
        // client ID can win the first-registration race and set its token.
        return Err((
            StatusCode::CONFLICT,
            "client requires authenticated administrator enrollment".to_string(),
        ));
    }
    match stored_hash.as_deref() {
        Some(expected_hash) => {
            let Some(provided_hash) = client_auth_token_hash.as_deref() else {
                return Err((StatusCode::UNAUTHORIZED, "client authentication token required".to_string()));
            };
            if provided_hash.as_bytes().ct_eq(expected_hash.as_bytes()).unwrap_u8() != 1 {
                return Err((StatusCode::UNAUTHORIZED, "invalid client authentication token".to_string()));
            }
        }
        None => {}
    }
    let role = payload.role.unwrap_or_else(|| "player".to_string());
    let client_agent = payload.client_agent;
    let identity_source = payload
        .identity_source
        .as_deref()
        .unwrap_or(if client_agent {
            "persistent_client_id"
        } else {
            "legacy"
        });
    log::debug!(
        "client register: id={} address={} agent={} identity_source={}",
        id,
        address,
        client_agent,
        identity_source
    );

    let command_result = payload
        .last_command_result
        .as_ref()
        .map(command_result_metadata);
    let client_record = ClientRegistration {
        id: id.clone(),
        name,
        address,
        role,
        version: payload.version,
        uptime_seconds: payload.uptime_seconds.map(|v| v as i64),
        cpu_usage: payload.cpu_usage.map(|v| v as f64),
        memory_used: payload.memory_used.map(|v| v as i64),
        memory_total: payload.memory_total.map(|v| v as i64),
        disk_used: payload.disk_used.map(|v| v as i64),
        disk_total: payload.disk_total.map(|v| v as i64),
        active_layout_id: payload.active_layout_id.clone(),
        last_seen: now.clone(),
        last_command_result: payload
            .last_command_result
            .as_ref()
            .map(sanitized_command_result)
            .and_then(|v| serde_json::to_string(&v).ok()),
        last_command_at: payload.last_command_at,
        installer_detected_max_displays: payload.installer_detected_max_displays,
        touch_binding: payload.touch_binding.as_ref().and_then(|v| serde_json::to_string(v).ok()),
        theme: payload.theme.clone(),
        max_displays: payload.max_displays,
        client_auth_token_hash,
    };
    let display_records = payload.displays.iter().map(|disp| ClientDisplayRegistration {
        display_index: disp.display_index,
        connector: disp.connector.clone(),
        label: disp.label.clone(),
        width: disp.width.map(|v| v as i64),
        height: disp.height.map(|v| v as i64),
        refresh_rate: disp.refresh_rate,
        enabled: disp.enabled,
        last_error: disp.last_error.clone(),
    }).collect::<Vec<_>>();
    if let Err(error) = repository
        .register_client(&client_record, &display_records)
        .await
    {
        if let Some((action, _ok)) = command_result.as_ref() {
            let _ = repository
                .record_audit_event(
                    Some(&id),
                    Some("client"),
                    "client_command_result",
                    Some(&id),
                    "failure",
                    Some("heartbeat result persistence failed"),
                    &now,
                )
                .await;
            let _ = repository
                .record_audit_event(
                    Some(&id),
                    Some("client"),
                    "client_command_ack",
                    Some(&id),
                    "failure",
                    Some(&format!("action={action}")),
                    &now,
                )
                .await;
        }
        return Err((StatusCode::INTERNAL_SERVER_ERROR, error));
    }
    if let Some((action, ok)) = command_result.as_ref() {
        let outcome = if *ok { "success" } else { "failure" };
        let detail = format!("action={action}");
        let _ = repository
            .record_audit_event(
                Some(&id),
                Some("client"),
                "client_command_execute",
                Some(&id),
                outcome,
                Some(&detail),
                &now,
            )
            .await;
        let _ = repository
            .record_audit_event(
                Some(&id),
                Some("client"),
                "client_command_result",
                Some(&id),
                outcome,
                Some(&detail),
                &now,
            )
            .await;
        let _ = repository
            .record_audit_event(
                Some(&id),
                Some("client"),
                "client_command_ack",
                Some(&id),
                "success",
                Some(&detail),
                &now,
            )
            .await;
    }
    if was_enrolled {
        let mut observations = nudge_registry().observed_layouts.lock().unwrap();
        // Registration is the only acknowledgement transport today. Clear the
        // previous value for this client/display before applying this heartbeat
        // so an omitted revision cannot leave a false old acknowledgement alive.
        observations.remove(&id);
        for display in &payload.displays {
            observations.remove(&format!("{}:{}", id, display.display_index));
        }
        if let (Some(layout_id), Some(revision)) = (
            payload.active_layout_id.as_ref(),
            payload.active_layout_revision.filter(|value| *value >= 0),
        ) {
            observations.insert(
                id.clone(),
                ObservedLayout {
                    layout_id: layout_id.clone(),
                    revision,
                    observed_at: now.clone(),
                },
            );
        }
        for display in &payload.displays {
            if let (Some(layout_id), Some(revision)) = (
                display.active_layout_id.as_ref().or(payload.active_layout_id.as_ref()),
                display.active_layout_revision.or(payload.active_layout_revision).filter(|value| *value >= 0),
            ) {
                observations.insert(
                    format!("{}:{}", id, display.display_index),
                    ObservedLayout {
                        layout_id: layout_id.clone(),
                        revision,
                        observed_at: now.clone(),
                    },
                );
            }
        }
        // Bound memory even if an unauthenticated caller submits arbitrary IDs.
        if observations.len() > 10_000 {
            observations.clear();
        }
    }

    let rows = repository
        .list_registered_clients()
        .await
        .map_err(|_| internal_server_error())?;
    load_registered_clients_from_rows(rows)
        .into_iter()
        .find(|c| c.id == id)
        .map(AxumJson)
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "registered client not found".to_string(),
        ))
}

fn collect_connected_clients_from_rows(rows: Vec<RegisteredClientRow>, run_search: bool) -> Vec<ConnectedClientInfo> {
    let local = get_system_status();
    let now = Utc::now().to_rfc3339();
    let show_server = std::env::var("RIPLEY_SHOW_SERVER_IN_CLIENTS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
        .unwrap_or(false);
    let mut clients: Vec<ConnectedClientInfo> = if show_server {
        vec![ConnectedClientInfo {
            id: format!("server-{}", local.hostname),
            pairing_code: client_pairing_code(&format!("server-{}", local.hostname)),
            name: local.hostname.clone(),
            address: local_ip().unwrap_or_else(|| "127.0.0.1".to_string()),
            role: "Server / Local Player".to_string(),
            status: "online".to_string(),
            enrolled: true,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            last_command_result: None,
            last_command_at: None,
            max_displays: Some(1),
            installer_detected_max_displays: Some(1),
            touch_binding: None,
            theme: None,
            uptime_seconds: Some(local.uptime_seconds),
            cpu_usage: Some(local.cpu_usage),
            memory_used: Some(local.memory_used),
            memory_total: Some(local.memory_total),
            disk_used: Some(local.disk_used),
            disk_total: Some(local.disk_total),
            active_layout_id: None,
            assigned_layout_id: None,
            last_seen: now.clone(),
            is_virtual: false,
        }]
    } else {
        Vec::new()
    };

    clients.extend(load_registered_clients_from_rows(rows));

    if run_search {
        // Historical behavior pinged every address in each /24, then displayed
        // every neighbor as a "not enrolled" candidate. That was noisy and slow.
        // We now keep search passive/fast: read the current neighbor cache and
        // only include hosts that answer the Ripley player status endpoint below.
    }

    // Only show LAN discovery candidates that are actually running the Ripley
    // player client/renderer. Previously this listed every ARP neighbor as a
    // "not enrolled" candidate, which exposed unrelated phones, PCs, printers,
    // etc. Search still warms neighbor cache, but candidates must answer the
    // player renderer status endpoint with a Ripley client marker.
    for candidate in discover_ripley_client_candidates() {
        if clients
            .iter()
            .any(|c| c.id == candidate.id || c.address == candidate.address)
        {
            continue;
        }
        clients.push(candidate);
    }
    clients
}

fn load_registered_clients(db: &Database) -> Vec<ConnectedClientInfo> {
    let rows = match db.list_registered_clients() {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    load_registered_clients_from_rows(rows)
}

fn load_registered_clients_from_rows(rows: Vec<RegisteredClientRow>) -> Vec<ConnectedClientInfo> {
    rows.into_iter()
        .map(|(
            id,
            name,
            address,
            role,
            version,
            uptime_seconds,
            cpu_usage,
            memory_used,
            memory_total,
            disk_used,
            disk_total,
            active_layout_id,
            assigned_layout_id,
            last_seen,
            enrolled,
            last_command_result,
            last_command_at,
            max_displays,
            installer_detected_max_displays,
            touch_binding,
            theme,
            is_virtual,
        )| ConnectedClientInfo {
            status: if is_virtual != 0 {
                "online".to_string()
            } else {
                client_status_from_last_seen(&last_seen)
            },
            pairing_code: client_pairing_code(&id),
            id,
            name,
            address,
            role,
            version,
            uptime_seconds: uptime_seconds.map(|v| v.max(0) as u64),
            cpu_usage,
            memory_used: memory_used.map(|v| v.max(0) as u64),
            memory_total: memory_total.map(|v| v.max(0) as u64),
            disk_used: disk_used.map(|v| v.max(0) as u64),
            disk_total: disk_total.map(|v| v.max(0) as u64),
            active_layout_id,
            assigned_layout_id,
            last_seen,
            enrolled: enrolled != 0,
            last_command_result: last_command_result
                .and_then(|s| serde_json::from_str(&s).ok()),
            last_command_at,
            max_displays,
            installer_detected_max_displays,
            touch_binding: touch_binding.and_then(|s| serde_json::from_str(&s).ok()),
            theme,
            is_virtual: is_virtual != 0,
        })
        .collect()
}

fn client_status_from_last_seen(last_seen: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(last_seen).map(|dt| dt.with_timezone(&Utc));
    if let Ok(seen) = parsed {
        let age = Utc::now().signed_duration_since(seen).num_seconds();
        if age <= 90 {
            "online".to_string()
        } else if age <= 300 {
            "stale".to_string()
        } else {
            "offline".to_string()
        }
    } else {
        "unknown".to_string()
    }
}

fn request_peer_ip() -> Option<String> {
    std::env::var("REMOTE_ADDR").ok()
}

fn local_ip() -> Option<String> {
    let mut command = Command::new("hostname");
    command.arg("-I");
    let (status, bytes) = bounded_command_output(command, 4096, Duration::from_secs(30)).ok()?;
    if !status.success() {
        return None;
    }
    String::from_utf8_lossy(&bytes)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

fn discover_neighbor_ips() -> Vec<String> {
    let mut ips = Vec::new();
    let mut command = Command::new("ip");
    command.args(["-4", "neigh", "show"]);
    if let Ok((status, bytes)) = bounded_command_output(command, 64 * 1024, Duration::from_secs(30)) {
        if !status.success() {
            return ips;
        }
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Some(ip) = line.split_whitespace().next() else {
                continue;
            };
            if ip.parse::<std::net::Ipv4Addr>().is_ok()
                && !ips.iter().any(|candidate| candidate == ip)
            {
                ips.push(ip.to_string());
            }
        }
    }
    ips
}

/// Probe discovered LAN neighbors for the Ripley player renderer status API.
/// This prevents the UI from showing every random device on the subnet while
/// still allowing installed-but-not-yet-enrolled clients to announce themselves.
fn discover_ripley_client_candidates() -> Vec<ConnectedClientInfo> {
    let now = Utc::now().to_rfc3339();
    let mut out = Vec::new();
    for ip in discover_neighbor_ips() {
        for port in 3900..=3903 {
            if let Some(status) = probe_ripley_status(&ip, port) {
                let client_id = status
                    .get("client_id")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| format!("candidate-{}-{}", ip.replace('.', "-"), port));
                let hostname = status
                    .get("hostname")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| v.to_string())
                    .or_else(|| reverse_name(&ip))
                    .unwrap_or_else(|| format!("Ripley Player {}", ip));
                let build = status
                    .get("build")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| v.to_string());
                let display_index = status
                    .get("display_index")
                    .and_then(|v| v.as_i64())
                    .unwrap_or((port - 3900) as i64);
                let display_label = status
                    .get("display_label")
                    .and_then(|v| v.as_str())
                    .or_else(|| status.get("display_connector").and_then(|v| v.as_str()))
                    .unwrap_or("display");
                out.push(ConnectedClientInfo {
                    pairing_code: status
                        .get("pairing_code")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| client_pairing_code(&client_id)),
                    id: client_id,
                    name: format!("{} · {} {}", hostname, display_label, display_index),
                    address: ip.clone(),
                    role: "Ripley Player Client".to_string(),
                    status: "not enrolled".to_string(),
                    enrolled: false,
                    version: build,
                    uptime_seconds: None,
                    cpu_usage: None,
                    memory_used: None,
                    memory_total: None,
                    disk_used: None,
                    disk_total: None,
                    active_layout_id: status
                        .get("assigned_layout_id")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    assigned_layout_id: None,
                    last_command_result: None,
                    last_command_at: None,
                    max_displays: Some(1),
                    installer_detected_max_displays: Some(1),
                    touch_binding: status.get("touch_binding").cloned(),
                    theme: status.get("theme").and_then(|v| v.as_str()).map(|v| v.to_string()),
                    last_seen: status
                        .get("last_seen")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| now.clone()),
                    is_virtual: false,
                });
                break;
            }
        }
    }
    out
}

fn probe_ripley_status(ip: &str, port: u16) -> Option<serde_json::Value> {
    let addr: SocketAddr = format!("{}:{}", ip, port).parse().ok()?;
    let timeout = Duration::from_millis(120);
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let req = format!(
        "GET /api/status.json HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nAccept: application/json\r\n\r\n",
        ip, port
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.take(64 * 1024).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    if !text.starts_with("HTTP/1.0 200") && !text.starts_with("HTTP/1.1 200") {
        return None;
    }
    let body = text.split("\r\n\r\n").nth(1)?;
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let has_marker = json
        .get("ripley_player_client")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || json.get("client_id").and_then(|v| v.as_str()).is_some()
        || json.get("server_url").and_then(|v| v.as_str()).is_some();
    if has_marker {
        Some(json)
    } else {
        None
    }
}

fn reverse_name(ip: &str) -> Option<String> {
    let mut command = Command::new("getent");
    command.args(["hosts", ip]);
    let (status, bytes) = bounded_command_output(command, 4096, Duration::from_secs(30)).ok()?;
    if !status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&bytes)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_registration_display_lists_are_bounded() {
        assert!(validate_client_registration_display_count(
            MAX_CLIENT_REGISTRATION_DISPLAYS
        )
        .is_ok());
        let (status, error) = validate_client_registration_display_count(
            MAX_CLIENT_REGISTRATION_DISPLAYS + 1,
        )
        .expect_err("oversized client display list should fail");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error, "too many client displays");
    }

    #[test]
    fn layout_set_item_lists_are_bounded() {
        assert_eq!(validate_layout_set_item_count(MAX_LAYOUT_SET_ITEMS).is_ok(), true);
        assert_eq!(
            validate_layout_set_item_count(MAX_LAYOUT_SET_ITEMS + 1),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn mutation_id_lists_are_bounded() {
        let ids = vec![0_i64; MAX_MUTATION_IDS + 1];
        let (status, error) = validate_mutation_id_count(&ids, "too many ids")
            .expect_err("oversized mutation id list should fail");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error, "too many ids");
    }

    #[test]
    fn bulk_command_client_ids_are_bounded() {
        let ids = vec!["client".to_string(); MAX_BULK_COMMAND_CLIENT_IDS + 1];
        let (status, error) = validate_bulk_command_client_ids(&ids)
            .expect_err("oversized bulk command client list should fail");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error, "too many client ids");
    }

    #[test]
    fn bounded_file_reader_rejects_oversized_files() {
        let path = std::env::temp_dir().join(format!("screentra-api-bound-{}", Uuid::new_v4()));
        fs::write(&path, vec![b'x'; 17]).unwrap();
        assert!(read_bounded_file(&path, 16).is_err());
        assert_eq!(read_bounded_file(&path, 17).unwrap().len(), 17);
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn bounded_async_file_reader_rejects_oversized_files() {
        let path = std::env::temp_dir().join(format!("screentra-api-async-bound-{}", Uuid::new_v4()));
        fs::write(&path, vec![b'x'; 17]).unwrap();
        assert!(read_bounded_async_file(&path, 16).await.is_err());
        assert_eq!(read_bounded_async_file(&path, 17).await.unwrap().len(), 17);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn bounded_command_output_terminates_timed_out_process() {
        let mut command = Command::new("python3");
        command.args(["-c", "import time; time.sleep(5)"]);
        let error = bounded_command_output(command, 1024, Duration::from_millis(50))
            .expect_err("timed-out command should be terminated");
        assert!(error.contains("command exceeded its timeout"));
    }
    #[tokio::test]
    async fn bounded_async_command_output_rejects_oversized_stdout() {
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", "import sys; sys.stdout.write('x' * 70000)"]);
        let error = bounded_async_command_output(
            command,
            Vec::new(),
            1024,
            Duration::from_secs(5),
        )
        .await
        .expect_err("oversized command output should be rejected");
        assert!(error.contains("stdout exceeds its bound"));
    }

    #[tokio::test]
    async fn bounded_async_status_kills_timed_out_process() {
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", "import time; time.sleep(5)"]);
        let error = bounded_async_status(command, Duration::from_millis(50))
            .await
            .expect_err("timed-out status command should be terminated");
        assert!(error.contains("command exceeded its timeout"));
    }

    #[test]
    fn reverse_name_does_not_execute_shell_input() {
        let marker = std::env::temp_dir().join(format!("screentra-reverse-name-{}", Uuid::new_v4()));
        let malicious = format!("127.0.0.1; touch {}", marker.display());
        let _ = reverse_name(&malicious);
        assert!(!marker.exists());
    }

    #[test]
    fn addon_execution_approval_payloads_reject_unknown_fields() {
        let update: Result<CapabilityApprovalUpdate, _> = serde_json::from_str(
            r#"{"approved":[],"unexpected":true}"#,
        );
        assert!(update.is_err());

        let create: Result<CreateAddonExecutionApproval, _> = serde_json::from_str(
            r#"{
                "addon_id":"example.payload",
                "addon_version":"1.0.0",
                "artifact_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "request":{
                    "targets":["server"],
                    "server":{
                        "image":"registry.example/server",
                        "image_digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "memory_bytes":536870912,
                        "cpu_millis":1000,
                        "pids":128,
                        "writable_bytes":134217728
                    },
                    "client":null,
                    "capabilities":[]
                },
                "unexpected":true
            }"#,
        );
        assert!(create.is_err());
    }
    #[tokio::test]
    async fn addon_execution_approval_list_requires_admin() {
        let result = list_addon_execution_approvals(Extension(crate::middleware::AuthenticatedPrincipal {
            username: "operator".to_string(),
            role: "operator".to_string(),
        }))
        .await;
        assert_eq!(result.unwrap_err().0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn addon_execution_approval_api_persists_review_and_revoke() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        let data_root = std::env::temp_dir().join(format!("screentra-addon-approval-api-{}", Uuid::new_v4()));
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
        }
        let db_path = std::env::temp_dir().join(format!("screentra-addon-approval-api-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open test database"));
        let state = RuntimeState::sqlite(db);
        let admin = crate::middleware::AuthenticatedPrincipal {
            username: "approval-admin".to_string(),
            role: "admin".to_string(),
        };
        let addon_id = "example.execution".to_string();
        let request = crate::addon_capabilities::AddonExecutionRequest {
            targets: vec![crate::addon_capabilities::ExecutionTarget::Client],
            server: None,
            client: Some(crate::addon_capabilities::AddonContainerSpec {
                image: "registry.example/execution".to_string(),
                image_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                memory_bytes: 128 * 1024 * 1024,
                cpu_millis: 500,
                pids: 32,
                writable_bytes: 16 * 1024 * 1024,
            }),
            capabilities: vec![crate::addon_capabilities::AddonCapabilityRequest::camera(
                crate::addon_capabilities::ExecutionTarget::Client,
            )],
        };
        let created = create_addon_execution_approval(
            State(state.clone()),
            Extension(admin.clone()),
            AxumJson(CreateAddonExecutionApproval {
                addon_id: addon_id.clone(),
                addon_version: "1.0.0".to_string(),
                artifact_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                request,
            }),
        )
        .await
        .expect("admin should create pending approval");
        assert_eq!(created.0.state, crate::addon_capabilities::ApprovalState::Pending);

        let updated = update_addon_execution_capabilities(
            State(state.clone()),
            Extension(admin.clone()),
            Path(addon_id.clone()),
            AxumJson(CapabilityApprovalUpdate { approved: vec![true] }),
        )
        .await
        .expect("admin should save capability decision");
        assert_eq!(updated.0.capability_approved, vec![true]);

        let approved = approve_addon_execution_approval(
            State(state.clone()),
            Extension(admin.clone()),
            Path(addon_id.clone()),
        )
        .await
        .expect("admin should approve execution");
        assert_eq!(approved.0.state, crate::addon_capabilities::ApprovalState::Approved);
        assert!(approved.0.can_start().is_ok());

        let revoked = revoke_addon_execution_approval(
            State(state),
            Extension(admin),
            Path(addon_id.clone()),
        )
        .await
        .expect("admin should revoke execution");
        assert_eq!(revoked.0.state, crate::addon_capabilities::ApprovalState::Revoked);
        assert!(revoked.0.can_start().is_err());

        let loaded = crate::addon_capabilities::AddonApprovalStore::load(&addon_id)
            .expect("revoked approval should persist");
        assert_eq!(loaded.state, crate::addon_capabilities::ApprovalState::Revoked);
        if let Some(value) = old_data_dir {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = std::fs::remove_dir_all(data_root);
        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn addon_execution_target_approval_is_persisted_independently() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        let data_root = std::env::temp_dir().join(format!("screentra-addon-target-api-{}", Uuid::new_v4()));
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
        }
        let db_path = std::env::temp_dir().join(format!("screentra-addon-target-api-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open target test database"));
        let state = RuntimeState::sqlite(db);
        let admin = crate::middleware::AuthenticatedPrincipal {
            username: "target-admin".to_string(),
            role: "admin".to_string(),
        };
        let addon_id = "example.dual-api".to_string();
        let request = crate::addon_capabilities::AddonExecutionRequest {
            targets: vec![
                crate::addon_capabilities::ExecutionTarget::Server,
                crate::addon_capabilities::ExecutionTarget::Client,
            ],
            server: Some(crate::addon_capabilities::AddonContainerSpec {
                image: "registry.example/server".to_string(),
                image_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                memory_bytes: 128 * 1024 * 1024,
                cpu_millis: 500,
                pids: 32,
                writable_bytes: 16 * 1024 * 1024,
            }),
            client: Some(crate::addon_capabilities::AddonContainerSpec {
                image: "registry.example/client".to_string(),
                image_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                memory_bytes: 128 * 1024 * 1024,
                cpu_millis: 500,
                pids: 32,
                writable_bytes: 16 * 1024 * 1024,
            }),
            capabilities: vec![
                crate::addon_capabilities::AddonCapabilityRequest::host_api_read(
                    crate::addon_capabilities::ExecutionTarget::Server,
                    "clients",
                ),
                crate::addon_capabilities::AddonCapabilityRequest::camera(
                    crate::addon_capabilities::ExecutionTarget::Client,
                ),
            ],
        };
        let _ = create_addon_execution_approval(
            State(state.clone()),
            Extension(admin.clone()),
            AxumJson(CreateAddonExecutionApproval {
                addon_id: addon_id.clone(),
                addon_version: "1.0.0".to_string(),
                artifact_digest: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string(),
                request,
            }),
        )
        .await
        .expect("admin should create dual-target approval");
        let _ = update_addon_execution_capabilities(
            State(state.clone()),
            Extension(admin.clone()),
            Path(addon_id.clone()),
            AxumJson(CapabilityApprovalUpdate { approved: vec![true, true] }),
        )
        .await
        .expect("admin should approve both requested capabilities");

        let server_approved = approve_addon_execution_target(
            State(state.clone()),
            Extension(admin.clone()),
            Path((addon_id.clone(), "server".to_string())),
        )
        .await
        .expect("server target approval should persist");
        assert_eq!(server_approved.0.target_approved, vec![true, false]);
        assert!(server_approved.0.can_start_for_target(crate::addon_capabilities::ExecutionTarget::Server).is_ok());
        assert_eq!(
            server_approved.0.can_start_for_target(crate::addon_capabilities::ExecutionTarget::Client),
            Err(crate::addon_capabilities::ApprovalError::TargetNotApproved)
        );

        let client_approved = approve_addon_execution_target(
            State(state.clone()),
            Extension(admin.clone()),
            Path((addon_id.clone(), "client".to_string())),
        )
        .await
        .expect("client target approval should persist independently");
        assert_eq!(client_approved.0.target_approved, vec![true, true]);

        let server_revoked = revoke_addon_execution_target(
            State(state.clone()),
            Extension(admin),
            Path((addon_id.clone(), "server".to_string())),
        )
        .await
        .expect("server target revocation should persist independently");
        assert_eq!(server_revoked.0.target_approved, vec![false, true]);
        assert_eq!(
            server_revoked.0.can_start_for_target(crate::addon_capabilities::ExecutionTarget::Server),
            Err(crate::addon_capabilities::ApprovalError::TargetNotApproved)
        );
        assert!(server_revoked.0.can_start_for_target(crate::addon_capabilities::ExecutionTarget::Client).is_ok());
        let loaded = crate::addon_capabilities::AddonApprovalStore::load(&addon_id)
            .expect("target decisions should persist");
        assert_eq!(loaded.target_approved, vec![false, true]);

        if let Some(value) = old_data_dir {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = std::fs::remove_dir_all(data_root);
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn trusted_service_upload_files_are_exclusive_and_private() {
        let root = std::env::temp_dir().join(format!("screentra-upload-{}", Uuid::new_v4()));
        create_private_trusted_service_upload_dir(&root)
            .expect("create private upload test directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::symlink_metadata(&root)
                    .expect("inspect upload directory")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let path = root.join("manifest.json");
        let file = create_private_trusted_service_upload_file(&path)
            .expect("create private upload file");
        drop(file);
        assert!(matches!(
            create_private_trusted_service_upload_file(&path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::symlink_metadata(&path)
                    .expect("inspect upload file")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn trusted_service_upload_rejects_symlinked_ancestors() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("screentra-upload-path-{}", Uuid::new_v4()));
        let real_dir = root.join("real");
        let linked_dir = root.join("linked");
        fs::create_dir_all(&real_dir).unwrap();
        symlink(&real_dir, &linked_dir).unwrap();

        let error = create_private_trusted_service_upload_file(&linked_dir.join("payload"))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("symlinked ancestor"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn weather_alerts_are_assigned_to_each_overlapping_forecast_day() {
        let alerts = vec![
            serde_json::json!({"properties":{"event":"Snow Squall Warning","onset":"2026-10-08T12:00:00-04:00","ends":"2026-10-09T01:00:00-04:00"}}),
            serde_json::json!({"properties":{"event":"Tornado Warning","onset":"2026-10-09T10:00:00-04:00","ends":"2026-10-09T11:00:00-04:00"}}),
            serde_json::json!({"properties":{"event":"Snow Squall Warning","onset":"2026-10-08T12:00:00-04:00","ends":"2026-10-09T01:00:00-04:00"}}),
        ];
        assert_eq!(nws_alerts_for_date(&alerts, "2026-10-08"), vec!["Snow Squall Warning"]);
        assert_eq!(nws_alerts_for_date(&alerts, "2026-10-09"), vec!["Snow Squall Warning", "Tornado Warning"]);
        assert!(nws_alerts_for_date(&alerts, "2026-10-10").is_empty());
    }
    #[test]
    fn weather_daily_precipitation_uses_full_day_maximum() {
        let forecast = vec![
            serde_json::json!({"startTime":"2026-10-08T06:00:00-04:00","probabilityOfPrecipitation":{"value":20}}),
            serde_json::json!({"startTime":"2026-10-08T18:00:00-04:00","probabilityOfPrecipitation":{"value":45}}),
        ];
        let hourly = vec![
            serde_json::json!({"startTime":"2026-10-08T13:00:00-04:00","temperature":65,"probabilityOfPrecipitation":{"value":70}}),
        ];
        assert_eq!(nws_daily_precipitation_probability(&forecast, &hourly, "2026-10-08"), 70.0);
    }

    #[test]
    fn weather_low_fallback_uses_hourly_minimum_when_night_period_is_missing() {
        let hourly = vec![
            serde_json::json!({"startTime":"2026-10-08T01:00:00-04:00","temperature":44}),
            serde_json::json!({"startTime":"2026-10-08T05:00:00-04:00","temperature":41}),
            serde_json::json!({"startTime":"2026-10-08T13:00:00-04:00","temperature":65}),
        ];
        assert_eq!(nws_hourly_min_temperature_for_date(&hourly, "2026-10-08"), Some(41.0));
    }
    #[test]
    fn repository_backend_requires_an_explicit_value() {
        assert_eq!(
            repository_backend_from_value(Some("postgres")),
            Ok(RepositoryBackend::Postgres)
        );
        assert_eq!(
            repository_backend_from_value(Some("sqlite")),
            Ok(RepositoryBackend::Sqlite)
        );
        assert!(repository_backend_from_value(None)
            .expect_err("missing backend must fail closed")
            .contains("must be set explicitly"));
        assert!(repository_backend_from_value(Some("POSTGRES"))
            .expect_err("unknown backend must fail closed")
            .contains("exactly 'postgres' or 'sqlite'"));
    }

    #[test]
    fn client_pairing_code_is_stable_six_digits_and_id_specific() {
        let first = client_pairing_code("client-a");
        assert_eq!(first, client_pairing_code("client-a"));
        assert_eq!(first.len(), 6);
        assert!(first.bytes().all(|byte| byte.is_ascii_digit()));
        assert_ne!(first, client_pairing_code("client-b"));
    }

    #[test]
    fn client_command_policy_rejects_unknown_actions_and_oversized_payloads() {
        assert!(validate_client_command("reload_layout", None).is_ok());
        assert!(validate_client_command("restart_client", None).is_ok());
        assert!(validate_client_command("start_display_preview", Some(&serde_json::json!({"display_index": 0, "duration_seconds": 30}))).is_ok());
        assert!(validate_client_command("stop_display_preview", Some(&serde_json::json!({"display_index": 0}))).is_ok());
        assert!(validate_client_command("run_shell", None).is_err());
        assert!(validate_client_command(
            "reload_layout",
            Some(&serde_json::json!("not an object"))
        )
        .is_err());
        let oversized = serde_json::json!({"data": "x".repeat(70_000)});
        assert!(validate_client_command("refresh_cache", Some(&oversized)).is_err());
    }

    #[test]
    fn client_service_contract_is_bounded_identity_bound_and_inert() {
        assert!(validate_client_command_for_client(
            "client-a",
            "client_service_status",
            Some(&serde_json::json!({"service_id": "speech-to-text"})),
        )
        .is_ok());
        assert!(validate_client_command_for_client(
            "client-a",
            "client_service_rollback_request",
            Some(&serde_json::json!({
                "service_id": "speech-to-text",
                "activation_id": "activation-1",
                "target_version": "1.0.0",
                "reason": "operator-request",
            })),
        )
        .is_ok());
        assert!(validate_client_command_for_client(
            "client-a",
            "client_service_status",
            Some(&serde_json::json!({"client_id": "client-b"})),
        )
        .is_err());
        assert!(validate_client_command_for_client(
            "client-a",
            "client_service_status",
            None,
        )
        .is_err());
        for status_payload in [
            serde_json::json!({"service_id": "speech/to-text"}),
            serde_json::json!({"service_id": "Password-service"}),
            serde_json::json!({"service_id": "speech-to-text", "health": true}),
            serde_json::json!({"service_id": "x".repeat(65)}),
        ] {
            assert!(validate_client_command_for_client(
                "client-a",
                "client_service_status",
                Some(&status_payload),
            )
            .is_err());
        }
        assert!(validate_client_command_for_client(
            "client-a",
            "client_service_rollback_request",
            Some(&serde_json::json!({"service_id": "speech-to-text"})),
        )
        .is_err());
        for action in [
            "client_service_start",
            "client_service_stop",
            "client_service_update",
            "client_service_rollback",
        ] {
            assert!(validate_client_command_for_client("client-a", action, None).is_err());
        }

        let transcribe = serde_json::json!({
            "service_id": "speech-to-text",
            "request_id": "request-001",
            "push_to_talk": true,
        });
        assert!(validate_client_command_for_client(
            "client-a",
            "client_service_transcribe_request",
            Some(&transcribe),
        )
        .is_ok());
        for payload in [
            serde_json::json!({"service_id": "speech-to-text", "request_id": "request-001"}),
            serde_json::json!({"service_id": "speech-to-text", "request_id": "request-001", "push_to_talk": "true"}),
            serde_json::json!({"service_id": "speech-to-text", "request_id": "request-001", "push_to_talk": false}),
            serde_json::json!({"service_id": "x".repeat(65), "request_id": "request-001", "push_to_talk": true}),
            serde_json::json!({"service_id": "password-token", "request_id": "request-001", "push_to_talk": true}),
            serde_json::json!({"service_id": "speech-to-text", "request_id": "token-1", "push_to_talk": true}),
            serde_json::json!({"service_id": "speech/to-text", "request_id": "request-001", "push_to_talk": true}),
            serde_json::json!({"service_id": "speech-to-text", "request_id": "request 001", "push_to_talk": true}),
            serde_json::json!({"service_id": "speech-to-text", "request_id": "request-001", "push_to_talk": true, "audio": "raw"}),
            serde_json::json!({"service_id": "speech-to-text", "request_id": "x".repeat(129), "push_to_talk": true}),
        ] {
            assert!(validate_client_command_for_client(
                "client-a",
                "client_service_transcribe_request",
                Some(&payload),
            )
            .is_err());
        }
    }

    #[tokio::test]
    async fn addon_storage_handlers_enforce_admin_and_round_trip() {
        let db_path = std::env::temp_dir().join(format!("screentra-addon-storage-api-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open test database"));
        let repository = AddonStorageRepository::Sqlite(db);
        let admin = crate::middleware::AuthenticatedPrincipal {
            username: "storage-admin".to_string(),
            role: "admin".to_string(),
        };
        let viewer = crate::middleware::AuthenticatedPrincipal {
            username: "storage-viewer".to_string(),
            role: "viewer".to_string(),
        };
        let addon_id = "example.storage".to_string();
        let storage_key = "settings".to_string();
        let value = serde_json::json!({"enabled": true, "refresh_secs": 30});

        let saved = set_addon_storage(
            State(repository.clone()),
            Extension(admin.clone()),
            Path((addon_id.clone(), storage_key.clone())),
            AxumJson(value.clone()),
        )
        .await
        .expect("admin should save addon storage");
        assert_eq!(saved.0, value);

        let fetched = get_addon_storage(
            State(repository.clone()),
            Extension(admin.clone()),
            Path((addon_id.clone(), storage_key.clone())),
        )
        .await
        .expect("admin should read addon storage");
        assert_eq!(fetched.0, value);

        let forbidden = get_addon_storage(
            State(repository.clone()),
            Extension(viewer),
            Path((addon_id.clone(), storage_key.clone())),
        )
        .await
        .expect_err("non-admin must not read addon storage");
        assert_eq!(forbidden.0, StatusCode::FORBIDDEN);

        let invalid = get_addon_storage(
            State(repository.clone()),
            Extension(admin.clone()),
            Path(("../other-addon".to_string(), storage_key.clone())),
        )
        .await
        .expect_err("traversal namespace must be rejected");
        assert_eq!(invalid.0, StatusCode::BAD_REQUEST);

        let deleted = delete_addon_storage(
            State(repository.clone()),
            Extension(admin.clone()),
            Path((addon_id.clone(), storage_key.clone())),
        )
        .await
        .expect("admin should delete addon storage");
        assert_eq!(deleted.0["deleted"], true);

        let missing = get_addon_storage(
            State(repository),
            Extension(admin),
            Path((addon_id, storage_key)),
        )
        .await
        .expect_err("deleted addon storage should be absent");
        assert_eq!(missing.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(db_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn trusted_service_boundary_selection_is_gate_first_and_socket_free() {
        let db_path = std::env::temp_dir().join(format!("screentra-trusted-boundary-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open test database"));
        let state = RuntimeState::sqlite(db);
        let peer_policy = crate::trusted_service_gateway::GatewayPeerPolicy::new(1000, 1000, 42)
            .expect("peer policy should validate");
        let (gateway, handler) = state
            .trusted_service_gateway("example.service", "activation-1", peer_policy)
            .expect("controlled trusted-service boundary should be constructible");
        let error = gateway
            .run(handler)
            .await
            .expect_err("disabled boundary must not bind a socket");
        assert!(error.contains("disabled"));
        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn trusted_service_database_handler_factory_binds_identity_and_audits_sqlite() {
        let db_path = std::env::temp_dir().join(format!("screentra-trusted-handler-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open test database"));
        let state = RuntimeState::sqlite(db);
        let handler = state
            .trusted_service_database_handler("example.service", "activation-1")
            .expect("SQLite trusted-service handler should be constructible");
        let request = crate::trusted_service_database::DatabaseRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-1".to_string(),
            operation: crate::trusted_service_database::DatabaseOperation::Put {
                key: "health".to_string(),
                value_json: "{\"ok\":true}".to_string(),
            },
        };
        let response = crate::trusted_service_gateway::GatewayOperationHandler::handle(
            &*handler,
            request,
        )
        .await
        .expect("trusted-service handler should write through the controlled boundary");
        assert_eq!(
            response,
            crate::trusted_service_database::DatabaseResponse::Written
        );
        let events = state
            .client_repository()
            .list_audit_events(10)
            .await
            .expect("database boundary audit events should be readable");
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event.event_type == "trusted_service_database"));
        let outcomes: Vec<_> = events.iter().map(|event| event.outcome.as_str()).collect();
        assert!(outcomes.contains(&"accepted"));
        assert!(outcomes.contains(&"succeeded"));
        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn trusted_service_stage_removal_is_audited() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let db_path = std::env::temp_dir().join(format!("screentra-trusted-stage-audit-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open test database"));
        let state = RuntimeState::sqlite(db);
        let admin = crate::middleware::AuthenticatedPrincipal {
            username: "stage-admin".to_string(),
            role: "admin".to_string(),
        };
        let root = std::env::temp_dir().join(format!("screentra-trusted-stage-root-{}", Uuid::new_v4()));
        let old_data_root = std::env::var("SIGNAGE_DATA_DIR").ok();
        unsafe { std::env::set_var("SIGNAGE_DATA_DIR", &root); }
        let stage_id = format!("missing.{}", Uuid::new_v4().simple());

        let removed = remove_trusted_service_stage(
            State(state.clone()),
            Extension(admin.clone()),
            Path(stage_id.clone()),
        )
        .await
        .expect("admin should be allowed to remove a staged service");
        assert_eq!(removed, StatusCode::NO_CONTENT);

        let events = state
            .client_repository()
            .list_audit_events(10)
            .await
            .expect("audit listing should succeed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "trusted_service_stage_remove");
        assert_eq!(events[0].target.as_deref(), Some(stage_id.as_str()));
        assert_eq!(events[0].outcome, "success");
        if let Some(value) = old_data_root {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn trusted_service_stage_handlers_require_admin() {
        let db_path = std::env::temp_dir().join(format!("screentra-trusted-stage-auth-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open test database"));
        let state = RuntimeState::sqlite(db);
        let viewer = crate::middleware::AuthenticatedPrincipal {
            username: "stage-viewer".to_string(),
            role: "viewer".to_string(),
        };
        let listed = list_trusted_service_stages(Extension(viewer.clone()))
            .await
            .expect_err("viewer must not list trusted service stages");
        assert_eq!(listed.0, StatusCode::FORBIDDEN);
        let removed = remove_trusted_service_stage(
            State(state),
            Extension(viewer),
            Path("example.weather".to_string()),
        )
        .await
        .expect_err("viewer must not remove trusted service stages");
        assert_eq!(removed.0, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn authenticated_audit_listing_reads_bounded_update_failure_events() {
        let db_path = std::env::temp_dir().join(format!("screentra-audit-api-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open test database"));
        let state = RuntimeState::sqlite(db.clone());
        let principal = crate::middleware::AuthenticatedPrincipal {
            username: "audit-test".to_string(),
            role: "admin".to_string(),
        };

        audit_update_import_failure(&state, &principal, "bundle file missing").await;
        let result = list_audit_events_handler(State(state), Query(HashMap::new()))
            .await
            .expect("audit listing should succeed");
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.0[0].event_type, "server_update_import");
        assert_eq!(result.0[0].outcome, "failure");
        assert_eq!(result.0[0].detail.as_deref(), Some("bundle file missing"));
        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn disposable_sqlite_invalid_update_failure_is_audited() {
        let db_path = std::env::temp_dir().join(format!("screentra-update-audit-{}.db", Uuid::new_v4()));
        let archive_path = std::env::temp_dir().join(format!("screentra-invalid-update-{}.tar.gz", Uuid::new_v4()));
        std::fs::write(&archive_path, b"not a tar archive").expect("write disposable invalid archive");
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open test database"));
        let state = RuntimeState::sqlite(db);
        let principal = crate::middleware::AuthenticatedPrincipal {
            username: "audit-test".to_string(),
            role: "admin".to_string(),
        };

        let error = crate::version::import_update_bundle(&archive_path, false)
            .expect_err("invalid disposable update must be rejected");
        let (stage_event, stage_detail) = update_failure_stage(&error);
        state
            .client_repository()
            .record_audit_event(
                Some(&principal.username),
                Some(&principal.role),
                stage_event,
                Some("server-update-bundle"),
                "failure",
                Some(stage_detail),
                &Utc::now().to_rfc3339(),
            )
            .await
            .expect("record staged update failure");
        audit_update_import_failure(&state, &principal, "bundle verification or application rejected").await;

        let result = list_audit_events_handler(State(state), Query(HashMap::new()))
            .await
            .expect("audit listing should succeed");
        assert_eq!(result.0.len(), 2);
        assert_eq!(result.0[0].event_type, "server_update_import");
        assert_eq!(result.0[0].detail.as_deref(), Some("bundle verification or application rejected"));
        assert_eq!(result.0[1].event_type, "server_update_verify");
        assert_eq!(result.0[1].detail.as_deref(), Some("archive validation rejected"));
        let _ = std::fs::remove_file(archive_path);
        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    #[ignore = "requires a disposable PostgreSQL database supplied through SIGNAGE_DATABASE_URL"]
    async fn postgres_authenticated_audit_listing_matches_sqlite_sanitization() {
        assert!(
            std::env::var("SIGNAGE_DATABASE_URL")
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false),
            "SIGNAGE_DATABASE_URL must point to a disposable PostgreSQL database"
        );
        crate::postgres::apply_initial_schema()
            .await
            .expect("apply PostgreSQL schema");
        let store = Arc::new(
            crate::postgres::PostgresStore::connect()
                .await
                .expect("connect to disposable PostgreSQL database"),
        );
        let state = RuntimeState {
            legacy_db: None,
            client_repository: ClientRepository::Postgres(store.clone()),
            addon_storage_repository: AddonStorageRepository::Postgres(store.clone()),
            auth_repository: crate::auth::AuthRepository::Postgres(store.clone()),
            backup_repository: backup_api::BackupMetadataRepository::Postgres(store),
            metrics: crate::observability::Metrics::new(),
        };
        let principal = crate::middleware::AuthenticatedPrincipal {
            username: "audit-test".to_string(),
            role: "admin".to_string(),
        };

        audit_update_import_failure(&state, &principal, "bundle file missing").await;
        let oversized_detail = format!("detail\n{}", "x".repeat(600));
        state
            .client_repository()
            .record_audit_event(
                Some("actor\nwith-control"),
                Some("administrator"),
                "test_event",
                Some("target"),
                "failure",
                Some(&oversized_detail),
                "2026-10-02T00:00:00Z",
            )
            .await
            .expect("record PostgreSQL audit event");

        let result = list_audit_events_handler(State(state), Query(HashMap::new()))
            .await
            .expect("PostgreSQL audit listing should succeed");
        assert_eq!(result.0.len(), 2);
        assert_eq!(result.0[1].event_type, "server_update_import");
        assert_eq!(result.0[1].detail.as_deref(), Some("bundle file missing"));
        assert_eq!(result.0[0].actor.as_deref(), Some("actorwith-control"));
        assert_eq!(result.0[0].detail.as_ref().map(String::len), Some(512));
        assert!(!result.0[0].detail.as_deref().unwrap().contains('\n'));
    }

    #[test]
    fn virtual_clients_are_not_classified_as_stale_or_pending_after_publish() {
        assert_eq!(
            effective_client_display_status(true, true, "1970-01-01T00:00:00Z"),
            "online"
        );
        assert_eq!(
            effective_client_display_status(true, false, "1970-01-01T00:00:00Z"),
            "disconnected"
        );
        assert_eq!(effective_observed_revision(true, 7, None), Some(7));
        assert_eq!(effective_observed_revision(false, 7, None), None);
        assert_eq!(effective_observed_revision(false, 7, Some(6)), Some(6));
        assert_eq!(effective_observed_revision(true, 0, None), None);
    }

    #[test]
    fn client_auth_token_matching_accepts_own_token_and_rejects_invalid_or_other_client_tokens() {
        let token_a = String::from_utf8(vec![b'a'; 40]).unwrap();
        let token_b = String::from_utf8(vec![b'b'; 40]).unwrap();
        let hash_a = hash_client_auth_token(&token_a);
        let hash_b = hash_client_auth_token(&token_b);
        assert!(client_auth_token_matches(&hash_a, &token_a));
        assert!(!client_auth_token_matches(&hash_a, &token_b));
        assert!(!client_auth_token_matches(&hash_b, &token_a));
        assert!(!client_auth_token_matches(&hash_a, "invalid"));
    }

    #[test]
    fn nws_daily_code_uses_the_prevailing_clause() {
        assert_eq!(weather_code_from_nws("Patchy Fog then Partly Sunny"), 2);
        assert_eq!(weather_code_from_nws("Mostly Cloudy then Mostly Sunny"), 1);
        assert_eq!(weather_code_from_nws("Fog"), 45);
    }

    #[test]
    fn nws_hourly_code_uses_midday_for_day_icon() {
        let periods = vec![
            serde_json::json!({"startTime":"2026-09-29T06:00:00-04:00","isDaytime":true,"shortForecast":"Patchy Fog"}),
            serde_json::json!({"startTime":"2026-09-29T08:00:00-04:00","isDaytime":true,"shortForecast":"Partly Sunny"}),
            serde_json::json!({"startTime":"2026-09-29T12:00:00-04:00","isDaytime":true,"shortForecast":"Partly Sunny"}),
            serde_json::json!({"startTime":"2026-09-29T22:00:00-04:00","isDaytime":false,"shortForecast":"Partly Cloudy"}),
        ];
        assert_eq!(nws_hourly_code_for_date(&periods, "2026-09-29", true, 12), Some(2));
        assert_eq!(nws_hourly_code_for_date(&periods, "2026-09-29", false, 22), Some(2));
        assert_eq!(nws_morning_transition_for_date(&periods, "2026-09-29", 2), Some((45, "2026-09-29T08:00:00-04:00".to_string())));
    }

    /// Widget templates are allowed for the "complex" widget types
    /// (weather, calendar, news, webpage/embed) and forbidden for the
    /// simple atomic ones (text, image, video, clock, date, datetime,
    /// countdown). This pins the contract so the dropdown options in
    /// index.html and the server's RESTRICTED_PRESET_TYPES stay in
    /// sync with the Widget Templates editor's intended scope.
    #[test]
    fn validate_preset_type_matches_documented_scope() {
        // Allowed: the four "complex" widget types.
        for t in ["weather", "calendar", "news", "webpage"] {
            assert!(
                validate_preset_type(t).is_ok(),
                "preset type {t:?} should be allowed"
            );
        }
        // Forbidden: the seven atomic widget types.
        for t in ["text", "image", "video", "clock", "date", "datetime", "countdown"] {
            assert!(
                validate_preset_type(t).is_err(),
                "preset type {t:?} should be rejected"
            );
        }
    }

    #[test]
    fn uploads_dir_accepts_allowlisted_categories() {
        for c in ["images", "videos", "audio", "templates", "icons", "misc"] {
            let d = uploads_dir(c).expect("should accept");
            assert!(d.ends_with(c), "category {} should yield {:?}", c, d);
        }
    }

    #[test]
    fn public_uploads_reject_active_content_and_force_safe_serving_policy() {
        for name in ["page.html", "script.js", "module.mjs", "style.css", "image.svg", "payload.wasm"] {
            assert!(is_active_upload_name(name), "must classify {name} as active content");
        }
        for name in ["photo.png", "movie.mp4", "font.woff2", "sound.ogg"] {
            assert!(!is_active_upload_name(name), "must allow inert asset {name}");
        }
    }

    #[test]
    fn uploads_dir_rejects_traversal_and_unknowns() {
        for bad in [
            "..", "../etc", "..\\foo", "/etc", "", "  ", "foo/bar", "foo\\bar", ".", "..\u{0}",
        ] {
            assert!(
                uploads_dir(bad).is_err(),
                "should reject {:?} but accepted",
                bad
            );
        }
    }

    #[test]
    fn widget_preset_props_normalizes_exported_props_json_string() {
        let props = normalize_widget_preset_props(Some(serde_json::Value::String(
            r#"{"template_elements":[{"type":"text"}],"template_width":800}"#.to_string(),
        )))
        .expect("exported props_json string should parse");
        assert!(props.is_object());
        assert_eq!(
            props
                .get("template_elements")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            props.get("template_width").and_then(|v| v.as_i64()),
            Some(800)
        );
    }

    #[test]
    fn widget_preset_export_shape_is_new_code_only() {
        let props = normalize_widget_preset_props(Some(serde_json::Value::String(
            r#"{"template_elements":[{"type":"shape"}],"template_height":480}"#.to_string(),
        )))
        .expect("props_json string should parse for export");
        let preset_export = serde_json::json!({
            "name": "portable",
            "widget_type": "weather",
            "props": props,
            "assets": []
        });
        assert!(preset_export.get("props_json").is_none());
        let props = preset_export.get("props").expect("props present");
        assert!(props.is_object(), "exported props must be a JSON object");
        assert_eq!(
            props
                .get("template_elements")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
    }

    #[test]
    fn widget_preset_assets_fall_back_to_custom_icons() {
        let props = serde_json::json!({
            "custom_icons": {
                "0": "/uploads/images/clear.png",
                "45": "/uploads/icons/fog.svg",
                "bad": ""
            }
        });
        let assets = widget_preset_assets_from_export(None, &props);
        let arr = assets.as_array().expect("assets array");
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0].get("kind").and_then(|v| v.as_str()),
            Some("weather_icon")
        );
        assert!(arr
            .iter()
            .any(|a| a.get("key").and_then(|v| v.as_str()) == Some("0")));
        assert!(arr
            .iter()
            .any(|a| a.get("key").and_then(|v| v.as_str()) == Some("45")));
    }

    #[test]
    fn widget_preset_assets_prefer_explicit_assets() {
        let props = serde_json::json!({
            "custom_icons": { "0": "/uploads/images/clear.png" }
        });
        let assets = widget_preset_assets_from_export(
            Some(serde_json::json!([
                {"kind":"weather_icon","key":"61","url":"/uploads/images/rain.png"}
            ])),
            &props,
        );
        let arr = assets.as_array().expect("assets array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("key").and_then(|v| v.as_str()), Some("61"));
    }

    #[test]
    fn widget_preset_props_accepts_object_and_rejects_non_object() {
        let props = normalize_widget_preset_props(Some(serde_json::json!({
            "template_elements": [],
            "template_width": 640,
        })))
        .expect("object props should be accepted");
        assert_eq!(
            props.get("template_width").and_then(|v| v.as_i64()),
            Some(640)
        );

        assert!(normalize_widget_preset_props(Some(serde_json::json!([]))).is_err());
        assert!(
            normalize_widget_preset_props(Some(serde_json::Value::String("[]".to_string())))
                .is_err()
        );
    }

    #[test]
    fn safe_filename_strips_path_components() {
        for (input, expected) in [
            ("hello.txt", "hello.txt"),
            ("../escape.txt", "escape.txt"),
            ("a/b/c.png", "c.png"),
            ("..\\windows.png", "windows.png"),
            ("plain", "plain"),
        ] {
            let got = safe_filename(input).expect("should accept");
            assert_eq!(got, expected, "input={:?}", input);
        }
    }

    #[test]
    fn safe_filename_normalizes_slash_prefix() {
        // Path components before the basename are stripped: "/foo" -> "foo".
        // This is intentional behavior so simple uploads do not 400.
        assert_eq!(safe_filename("/foo").unwrap(), "foo");
        assert_eq!(safe_filename("a/b/foo").unwrap(), "foo");
    }

    #[test]
    fn safe_filename_rejects_empty_and_traversal_only() {
        for bad in ["", ".", "..", "/", "\\", "\u{0010}"] {
            assert!(safe_filename(bad).is_err(), "should reject {:?}", bad);
        }
    }

    #[test]
    fn safe_filename_rejects_nul_and_control_bytes() {
        for bad in ["ok\u{0}.txt", "ok\n.txt", "ok\t.txt"] {
            assert!(safe_filename(bad).is_err(), "should reject {:?}", bad);
        }
    }
}

#[test]
fn validate_proxy_url_rejects_disallowed_schemes() {
    for bad in [
        "file:///etc/passwd",
        "ftp://example.com/foo",
        "gopher://example.com/",
        "javascript:alert(1)",
    ] {
        assert!(validate_proxy_url(bad).is_err(), "should reject {:?}", bad);
    }
}

#[test]
fn validate_proxy_url_rejects_localhost_aliases() {
    for bad in [
        "http://localhost/cal.ics",
        "http://Localhost/cal.ics",
        "http://x.localhost/cal.ics",
        "http://printer.local/cal.ics",
    ] {
        let r = validate_proxy_url(bad);
        assert!(r.is_err(), "should reject {:?}, got {:?}", bad, r);
    }
}

#[test]
fn validate_proxy_url_rejects_loopback_ip_literals() {
    for bad in [
        "http://127.0.0.1/cal.ics",
        "http://127.255.255.254/cal.ics",
        "http://[::1]/cal.ics",
        "http://[::]/cal.ics",
    ] {
        let r = validate_proxy_url(bad);
        assert!(r.is_err(), "should reject {:?}, got {:?}", bad, r);
    }
}

#[test]
fn validate_proxy_url_rejects_private_ip_literals() {
    for bad in [
        "http://10.0.0.1/cal.ics",
        "http://192.168.1.1/cal.ics",
        "http://172.16.0.1/cal.ics",
        "http://169.254.169.254/latest/meta-data/", // AWS link-local
        "http://[fc00::1]/cal.ics",
        "http://[fd00::1]/cal.ics",
        "http://[fe80::1]/cal.ics",
    ] {
        let r = validate_proxy_url(bad);
        assert!(r.is_err(), "should reject {:?}, got {:?}", bad, r);
    }
}

#[test]
fn validate_proxy_url_rejects_malformed() {
    for bad in ["", "not a url", "http://"] {
        assert!(validate_proxy_url(bad).is_err(), "should reject {:?}", bad);
    }
}

#[test]
fn validate_proxy_url_accepts_public_http_https() {
    // These must not resolve to private/loopback ranges. Use example.com.
    for ok in ["https://example.com/cal.ics", "http://example.com/cal.ics"] {
        let r = validate_proxy_url(ok);
        assert!(r.is_ok(), "should accept {:?}, got {:?}", ok, r);
    }
}

#[test]
fn interactive_normalize_field_caps_length_and_rejects_control_chars() {
    // Under cap passes through.
    assert_eq!(normalize_field("hello", 64).unwrap(), "hello");
    // Empty returns empty.
    assert_eq!(normalize_field("", 64).unwrap(), "");
    // At cap passes.
    let s = "a".repeat(64);
    assert_eq!(normalize_field(&s, 64).unwrap().len(), 64);
    // Over cap fails.
    let s = "a".repeat(65);
    assert!(normalize_field(&s, 64).is_err());
    // Control char fails.
    assert!(normalize_field("a\nb", 64).is_err());
    assert!(normalize_field("a\t", 64).is_err());
}

#[test]
fn interactive_resolve_client_id_prefers_header_over_body() {
    assert_eq!(
        resolve_client_id(Some("header-id"), Some("body-id")),
        "header-id"
    );
    assert_eq!(resolve_client_id(None, Some("body-id")), "body-id");
    assert_eq!(resolve_client_id(Some(""), Some("body-id")), "body-id");
    assert_eq!(resolve_client_id(None, None), "unknown");
}

#[test]
fn interactive_resolve_client_id_strips_control_chars_and_caps() {
    let mut big = "a".repeat(200);
    big.push('\n');
    let out = resolve_client_id(Some(&big), None);
    assert_eq!(out.len(), 64);
    assert!(!out.contains('\n'));
}

#[test]
fn interactive_validate_collected_at_accepts_valid_and_rejects_invalid() {
    assert!(validate_collected_at(Some("2026-08-01T20:14:33-04:00")).is_ok());
    assert!(validate_collected_at(Some("2026-08-01T20:14:33Z")).is_ok());
    assert!(validate_collected_at(None).is_ok()); // server fills now
    assert!(validate_collected_at(Some("")).is_ok()); // empty -> now
    assert!(validate_collected_at(Some("yesterday")).is_err());
    assert!(validate_collected_at(Some("1234567890")).is_err());
}

#[test]
fn interactive_validate_payload_button_accepts_canonical_shape() {
    let p = serde_json::json!({ "values": {"name": "Alice"}, "acknowledged": true });
    let s = validate_payload("button", &p).unwrap();
    assert!(s.contains("\"values\""));
    assert!(s.contains("\"Alice\""));
    assert!(s.contains("\"acknowledged\":true"));
}

#[test]
fn interactive_validate_payload_button_rejects_non_object() {
    let p = serde_json::json!("just a string");
    assert!(validate_payload("button", &p).is_err());
    let p = serde_json::json!(42);
    assert!(validate_payload("button", &p).is_err());
}

#[test]
fn interactive_validate_payload_button_rejects_oversized_values() {
    // 5 KiB string in values
    let big = "a".repeat(5 * 1024);
    let p = serde_json::json!({ "values": {"x": big} });
    let err = validate_payload("button", &p).unwrap_err();
    assert!(err.contains("exceeds"), "got: {err}");
}

#[test]
fn interactive_validate_payload_textbox_dropdown_radio_caps_string() {
    assert!(validate_payload("textbox", &serde_json::json!("hi")).is_ok());
    assert!(validate_payload("dropdown", &serde_json::json!("opt")).is_ok());
    assert!(validate_payload("radio", &serde_json::json!("one")).is_ok());
    // Wrong type
    assert!(validate_payload("textbox", &serde_json::json!(1)).is_err());
    // Too long
    let big = "a".repeat(257);
    assert!(validate_payload("textbox", &serde_json::json!(big)).is_err());
}

#[test]
fn interactive_validate_payload_checkbox_requires_bool() {
    assert!(validate_payload("checkbox", &serde_json::json!(true)).is_ok());
    assert!(validate_payload("checkbox", &serde_json::json!(false)).is_ok());
    assert!(validate_payload("checkbox", &serde_json::json!("yes")).is_err());
}

#[test]
fn interactive_validate_payload_checkbox_list_caps_items_and_strings() {
    let arr: Vec<&str> = (0..64).map(|_| "x").collect();
    let p = serde_json::json!(arr);
    assert!(validate_payload("checkbox-list", &p).is_ok());

    let arr: Vec<&str> = (0..65).map(|_| "x").collect();
    let p = serde_json::json!(arr);
    assert!(validate_payload("checkbox-list", &p).is_err());

    let big = "a".repeat(129);
    let p = serde_json::json!(vec![big.as_str()]);
    assert!(validate_payload("checkbox-list", &p).is_err());

    // Non-string item
    let p = serde_json::json!(vec![1, 2, 3]);
    assert!(validate_payload("checkbox-list", &p).is_err());
}

#[cfg(test)]
mod signed_client_package_http_tests {
    use super::*;
    use axum::{body::{to_bytes, Body}, http::{header, Request}};
    use flate2::{write::GzEncoder, Compression};
    use sha2::{Digest, Sha256};
    use std::{fs, path::{Path, PathBuf}, process::Command, sync::Mutex};
    use tar::Builder;

    struct Fixture {
        root: PathBuf,
        addon_dir: PathBuf,
        old_data_root: Option<std::ffi::OsString>,
        old_public_key: Option<std::ffi::OsString>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
            unsafe {
                match self.old_data_root.take() {
                    Some(value) => std::env::set_var("SIGNAGE_DATA_DIR", value),
                    None => std::env::remove_var("SIGNAGE_DATA_DIR"),
                }
                match self.old_public_key.take() {
                    Some(value) => std::env::set_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY", value),
                    None => std::env::remove_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY"),
                }
            }
        }
    }

    fn setup_fixture() -> (Fixture, PathBuf) {
        let root = std::env::temp_dir().join(format!("screentra-addon-http-{}", Uuid::new_v4()));
        let data_root = root.join("data");
        let private_key = root.join("addon-signing.key");
        let public_key = root.join("addon-signing.pub");
        fs::create_dir_all(&root).unwrap();
        assert!(Command::new("/usr/bin/openssl")
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("/usr/bin/openssl")
            .args(["pkey", "-in"])
            .arg(&private_key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        let fixture = Fixture {
            addon_dir: data_root.join("addons/example.weather"),
            old_data_root: std::env::var_os("SIGNAGE_DATA_DIR"),
            old_public_key: std::env::var_os("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY"),
            root,
        };
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
            std::env::set_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY", &public_key);
        }
        (fixture, private_key)
    }

    fn package(
        root: &Path,
        private_key: &Path,
        id: &str,
        version: &str,
        signed_payload: &[u8],
        archive_payload: &[u8],
    ) -> PathBuf {
        let package_root = root.join(format!("package-{}", Uuid::new_v4()));
        let client_root = package_root.join("client");
        fs::create_dir_all(&client_root).unwrap();
        let manifest = serde_json::json!({
            "schema_version": 1,
            "id": id,
            "name": "HTTP package",
            "description": "signed HTTP test package",
            "version": version,
            "kind": "client_extension",
            "widget_types": [],
            "permissions": [],
            "client_capabilities": [],
            "client_entrypoint": "client/index.js",
            "client_sha256": format!("{:x}", Sha256::digest(signed_payload)),
            "client_api_version": "1"
        });
        let manifest_path = package_root.join("manifest.json");
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        let signature_path = package_root.join("manifest.sig");
        assert!(Command::new("/usr/bin/openssl")
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(private_key)
            .args(["-in"])
            .arg(&manifest_path)
            .args(["-out"])
            .arg(&signature_path)
            .status()
            .unwrap()
            .success());
        fs::write(client_root.join("index.js"), archive_payload).unwrap();
        let archive = root.join(format!("package-{}.tar.gz", Uuid::new_v4()));
        let archive_file = fs::File::create(&archive).unwrap();
        let encoder = GzEncoder::new(archive_file, Compression::default());
        let mut builder = Builder::new(encoder);
        builder.append_path_with_name(&manifest_path, "manifest.json").unwrap();
        builder.append_path_with_name(&signature_path, "manifest.sig").unwrap();
        builder.append_dir_all("client", &client_root).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        archive
    }

    fn multipart_request(method: &str, archive: &[u8]) -> Request<Body> {
        let boundary = "signed-package-test-boundary";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"package\"; filename=\"addon.tar.gz\"\r\nContent-Type: application/gzip\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(archive);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        Request::builder()
            .method(method)
            .uri("/api/addons/package")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap()
    }

    async fn send(app: &Router, method: &str, archive: &Path) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(multipart_request(method, &fs::read(archive).unwrap()))
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn addon_package_upload_rejects_oversized_payload() {
        let app = rollback_app("admin");
        let oversized = vec![0u8; 8 * 1024 * 1024 + 1];
        let response = app
            .oneshot(multipart_request("POST", &oversized))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(
            String::from_utf8(body.to_vec()).unwrap().contains("too large"),
            "unexpected response body: {}",
            String::from_utf8_lossy(&body)
        );
    }

    fn rollback_request(id: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/api/addons/package/{id}/rollback"))
            .body(Body::empty())
            .unwrap()
    }

    async fn send_rollback(app: &Router, id: &str) -> (StatusCode, String) {
        let response = app.clone().oneshot(rollback_request(id)).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn rollback_app(role: &str) -> Router {
        let db = Arc::new(Database::open(":memory:").expect("open in-memory test database"));
        rollback_app_with_state(role, RuntimeState::sqlite(db))
    }

    fn rollback_app_with_state(role: &str, state: RuntimeState) -> Router {
        Router::new()
            .route(
                "/api/addons/package",
                post(upload_addon_package).put(update_addon_package),
            )
            .route(
                "/api/addons/package/{id}/rollback",
                post(rollback_signed_client_package_handler),
            )
            .route("/api/addons/{id}/rollback", post(rollback_addon_handler))
            .layer(Extension(crate::middleware::AuthenticatedPrincipal {
                username: "test-user".to_string(),
                role: role.to_string(),
            }))
            .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
            .with_state(state)
    }

    #[tokio::test]
    async fn signed_client_package_http_rollback_audits_success_without_package_contents() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let (fixture, private_key) = setup_fixture();
        let db_path = std::env::temp_dir().join(format!("screentra-addon-audit-{}.db", Uuid::new_v4()));
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open audit test database"));
        let state = RuntimeState::sqlite(db);
        let app = rollback_app_with_state("admin", state.clone());
        let old_payload = b"window.ScreentraAddon = { version: '0.1.0' };\\n";
        let new_payload = b"window.ScreentraAddon = { version: '0.2.0' };\\n";

        let install = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.1.0",
            old_payload,
            old_payload,
        );
        let (status, body) = send(&app, "POST", &install).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let update = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        let (status, body) = send(&app, "PUT", &update).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = send_rollback(&app, "example.weather").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let events = state
            .client_repository()
            .list_audit_events(10)
            .await
            .expect("read rollback audit row");
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.actor.as_deref(), Some("test-user"));
        assert_eq!(event.role.as_deref(), Some("admin"));
        assert_eq!(event.event_type, "signed_client_package_rollback");
        assert_eq!(event.target.as_deref(), Some("example.weather"));
        assert_eq!(event.outcome, "success");
        assert_eq!(event.detail, None);
        assert!(!event.event_type.contains("window.ScreentraAddon"));
        assert!(!event.target.as_deref().unwrap().contains("client/index.js"));
        drop(state);
        let _ = fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn signed_client_package_http_rollback_is_inert_and_preserves_state() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let (fixture, private_key) = setup_fixture();
        let old_payload = b"window.ScreentraAddon = { version: '0.1.0' };\\n";
        let new_payload = b"window.ScreentraAddon = { version: '0.2.0' };\\n";
        let app = rollback_app("admin");

        let install = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.1.0",
            old_payload,
            old_payload,
        );
        let (status, body) = send(&app, "POST", &install).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        crate::addons::set_enabled("example.weather", false).unwrap();

        let update = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        let (status, body) = send(&app, "PUT", &update).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!serde_json::from_str::<AddonRecord>(&body).unwrap().enabled);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);

        let (status, body) = send_rollback(&app, "example.weather").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let restored: AddonRecord = serde_json::from_str(&body).unwrap();
        assert_eq!(restored.manifest.version, "0.1.0");
        assert!(!restored.enabled);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), old_payload);
        assert!(!fixture.root.join("javascript-executed").exists());
        assert!(!fixture.root.join("process-executed").exists());
    }

    #[tokio::test]
    async fn signed_client_package_http_rollback_rejects_tampered_or_missing_backup() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let (fixture, private_key) = setup_fixture();
        let state = RuntimeState::sqlite(Arc::new(
            Database::open(":memory:").expect("open in-memory audit database"),
        ));
        let app = rollback_app_with_state("admin", state.clone());
        let old_payload = b"window.ScreentraAddon = { version: '0.1.0' };\\n";
        let new_payload = b"window.ScreentraAddon = { version: '0.2.0' };\\n";
        let install = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.1.0",
            old_payload,
            old_payload,
        );
        let (status, body) = send(&app, "POST", &install).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        crate::addons::set_enabled("example.weather", false).unwrap();
        let update = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        let (status, body) = send(&app, "PUT", &update).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let rollback_dir = fixture
            .addon_dir
            .parent()
            .unwrap()
            .join(".rollback")
            .join("example.weather");
        let manifest_path = rollback_dir.join("manifest.json");
        let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["name"] = serde_json::Value::String("tampered rollback".to_string());
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let (status, _) = send_rollback(&app, "example.weather").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);
        let events = state
            .client_repository()
            .list_audit_events(10)
            .await
            .expect("read signed rollback failure audit row");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "signed_client_package_rollback");
        assert_eq!(events[0].outcome, "failure");
        assert_eq!(events[0].detail.as_deref(), Some("rollback rejected"));

        fs::remove_dir_all(&rollback_dir).unwrap();
        let (status, _) = send_rollback(&app, "example.weather").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);
        let events = state
            .client_repository()
            .list_audit_events(10)
            .await
            .expect("read missing signed rollback failure audit row");
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].event_type, "signed_client_package_rollback");
        assert_eq!(events[1].outcome, "failure");
        assert_eq!(events[1].detail.as_deref(), Some("rollback rejected"));
    }

    #[tokio::test]
    async fn signed_client_package_http_rollback_rejects_bad_identity_and_non_admin() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let (fixture, private_key) = setup_fixture();
        let admin_app = rollback_app("admin");
        let old_payload = b"window.ScreentraAddon = { version: '0.1.0' };\\n";
        let install = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.1.0",
            old_payload,
            old_payload,
        );
        let (status, body) = send(&admin_app, "POST", &install).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, _) = send_rollback(&admin_app, "bad%20identity").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = send_rollback(&admin_app, "%2e%2e%2fexample.weather").await;
        assert!(matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND));
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), old_payload);

        let operator_app = rollback_app("operator");
        let (status, _) = send_rollback(&operator_app, "example.weather").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), old_payload);
    }

    #[tokio::test]
    async fn declarative_addon_http_rollback_restores_manifest_and_audits_success() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!("screentra-declarative-http-{}", Uuid::new_v4()));
        let data_dir = root.join("data");
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe { std::env::set_var("SIGNAGE_DATA_DIR", &data_dir); }
        let addon_id = format!("example.http.{}", Uuid::new_v4().simple());
        let initial = AddonManifest {
            schema_version: 1,
            id: addon_id.clone(),
            name: "HTTP declarative test".to_string(),
            description: "metadata-only rollback test".to_string(),
            version: "0.1.0".to_string(),
            kind: "declarative".to_string(),
            min_server_version: None,
            min_client_version: None,
            widget_types: Vec::new(),
            permissions: Vec::new(),
            client_capabilities: Vec::new(),
            data_contract: None,
            client_entrypoint: None,
            server_entrypoint: None,
            trusted_service: None,
            client_service: None,
            client_sha256: None,
            client_api_version: None,
        };
        crate::addons::install(initial.clone()).expect("install declarative test addon");
        let mut updated = initial;
        updated.version = "0.2.0".to_string();
        updated.name = "Updated HTTP declarative test".to_string();
        crate::addons::update(updated).expect("update declarative test addon");

        let state = RuntimeState::sqlite(Arc::new(
            Database::open(":memory:").expect("open in-memory audit database"),
        ));
        let operator_app = rollback_app_with_state("operator", state.clone());
        let denied = operator_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/addons/{addon_id}/rollback"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let app = rollback_app_with_state("admin", state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/addons/{addon_id}/rollback"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let restored: AddonRecord = serde_json::from_slice(&body).expect("rollback response");

        let events = state
            .client_repository()
            .list_audit_events(10)
            .await
            .expect("read addon rollback audit row");
        if let Some(value) = old_data_dir {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = fs::remove_dir_all(root);

        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(restored.manifest.version, "0.1.0");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "addon_rollback");
        assert_eq!(events[0].target.as_deref(), Some(addon_id.as_str()));
        assert_eq!(events[0].outcome, "success");
    }

    #[tokio::test]
    async fn declarative_addon_http_rollback_failure_is_audited_without_mutation() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!("screentra-declarative-http-failure-{}", Uuid::new_v4()));
        let data_dir = root.join("data");
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe { std::env::set_var("SIGNAGE_DATA_DIR", &data_dir); }
        let addon_id = format!("example.http.failure.{}", Uuid::new_v4().simple());
        let initial = AddonManifest {
            schema_version: 1,
            id: addon_id.clone(),
            name: "HTTP declarative failure test".to_string(),
            description: String::new(),
            version: "0.1.0".to_string(),
            kind: "declarative".to_string(),
            min_server_version: None,
            min_client_version: None,
            widget_types: Vec::new(),
            permissions: Vec::new(),
            client_capabilities: Vec::new(),
            data_contract: None,
            client_entrypoint: None,
            server_entrypoint: None,
            trusted_service: None,
            client_service: None,
            client_sha256: None,
            client_api_version: None,
        };
        crate::addons::install(initial).expect("install declarative test addon");

        let state = RuntimeState::sqlite(Arc::new(
            Database::open(":memory:").expect("open in-memory audit database"),
        ));
        let app = rollback_app_with_state("admin", state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/addons/{addon_id}/rollback"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let events = state
            .client_repository()
            .list_audit_events(10)
            .await
            .expect("read addon rollback failure audit row");
        let current = crate::addons::list()
            .expect("list addon after rejected rollback")
            .into_iter()
            .find(|record| record.manifest.id == addon_id)
            .expect("current addon remains installed");
        if let Some(value) = old_data_dir {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = fs::remove_dir_all(root);

        assert_eq!(status, StatusCode::NOT_FOUND, "{}", String::from_utf8_lossy(&body));
        assert_eq!(current.manifest.version, "0.1.0");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "addon_rollback");
        assert_eq!(events[0].target.as_deref(), Some(addon_id.as_str()));
        assert_eq!(events[0].outcome, "failure");
        assert_eq!(events[0].detail.as_deref(), Some("rollback rejected"));
    }

    #[tokio::test]
    async fn signed_client_package_http_update_is_install_only_and_inert() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let (fixture, private_key) = setup_fixture();
        let old_payload = b"window.ScreentraAddon = { version: '0.1.0' };\\n";
        let new_payload = b"window.ScreentraAddon = { version: '0.2.0' };\\n";
        let app = Router::new().route(
            "/api/addons/package",
            post(upload_addon_package).put(update_addon_package),
        );

        let install = package(&fixture.root, &private_key, "example.weather", "0.1.0", old_payload, old_payload);
        let (status, body) = send(&app, "POST", &install).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        crate::addons::set_enabled("example.weather", false).unwrap();

        let attempted_post_update = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        let (status, _) = send(&app, "POST", &attempted_post_update).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), old_payload);

        let update = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        let (status, body) = send(&app, "PUT", &update).await;
        assert_eq!(status, StatusCode::OK);
        let record: AddonRecord = serde_json::from_str(&body).unwrap();
        assert_eq!(record.manifest.version, "0.2.0");
        assert!(!record.enabled);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);
        assert!(!fixture.root.join("javascript-executed").exists());

        let tampered = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.3.0",
            b"signed payload",
            b"tampered payload",
        );
        let (status, _) = send(&app, "PUT", &tampered).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);

        let mismatched = package(
            &fixture.root,
            &private_key,
            "different.extension",
            "0.3.0",
            b"different identity",
            b"different identity",
        );
        let (status, _) = send(&app, "PUT", &mismatched).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);

        let same_version = package(
            &fixture.root,
            &private_key,
            "example.weather",
            "0.2.0",
            b"same version",
            b"same version",
        );
        let (status, _) = send(&app, "PUT", &same_version).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);
    }
}

#[cfg(test)]
mod audit_validation_tests {
    use super::*;

    fn layout_schedule() -> ScheduleEntry {
        ScheduleEntry {
            id: "s1".into(),
            display: "DP-1".into(),
            layout_id: "layout-1".into(),
            layout_set_id: None,
            day_of_week: crate::schedule::DAY_EVERY,
            start_time: "08:00".into(),
            end_time: "17:00".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            action: "switch_layout".into(),
            target_client: None,
            power_state: None,
            display_name: None,
            control_method: None,
            display_index: None,
            brightness: None,
            contrast: None,
            end_action: "none".into(),
        }
    }

    #[test]
    fn schedule_validation_rejects_malformed_time_and_ambiguous_layout() {
        let mut entry = layout_schedule();
        entry.start_time = "25:00".into();
        assert!(validate_schedule_entry(&entry).is_err());
        entry.start_time = "08:00".into();
        entry.layout_set_id = Some("set-1".into());
        assert!(validate_schedule_entry(&entry).is_err());
    }

    #[test]
    fn schedule_validation_accepts_client_targeted_power_schedule_without_legacy_display() {
        let mut entry = layout_schedule();
        entry.display.clear();
        entry.layout_id.clear();
        entry.target_client = Some("client-1".into());
        entry.action = "power_off".into();
        entry.power_state = Some("off".into());
        entry.display_index = Some(0);

        assert!(validate_schedule_entry(&entry).is_ok());
    }

    #[test]
    fn schedule_validation_accepts_client_targeted_display_sleep() {
        let mut entry = layout_schedule();
        entry.action = "display_sleep".into();
        entry.layout_id.clear();
        entry.target_client = Some("client-1".into());
        entry.power_state = Some("sleep".into());
        entry.control_method = Some("ddc".into());
        entry.display_index = Some(1);

        assert!(validate_schedule_entry(&entry).is_ok());
    }

    #[test]
    fn schedule_validation_accepts_brightness_only_display_settings() {
        let mut entry = layout_schedule();
        entry.action = "display_settings".into();
        entry.layout_id.clear();
        entry.target_client = Some("client-1".into());
        entry.control_method = Some("ddc".into());
        entry.display_index = Some(2);
        entry.brightness = Some(0);
        assert!(validate_schedule_entry(&entry).is_ok());
    }

    #[test]
    fn schedule_validation_rejects_invalid_display_settings_contract() {
        let mut entry = layout_schedule();
        entry.action = "display_settings".into();
        entry.layout_id.clear();
        entry.target_client = Some("client-1".into());
        entry.control_method = Some("auto".into());
        entry.brightness = Some(50);
        assert!(validate_schedule_entry(&entry).is_err());

        entry.control_method = Some("ddc".into());
        entry.brightness = None;
        entry.contrast = None;
        assert!(validate_schedule_entry(&entry).is_err());

        entry.brightness = Some(101);
        assert!(validate_schedule_entry(&entry).is_err());
    }
    #[test]
    fn schedule_validation_rejects_invalid_power_actions_and_states() {
        let mut entry = layout_schedule();
        entry.layout_id.clear();
        entry.target_client = Some("client-1".into());
        entry.action = "display_sleep".into();
        entry.power_state = Some("on".into());
        assert!(validate_schedule_entry(&entry).is_err());

        entry.action = "power_on".into();
        entry.power_state = Some("sleep".into());
        assert!(validate_schedule_entry(&entry).is_err());

        entry.action = "power_cycle".into();
        entry.power_state = Some("on".into());
        assert!(validate_schedule_entry(&entry).is_err());
    }

    #[test]
    fn schedule_validation_rejects_display_sleep_without_target_client() {
        let mut entry = layout_schedule();
        entry.action = "display_sleep".into();
        entry.layout_id.clear();
        entry.power_state = Some("sleep".into());

        assert!(validate_schedule_entry(&entry).is_err());
    }

    #[test]
    fn schedule_validation_rejects_invalid_display_sleep_controls() {
        let mut entry = layout_schedule();
        entry.action = "display_sleep".into();
        entry.layout_id.clear();
        entry.target_client = Some("client-1".into());
        entry.power_state = Some("sleep".into());

        entry.control_method = Some("invalid".into());
        assert!(validate_schedule_entry(&entry).is_err());

        entry.control_method = Some("ddc".into());
        entry.display_index = Some(64);
        assert!(validate_schedule_entry(&entry).is_err());
    }

    #[test]
    fn schedule_validation_accepts_overnight_power_schedule() {
        let mut entry = layout_schedule();
        entry.action = "power_off".into();
        entry.layout_id.clear();
        entry.target_client = Some("client-1".into());
        entry.power_state = Some("off".into());
        entry.start_time = "22:00".into();
        entry.end_time = "02:00".into();
        assert!(validate_schedule_entry(&entry).is_ok());
    }

    #[test]
    fn layout_set_validation_rejects_empty_ids_and_unbounded_values() {
        let set = LayoutSetWithItems {
            id: "".into(),
            name: "set".into(),
            shuffle: false,
            dwell_secs: 30,
            created_at: "now".into(),
            updated_at: "now".into(),
            items: Vec::new(),
        };
        assert_eq!(validate_layout_set_values(&set), Err(StatusCode::BAD_REQUEST));

        let set = LayoutSetWithItems {
            id: "set-1".into(),
            name: "set".into(),
            shuffle: false,
            dwell_secs: 30,
            created_at: "now".into(),
            updated_at: "now".into(),
            items: vec![LayoutSetEntry {
                id: "entry-1".into(),
                layout_set_id: "set-1".into(),
                layout_id: "layout-1".into(),
                position: 0,
                interval_secs: Some(0),
                transition: None,
            }],
        };
        assert_eq!(validate_layout_set_values(&set), Err(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn delete_addon_cleans_namespaced_storage_before_registry_removal() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!("screentra-addon-delete-{}", Uuid::new_v4()));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe { std::env::set_var("SIGNAGE_DATA_DIR", &data_dir); }
        let addon_id = format!("example.delete.{}", Uuid::new_v4().simple());
        let staged_dir = data_dir.join("trusted-services").join(&addon_id);
        let runtime_dir = data_dir.join("trusted-service-runtime").join(&addon_id);
        let manifest = AddonManifest {
            schema_version: 1,
            id: addon_id.clone(),
            name: "Delete test".to_string(),
            description: String::new(),
            version: "1.0.0".to_string(),
            kind: "declarative".to_string(),
            min_server_version: None,
            min_client_version: None,
            widget_types: Vec::new(),
            permissions: Vec::new(),
            client_capabilities: Vec::new(),
            data_contract: None,
            client_entrypoint: None,
            server_entrypoint: None,
            trusted_service: None,
            client_service: None,
            client_sha256: None,
            client_api_version: None,
        };
        crate::addons::install(manifest).expect("install test addon");
        std::fs::create_dir_all(&staged_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                data_dir.join("trusted-services"),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        std::fs::write(staged_dir.join("state.json"), b"staged").unwrap();
        let runtime_root = data_dir.join("trusted-service-runtime");
        std::fs::create_dir_all(&runtime_root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&runtime_root, std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        std::fs::create_dir_all(&runtime_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        std::fs::write(runtime_dir.join("runtime.json"), b"inert").unwrap();
        let db_path = data_dir.join("signage.db");
        let db = Arc::new(Database::open(db_path.to_str().unwrap()).expect("open storage database"));
        db.set_addon_storage(&addon_id, "settings", "{\"enabled\":true}", "2026-10-15T00:00:00Z")
            .expect("seed addon storage");
        let removed = delete_addon(State(AddonStorageRepository::Sqlite(db.clone())), Path(addon_id.clone()))
            .await
            .expect("addon removal should succeed");
        assert_eq!(removed, StatusCode::NO_CONTENT);
        assert_eq!(db.get_addon_storage(&addon_id, "settings").unwrap(), None);
        assert!(!staged_dir.exists());
        assert!(!runtime_dir.exists());
        assert!(!crate::addons::list().unwrap().iter().any(|record| record.manifest.id == addon_id));
        if let Some(value) = old_data_dir {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[derive(Clone)]
struct RateLimitLayerState {
    limiter: std::sync::Arc<crate::rate_limit::RateLimiter>,
    runtime_state: RuntimeState,
}

/// Tower layer that throttles public traffic and high-impact authenticated
/// mutations per client IP. High-impact denials are durably audited without
/// retaining request bodies or credentials.
///
/// Returns 429 with `Retry-After: <secs>` when the bucket is exhausted.
/// Buckets:
/// - /api/files/*: 60 requests / 60 seconds / IP
/// - client registration/polling traffic: 120 requests / 60 seconds / IP
/// - authenticated client-control mutations: 30 requests / 60 seconds / IP
/// - server update export/import: 3 requests / 300 seconds / IP
/// - /api/interactive/submit: 60 requests / 60 seconds / IP
async fn rate_limit_files_layer(
    State(layer_state): State<RateLimitLayerState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().as_str();
    let client_registration = path == "/api/clients/register" || path.ends_with("/enroll");
    let client_control = path.starts_with("/api/clients/")
        && method != "GET"
        && method != "OPTIONS"
        && !client_registration;
    let (bucket, policy) = if path.starts_with("/api/files/") {
        ("files", crate::rate_limit::POLICY_FILES)
    } else if path == "/api/version/update/export" || path == "/api/version/update/import" {
        ("server_update", crate::rate_limit::POLICY_SERVER_UPDATE)
    } else if client_control {
        ("client_control", crate::rate_limit::POLICY_CLIENT_CONTROL)
    } else if path.starts_with("/api/clients/") {
        ("clients", crate::rate_limit::POLICY_REGISTER)
    } else if path == "/api/interactive/submit" {
        ("interactive_submit", crate::rate_limit::POLICY_INTERACTIVE_SUBMIT)
    } else if path == "/api/traffic/events" || path == "/api/traffic/map.png" {
        ("traffic", crate::rate_limit::POLICY_TRAFFIC)
    } else {
        return next.run(req).await;
    };
    let ip = crate::auth::client_ip(req.headers());
    match layer_state
        .limiter
        .check(&format!("{}:{}", bucket, ip), policy)
    {
        crate::rate_limit::Decision::Allow => next.run(req).await,
        crate::rate_limit::Decision::Deny { retry_after_secs } => {
            if bucket == "client_control" || bucket == "server_update" {
                let principal = req
                    .extensions()
                    .get::<crate::middleware::AuthenticatedPrincipal>();
                let _ = layer_state
                    .runtime_state
                    .client_repository()
                    .record_audit_event(
                        principal.map(|p| p.username.as_str()),
                        principal.map(|p| p.role.as_str()),
                        "rate_limit_reject",
                        Some(bucket),
                        "failure",
                        Some("high-impact rate limit exceeded"),
                        &Utc::now().to_rfc3339(),
                    )
                    .await;
            }
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(
                    axum::http::header::RETRY_AFTER,
                    retry_after_secs.to_string(),
                )],
                format!("Too many {} requests; retry in {}s", bucket, retry_after_secs),
            )
                .into_response()
        }
    }
}

/// Tower layer that sets a Content-Security-Policy header on every response.
///
/// Restrictive: default-src 'self', no inline scripts, no eval. Allows
/// unsafe-inline for styles because the editor sets inline styles
/// for widget positioning. Tighten further with a nonce-based approach if
/// needed.
const CSP_HEADER: &str = "default-src 'self'; script-src 'self' https://www.youtube.com; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; img-src 'self' data: blob:; font-src 'self' data: https://fonts.gstatic.com; connect-src 'self' https://api.weather.gov https://nominatim.openstreetmap.org https://www.youtube.com; frame-src 'self' https:; frame-ancestors 'self'; object-src 'none'; base-uri 'self'; form-action 'self'";

async fn csp_layer(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut resp = next.run(req).await;
    let value = axum::http::HeaderValue::from_static(CSP_HEADER);
    resp.headers_mut()
        .insert(axum::http::header::CONTENT_SECURITY_POLICY, value);
    resp.headers_mut().insert(
        axum::http::header::X_FRAME_OPTIONS,
        axum::http::HeaderValue::from_static("SAMEORIGIN"),
    );
    resp
}

#[derive(Serialize, Deserialize)]
struct ClientDiagnosticsEntry {
    ts: String,
    action: String,
    status: String,
    detail: Option<String>,
    client_id: Option<String>,
    duration_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct ClientDiagnosticsReport {
    generated_at: String,
    browser: Option<serde_json::Value>,
    api: Option<serde_json::Value>,
    checks: Option<Vec<serde_json::Value>>,
    clients: Option<serde_json::Value>,
    totals: Option<serde_json::Value>,
    errors: Option<u64>,
    warnings: Option<u64>,
    event_count: Option<u64>,
    note: Option<String>,
}

#[derive(Deserialize)]
struct ClientDiagnosticsUpload {
    report: Option<ClientDiagnosticsReport>,
    events: Option<Vec<ClientDiagnosticsEntry>>,
    totals: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct ClientDiagnosticsSummary {
    filename: String,
    size_bytes: u64,
    generated_at: String,
    client_count: Option<u64>,
    error_count: Option<u64>,
    warning_count: Option<u64>,
    user: Option<String>,
}

fn client_diagnostics_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/ripley".to_string());
    let dir = std::path::PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("ripley-kiosk")
        .join("diagnostics");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn safe_diag_filename(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        }
    }
    if out.is_empty() {
        "diagnostics".to_string()
    } else {
        out
    }
}

async fn save_client_diagnostics(
    headers: axum::http::HeaderMap,
    AxumJson(body): AxumJson<ClientDiagnosticsUpload>,
) -> Result<AxumJson<ClientDiagnosticsSummary>, (StatusCode, String)> {
    let user = {
        use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
        let tok = crate::auth::extract_token(&headers);
        let username = tok
            .as_deref()
            .and_then(|t| t.rsplit_once(char::from(46)).map(|(body, _)| body))
            .and_then(|body| {
                BASE64
                    .decode(body.as_bytes())
                    .ok()
            })
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("sub").and_then(|s| s.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "authenticated".to_string());
        if username.is_empty() {
            "authenticated".to_string()
        } else {
            username
        }
    };
    let now = chrono::Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let user_part = safe_diag_filename(&user);
    let filename = format!("client-diag-{}-{}.json", user_part, stamp);
    let dir = client_diagnostics_dir();
    let dest = dir.join(&filename);
    let payload = serde_json::json!({
        "saved_at": now.to_rfc3339(),
        "saved_by": user,
        "report": body.report,
        "events": body.events,
        "totals": body.totals,
    });
    let body_bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|_| internal_server_error())?;
    std::fs::write(&dest, &body_bytes).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("write {}: {}", dest.display(), e),
        )
    })?;
    let size = body_bytes.len() as u64;
    // Best-effort DB linkage: log to a diagnostics table if present, otherwise just keep the file.
    let client_count = body
        .report
        .as_ref()
        .and_then(|r| r.clients.as_ref())
        .and_then(|c| c.get("known"))
        .and_then(|v| v.as_u64());
    let errors = body.report.as_ref().and_then(|r| r.errors).or_else(|| {
        body.totals
            .as_ref()
            .and_then(|t| t.get("errors"))
            .and_then(|v| v.as_u64())
    });
    let warnings = body.report.as_ref().and_then(|r| r.warnings).or_else(|| {
        body.totals
            .as_ref()
            .and_then(|t| t.get("warnings"))
            .and_then(|v| v.as_u64())
    });
    log::info!(
        "client diagnostics saved filename={} bytes={} user={} clients={:?} errors={:?} warnings={:?}",
        filename, size, user, client_count, errors, warnings
    );
    Ok(AxumJson(ClientDiagnosticsSummary {
        filename: filename,
        size_bytes: size,
        generated_at: now.to_rfc3339(),
        client_count,
        error_count: errors,
        warning_count: warnings,
        user: Some(user),
    }))
}

async fn list_client_diagnostics(
) -> Result<AxumJson<Vec<ClientDiagnosticsSummary>>, (StatusCode, String)> {
    const MAX_DIAGNOSTIC_ENTRIES: usize = 4096;
    let dir = client_diagnostics_dir();
    let mut entries: Vec<ClientDiagnosticsSummary> = Vec::new();
    let mut scanned_entries = 0usize;
    let read_dir = std::fs::read_dir(&dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("read {}: {}", dir.display(), e),
        )
    })?;
    for e in read_dir.flatten() {
        scanned_entries = scanned_entries.saturating_add(1);
        if scanned_entries > MAX_DIAGNOSTIC_ENTRIES {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "Too many client diagnostic entries".to_string(),
            ));
        }
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if !name.starts_with("client-diag-") || !name.ends_with(".json") {
            continue;
        }
        let meta = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Parse file for counts/best-effort metadata.
        let mut client_count = None;
        let mut error_count = None;
        let mut warning_count = None;
        let mut user = None;
        let mut generated_at = String::new();
        if let Ok(bytes) = read_bounded_file(&path, 16 * 1024 * 1024) {
            if let Ok(text) = String::from_utf8(bytes) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(s) = v.get("saved_at").and_then(|x| x.as_str()) {
                    generated_at = s.to_string();
                }
                if let Some(u) = v.get("saved_by").and_then(|x| x.as_str()) {
                    user = Some(u.to_string());
                }
                if let Some(r) = v.get("report").and_then(|x| x.as_object()) {
                    if let Some(c) = r.get("clients").and_then(|x| x.as_object()) {
                        if let Some(n) = c.get("known").and_then(|x| x.as_u64()) {
                            client_count = Some(n);
                        }
                    }
                    if let Some(n) = r.get("errors").and_then(|x| x.as_u64()) {
                        error_count = Some(n);
                    }
                    if let Some(n) = r.get("warnings").and_then(|x| x.as_u64()) {
                        warning_count = Some(n);
                    }
                }
                }
            }
        }
        if generated_at.is_empty() {
            if let Ok(mtime) = meta.modified() {
                let dt: chrono::DateTime<chrono::Utc> = mtime.into();
                generated_at = dt.to_rfc3339();
            }
        }
        entries.push(ClientDiagnosticsSummary {
            filename: name,
            size_bytes: meta.len(),
            generated_at,
            client_count,
            error_count,
            warning_count,
            user,
        });
    }
    entries.sort_by(|a, b| b.generated_at.cmp(&a.generated_at));
    Ok(AxumJson(entries))
}

async fn download_client_diagnostics(
    Path(filename): Path<String>,
) -> Result<
    (
        [(axum::http::HeaderName, axum::http::HeaderValue); 2],
        Vec<u8>,
    ),
    (StatusCode, String),
> {
    let safe = safe_diag_filename(&filename);
    if safe != filename || !safe.starts_with("client-diag-") || !safe.ends_with(".json") {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid diagnostics filename".to_string(),
        ));
    }
    let path = client_diagnostics_dir().join(&safe);
    if !path.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            "Diagnostics file not found".to_string(),
        ));
    }
    let bytes = read_bounded_file(&path, 16 * 1024 * 1024).map_err(|e| {
        (
            if e == "file exceeds its bound" {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            },
            format!("read {}: {}", path.display(), e),
        )
    })?;
    use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    let cd = format!("attachment; filename=\"{}\"", safe);
    let headers = [
        (
            CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        ),
        (
            CONTENT_DISPOSITION,
            axum::http::HeaderValue::from_str(&cd)
                .map_err(|_| internal_server_error())?,
        ),
    ];
    Ok((headers, bytes))
}

// ── Interactive Submissions handlers ────────────────────────────

#[derive(serde::Deserialize)]
struct InteractiveSubmissionPayload {
    #[serde(default)]
    client_id: Option<String>,
    client_address: Option<String>,
    interactive_layout_id: Option<String>,
    interactive_layout_name: Option<String>,
    widget_id: String,
    widget_type: String,
    widget_label: Option<String>,
    payload: serde_json::Value,
    collected_at: Option<String>,
}

/// Allowed widget types for interactive submissions. Unknown types are rejected with 400.
const ALLOWED_WIDGET_TYPES: &[&str] = &[
    "button",
    "textbox",
    "dropdown",
    "checkbox",
    "checkbox-list",
    "radio",
];

/// Per-field length caps (bytes).
const MAX_WIDGET_ID_LEN: usize = 64;
const MAX_WIDGET_LABEL_LEN: usize = 128;
const MAX_PAGE_ID_LEN: usize = 64;
const MAX_PAGE_NAME_LEN: usize = 128;
const MAX_CLIENT_ID_LEN: usize = 64;

/// Per-payload size caps (bytes). See `interactive-submission-contract` skill.
const MAX_PAYLOAD_BYTES_BUTTON: usize = 4 * 1024;
const MAX_PAYLOAD_BYTES_CHECKBOX_LIST: usize = 8 * 1024;
const MAX_PAYLOAD_BYTES_SMALL: usize = 256;
const MAX_CHECKBOX_LIST_ITEMS: usize = 64;
const MAX_CHECKBOX_LIST_ITEM_LEN: usize = 128;

/// Body cap for /api/interactive/submit. The router-wide DefaultBodyLimit remains 1 GiB
/// for /api/version/update/import; we narrow it locally here.
const MAX_INTERACTIVE_BODY_BYTES: u64 = 8 * 1024;

/// Reject control chars (newlines, tabs, escape sequences) and cap length.
fn normalize_field(raw: &str, max: usize) -> Result<String, String> {
    if raw.is_empty() {
        return Ok(String::new());
    }
    if raw.len() > max {
        return Err(format!("field exceeds {} bytes", max));
    }
    if raw.chars().any(|c| c.is_control()) {
        return Err("field contains control characters".to_string());
    }
    Ok(raw.to_string())
}

/// Validate the payload shape for the given widget_type. Returns a canonical
/// JSON-serialized value to store in `payload_json`.
fn validate_payload(widget_type: &str, payload: &serde_json::Value) -> Result<String, String> {
    use serde_json::json;
    let canonical = match widget_type {
        "button" => {
            let obj = payload.as_object().ok_or_else(|| {
                format!(
                    "payload shape mismatch: expected object with 'values'/'acknowledged', got {}",
                    payload_type(payload)
                )
            })?;
            let values = obj.get("values").cloned().unwrap_or_else(|| json!({}));
            if !values.is_object() {
                return Err("payload.values must be an object".to_string());
            }
            let values_str = serde_json::to_string(&values)
                .map_err(|e| format!("payload.values not serializable: {e}"))?;
            if values_str.len() > MAX_PAYLOAD_BYTES_BUTTON {
                return Err(format!(
                    "payload.values exceeds {} bytes",
                    MAX_PAYLOAD_BYTES_BUTTON
                ));
            }
            let acknowledged = obj
                .get("acknowledged")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            json!({ "values": values, "acknowledged": acknowledged })
        }
        "textbox" | "dropdown" | "radio" => {
            let s = payload.as_str().ok_or_else(|| {
                format!(
                    "payload shape mismatch: expected string for {}, got {}",
                    widget_type,
                    payload_type(payload)
                )
            })?;
            if s.len() > MAX_PAYLOAD_BYTES_SMALL {
                return Err(format!(
                    "payload string exceeds {} bytes",
                    MAX_PAYLOAD_BYTES_SMALL
                ));
            }
            json!(s)
        }
        "checkbox" => {
            let b = payload.as_bool().ok_or_else(|| {
                "payload shape mismatch: expected boolean for checkbox".to_string()
            })?;
            json!(b)
        }
        "checkbox-list" => {
            let arr = payload.as_array().ok_or_else(|| {
                "payload shape mismatch: expected array of strings for checkbox-list".to_string()
            })?;
            if arr.len() > MAX_CHECKBOX_LIST_ITEMS {
                return Err(format!(
                    "checkbox-list exceeds {} items",
                    MAX_CHECKBOX_LIST_ITEMS
                ));
            }
            let mut canonical_items: Vec<&str> = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| "checkbox-list items must be strings".to_string())?;
                if s.len() > MAX_CHECKBOX_LIST_ITEM_LEN {
                    return Err(format!(
                        "checkbox-list item exceeds {} bytes",
                        MAX_CHECKBOX_LIST_ITEM_LEN
                    ));
                }
                canonical_items.push(s);
            }
            let total = canonical_items.iter().map(|s| s.len()).sum::<usize>();
            if total > MAX_PAYLOAD_BYTES_CHECKBOX_LIST {
                return Err(format!(
                    "checkbox-list payload exceeds {} bytes",
                    MAX_PAYLOAD_BYTES_CHECKBOX_LIST
                ));
            }
            json!(canonical_items)
        }
        other => {
            return Err(format!("unknown widget_type for validation: {other}"));
        }
    };
    serde_json::to_string(&canonical)
        .map_err(|e| format!("canonical payload serialization failed: {e}"))
}

fn payload_type(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Header-wins-or-body client_id resolver. Strips control chars and caps length.
/// Empty strings on either side are treated as missing.
fn resolve_client_id(header_val: Option<&str>, body_val: Option<&str>) -> String {
    let raw = match header_val {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => match body_val {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return "unknown".to_string(),
        },
    };
    if raw.is_empty() {
        return "unknown".to_string();
    }
    if raw.len() > MAX_CLIENT_ID_LEN {
        return raw.chars().take(MAX_CLIENT_ID_LEN).collect();
    }
    if raw.chars().any(|c| c.is_control()) {
        return raw
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_CLIENT_ID_LEN)
            .collect();
    }
    raw
}

/// Validate `collected_at` is a valid RFC 3339 string, or return the fallback.
fn validate_collected_at(raw: Option<&str>) -> Result<String, String> {
    match raw {
        Some(s) if !s.is_empty() => {
            // Try to parse; on failure, reject.
            if chrono::DateTime::parse_from_rfc3339(s).is_err() {
                return Err(format!(
                    "collected_at must be RFC 3339 (e.g. 2026-08-01T20:14:33-04:00), got: {s}"
                ));
            }
            Ok(s.to_string())
        }
        _ => Ok(chrono::Utc::now().to_rfc3339()),
    }
}

async fn post_interactive_submission_handler(
    State(repository): State<ClientRepository>,
    headers: axum::http::HeaderMap,
    AxumJson(payload): AxumJson<InteractiveSubmissionPayload>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    // Body size cap. Axum may have already truncated Content-Length, but the
    // router-wide DefaultBodyLimit is 1 GiB (needed for version-update imports),
    // so we enforce a tighter cap locally.
    if let Some(cl) = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    {
        if cl > MAX_INTERACTIVE_BODY_BYTES {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "interactive submission body too large ({} bytes, max {})",
                    cl, MAX_INTERACTIVE_BODY_BYTES
                ),
            ));
        }
    }

    let widget_type = payload.widget_type.trim();
    if widget_type.is_empty() || payload.widget_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "widget_id and widget_type are required".to_string(),
        ));
    }
    if !ALLOWED_WIDGET_TYPES.contains(&widget_type) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "widget_type must be one of {:?}, got {:?}",
                ALLOWED_WIDGET_TYPES, widget_type
            ),
        ));
    }

    let widget_id = normalize_field(payload.widget_id.trim(), MAX_WIDGET_ID_LEN)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let widget_label = payload
        .widget_label
        .as_deref()
        .map(|s| normalize_field(s, MAX_WIDGET_LABEL_LEN).map_err(|e| (StatusCode::BAD_REQUEST, e)))
        .transpose()?;
    let interactive_layout_id = payload
        .interactive_layout_id
        .as_deref()
        .map(|s| normalize_field(s, MAX_PAGE_ID_LEN).map_err(|e| (StatusCode::BAD_REQUEST, e)))
        .transpose()?;
    let interactive_layout_name = payload
        .interactive_layout_name
        .as_deref()
        .map(|s| normalize_field(s, MAX_PAGE_NAME_LEN).map_err(|e| (StatusCode::BAD_REQUEST, e)))
        .transpose()?;

    // Header-wins client_id (anti-spoofing when player sends both).
    let client_id = resolve_client_id(
        headers
            .get("x-ripley-client-id")
            .and_then(|v| v.to_str().ok()),
        payload.client_id.as_deref(),
    );
    let client_address = headers
        .get("x-ripley-client-address")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or(payload.client_address.clone());
    let collected_at = validate_collected_at(payload.collected_at.as_deref())
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Re-serialize the payload through validation; we never trust the client's
    // raw JSON (whitespace normalization + shape enforcement).
    let payload_json = validate_payload(widget_type, &payload.payload)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let id = repository
        .insert_interactive_submission(
            &client_id,
            client_address.as_deref(),
            interactive_layout_id.as_deref(),
            interactive_layout_name.as_deref(),
            &widget_id,
            widget_type,
            widget_label.as_deref(),
            &payload_json,
            &collected_at,
        )
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({
        "id": id,
        "status": "received",
        "google_sync_status": "pending",
    })))
}


#[derive(serde::Deserialize, Default)]
struct InteractiveSubmissionsQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    page_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn get_interactive_submissions_handler(
    State(repository): State<ClientRepository>,
    Query(q): Query<InteractiveSubmissionsQuery>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let submissions = repository
        .list_interactive_submissions(
            limit,
            q.client_id.as_deref(),
            q.page_id.as_deref(),
            q.status.as_deref(),
        )
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({
        "submissions": submissions,
        "count": submissions.len(),
    })))
}

async fn get_interactive_submissions_stats_handler(
    State(repository): State<ClientRepository>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let total = repository
        .count_interactive_submissions(None)
        .await
        .map_err(|_| internal_server_error())?;
    let pending = repository
        .count_interactive_submissions(Some("pending"))
        .await
        .map_err(|_| internal_server_error())?;
    let success = repository
        .count_interactive_submissions(Some("success"))
        .await
        .map_err(|_| internal_server_error())?;
    let failed = repository
        .count_interactive_submissions(Some("failed"))
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({
        "total": total,
        "pending": pending,
        "success": success,
        "failed": failed,
    })))
}

async fn export_interactive_submissions_csv_handler(
    State(repository): State<ClientRepository>,
    Query(q): Query<InteractiveSubmissionsQuery>,
) -> Result<
    (
        [(axum::http::header::HeaderName, axum::http::HeaderValue); 2],
        Vec<u8>,
    ),
    (StatusCode, String),
> {
    let limit = q.limit.unwrap_or(10000).clamp(1, 50000);
    let submissions = repository
        .list_interactive_submissions(
            limit,
            q.client_id.as_deref(),
            q.page_id.as_deref(),
            q.status.as_deref(),
        )
        .await
        .map_err(|_| internal_server_error())?;

    // Build CSV with a header row
    let mut csv = String::new();
    csv.push_str("id,collected_at,received_at,client_id,client_address,interactive_layout_id,interactive_layout_name,widget_id,widget_type,widget_label,payload,google_sync_status,google_sync_message,google_synced_at,google_sheet_id\n");
    for s in &submissions {
        let cells = [
            s["id"].to_string(),
            s["collected_at"].as_str().unwrap_or("").to_string(),
            s["received_at"].as_str().unwrap_or("").to_string(),
            csv_escape(s["client_id"].as_str().unwrap_or("")),
            csv_escape(s["client_address"].as_str().unwrap_or("")),
            csv_escape(s["interactive_layout_id"].as_str().unwrap_or("")),
            csv_escape(s["interactive_layout_name"].as_str().unwrap_or("")),
            csv_escape(s["widget_id"].as_str().unwrap_or("")),
            csv_escape(s["widget_type"].as_str().unwrap_or("")),
            csv_escape(s["widget_label"].as_str().unwrap_or("")),
            csv_escape(s["payload_json"].as_str().unwrap_or("")),
            csv_escape(s["google_sync_status"].as_str().unwrap_or("")),
            csv_escape(s["google_sync_message"].as_str().unwrap_or("")),
            csv_escape(s["google_synced_at"].as_str().unwrap_or("")),
            csv_escape(s["google_sheet_id"].as_str().unwrap_or("")),
        ];
        csv.push_str(&cells.join(","));
        csv.push_str("\n");
    }

    use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    let cd = format!(
        "attachment; filename=\"interactive-submissions-{}.csv\"",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let headers = [
        (
            CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/csv; charset=utf-8"),
        ),
        (
            CONTENT_DISPOSITION,
            axum::http::HeaderValue::from_str(&cd)
                .map_err(|_| internal_server_error())?,
        ),
    ];
    Ok((headers, csv.into_bytes()))
}

fn csv_escape(s: &str) -> String {
    let needs_quotes = s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    let escaped = s.replace('"', "\"\"");
    if needs_quotes {
        format!("\"{}\"", escaped)
    } else {
        escaped
    }
}

#[derive(serde::Deserialize)]
struct DeleteInteractiveSubmissionsPayload {
    ids: Vec<i64>,
}

const MAX_MUTATION_IDS: usize = 10_000;

fn validate_mutation_id_count<T>(ids: &[T], error_message: &str) -> Result<(), (StatusCode, String)> {
    if ids.len() > MAX_MUTATION_IDS {
        return Err((StatusCode::BAD_REQUEST, error_message.to_string()));
    }
    Ok(())
}

async fn delete_interactive_submissions_handler(
    State(repository): State<ClientRepository>,
    AxumJson(payload): AxumJson<DeleteInteractiveSubmissionsPayload>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    validate_mutation_id_count(&payload.ids, "too many interactive submission ids")?;
    let n = repository
        .delete_interactive_submissions(&payload.ids)
        .await
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(serde_json::json!({"deleted": n})))
}

#[derive(serde::Deserialize)]
struct SyncToGooglePayload {
    #[serde(default)]
    ids: Option<Vec<i64>>,
    #[serde(default)]
    layout_id: Option<String>,
    #[serde(default = "default_sync_action")]
    action: String, // "sync_all_pending" | "sync_pending_for_layout" | "sync_specific"
    #[serde(default)]
    spreadsheet_id: Option<String>,
}

fn default_sync_action() -> String {
    "sync_all_pending".to_string()
}

async fn sync_interactive_submissions_to_google_handler(
    State(repository): State<ClientRepository>,
    AxumJson(payload): AxumJson<SyncToGooglePayload>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    // Resolve which submission IDs to sync.
    if let Some(specific) = payload.ids.as_deref() {
        validate_mutation_id_count(specific, "too many interactive submission ids")?;
    }
    let ids: Vec<i64> = if let Some(specific) = payload.ids.clone() {
        specific
    } else {
        match payload.action.as_str() {
            "sync_pending_for_layout" => {
                if let Some(layout_id) = payload.layout_id.as_deref() {
                    let rows = repository
                        .list_interactive_submissions(5000, None, Some(layout_id), Some("pending"))
                        .await
                        .map_err(|_| internal_server_error())?;
                    rows.iter().filter_map(|v| v["id"].as_i64()).collect()
                } else {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "layout_id required for sync_pending_for_layout".to_string(),
                    ));
                }
            }
            _ => {
                // sync_all_pending
                let rows = repository
                    .list_interactive_submissions(5000, None, None, Some("pending"))
                    .await
                    .map_err(|_| internal_server_error())?;
                rows.iter().filter_map(|v| v["id"].as_i64()).collect()
            }
        }
    };

    if ids.is_empty() {
        return Ok(AxumJson(serde_json::json!({
            "synced": 0,
            "message": "No pending submissions to sync.",
        })));
    }

    // If Google is not configured, mark them as "skipped" and return a friendly response.
    let google_configured = std::env::var("SIGNAGE_GOOGLE_SHEETS_SA_KEY").is_ok()
        || std::env::var("SIGNAGE_GOOGLE_SHEETS_DEFAULT_ID").is_ok();
    let spreadsheet_id = payload
        .spreadsheet_id
        .clone()
        .or_else(|| std::env::var("SIGNAGE_GOOGLE_SHEETS_DEFAULT_ID").ok());

    if !google_configured {
        let n = repository
            .update_interactive_submission_sync_status(
                &ids,
                "skipped",
                Some(
                    "Google Workspace not configured on server (set SIGNAGE_GOOGLE_SHEETS_SA_KEY)",
                ),
                None,
            )
            .await
            .map_err(|_| internal_server_error())?;
        return Ok(AxumJson(serde_json::json!({
            "synced": n,
            "google_configured": false,
            "message": "Google Workspace is not configured on the server. Submissions were marked as 'skipped' and remain available locally. Export to CSV or configure Google in System settings.",
        })));
    }

    // Best-effort: push rows to Google Sheets via the configured service account.
    let push_result = push_submissions_to_google_sheets(&ids, &repository, spreadsheet_id.as_deref()).await;

    match push_result {
        Ok(sheet_id) => {
            let n = repository
                .update_interactive_submission_sync_status(
                    &ids,
                    "success",
                    Some("Pushed to Google Sheets"),
                    sheet_id.as_deref(),
                )
                .await
                .map_err(|_| internal_server_error())?;
            Ok(AxumJson(serde_json::json!({
                "synced": n,
                "google_configured": true,
                "spreadsheet_id": sheet_id,
                "message": format!("Pushed {} submission(s) to Google Sheets.", n),
            })))
        }
        Err((status, msg)) => {
            let n = repository
                .update_interactive_submission_sync_status(
                    &ids,
                    "failed",
                    Some(&msg),
                    spreadsheet_id.as_deref(),
                )
                .await
                .map_err(|_| internal_server_error())?;
            Ok(AxumJson(serde_json::json!({
                "synced": n,
                "google_configured": true,
                "spreadsheet_id": spreadsheet_id,
                "status_code": status.as_u16(),
                "message": msg,
            })))
        }
    }
}

// ── Table Management ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CreateTablePayload {
    name: String,
    columns: Vec<ColumnDef>,
}

#[derive(serde::Deserialize)]
struct ColumnDef {
    name: String,
    col_type: String,
}

#[derive(serde::Deserialize)]
struct DeleteRowsPayload {
    ids: Vec<i64>,
}

#[derive(serde::Serialize)]
struct TableInfo {
    name: String,
}

#[derive(serde::Serialize)]
struct ColumnMeta {
    name: String,
    col_type: String,
    notnull: bool,
    dflt_value: Option<String>,
    pk: bool,
}

#[derive(serde::Serialize)]
struct TableRows {
    columns: Vec<ColumnMeta>,
    rows: Vec<Vec<serde_json::Value>>,
    total: i64,
    limit: i64,
    offset: i64,
}

async fn list_tables_handler(
    State(state): State<RuntimeState>,
) -> Result<AxumJson<Vec<TableInfo>>, (StatusCode, String)> {
    let db = state.legacy_db.clone().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Table inspection is only supported for SQLite installations".to_string(),
    ))?;
    let names = db
        .list_table_names()
        .map_err(|_| internal_server_error())?;
    Ok(AxumJson(
        names.into_iter().map(|name| TableInfo { name }).collect(),
    ))
}


struct TableQueryParams {
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn query_table_handler(
    State(state): State<RuntimeState>,
    axum::extract::Path(table): axum::extract::Path<String>,
    Query(params): Query<TableQueryParams>,
) -> Result<AxumJson<TableRows>, (StatusCode, String)> {
    let db = state.legacy_db.clone().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Table inspection is only supported for SQLite installations".to_string(),
    ))?;
    let limit = params.limit.unwrap_or(100).min(500);
    let offset = params.offset.unwrap_or(0);

    let safe_table = table.replace('-', "_");
    if !safe_table.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let result = db
        .query_table_rows(&safe_table, limit, offset)
        .map_err(|_| internal_server_error())?;

    let columns = result
        .columns
        .into_iter()
        .map(|column| ColumnMeta {
            name: column.name,
            col_type: column.col_type,
            notnull: column.notnull,
            dflt_value: column.dflt_value,
            pk: column.pk,
        })
        .collect();

    Ok(AxumJson(TableRows {
        columns,
        rows: result.rows,
        total: result.total,
        limit: limit as i64,
        offset: offset as i64,
    }))
}

async fn delete_table_rows_handler(
    State(state): State<RuntimeState>,
    axum::extract::Path(table): axum::extract::Path<String>,
    AxumJson(payload): AxumJson<DeleteRowsPayload>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let db = state.legacy_db.clone().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Table inspection is only supported for SQLite installations".to_string(),
    ))?;
    let safe_table = table.replace('-', "_");
    if !safe_table.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let protected = ["users", "backups", "sessions", "client_heartbeats", "client_commands"];
    if protected.contains(&safe_table.as_str()) {
        return Err((StatusCode::FORBIDDEN, "Cannot delete from protected table".into()));
    }

    if payload.ids.is_empty() {
        return Ok(AxumJson(serde_json::json!({"deleted": 0})));
    }
    validate_mutation_id_count(&payload.ids, "too many table row ids")?;

    let deleted = db
        .delete_table_rows(&safe_table, &payload.ids)
        .map_err(|_| internal_server_error())?;

    Ok(AxumJson(serde_json::json!({"deleted": deleted})))
}

async fn export_table_csv_handler(
    State(state): State<RuntimeState>,
    axum::extract::Path(table): axum::extract::Path<String>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, String)> {
    let db = state.legacy_db.clone().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Table inspection is unavailable on PostgreSQL-backed servers".to_string(),
    ))?;
    let safe_table = table.replace('-', "_");
    if !safe_table.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err((StatusCode::BAD_REQUEST, "Invalid table name".into()));
    }

    let export = db
        .export_table_rows(&safe_table)
        .map_err(|_| internal_server_error())?;
    let mut csv = String::new();
    csv.push_str(
        &export
            .columns
            .iter()
            .map(|column| csv_escape(column))
            .collect::<Vec<_>>()
            .join(","),
    );
    csv.push('\n');
    for row in export.rows {
        let values = row
            .into_iter()
            .map(|value| {
                let text = match value {
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::String(value) => value,
                    value => value.to_string(),
                };
                csv_escape(&text)
            })
            .collect::<Vec<_>>();
        csv.push_str(&values.join(","));
        csv.push('\n');
    }
    let csv_data = csv;

    let filename = format!("{}.csv", safe_table);
    Ok(axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/csv; charset=utf-8")
        .header("Content-Disposition", format!("attachment; filename=\"{}\");", filename))
        .body(axum::body::Body::from(csv_data))
        .unwrap())
}

async fn push_submissions_to_google_sheets(
    ids: &[i64],
    repository: &ClientRepository,
    spreadsheet_id: Option<&str>,
) -> Result<Option<String>, (StatusCode, String)> {
    // Fetch the submissions from the repository.
    let all = repository
        .list_interactive_submissions(5000, None, None, None)
        .await
        .map_err(|_| internal_server_error())?;
    let rows: Vec<serde_json::Value> = all
        .into_iter()
        .filter(|v| {
            v["id"]
                .as_i64()
                .map(|id| ids.contains(&id))
                .unwrap_or(false)
        })
        .collect();

    if rows.is_empty() {
        return Ok(spreadsheet_id.map(|s| s.to_string()));
    }

    // Build a CSV blob in-memory and hand it to the configured python helper
    // `tools/google-sheets-push.py`. The helper expects:
    //   argv[1]: spreadsheet_id
    //   stdin: CSV data
    // and prints "OK <sheet_id>" on success, otherwise a message starting with "ERR".
    let mut csv = String::new();
    csv.push_str("id,collected_at,client_id,interactive_layout_name,widget_type,widget_label,payload,google_sync_status\n");
    for r in &rows {
        let line = [
            r["id"].to_string(),
            csv_escape(r["collected_at"].as_str().unwrap_or("")),
            csv_escape(r["client_id"].as_str().unwrap_or("")),
            csv_escape(r["interactive_layout_name"].as_str().unwrap_or("")),
            csv_escape(r["widget_type"].as_str().unwrap_or("")),
            csv_escape(r["widget_label"].as_str().unwrap_or("")),
            csv_escape(r["payload_json"].as_str().unwrap_or("")),
            csv_escape(r["google_sync_status"].as_str().unwrap_or("")),
        ]
        .join(",");
        csv.push_str(&line);
        csv.push_str("\n");
    }

    let script = std::path::PathBuf::from(crate::version::install_dir_for_static())
        .join("tools/google-sheets-push.py");
    if !script.exists() {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "Google Sheets push helper (tools/google-sheets-push.py) is missing".to_string(),
        ));
    }
    let sheet_arg = match spreadsheet_id {
        Some(s) => s.to_string(),
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                "spreadsheet_id is required (set SIGNAGE_GOOGLE_SHEETS_DEFAULT_ID or pass spreadsheet_id)".to_string(),
            ));
        }
    };

    let mut command = tokio::process::Command::new("python3");
    command
        .arg(&script)
        .arg(&sheet_arg)
        .current_dir(crate::version::install_dir_for_static());
    let output = bounded_async_command_output(
        command,
        csv.into_bytes(),
        64 * 1024,
        Duration::from_secs(30),
    )
    .await
    .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Google Sheets push failed: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() && stdout.starts_with("OK") {
        let sheet_id = stdout
            .split_whitespace()
            .nth(1)
            .map(|s| s.to_string())
            .or(Some(sheet_arg));
        Ok(sheet_id)
    } else {
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        Err((
            StatusCode::BAD_GATEWAY,
            format!("Google Sheets push failed: {msg}"),
        ))
    }
}

// ─── Virtual client handlers ────────────────────────────────────────────────

async fn create_virtual_client(
    State(repository): State<ClientRepository>,
    Json(req): Json<CreateVirtualClientRequest>,
) -> Result<AxumJson<VirtualClientResponse>, (StatusCode, String)> {
    let id = Uuid::new_v4().to_string();
    let name = req
        .name
        .unwrap_or_else(|| "Virtual Client".to_string());
    repository
        .create_virtual_client(&id, &name)
        .await
        .map_err(|_| internal_server_error())?;
    let url = format!("/player.html?client_id={}", id);
    Ok(AxumJson(VirtualClientResponse { id, name, url }))
}

async fn get_virtual_client(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
) -> Result<AxumJson<VirtualClientResponse>, (StatusCode, String)> {
    let (name, is_virtual) = repository
        .get_virtual_client(&id)
        .await
        .map_err(|_| internal_server_error())?
        .ok_or((StatusCode::NOT_FOUND, "Virtual client not found".to_string()))?;
    if !is_virtual {
        return Err((StatusCode::NOT_FOUND, "Not a virtual client".to_string()));
    }
    let url = format!("/player.html?client_id={}", id);
    Ok(AxumJson(VirtualClientResponse {
        id,
        name,
        url,
    }))
}

async fn update_virtual_client(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(req): Json<CreateVirtualClientRequest>,
) -> Result<AxumJson<VirtualClientResponse>, (StatusCode, String)> {
    let is_virtual = repository
        .is_virtual_client(&id)
        .await
        .map_err(|_| internal_server_error())?;
    if !is_virtual {
        return Err((StatusCode::NOT_FOUND, "Not a virtual client".to_string()));
    }
    let name = req
        .name
        .unwrap_or_else(|| "Virtual Client".to_string());
    repository
        .update_virtual_client_name(&id, &name)
        .await
        .map_err(|_| internal_server_error())?;
    let url = format!("/player.html?client_id={}", id);
    Ok(AxumJson(VirtualClientResponse {
        id,
        name,
        url,
    }))
}

async fn set_virtual_client_layout(
    State(repository): State<ClientRepository>,
    Path(id): Path<String>,
    Json(req): Json<SetVirtualClientLayoutRequest>,
) -> Result<AxumJson<serde_json::Value>, (StatusCode, String)> {
    let is_virtual = repository
        .is_virtual_client(&id)
        .await
        .map_err(|_| internal_server_error())?;
    if !is_virtual {
        return Err((StatusCode::NOT_FOUND, "Not a virtual client".to_string()));
    }
    let layout_id = req.layout_id.clone().unwrap_or_default();
    let updated = repository
        .set_virtual_client_layout(&id, &layout_id)
        .await
        .map_err(|_| internal_server_error())?;
    if updated == 0 {
        return Err((StatusCode::NOT_FOUND, "Virtual client not found".to_string()));
    }
    Ok(AxumJson(serde_json::json!({
        "client_id": id,
        "assigned_layout_id": req.layout_id,
    })))
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace("\"", "&quot;")
        .replace("\'", "&#39;")
}


