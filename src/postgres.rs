//! PostgreSQL connection and migration bootstrap for the server runtime.
//!
//! The normal server path still opens SQLite until the repository backend is
//! replaced. The explicit `--postgres-schema` probe gives deployment tooling a
//! safe way to validate configuration and initialize the PostgreSQL schema
//! without changing the current default runtime.

use crate::db::{TableColumnRecord, TableExportResult, TableQueryResult, UserRecord};
use crate::schedule::ScheduleEntry;
use crate::models::{AddLayoutSetEntry, Backup, DisplayAssignment, DisplayState, Layout, LayoutFolder, LayoutRevision, LayoutSetEntry, LayoutSetSummary, LayoutSetWithItems, NewsFeed, ScheduleRun, AuditEvent, TemplateMeta, UpdateLayoutSetEntry, Widget, WidgetPreset, WidgetPresetAsset, WidgetPresetMeta, WidgetType};
use crate::db::{ClientAdminUser, ClientDisplayRecord, ClientDisplayRegistration, ClientRegistration};
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, NoTls};
use uuid::Uuid;

const DATABASE_URL_ENV: &str = "SIGNAGE_DATABASE_URL";
const INITIAL_SCHEMA: &str = include_str!("../migrations/postgres/0001_initial.sql");
const RUNTIME_COLUMNS_MIGRATION: &str = include_str!("../migrations/postgres/0002_runtime_columns.sql");
const ADDON_STORAGE_MIGRATION: &str = include_str!("../migrations/postgres/0003_addon_storage.sql");
const DISPLAY_SETTINGS_MIGRATION: &str = include_str!("../migrations/postgres/0004_schedule_display_settings.sql");

pub fn configured_database_url() -> Result<String, String> {
    database_url_from(std::env::var(DATABASE_URL_ENV).ok())
}

fn database_url_from(value: Option<String>) -> Result<String, String> {
    let value = value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{DATABASE_URL_ENV} is not configured"))?;
    Ok(value)
}

async fn connect() -> Result<Client, String> {
    let url = configured_database_url()?;
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .map_err(|error| format!("PostgreSQL connection failed: {error}"))?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            log::error!("PostgreSQL connection task stopped: {error}");
        }
    });
    Ok(client)
}

pub struct PostgresStore {
    client: Client,
}

impl PostgresStore {
    pub async fn connect() -> Result<Self, String> {
        let store = Self { client: connect().await? };
        store
            .client
            .batch_execute(
                "ALTER TABLE client_command_cooldowns
                 ADD COLUMN IF NOT EXISTS burst_started_at text;
                 ALTER TABLE client_command_cooldowns
                 ADD COLUMN IF NOT EXISTS burst_count integer NOT NULL DEFAULT 0;
                 ALTER TABLE client_command_cooldowns
                 ADD COLUMN IF NOT EXISTS cooldown_until text;",
            )
            .await
            .map_err(|error| format!("migrate client command cooldowns: {error}"))?;
        Ok(store)
    }

    pub async fn set_addon_storage(
        &self,
        addon_id: &str,
        storage_key: &str,
        value_json: &str,
        updated_at: &str,
    ) -> Result<(), String> {
        crate::addons::validate_addon_storage_namespace(addon_id)?;
        crate::addons::validate_addon_storage_key(storage_key)?;
        crate::addons::validate_addon_storage_value(value_json)?;
        if updated_at.is_empty() || updated_at.len() > 80 || updated_at.chars().any(|ch| ch.is_control()) {
            return Err("Invalid addon storage timestamp".to_string());
        }
        self.client
            .execute(
                "INSERT INTO addon_storage (addon_id, storage_key, value_json, updated_at)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (addon_id, storage_key) DO UPDATE SET
                 value_json = EXCLUDED.value_json, updated_at = EXCLUDED.updated_at",
                &[&addon_id, &storage_key, &value_json, &updated_at],
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("set addon storage: {error}"))
    }

    pub async fn get_addon_storage(
        &self,
        addon_id: &str,
        storage_key: &str,
    ) -> Result<Option<String>, String> {
        crate::addons::validate_addon_storage_namespace(addon_id)?;
        crate::addons::validate_addon_storage_key(storage_key)?;
        self.client
            .query_opt(
                "SELECT value_json FROM addon_storage WHERE addon_id = $1 AND storage_key = $2",
                &[&addon_id, &storage_key],
            )
            .await
            .map(|row| row.map(|row| row.get(0)))
            .map_err(|error| format!("get addon storage: {error}"))
    }

    pub async fn list_addon_storage(
        &self,
        addon_id: &str,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<(String, String)>, String> {
        crate::addons::validate_addon_storage_namespace(addon_id)?;
        if prefix.len() > crate::addons::MAX_ADDON_STORAGE_KEY_BYTES
            || prefix.chars().any(|ch| ch.is_control() || ch == '/')
        {
            return Err("Invalid addon storage key prefix".to_string());
        }
        if limit == 0 || limit > 1_000 {
            return Err("Invalid addon storage list limit".to_string());
        }
        let pattern = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        self.client
            .query(
                "SELECT storage_key, value_json FROM addon_storage
                 WHERE addon_id = $1 AND storage_key LIKE $2 ESCAPE '\\'
                 ORDER BY storage_key LIMIT $3",
                &[&addon_id, &pattern, &(limit as i64)],
            )
            .await
            .map(|rows| rows.into_iter().map(|row| (row.get(0), row.get(1))).collect())
            .map_err(|error| format!("list addon storage: {error}"))
    }

    pub async fn delete_addon_storage(&self, addon_id: &str, storage_key: &str) -> Result<bool, String> {
        crate::addons::validate_addon_storage_namespace(addon_id)?;
        crate::addons::validate_addon_storage_key(storage_key)?;
        self.client
            .execute(
                "DELETE FROM addon_storage WHERE addon_id = $1 AND storage_key = $2",
                &[&addon_id, &storage_key],
            )
            .await
            .map(|count| count > 0)
            .map_err(|error| format!("delete addon storage: {error}"))
    }

    pub async fn delete_addon_storage_namespace(&self, addon_id: &str) -> Result<u64, String> {
        crate::addons::validate_addon_storage_namespace(addon_id)?;
        self.client
            .execute(
                "DELETE FROM addon_storage WHERE addon_id = $1",
                &[&addon_id],
            )
            .await
            .map_err(|error| format!("delete addon storage namespace: {error}"))
    }

    pub async fn insert_backup(&self, backup: &Backup) -> Result<(), String> {
        self.client
            .execute(
                "INSERT INTO backups (id, name, created_at, file_path, size_bytes) VALUES ($1, $2, $3, $4, $5)",
                &[&backup.id, &backup.name, &backup.created_at, &backup.file_path, &(backup.size_bytes as i64)],
            )
            .await
            .map_err(|error| format!("insert backup: {error}"))?;
        Ok(())
    }

    pub async fn create_backup(&self, name: &str, file_path: &str, size_bytes: i64) -> Result<(), String> {
        let backup = Backup {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            file_path: file_path.to_string(),
            size_bytes: u64::try_from(size_bytes).map_err(|_| "backup size must not be negative".to_string())?,
        };
        self.insert_backup(&backup).await
    }

    pub async fn get_backup_file_path(&self, id: &str) -> Result<String, String> {
        self.client
            .query_one("SELECT file_path FROM backups WHERE id = $1", &[&id])
            .await
            .map(|row| row.get(0))
            .map_err(|error| format!("get backup path: {error}"))
    }

    pub async fn list_backups(&self) -> Result<Vec<Backup>, String> {
        let rows = self
            .client
            .query(
                "SELECT id, name, created_at, file_path, size_bytes FROM backups ORDER BY created_at DESC",
                &[],
            )
            .await
            .map_err(|error| format!("list backups: {error}"))?;
        rows.into_iter()
            .map(|row| {
                let size: i64 = row.get(4);
                Ok(Backup {
                    id: row.get(0),
                    name: row.get(1),
                    created_at: row.get(2),
                    file_path: row.get(3),
                    size_bytes: u64::try_from(size).map_err(|_| "negative backup size".to_string())?,
                })
            })
            .collect()
    }

    pub async fn delete_backup(&self, id: &str) -> Result<u64, String> {
        self.client
            .execute("DELETE FROM backups WHERE id = $1", &[&id])
            .await
            .map_err(|error| format!("delete backup: {error}"))
    }

    pub async fn find_active_user_credentials(
        &self,
        username: &str,
    ) -> Result<Option<(String, String, String)>, String> {
        self.client
            .query_opt(
                "SELECT username, role, password_hash FROM users WHERE lower(username)=lower($1) AND active=true",
                &[&username],
            )
            .await
            .map(|row| row.map(|row| (row.get(0), row.get(1), row.get(2))))
            .map_err(|error| format!("find active user: {error}"))
    }

    pub async fn record_user_login(&self, username: &str, now: &str) -> Result<(), String> {
        self.client
            .execute(
                "UPDATE users SET last_login_at=$1, updated_at=$1 WHERE username=$2",
                &[&now, &username],
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("record user login: {error}"))
    }

    pub async fn get_user_account(
        &self,
        username: &str,
    ) -> Result<Option<(String, String, String, bool, Option<String>)>, String> {
        self.client
            .query_opt(
                "SELECT username, display_name, role, active, google_email FROM users WHERE lower(username)=lower($1)",
                &[&username],
            )
            .await
            .map(|row| {
                row.map(|row| {
                    (
                        row.get(0),
                        row.get(1),
                        row.get(2),
                        row.get(3),
                        row.get(4),
                    )
                })
            })
            .map_err(|error| format!("get user account: {error}"))
    }

    pub async fn get_active_password_hash(&self, username: &str) -> Result<Option<String>, String> {
        self.client
            .query_opt(
                "SELECT password_hash FROM users WHERE lower(username)=lower($1) AND active=true",
                &[&username],
            )
            .await
            .map(|row| row.map(|row| row.get(0)))
            .map_err(|error| format!("get password hash: {error}"))
    }

    pub async fn update_user_password(
        &self,
        username: &str,
        password_hash: &str,
        now: &str,
    ) -> Result<bool, String> {
        self.client
            .execute(
                "UPDATE users SET password_hash=$1, updated_at=$2 WHERE lower(username)=lower($3)",
                &[&password_hash, &now, &username],
            )
            .await
            .map(|rows| rows > 0)
            .map_err(|error| format!("update password: {error}"))
    }

    pub async fn clear_google_binding(&self, username: &str, now: &str) -> Result<bool, String> {
        self.client
            .execute(
                "UPDATE users SET google_sub=NULL, google_email=NULL, updated_at=$1 WHERE lower(username)=lower($2)",
                &[&now, &username],
            )
            .await
            .map(|rows| rows > 0)
            .map_err(|error| format!("clear Google binding: {error}"))
    }

    pub async fn bind_google_account(
        &self,
        username: &str,
        google_sub: &str,
        google_email: &str,
        now: &str,
    ) -> Result<bool, String> {
        self.client
            .execute(
                "UPDATE users SET google_sub=$1, google_email=$2, updated_at=$3 WHERE lower(username)=lower($4)",
                &[&google_sub, &google_email, &now, &username],
            )
            .await
            .map(|rows| rows > 0)
            .map_err(|error| format!("bind Google account: {error}"))
    }

    pub async fn find_active_google_user(
        &self,
        google_sub: &str,
    ) -> Result<Option<(String, String)>, String> {
        self.client
            .query_opt(
                "SELECT username, role FROM users WHERE google_sub=$1 AND active=true",
                &[&google_sub],
            )
            .await
            .map(|row| row.map(|row| (row.get(0), row.get(1))))
            .map_err(|error| format!("find Google user: {error}"))
    }

    pub async fn delete_user(&self, id: &str) -> Result<u64, String> {
        self.client
            .execute("DELETE FROM users WHERE id = $1", &[&id])
            .await
            .map_err(|error| format!("delete user: {error}"))
    }

    pub async fn auth_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_auth_probe_{}", Uuid::new_v4());
        let username = format!("probe_{}", Uuid::new_v4());
        self.client
            .execute(
                "INSERT INTO users (id, username, display_name, role, password_hash, active, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, true, $6, $6)",
                &[&id, &username, &"PostgreSQL Probe", &"admin", &"hash-one", &"1970-01-01T00:00:00Z"],
            )
            .await
            .map_err(|error| format!("seed auth probe: {error}"))?;
        let credentials = self
            .find_active_user_credentials(&username.to_uppercase())
            .await?
            .ok_or_else(|| "active credential lookup returned no row".to_string())?;
        if credentials.0 != username || credentials.2 != "hash-one" {
            return Err("active credential round-trip mismatch".into());
        }
        if !self
            .update_user_password(&username, "hash-two", "1970-01-01T00:00:01Z")
            .await?
        {
            return Err("password update affected no row".into());
        }
        self.record_user_login(&username, "1970-01-01T00:00:02Z")
            .await?;
        self.bind_google_account(
            &username,
            "probe-google-sub",
            "probe@example.invalid",
            "1970-01-01T00:00:03Z",
        )
        .await?;
        if self.find_active_google_user("probe-google-sub").await?.is_none() {
            return Err("Google binding lookup returned no row".into());
        }
        self.clear_google_binding(&username, "1970-01-01T00:00:04Z")
            .await?;
        if self.delete_user(&id).await? != 1 {
            return Err("auth probe cleanup affected the wrong number of rows".into());
        }
        Ok(())
    }

    pub async fn get_client_assignment(&self, client_id: &str) -> Result<Option<String>, String> {
        self.client
            .query_opt("SELECT assigned_layout_id FROM clients WHERE id=$1", &[&client_id])
            .await
            .map(|row| row.and_then(|row| row.get::<_, Option<String>>(0)))
            .map_err(|error| format!("get client assignment: {error}"))
    }

    pub async fn set_client_assignment(
        &self,
        client_id: &str,
        layout_id: Option<&str>,
    ) -> Result<bool, String> {
        self.client
            .execute(
                "UPDATE clients SET assigned_layout_id=$1 WHERE id=$2",
                &[&layout_id, &client_id],
            )
            .await
            .map(|rows| rows > 0)
            .map_err(|error| format!("set client assignment: {error}"))
    }

    pub async fn bulk_set_client_assignment(
        &self,
        client_ids: &[String],
        layout_id: Option<&str>,
    ) -> Result<usize, String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin client assignment: {error}"))?;
        let mut count = 0usize;
        for client_id in client_ids {
            count += transaction
                .execute(
                    "UPDATE clients SET assigned_layout_id=$1 WHERE id=$2",
                    &[&layout_id, &client_id],
                )
                .await
                .map_err(|error| format!("bulk set client assignment: {error}"))?
                as usize;
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit client assignment: {error}"))?;
        Ok(count)
    }

    pub async fn get_client_theme(&self, client_id: &str) -> Result<Option<String>, String> {
        self.client
            .query_opt("SELECT theme FROM clients WHERE id=$1", &[&client_id])
            .await
            .map(|row| row.and_then(|row| row.get::<_, Option<String>>(0)))
            .map_err(|error| format!("get client theme: {error}"))
    }

    pub async fn set_client_theme(&self, client_id: &str, theme: &str) -> Result<bool, String> {
        self.client
            .execute(
                "UPDATE clients SET theme=$1 WHERE id=$2",
                &[&theme, &client_id],
            )
            .await
            .map(|rows| rows > 0)
            .map_err(|error| format!("set client theme: {error}"))
    }

    pub async fn set_client_enrolled(&self, client_id: &str) -> Result<bool, String> {
        self.client
            .execute(
                "UPDATE clients SET enrolled=true WHERE id=$1 AND is_virtual=false",
                &[&client_id],
            )
            .await
            .map(|rows| rows > 0)
            .map_err(|error| format!("enroll client: {error}"))
    }

    pub async fn get_client_virtual_flag(&self, client_id: &str) -> Result<Option<bool>, String> {
        self.client
            .query_opt("SELECT is_virtual FROM clients WHERE id=$1", &[&client_id])
            .await
            .map(|row| row.map(|row| row.get::<_, bool>(0)))
            .map_err(|error| format!("get client virtual flag: {error}"))
    }

    pub async fn create_virtual_client(&self, id: &str, name: &str) -> Result<(), String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin virtual client transaction: {error}"))?;
        let now = chrono::Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO clients (id,name,address,role,is_virtual,last_seen) VALUES ($1,$2,$3,$4,true,$5)",
                &[&id, &name, &"virtual", &"player", &now],
            )
            .await
            .map_err(|error| format!("create virtual client: {error}"))?;
        transaction
            .execute(
                "INSERT INTO client_displays (client_id,display_index,connector,label,width,height,refresh_rate,enabled,last_seen) VALUES ($1,0,$2,$3,$4,$5,$6,true,$7)",
                &[&id, &"Virtual-1", &format!("{name} display"), &1920i32, &1080i32, &60.0f64, &now],
            )
            .await
            .map_err(|error| format!("create virtual client display: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit virtual client: {error}"))?;
        Ok(())
    }

    pub async fn get_virtual_client(&self, id: &str) -> Result<Option<(String, bool)>, String> {
        self.client
            .query_opt("SELECT name,is_virtual FROM clients WHERE id=$1 AND is_virtual=true", &[&id])
            .await
            .map(|row| row.map(|row| (row.get(0), row.get(1))))
            .map_err(|error| format!("get virtual client: {error}"))
    }

    pub async fn update_virtual_client_name(&self, id: &str, name: &str) -> Result<bool, String> {
        self.client
            .execute("UPDATE clients SET name=$1 WHERE id=$2 AND is_virtual=true", &[&name, &id])
            .await
            .map(|rows| rows > 0)
            .map_err(|error| format!("update virtual client name: {error}"))
    }

    pub async fn is_virtual_client(&self, id: &str) -> Result<bool, String> {
        self.client
            .query_one("SELECT is_virtual FROM clients WHERE id=$1", &[&id])
            .await
            .map(|row| row.get(0))
            .map_err(|error| format!("get virtual client flag: {error}"))
    }

    pub async fn bulk_set_display_layout(&self, client_id: &str, layout_id: Option<&str>) -> Result<usize, String> {
        self.client
            .execute("UPDATE client_displays SET assigned_layout_id=$1 WHERE client_id=$2", &[&layout_id, &client_id])
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("bulk set display layout: {error}"))
    }

    pub async fn get_display_config(&self, client_id: &str, display_index: i64) -> Result<(Option<String>, String), String> {
        let index = i32::try_from(display_index).map_err(|_| "display index out of range".to_string())?;
        let display = self
            .client
            .query_opt("SELECT assigned_layout_id,orientation FROM client_displays WHERE client_id=$1 AND display_index=$2", &[&client_id, &index])
            .await
            .map_err(|error| format!("get display config: {error}"))?;
        let (display_layout, orientation) = display
            .map(|row| (row.get::<_, Option<String>>(0), row.get::<_, Option<String>>(1).unwrap_or_else(|| "normal".into())))
            .unwrap_or((None, "normal".into()));
        if display_layout.is_some() {
            return Ok((display_layout, orientation));
        }
        let client_layout = self
            .client
            .query_opt("SELECT assigned_layout_id FROM clients WHERE id=$1", &[&client_id])
            .await
            .map_err(|error| format!("get client fallback layout: {error}"))?
            .and_then(|row| row.get::<_, Option<String>>(0));
        Ok((client_layout, orientation))
    }

    pub async fn set_display_layout(&self, client_id: &str, display_index: i64, layout_id: Option<&str>) -> Result<usize, String> {
        let index = i32::try_from(display_index).map_err(|_| "display index out of range".to_string())?;
        self.client
            .execute("UPDATE client_displays SET assigned_layout_id=$1 WHERE client_id=$2 AND display_index=$3", &[&layout_id, &client_id, &index])
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("set display layout: {error}"))
    }

    pub async fn set_virtual_client_layout(&self, client_id: &str, layout_id: &str) -> Result<usize, String> {
        self.client
            .execute("UPDATE clients SET assigned_layout_id=$1,active_layout_id=NULL WHERE id=$2 AND is_virtual=true", &[&layout_id, &client_id])
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("set virtual client layout: {error}"))
    }

    pub async fn clear_client_display_command(&self, client_id: &str, display_index: i64) -> Result<usize, String> {
        let index = display_index.to_string();
        let targeted = self
            .client
            .execute("DELETE FROM client_commands WHERE client_id=$1 AND payload IS NOT NULL AND (payload::jsonb ->> 'display_index')=$2", &[&client_id, &index])
            .await
            .map_err(|error| format!("clear targeted display command: {error}"))?;
        if targeted > 0 {
            return Ok(targeted as usize);
        }
        self.client
            .execute("DELETE FROM client_commands WHERE client_id=$1", &[&client_id])
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("clear client display command: {error}"))
    }

    pub async fn virtual_client_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_virtual_probe_{}", Uuid::new_v4());
        self.create_virtual_client(&id, "PostgreSQL virtual probe").await?;
        if self.get_virtual_client(&id).await? != Some(("PostgreSQL virtual probe".into(), true)) {
            return Err("virtual client creation mismatch".into());
        }
        if !self.update_virtual_client_name(&id, "Updated virtual probe").await? {
            return Err("virtual client update affected no rows".into());
        }
        if self.get_virtual_client(&id).await?.map(|value| value.0) != Some("Updated virtual probe".into()) {
            return Err("virtual client name update mismatch".into());
        }
        if !self.is_virtual_client(&id).await? {
            return Err("virtual client flag mismatch".into());
        }
        if self.set_virtual_client_layout(&id, "virtual-layout").await? != 1 || self.get_display_config(&id, 0).await?.0.as_deref() != Some("virtual-layout") {
            return Err("virtual client layout mismatch".into());
        }
        if self.set_display_layout(&id, 0, Some("display-layout")).await? != 1 || self.get_display_config(&id, 0).await?.0.as_deref() != Some("display-layout") {
            return Err("display layout assignment mismatch".into());
        }
        if self.bulk_set_display_layout(&id, None).await? != 1 || self.get_display_config(&id, 0).await?.0.as_deref() != Some("virtual-layout") {
            return Err("bulk display layout fallback mismatch".into());
        }
        self.queue_client_command(&id, "reload_layout", Some(r#"{"display_index":0}"#), "2026-01-01T00:00:00Z", Some("probe")).await?;
        if self.clear_client_display_command(&id, 0).await? != 1 || self.get_client_command(&id).await?.is_some() {
            return Err("targeted display command cleanup mismatch".into());
        }
        let deleted = self.client.execute("DELETE FROM clients WHERE id=$1", &[&id]).await
            .map_err(|error| format!("clean virtual client probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one deleted virtual client, got {deleted}"));
        }
        let remaining_displays: i64 = self.client.query_one(
            "SELECT count(*) FROM client_displays WHERE client_id=$1", &[&id]
        ).await.map_err(|error| format!("check virtual display cleanup: {error}"))?.get(0);
        if remaining_displays != 0 {
            return Err(format!("virtual client displays were not cascaded: {remaining_displays}"));
        }
        Ok(())
    }

    pub async fn session_state(&self, token: &str) -> Result<Option<bool>, String> {
        let now = chrono::Utc::now().timestamp();
        self.client
            .query_opt(
                "SELECT revoked FROM sessions WHERE token=$1 AND expires_at>$2",
                &[&token, &now],
            )
            .await
            .map(|row| row.map(|row| row.get(0)))
            .map_err(|error| format!("check session state: {error}"))
    }

    pub async fn session_principal(
        &self,
        token: &str,
    ) -> Result<Option<(String, Option<String>, Option<bool>)>, String> {
        let now = chrono::Utc::now().timestamp();
        self.client
            .query_opt(
                "SELECT s.username, u.role, u.active FROM sessions s LEFT JOIN users u ON lower(u.username)=lower(s.username) WHERE s.token=$1 AND s.expires_at>$2 AND s.revoked=false",
                &[&token, &now],
            )
            .await
            .map(|row| {
                row.map(|row| {
                    (
                        row.get(0),
                        row.get(1),
                        row.get(2),
                    )
                })
            })
            .map_err(|error| format!("get session principal: {error}"))
    }

    pub async fn revoke_session(&self, token: &str) -> Result<u64, String> {
        self.client
            .execute("UPDATE sessions SET revoked=true WHERE token=$1", &[&token])
            .await
            .map_err(|error| format!("revoke session: {error}"))
    }

    pub async fn purge_expired_sessions(&self) -> Result<u64, String> {
        let now = chrono::Utc::now().timestamp();
        self.client
            .execute("DELETE FROM sessions WHERE expires_at<$1", &[&now])
            .await
            .map_err(|error| format!("purge expired sessions: {error}"))
    }

    pub async fn create_session(&self, token: &str, username: &str, expires_at: i64) -> Result<(), String> {
        self.client
            .execute(
                "INSERT INTO sessions (token,username,expires_at,revoked) VALUES ($1,$2,$3,false) ON CONFLICT(token) DO UPDATE SET username=EXCLUDED.username, expires_at=EXCLUDED.expires_at, revoked=false",
                &[&token, &username, &expires_at],
            )
            .await
            .map_err(|error| format!("create session: {error}"))?;
        Ok(())
    }

    pub async fn session_round_trip_probe(&self) -> Result<(), String> {
        let token = format!("__postgres_session_probe_{}", Uuid::new_v4());
        let expired = format!("__postgres_expired_session_probe_{}", Uuid::new_v4());
        let now = chrono::Utc::now().timestamp();
        self.create_session(&token, "postgres-session-probe", now + 3600).await?;
        if self.session_state(&token).await? != Some(false) {
            return Err("active session round-trip mismatch".into());
        }
        if self.revoke_session(&token).await? != 1 || self.session_state(&token).await? != Some(true) {
            return Err("revoked session round-trip mismatch".into());
        }
        self.create_session(&expired, "postgres-session-probe", now - 3600).await?;
        if self.purge_expired_sessions().await? != 1 {
            return Err("expired session purge count mismatch".into());
        }
        if self.session_state(&expired).await?.is_some() {
            return Err("expired session remained after purge".into());
        }
        let deleted = self.client.execute("DELETE FROM sessions WHERE token=$1", &[&token]).await
            .map_err(|error| format!("clean session probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one session cleanup row, got {deleted}"));
        }
        Ok(())
    }

    pub async fn client_assignment_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_assignment_probe_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: id.clone(),
            name: "PostgreSQL assignment probe".into(),
            address: "127.0.0.1".into(),
            role: "player".into(),
            version: None,
            uptime_seconds: None,
            cpu_usage: None,
            memory_used: None,
            memory_total: None,
            disk_used: None,
            disk_total: None,
            active_layout_id: None,
            last_seen: "1970-01-01T00:00:00Z".into(),
            last_command_result: None,
            last_command_at: None,
            installer_detected_max_displays: None,
            touch_binding: None,
            theme: None,
            max_displays: None,
            client_auth_token_hash: None,
        };
        self.register_client(&client, &[]).await?;
        if self.get_client_assignment(&id).await?.is_some() {
            return Err("new client assignment was not null".into());
        }
        if !self.set_client_assignment(&id, Some("layout-probe")).await? {
            return Err("setting client assignment affected no rows".into());
        }
        if self.get_client_assignment(&id).await?.as_deref() != Some("layout-probe") {
            return Err("client assignment round-trip mismatch".into());
        }
        if self.bulk_set_client_assignment(std::slice::from_ref(&id), None).await? != 1 {
            return Err("bulk client assignment affected the wrong number of rows".into());
        }
        if !self.set_client_theme(&id, "crimson").await? || self.get_client_theme(&id).await?.as_deref() != Some("crimson") {
            return Err("client theme round-trip mismatch".into());
        }
        if !self.set_client_enrolled(&id).await? || self.get_client_virtual_flag(&id).await? != Some(false) {
            return Err("client enrollment or virtual flag mismatch".into());
        }
        let deleted = self.client.execute("DELETE FROM clients WHERE id=$1", &[&id]).await
            .map_err(|error| format!("clean assignment probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one deleted assignment probe row, got {deleted}"));
        }
        Ok(())
    }

    pub async fn publish_scene_assignments(
        &self,
        client_id: &str,
        assignments: &[(i64, Option<String>)],
    ) -> Result<usize, String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin scene publish: {error}"))?;
        let mut updated = 0usize;
        for (display_index, layout_id) in assignments {
            let display_index = *display_index as i32;
            let layout_id = layout_id.as_deref();
            let params: [&(dyn ToSql + Sync); 3] = [&layout_id, &client_id, &display_index];
            updated += transaction
                .execute(
                    "UPDATE client_displays SET assigned_layout_id=$1 WHERE client_id=$2 AND display_index=$3",
                    &params,
                )
                .await
                .map_err(|error| format!("publish scene assignment: {error}"))?
                as usize;
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit scene publish: {error}"))?;
        Ok(updated)
    }

    pub async fn get_scene(
        &self,
        scene_id: &str,
    ) -> Result<Option<(String, String, Option<String>, String, String, Vec<(i64, Option<String>)>)>, String> {
        let Some(row) = self
            .client
            .query_opt(
                "SELECT id, name, description, created_at, updated_at FROM scenes WHERE id=$1",
                &[&scene_id],
            )
            .await
            .map_err(|error| format!("get scene: {error}"))?
        else {
            return Ok(None);
        };
        let base = (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, Option<String>>(2),
            row.get::<_, String>(3),
            row.get::<_, String>(4),
        );
        let rows = self
            .client
            .query(
                "SELECT display_index, layout_id FROM scene_display_layouts WHERE scene_id=$1 ORDER BY display_index ASC",
                &[&scene_id],
            )
            .await
            .map_err(|error| format!("get scene assignments: {error}"))?;
        let assignments = rows
            .into_iter()
            .map(|row| (row.get::<_, i32>(0) as i64, row.get::<_, Option<String>>(1)))
            .collect();
        Ok(Some((base.0, base.1, base.2, base.3, base.4, assignments)))
    }

    pub async fn list_scene_ids(&self) -> Result<Vec<String>, String> {
        self.client
            .query("SELECT id FROM scenes ORDER BY updated_at DESC", &[])
            .await
            .map(|rows| rows.into_iter().map(|row| row.get(0)).collect())
            .map_err(|error| format!("list scenes: {error}"))
    }

    pub async fn save_scene(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        timestamp: &str,
        assignments: &[(i64, Option<String>)],
        update: bool,
    ) -> Result<bool, String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin scene save: {error}"))?;
        let changed = if update {
            let params: [&(dyn ToSql + Sync); 4] = [&name, &description, &timestamp, &id];
            transaction
                .execute(
                    "UPDATE scenes SET name=$1, description=$2, updated_at=$3 WHERE id=$4",
                    &params,
                )
                .await
                .map_err(|error| format!("update scene: {error}"))?
        } else {
            let params: [&(dyn ToSql + Sync); 5] = [&id, &name, &description, &timestamp, &timestamp];
            transaction
                .execute(
                    "INSERT INTO scenes (id, name, description, created_at, updated_at) VALUES ($1,$2,$3,$4,$5)",
                    &params,
                )
                .await
                .map_err(|error| format!("create scene: {error}"))?
        };
        if changed == 0 {
            transaction
                .rollback()
                .await
                .map_err(|error| format!("rollback missing scene save: {error}"))?;
            return Ok(false);
        }
        transaction
            .execute("DELETE FROM scene_display_layouts WHERE scene_id=$1", &[&id])
            .await
            .map_err(|error| format!("replace scene assignments: {error}"))?;
        for (display_index, layout_id) in assignments {
            let display_index = *display_index as i32;
            let layout_id = layout_id.as_deref();
            let params: [&(dyn ToSql + Sync); 3] = [&id, &display_index, &layout_id];
            transaction
                .execute(
                    "INSERT INTO scene_display_layouts (scene_id, display_index, layout_id) VALUES ($1,$2,$3)",
                    &params,
                )
                .await
                .map_err(|error| format!("insert scene assignment: {error}"))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit scene save: {error}"))?;
        Ok(true)
    }

    pub async fn delete_scene(&self, scene_id: &str) -> Result<usize, String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin scene delete: {error}"))?;
        transaction
            .execute("DELETE FROM scene_display_layouts WHERE scene_id=$1", &[&scene_id])
            .await
            .map_err(|error| format!("delete scene assignments: {error}"))?;
        let deleted = transaction
            .execute("DELETE FROM scenes WHERE id=$1", &[&scene_id])
            .await
            .map_err(|error| format!("delete scene: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit scene delete: {error}"))?;
        Ok(deleted as usize)
    }

    pub async fn scene_round_trip_probe(&self) -> Result<(), String> {
        let scene_id = format!("__postgres_scene_probe_{}", Uuid::new_v4());
        let layout_id = format!("__postgres_layout_probe_{}", Uuid::new_v4());
        self.client
            .execute(
                "INSERT INTO layouts (id, name, created_at, updated_at) VALUES ($1,$2,$3,$4)",
                &[&layout_id, &"PostgreSQL layout probe", &"2026-01-01T00:00:00Z", &"2026-01-01T00:00:00Z"],
            )
            .await
            .map_err(|error| format!("create scene layout probe: {error}"))?;
        let assignments = vec![(0, Some(layout_id.clone())), (1, None)];
        if !self.save_scene(&scene_id, "PostgreSQL scene probe", Some("round trip"), "2026-01-01T00:00:00Z", &assignments, false).await? {
            return Err("scene create reported no change".into());
        }
        let scene = self.get_scene(&scene_id).await?.ok_or("saved scene missing")?;
        if scene.1 != "PostgreSQL scene probe" || scene.2.as_deref() != Some("round trip") || scene.5 != assignments {
            return Err("scene round-trip mismatch".into());
        }
        if !self.save_scene(&scene_id, "Updated scene", None, "2026-01-01T00:01:00Z", &[(0, None)], true).await? {
            return Err("scene update reported no change".into());
        }
        if !self.list_scene_ids().await?.contains(&scene_id) {
            return Err("saved scene was not listed".into());
        }
        let client_id = format!("__postgres_scene_client_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: client_id.clone(), name: "Scene probe client".into(), address: "127.0.0.1".into(), role: "player".into(),
            version: None, uptime_seconds: None, cpu_usage: None, memory_used: None, memory_total: None,
            disk_used: None, disk_total: None, active_layout_id: None, last_seen: "1970-01-01T00:00:00Z".into(),
            last_command_result: None, last_command_at: None, installer_detected_max_displays: None,
            touch_binding: None, theme: None, max_displays: None,
            client_auth_token_hash: None,
        };
        self.register_client(&client, &[ClientDisplayRegistration { display_index: 0, connector: None, label: None, width: None, height: None, refresh_rate: None, enabled: None, last_error: None }]).await?;
        if self.publish_scene_assignments(&client_id, &[(0, Some(scene_id.clone()))]).await? != 1 {
            return Err("scene publish affected the wrong number of displays".into());
        }
        if self.delete_scene(&scene_id).await? != 1 || self.get_scene(&scene_id).await?.is_some() {
            return Err("scene deletion round-trip mismatch".into());
        }
        let deleted = self.client.execute("DELETE FROM clients WHERE id=$1", &[&client_id]).await
            .map_err(|error| format!("clean scene client probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one deleted scene client probe row, got {deleted}"));
        }
        let deleted_layout = self.client.execute("DELETE FROM layouts WHERE id=$1", &[&layout_id]).await
            .map_err(|error| format!("clean scene layout probe: {error}"))?;
        if deleted_layout != 1 {
            return Err(format!("expected one deleted scene layout probe row, got {deleted_layout}"));
        }
        Ok(())
    }

    pub async fn queue_client_command(
        &self,
        client_id: &str,
        action: &str,
        payload: Option<&str>,
        requested_at: &str,
        requested_by: Option<&str>,
    ) -> Result<(), String> {
        let burst_limit = crate::rate_limit::CLIENT_COMMAND_BURST_LIMIT;
        // `$7` is used in PostgreSQL as a double-precision interval multiplier.
        // Keep the policy constant integral for SQLite and cooldown messages, but
        // bind the PostgreSQL parameter as f64 so tokio-postgres accepts float8.
        let cooldown_secs = crate::rate_limit::CLIENT_COMMAND_BURST_COOLDOWN_SECS as f64;
        let payload_param = payload.map(str::to_owned);
        let requested_by_param = requested_by.map(str::to_owned);
        if self
            .client
            .query_opt("SELECT 1 FROM client_commands WHERE client_id=$1", &[&client_id])
            .await
            .map_err(|error| format!("check pending client command: {error}"))?
            .is_some()
        {
            return Err("command_pending".to_string());
        }
        let rows = self
            .client
            .execute(
                r#"WITH eligible AS (
                    INSERT INTO client_command_cooldowns
                        (client_id, action, last_requested_at, burst_started_at, burst_count, cooldown_until)
                    VALUES ($1, $2, $4, NOW()::text, 1, NULL)
                    ON CONFLICT (client_id, action) DO UPDATE
                    SET last_requested_at = EXCLUDED.last_requested_at,
                        burst_started_at = CASE
                            WHEN client_command_cooldowns.burst_started_at IS NULL
                                 OR client_command_cooldowns.burst_started_at::timestamptz <= NOW() - ($7::double precision * interval '1 second')
                            THEN NOW()::text
                            ELSE client_command_cooldowns.burst_started_at
                        END,
                        burst_count = CASE
                            WHEN client_command_cooldowns.burst_started_at IS NULL
                                 OR client_command_cooldowns.burst_started_at::timestamptz <= NOW() - ($7::double precision * interval '1 second')
                            THEN 1
                            ELSE client_command_cooldowns.burst_count + 1
                        END,
                        cooldown_until = CASE
                            WHEN client_command_cooldowns.burst_started_at IS NULL
                                 OR client_command_cooldowns.burst_started_at::timestamptz <= NOW() - ($7::double precision * interval '1 second')
                            THEN NULL
                            WHEN client_command_cooldowns.burst_count + 1 >= $6::bigint
                            THEN (NOW() + ($7::double precision * interval '1 second'))::text
                            ELSE NULL
                        END
                    WHERE (client_command_cooldowns.cooldown_until IS NULL
                           OR client_command_cooldowns.cooldown_until::timestamptz <= NOW())
                      AND (client_command_cooldowns.burst_started_at IS NULL
                           OR client_command_cooldowns.burst_started_at::timestamptz <= NOW() - ($7::double precision * interval '1 second')
                           OR client_command_cooldowns.burst_count < $6::bigint)
                    RETURNING 1
                )
                INSERT INTO client_commands (client_id, action, payload, requested_at, requested_by)
                SELECT $1, $2, $3, $4, $5 FROM eligible
                WHERE NOT EXISTS (SELECT 1 FROM client_commands WHERE client_id=$1)
                ON CONFLICT (client_id) DO NOTHING"#,
                &[
                    &client_id,
                    &action,
                    &payload_param,
                    &requested_at,
                    &requested_by_param,
                    &burst_limit,
                    &cooldown_secs,
                ],
            )
            .await
            .map_err(|error| {
                if let Some(db_error) = error.as_db_error() {
                    log::error!(
                        "client command queue database rejection client_id={} action={} sqlstate={} message={}",
                        client_id,
                        action,
                        db_error.code().code(),
                        db_error.message()
                    );
                } else {
                    log::error!(
                        "client command queue database transport failure client_id={} action={} error={}",
                        client_id,
                        action,
                        error
                    );
                }
                format!("queue client command: {error}")
            })?;
        if rows == 0 {
            if self
                .client
                .query_opt("SELECT 1 FROM client_commands WHERE client_id=$1", &[&client_id])
                .await
                .map_err(|error| format!("check queued client command: {error}"))?
                .is_some()
            {
                return Err("command_pending".to_string());
            }
            return Err(format!("cooldown:{cooldown_secs}"));
        }
        Ok(())
    }

    pub async fn get_client_command(
        &self,
        client_id: &str,
    ) -> Result<Option<(String, String, Option<String>, String, Option<String>)>, String> {
        self.client
            .query_opt(
                "SELECT client_id, action, payload, requested_at, requested_by FROM client_commands WHERE client_id=$1",
                &[&client_id],
            )
            .await
            .map(|row| {
                row.map(|row| {
                    (
                        row.get(0),
                        row.get(1),
                        row.get::<_, Option<String>>(2),
                        row.get(3),
                        row.get::<_, Option<String>>(4),
                    )
                })
            })
            .map_err(|error| format!("get client command: {error}"))
    }

    pub async fn delete_client_command_if_requested_at(
        &self,
        client_id: &str,
        requested_at: &str,
    ) -> Result<usize, String> {
        self.client
            .execute(
                "DELETE FROM client_commands WHERE client_id=$1 AND requested_at=$2",
                &[&client_id, &requested_at],
            )
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("delete client command: {error}"))
    }

    pub async fn clear_client_command(&self, client_id: &str) -> Result<usize, String> {
        self.client
            .execute("DELETE FROM client_commands WHERE client_id=$1", &[&client_id])
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("clear client command: {error}"))
    }

    pub async fn client_command_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_command_probe_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: id.clone(),
            name: "PostgreSQL command probe".into(),
            address: "127.0.0.1".into(),
            role: "player".into(),
            version: None,
            uptime_seconds: None,
            cpu_usage: None,
            memory_used: None,
            memory_total: None,
            disk_used: None,
            disk_total: None,
            active_layout_id: None,
            last_seen: "1970-01-01T00:00:00Z".into(),
            last_command_result: None,
            last_command_at: None,
            installer_detected_max_displays: None,
            touch_binding: None,
            theme: None,
            max_displays: None,
            client_auth_token_hash: None,
        };
        self.register_client(&client, &[]).await?;
        self.queue_client_command(&id, "restart", Some("{\"scope\":\"kiosk\"}"), "2026-01-01T00:00:00Z", Some("probe"))
            .await?;
        let command = self.get_client_command(&id).await?.ok_or("queued command missing")?;
        if command.1 != "restart" || command.2.as_deref() != Some("{\"scope\":\"kiosk\"}") || command.4.as_deref() != Some("probe") {
            return Err("client command round-trip mismatch".into());
        }
        let pending = self.queue_client_command(&id, "audio_set_volume", None, "2026-01-01T00:01:00Z", None).await;
        if !matches!(pending, Err(ref error) if error == "command_pending") {
            return Err("pending client command was not preserved".into());
        }
        let retained = self.get_client_command(&id).await?.ok_or("retained command missing")?;
        if retained.1 != "restart" || retained.2.as_deref() != Some("{\"scope\":\"kiosk\"}") {
            return Err("pending client command changed unexpectedly".into());
        }
        if self.delete_client_command_if_requested_at(&id, "wrong-time").await? != 0 {
            return Err("conditional command delete removed the wrong row".into());
        }
        if self.delete_client_command_if_requested_at(&id, "2026-01-01T00:00:00Z").await? != 1 {
            return Err("conditional command delete affected the wrong number of rows".into());
        }
        self.queue_client_command(&id, "restart", None, "2026-01-01T00:02:00Z", None).await?;
        if self.clear_client_command(&id).await? != 1 || self.get_client_command(&id).await?.is_some() {
            return Err("client command clear round-trip mismatch".into());
        }
        self.queue_client_command(&id, "restart", None, "2026-01-01T00:03:00Z", None).await?;
        if self.get_client_command(&id).await?.is_none() {
            return Err("client command missing before client deletion".into());
        }
        if !self.delete_client(&id).await? || self.client_exists(&id).await? {
            return Err("client deletion did not remove the client and queued command".into());
        }
        let remaining_commands: i64 = self.client.query_one(
            "SELECT count(*) FROM client_commands WHERE client_id=$1", &[&id]
        ).await.map_err(|error| format!("check command cleanup: {error}"))?.get(0);
        let remaining_cooldowns: i64 = self.client.query_one(
            "SELECT count(*) FROM client_command_cooldowns WHERE client_id=$1", &[&id]
        ).await.map_err(|error| format!("check cooldown cleanup: {error}"))?.get(0);
        if remaining_commands != 0 || remaining_cooldowns != 0 {
            return Err(format!("client command cleanup left {remaining_commands} commands and {remaining_cooldowns} cooldowns"));
        }
        Ok(())
    }

    pub async fn set_client_display_count(
        &self,
        client_id: &str,
        requested: Option<i64>,
    ) -> Result<Option<(i64, i64)>, String> {
        let installer = self
            .client
            .query_opt(
                "SELECT COALESCE(installer_detected_max_displays, 1) FROM clients WHERE id=$1",
                &[&client_id],
            )
            .await
            .map_err(|error| format!("read client display limit: {error}"))?
            .map(|row| row.get::<_, i32>(0));
        let Some(installer) = installer else {
            return Ok(None);
        };
        let installer = installer.clamp(1, 4);
        let requested_i32 = requested.map(|value| value.clamp(1, installer as i64) as i32);
        let active = requested_i32.unwrap_or(installer);
        self.client
            .execute(
                "UPDATE clients SET max_displays=$1 WHERE id=$2",
                &[&requested_i32, &client_id],
            )
            .await
            .map_err(|error| format!("set client display count: {error}"))?;
        Ok(Some((installer as i64, active as i64)))
    }

    pub async fn client_exists(&self, client_id: &str) -> Result<bool, String> {
        self.client
            .query_one("SELECT EXISTS(SELECT 1 FROM clients WHERE id=$1)", &[&client_id])
            .await
            .map(|row| row.get(0))
            .map_err(|error| format!("check client existence: {error}"))
    }

    pub async fn get_client_auth_token_hash(&self, client_id: &str) -> Result<Option<String>, String> {
        self.client
            .query_opt(
                "SELECT client_auth_token_hash FROM clients WHERE id=$1",
                &[&client_id],
            )
            .await
            .map(|row| row.and_then(|value| value.get(0)))
            .map_err(|error| format!("read client authentication state: {error}"))
    }

    pub async fn set_client_touch_binding(
        &self,
        client_id: &str,
        binding_json: &str,
    ) -> Result<bool, String> {
        self.client
            .execute(
                "UPDATE clients SET touch_binding=$1 WHERE id=$2",
                &[&binding_json, &client_id],
            )
            .await
            .map(|rows| rows > 0)
            .map_err(|error| format!("set client touch binding: {error}"))
    }

    pub async fn client_controls_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_controls_probe_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: id.clone(),
            name: "PostgreSQL controls probe".into(),
            address: "127.0.0.1".into(),
            role: "player".into(),
            version: None,
            uptime_seconds: None,
            cpu_usage: None,
            memory_used: None,
            memory_total: None,
            disk_used: None,
            disk_total: None,
            active_layout_id: None,
            last_seen: "1970-01-01T00:00:00Z".into(),
            last_command_result: None,
            last_command_at: None,
            installer_detected_max_displays: Some(3),
            touch_binding: None,
            theme: None,
            max_displays: None,
            client_auth_token_hash: None,
        };
        self.register_client(&client, &[]).await?;
        if !self.client_exists(&id).await? {
            return Err("registered client was not found".into());
        }
        if self.set_client_display_count(&id, Some(9)).await? != Some((3, 3)) {
            return Err("client display count was not clamped correctly".into());
        }
        if self.set_client_display_count(&id, None).await? != Some((3, 3)) {
            return Err("client display count default was not returned correctly".into());
        }
        if !self.set_client_touch_binding(&id, "{\"display\":0}").await? {
            return Err("client touch binding affected no rows".into());
        }
        let binding: Option<String> = self
            .client
            .query_one("SELECT touch_binding FROM clients WHERE id=$1", &[&id])
            .await
            .map_err(|error| format!("read touch binding probe: {error}"))?
            .get(0);
        if binding.as_deref() != Some("{\"display\":0}") {
            return Err("client touch binding round-trip mismatch".into());
        }
        let deleted = self.client.execute("DELETE FROM clients WHERE id=$1", &[&id]).await
            .map_err(|error| format!("clean controls probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one deleted controls probe row, got {deleted}"));
        }
        Ok(())
    }

    pub async fn get_display_state(&self) -> Result<DisplayState, String> {
        self.client.query("SELECT display,layout_id FROM display_state ORDER BY display", &[]).await.map(|rows| DisplayState { assignments: rows.into_iter().map(|row| DisplayAssignment { display: row.get(0), layout_id: row.get(1) }).collect() }).map_err(|error| format!("get display state: {error}"))
    }

    pub async fn get_display_assignment(&self, display: &str) -> Result<Option<DisplayAssignment>, String> {
        self.client.query_opt("SELECT display,layout_id FROM display_state WHERE display=$1", &[&display]).await.map(|row| row.map(|row| DisplayAssignment { display: row.get(0), layout_id: row.get(1) })).map_err(|error| format!("get display assignment: {error}"))
    }

    pub async fn set_display_assignment(&self, display: &str, layout_id: Option<&str>) -> Result<(), String> {
        self.client.execute("INSERT INTO display_state (display,layout_id) VALUES ($1,$2) ON CONFLICT(display) DO UPDATE SET layout_id=EXCLUDED.layout_id", &[&display,&layout_id]).await.map(|_| ()).map_err(|error| format!("set display assignment: {error}"))
    }

    pub async fn delete_display_assignment(&self, display: &str) -> Result<usize, String> {
        self.client.execute("DELETE FROM display_state WHERE display=$1", &[&display]).await.map(|count| count as usize).map_err(|error| format!("delete display assignment: {error}"))
    }

    pub async fn display_assignment_round_trip_probe(&self) -> Result<(), String> {
        let display = format!("__postgres_display_probe_{}", Uuid::new_v4());
        self.set_display_assignment(&display, Some("layout-probe")).await?;
        if self.get_display_assignment(&display).await?.ok_or("display assignment missing")?.layout_id != Some("layout-probe".into()) { return Err("display assignment mismatch".into()); }
        if self.get_display_state().await?.assignments.iter().all(|assignment| assignment.display != display) { return Err("display state listing mismatch".into()); }
        if self.delete_display_assignment(&display).await? != 1 { return Err("display assignment cleanup mismatch".into()); }
        Ok(())
    }
    fn user_from_row(row: &tokio_postgres::Row) -> UserRecord {
        UserRecord { id: row.get(0), username: row.get(1), display_name: row.get(2), role: row.get(3), active: row.get(4), created_at: row.get(5), updated_at: row.get(6), last_login_at: row.get(7) }
    }

    pub async fn list_users(&self) -> Result<Vec<UserRecord>, String> {
        self.client.query("SELECT id,username,display_name,role,active,created_at,updated_at,last_login_at FROM users ORDER BY active DESC,role,username", &[]).await.map(|rows| rows.iter().map(Self::user_from_row).collect()).map_err(|error| format!("list users: {error}"))
    }

    pub async fn get_user(&self, id: &str) -> Result<Option<UserRecord>, String> {
        self.client.query_opt("SELECT id,username,display_name,role,active,created_at,updated_at,last_login_at FROM users WHERE id=$1", &[&id]).await.map(|row| row.as_ref().map(Self::user_from_row)).map_err(|error| format!("get user: {error}"))
    }

    pub async fn count_active_admins(&self) -> Result<i64, String> {
        self.client.query_one("SELECT count(*) FROM users WHERE role='admin' AND active=true", &[]).await.map(|row| row.get(0)).map_err(|error| format!("count active admins: {error}"))
    }

    pub async fn create_user(&self, id: &str, username: &str, display_name: &str, role: &str, password_hash: &str, active: bool, now: &str) -> Result<(), String> {
        self.client.execute("INSERT INTO users (id,username,display_name,role,password_hash,active,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$7)", &[&id,&username,&display_name,&role,&password_hash,&active,&now]).await.map(|_| ()).map_err(|error| format!("create user: {error}"))
    }

    pub async fn create_initial_admin(
        &self,
        id: &str,
        username: &str,
        display_name: &str,
        password_hash: &str,
        now: &str,
    ) -> Result<bool, String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin initial admin transaction: {error}"))?;
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended('screentra.initial_admin', 0))",
                &[],
            )
            .await
            .map_err(|error| format!("lock initial admin setup: {error}"))?;
        let existing: i64 = transaction
            .query_one(
                "SELECT count(*) FROM users WHERE role='admin' AND active=true",
                &[],
            )
            .await
            .map_err(|error| format!("count active admins: {error}"))?
            .get(0);
        if existing > 0 {
            return Ok(false);
        }
        transaction
            .execute(
                "INSERT INTO users (id,username,display_name,role,password_hash,active,created_at,updated_at) VALUES ($1,$2,$3,'admin',$4,true,$5,$5)",
                &[&id, &username, &display_name, &password_hash, &now],
            )
            .await
            .map_err(|error| format!("create initial admin: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit initial admin: {error}"))?;
        Ok(true)
    }

    pub async fn update_user(&self, id: &str, username: &str, display_name: &str, role: &str, password_hash: Option<&str>, active: bool, now: &str) -> Result<bool, String> {
        let changed = match password_hash { Some(hash) => self.client.execute("UPDATE users SET username=$1,display_name=$2,role=$3,password_hash=$4,active=$5,updated_at=$6 WHERE id=$7", &[&username,&display_name,&role,&hash,&active,&now,&id]).await, None => self.client.execute("UPDATE users SET username=$1,display_name=$2,role=$3,active=$4,updated_at=$5 WHERE id=$6", &[&username,&display_name,&role,&active,&now,&id]).await }.map_err(|error| format!("update user: {error}"))?;
        Ok(changed > 0)
    }

    pub async fn delete_user_record(&self, id: &str) -> Result<bool, String> {
        self.client.execute("DELETE FROM users WHERE id=$1", &[&id]).await.map(|count| count > 0).map_err(|error| format!("delete user: {error}"))
    }

    pub async fn user_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_user_probe_{}", Uuid::new_v4());
        self.create_user(&id,"probe-admin","Probe Admin","admin","hash-1",true,"2026-01-01T00:00:00Z").await?;
        if self.get_user(&id).await?.ok_or("user missing")?.username != "probe-admin" { return Err("user round-trip mismatch".into()); }
        if self.count_active_admins().await? != 1 { return Err("active admin count mismatch".into()); }
        if !self.update_user(&id,"probe-admin-2","Probe Admin 2","admin",None,false,"2026-01-02T00:00:00Z").await? { return Err("user update failed".into()); }
        if self.get_user(&id).await?.ok_or("updated user missing")?.active { return Err("user active flag mismatch".into()); }
        if !self.delete_user_record(&id).await? || self.get_user(&id).await?.is_some() { return Err("user cleanup failed".into()); }
        Ok(())
    }
    fn validate_table_name(table: &str) -> Result<(), String> {
        if table.is_empty() || !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { Err("Invalid table name".into()) } else { Ok(()) }
    }

    pub async fn list_table_names(&self) -> Result<Vec<String>, String> {
        self.client.query("SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' ORDER BY table_name", &[]).await.map(|rows| rows.into_iter().map(|row| row.get(0)).collect()).map_err(|error| format!("list table names: {error}"))
    }

    pub async fn query_table_rows(&self, table: &str, limit: u32, offset: u32) -> Result<TableQueryResult, String> {
        Self::validate_table_name(table)?;
        let columns = self.client.query("SELECT column_name,data_type,is_nullable,column_default FROM information_schema.columns WHERE table_schema='public' AND table_name=$1 ORDER BY ordinal_position", &[&table]).await.map_err(|error| format!("table columns: {error}"))?.into_iter().map(|row| TableColumnRecord { name: row.get(0), col_type: row.get(1), notnull: row.get::<_, String>(2) == "NO", dflt_value: row.get(3), pk: false }).collect::<Vec<_>>();
        let total = self.client.query_one(&format!("SELECT count(*) FROM public.\"{table}\""), &[]).await.map(|row| row.get(0)).map_err(|error| format!("table count: {error}"))?;
        let rows = self.client.query(&format!("SELECT row_to_json(t)::text FROM (SELECT * FROM public.\"{table}\" LIMIT {limit} OFFSET {offset}) t"), &[]).await.map_err(|error| format!("table rows: {error}"))?;
        let values = rows.into_iter().map(|row| { let object: serde_json::Value = serde_json::from_str(row.get::<_, String>(0).as_str()).unwrap_or(serde_json::Value::Null); columns.iter().map(|column| object.get(&column.name).cloned().unwrap_or(serde_json::Value::Null)).collect() }).collect();
        Ok(TableQueryResult { columns, rows: values, total })
    }

    pub async fn export_table_rows(&self, table: &str) -> Result<TableExportResult, String> {
        let result = self.query_table_rows(table, u32::MAX, 0).await?;
        Ok(TableExportResult { columns: result.columns.iter().map(|column| column.name.clone()).collect(), rows: result.rows })
    }

    pub async fn delete_table_rows(&self, table: &str, ids: &[i64]) -> Result<usize, String> {
        Self::validate_table_name(table)?;
        let pk = self.client.query_opt("SELECT kcu.column_name FROM information_schema.table_constraints tc JOIN information_schema.key_column_usage kcu ON tc.constraint_name=kcu.constraint_name AND tc.table_schema=kcu.table_schema WHERE tc.table_schema='public' AND tc.table_name=$1 AND tc.constraint_type='PRIMARY KEY' ORDER BY kcu.ordinal_position LIMIT 1", &[&table]).await.map_err(|error| format!("table primary key: {error}"))?.ok_or("Table has no primary key")?.get::<_, String>(0);
        let mut deleted = 0; for id in ids { deleted += self.client.execute(&format!("DELETE FROM public.\"{table}\" WHERE \"{pk}\"=$1"), &[id]).await.map_err(|error| format!("delete table row: {error}"))? as usize; } Ok(deleted)
    }
    pub async fn table_admin_round_trip_probe(&self) -> Result<(), String> {
        let tables = self.list_table_names().await?;
        if !tables.iter().any(|table| table == "clients") { return Err("clients table missing".into()); }
        let result = self.query_table_rows("refresh_state", 10, 0).await?;
        if result.columns.iter().all(|column| column.name != "epoch") { return Err("refresh_state metadata missing epoch".into()); }
        let export = self.export_table_rows("refresh_state").await?;
        if export.columns.len() != result.columns.len() || export.rows.len() != 1 { return Err("table export mismatch".into()); }
        if self.query_table_rows("bad-name", 1, 0).await.is_ok() { return Err("invalid table name accepted".into()); }
        Ok(())
    }

    pub async fn vacuum(&self) -> Result<(), String> {
        self.client
            .batch_execute("VACUUM")
            .await
            .map_err(|error| format!("vacuum: {error}"))
    }

    pub async fn maintenance_round_trip_probe(&self) -> Result<(), String> {
        self.vacuum().await
    }

    pub async fn get_refresh_epoch(&self) -> Result<i64, String> {
        self.client.query_one("SELECT epoch FROM refresh_state WHERE id=1", &[]).await.map(|row| row.get(0)).map_err(|error| format!("get refresh epoch: {error}"))
    }

    pub async fn bump_refresh_epoch(&self) -> Result<i64, String> {
        self.client.execute("UPDATE refresh_state SET epoch=epoch+1 WHERE id=1", &[]).await.map_err(|error| format!("bump refresh epoch: {error}"))?;
        self.get_refresh_epoch().await
    }

    pub async fn refresh_epoch_round_trip_probe(&self) -> Result<(), String> {
        let before = self.get_refresh_epoch().await?;
        let after = self.bump_refresh_epoch().await?;
        if after != before + 1 { return Err(format!("refresh epoch did not increment: before={before}, after={after}")); }
        Ok(())
    }
    async fn interactive_submission_rows(&self, id: Option<i64>) -> Result<Vec<serde_json::Value>, String> {
        let sql = "SELECT id,client_id,client_address,interactive_layout_id,interactive_layout_name,widget_id,widget_type,widget_label,payload_json,collected_at,received_at,google_sync_status,google_sync_message,google_synced_at,google_sheet_id FROM interactive_submissions";
        let rows = match id { Some(id) => self.client.query(&format!("{sql} WHERE id=$1"), &[&id]).await, None => self.client.query(&format!("{sql} ORDER BY id DESC"), &[]).await }.map_err(|error| format!("list interactive submissions: {error}"))?;
        Ok(rows.into_iter().map(|row| serde_json::json!({"id": row.get::<_, i64>(0), "client_id": row.get::<_, String>(1), "client_address": row.get::<_, Option<String>>(2), "interactive_layout_id": row.get::<_, Option<String>>(3), "interactive_layout_name": row.get::<_, Option<String>>(4), "widget_id": row.get::<_, String>(5), "widget_type": row.get::<_, String>(6), "widget_label": row.get::<_, Option<String>>(7), "payload_json": row.get::<_, String>(8), "collected_at": row.get::<_, String>(9), "received_at": row.get::<_, String>(10), "google_sync_status": row.get::<_, String>(11), "google_sync_message": row.get::<_, Option<String>>(12), "google_synced_at": row.get::<_, Option<String>>(13), "google_sheet_id": row.get::<_, Option<String>>(14)})).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_interactive_submission(&self, client_id: &str, client_address: Option<&str>, interactive_layout_id: Option<&str>, interactive_layout_name: Option<&str>, widget_id: &str, widget_type: &str, widget_label: Option<&str>, payload_json: &str, collected_at: &str) -> Result<i64, String> {
        let row = self.client.query_one("INSERT INTO interactive_submissions (client_id,client_address,interactive_layout_id,interactive_layout_name,widget_id,widget_type,widget_label,payload_json,collected_at,received_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id", &[&client_id, &client_address, &interactive_layout_id, &interactive_layout_name, &widget_id, &widget_type, &widget_label, &payload_json, &collected_at, &chrono::Utc::now().to_rfc3339()]).await.map_err(|error| format!("insert interactive submission: {error}"))?;
        Ok(row.get(0))
    }

    pub async fn list_interactive_submissions(&self, limit: i64, client_filter: Option<&str>, page_filter: Option<&str>, status_filter: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
        let mut rows = self.interactive_submission_rows(None).await?;
        rows.retain(|row| client_filter.map(|value| row["client_id"] == value).unwrap_or(true) && page_filter.map(|value| row["interactive_layout_id"] == value).unwrap_or(true) && status_filter.map(|value| row["google_sync_status"] == value).unwrap_or(true));
        rows.truncate(limit.max(0) as usize);
        Ok(rows)
    }

    pub async fn get_interactive_submission(&self, id: i64) -> Result<Option<serde_json::Value>, String> {
        Ok(self.interactive_submission_rows(Some(id)).await?.into_iter().next())
    }

    pub async fn update_interactive_submission_sync_status(&self, ids: &[i64], status: &str, message: Option<&str>, sheet_id: Option<&str>) -> Result<usize, String> {
        let mut updated = 0;
        for id in ids { updated += self.client.execute("UPDATE interactive_submissions SET google_sync_status=$1,google_sync_message=$2,google_synced_at=$3,google_sheet_id=$4 WHERE id=$5", &[&status, &message, &chrono::Utc::now().to_rfc3339(), &sheet_id, &id]).await.map_err(|error| format!("update submission sync status: {error}"))? as usize; }
        Ok(updated)
    }

    pub async fn count_interactive_submissions(&self, status_filter: Option<&str>) -> Result<i64, String> {
        self.client.query_one("SELECT count(*) FROM interactive_submissions WHERE ($1::text IS NULL OR google_sync_status=$1)", &[&status_filter]).await.map(|row| row.get(0)).map_err(|error| format!("count interactive submissions: {error}"))
    }

    pub async fn delete_interactive_submissions(&self, ids: &[i64]) -> Result<usize, String> {
        let mut deleted = 0; for id in ids { deleted += self.client.execute("DELETE FROM interactive_submissions WHERE id=$1", &[&id]).await.map_err(|error| format!("delete interactive submission: {error}"))? as usize; } Ok(deleted)
    }

    pub async fn interactive_submission_round_trip_probe(&self) -> Result<(), String> {
        let client_id = format!("__postgres_submission_probe_{}", Uuid::new_v4());
        let id = self.insert_interactive_submission(&client_id, Some("127.0.0.1"), Some("interactive-page"), Some("Probe page"), "button-1", "button", Some("Submit"), "{\"value\":true}", "2026-01-01T00:00:00Z").await?;
        if self.get_interactive_submission(id).await?.ok_or("submission missing")?["client_id"] != client_id { return Err("submission round-trip mismatch".into()); }
        if self.count_interactive_submissions(Some("pending")).await? != 1 { return Err("submission count mismatch".into()); }
        self.update_interactive_submission_sync_status(&[id], "synced", Some("ok"), Some("sheet-1")).await?;
        if self.list_interactive_submissions(10, None, None, Some("synced")).await?.len() != 1 { return Err("submission status filter mismatch".into()); }
        if self.delete_interactive_submissions(&[id]).await? != 1 { return Err("submission delete mismatch".into()); }
        Ok(())
    }
    fn schedule_from_row(row: &tokio_postgres::Row) -> ScheduleEntry {
        ScheduleEntry { id: row.get(0), display: row.get(1), layout_id: row.get::<_, Option<String>>(2).unwrap_or_default(), day_of_week: row.get(3), start_time: row.get(4), end_time: row.get(5), created_at: row.get(6), layout_set_id: row.get(7), action: row.get(8), target_client: row.get(9), power_state: row.get(10), display_name: row.get(12), control_method: row.get(13), display_index: row.get::<_, Option<i32>>(14), brightness: row.get::<_, Option<i32>>(15), contrast: row.get::<_, Option<i32>>(16) }
    }

    pub async fn list_schedules(&self) -> Result<Vec<ScheduleEntry>, String> {
        self.client.query("SELECT id,display,layout_id,day_of_week,start_time,end_time,created_at,layout_set_id,action,target_client,power_state,updated_at,display_name,control_method,display_index,brightness,contrast FROM schedules ORDER BY day_of_week,start_time", &[]).await.map(|rows| rows.iter().map(Self::schedule_from_row).collect()).map_err(|error| format!("list schedules: {error}"))
    }

    pub async fn get_schedule(&self, id: &str) -> Result<Option<ScheduleEntry>, String> {
        self.client.query_opt("SELECT id,display,layout_id,day_of_week,start_time,end_time,created_at,layout_set_id,action,target_client,power_state,updated_at,display_name,control_method,display_index,brightness,contrast FROM schedules WHERE id=$1", &[&id]).await.map(|row| row.as_ref().map(Self::schedule_from_row)).map_err(|error| format!("get schedule: {error}"))
    }

    pub async fn create_schedule(&self, entry: &ScheduleEntry) -> Result<(), String> {
        let index = entry.display_index.unwrap_or(0);
        let control_method = entry.control_method.as_deref().unwrap_or("auto");
        self.client.execute("INSERT INTO schedules (id,display,layout_id,layout_set_id,day_of_week,start_time,end_time,created_at,action,target_client,power_state,display_name,control_method,display_index,brightness,contrast) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)", &[&entry.id, &entry.display, &entry.layout_id, &entry.layout_set_id, &entry.day_of_week, &entry.start_time, &entry.end_time, &entry.created_at, &entry.action, &entry.target_client, &entry.power_state, &entry.display_name, &control_method, &index, &entry.brightness, &entry.contrast]).await.map(|_| ()).map_err(|error| format!("create schedule: {error}"))
    }

    pub async fn update_schedule(&self, entry: &ScheduleEntry) -> Result<(), String> {
        let index = entry.display_index.unwrap_or(0);
        let control_method = entry.control_method.as_deref().unwrap_or("auto");
        if self.client.execute("UPDATE schedules SET display=$2,layout_id=$3,layout_set_id=$4,day_of_week=$5,start_time=$6,end_time=$7,action=$8,target_client=$9,power_state=$10,display_name=$11,control_method=$12,display_index=$13,brightness=$14,contrast=$15,updated_at=$16 WHERE id=$1", &[&entry.id, &entry.display, &entry.layout_id, &entry.layout_set_id, &entry.day_of_week, &entry.start_time, &entry.end_time, &entry.action, &entry.target_client, &entry.power_state, &entry.display_name, &control_method, &index, &entry.brightness, &entry.contrast, &entry.created_at]).await.map_err(|error| format!("update schedule: {error}"))? == 0 { return Err("Schedule not found".into()); }
        Ok(())
    }

    pub async fn delete_schedule(&self, id: &str) -> Result<(), String> {
        if self.client.execute("DELETE FROM schedules WHERE id=$1", &[&id]).await.map_err(|error| format!("delete schedule: {error}"))? == 0 { return Err("Schedule not found".into()); }
        Ok(())
    }

    pub async fn get_active_schedules_full(&self, current_time: &str, day_of_week: i32) -> Result<Vec<ScheduleEntry>, String> {
        if !(0..=6).contains(&day_of_week) {
            return Err("day_of_week must be between 0 and 6".into());
        }
        let day_bit = 1i32 << day_of_week;
        let previous_day_bit = 1i32 << ((day_of_week + 6) % 7);
        self.client
            .query(
                "SELECT id,display,layout_id,day_of_week,start_time,end_time,created_at,layout_set_id,action,target_client,power_state,updated_at,display_name,control_method,display_index,brightness,contrast FROM schedules WHERE (start_time <= end_time AND (day_of_week=-1 OR (day_of_week & $1) <> 0) AND start_time <= $3 AND end_time >= $3) OR (start_time > end_time AND (((day_of_week=-1 OR (day_of_week & $1) <> 0) AND start_time <= $3) OR ((day_of_week=-1 OR (day_of_week & $2) <> 0) AND end_time >= $3))) ORDER BY start_time,id",
                &[&day_bit, &previous_day_bit, &current_time],
            )
            .await
            .map(|rows| rows.iter().map(Self::schedule_from_row).collect())
            .map_err(|error| format!("get active schedules: {error}"))
    }

    pub async fn log_schedule_run(&self, schedule_id: &str, action: &str, target: &str, success: bool, message: Option<&str>, occurred_at: &str) -> Result<i64, String> {
        self.client.query_one("INSERT INTO schedule_runs (schedule_id,action,target,success,message,occurred_at) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id", &[&schedule_id,&action,&target,&success,&message,&occurred_at]).await.map(|row| row.get(0)).map_err(|error| format!("log schedule run: {error}"))
    }
    pub async fn list_schedule_runs(&self, limit: i64) -> Result<Vec<ScheduleRun>, String> {
        let limit = limit.clamp(1, 500);
        self.client.query("SELECT id,schedule_id,action,target,success,message,occurred_at FROM schedule_runs ORDER BY occurred_at DESC LIMIT $1", &[&limit]).await.map(|rows| rows.into_iter().map(|row| ScheduleRun { id: row.get(0), schedule_id: row.get(1), action: row.get(2), target: row.get(3), success: row.get(4), message: row.get(5), occurred_at: row.get(6) }).collect()).map_err(|error| format!("list schedule runs: {error}"))
    }
    pub async fn record_audit_event(
        &self,
        actor: Option<&str>,
        role: Option<&str>,
        event_type: &str,
        target: Option<&str>,
        outcome: &str,
        detail: Option<&str>,
        occurred_at: &str,
    ) -> Result<i64, String> {
        fn bounded(value: &str, max: usize) -> String {
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(max)
                .collect()
        }
        let actor = actor.map(|value| bounded(value, 128));
        let role = role.map(|value| bounded(value, 64));
        let event_type = bounded(event_type, 96);
        let target = target.map(|value| bounded(value, 256));
        let outcome = bounded(outcome, 32);
        let detail = detail.map(|value| bounded(value, 512));
        let occurred_at = bounded(occurred_at, 64);
        self.client
            .query_one(
                "INSERT INTO audit_events (actor,role,event_type,target,outcome,detail,occurred_at) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id",
                &[&actor, &role, &event_type, &target, &outcome, &detail, &occurred_at],
            )
            .await
            .map(|row| row.get(0))
            .map_err(|error| format!("record audit event: {error}"))
    }

    pub async fn list_audit_events(&self, limit: i64) -> Result<Vec<AuditEvent>, String> {
        let limit = limit.clamp(1, 500);
        self.client
            .query(
                "SELECT id,actor,role,event_type,target,outcome,detail,occurred_at FROM audit_events ORDER BY id DESC LIMIT $1",
                &[&limit],
            )
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| AuditEvent {
                        id: row.get(0),
                        actor: row.get(1),
                        role: row.get(2),
                        event_type: row.get(3),
                        target: row.get(4),
                        outcome: row.get(5),
                        detail: row.get(6),
                        occurred_at: row.get(7),
                    })
                    .collect()
            })
            .map_err(|error| format!("list audit events: {error}"))
    }
    pub async fn schedule_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_schedule_probe_{}", Uuid::new_v4());
        let layout_id = format!("__postgres_schedule_layout_{}", Uuid::new_v4());
        self.client.execute("INSERT INTO layouts (id,name,created_at,updated_at) VALUES ($1,$2,$3,$4)", &[&layout_id, &"Schedule probe layout", &"2026-01-01T00:00:00Z", &"2026-01-01T00:00:00Z"]).await.map_err(|error| format!("create schedule probe layout: {error}"))?;
        let entry = ScheduleEntry { id: id.clone(), display: "HDMI-A-1".into(), layout_id: layout_id.clone(), layout_set_id: None, day_of_week: -1, start_time: "08:00".into(), end_time: "17:00".into(), created_at: "2026-01-01T00:00:00Z".into(), action: "switch_layout".into(), target_client: None, power_state: None, display_name: Some("Probe schedule".into()), control_method: None, display_index: None, brightness: None, contrast: None };
        self.create_schedule(&entry).await?;
        let stored = self.get_schedule(&id).await?.ok_or("schedule missing")?;
        if stored.display != entry.display || stored.day_of_week != -1 || stored.display_name != Some("Probe schedule".into()) || stored.control_method != Some("auto".into()) || stored.display_index != Some(0) { return Err("schedule round-trip mismatch".into()); }
        let active = self.get_active_schedules_full("10:00", 1).await?;
        if !active.iter().any(|item| item.id == id) { return Err("active schedule lookup mismatch".into()); }
        if self.get_active_schedules_full("19:00", 1).await?.iter().any(|item| item.id == id) { return Err("inactive schedule was returned".into()); }
        let mut updated = stored; updated.end_time = "18:00".into(); self.update_schedule(&updated).await?;
        if self.get_schedule(&id).await?.ok_or("updated schedule missing")?.end_time != "18:00" { return Err("schedule update mismatch".into()); }
        let run_id = self.log_schedule_run(&id, "switch_layout", "HDMI-A-1", true, Some("ok"), "2026-01-01T08:00:00Z").await?;
        if self.list_schedule_runs(0).await?.is_empty() { return Err("schedule run listing mismatch".into()); }
        self.client.execute("DELETE FROM schedule_runs WHERE id=$1", &[&run_id]).await.map_err(|error| format!("clean schedule run: {error}"))?;
        self.delete_schedule(&id).await?;
        self.client.execute("DELETE FROM layouts WHERE id=$1", &[&layout_id]).await.map_err(|error| format!("clean schedule probe layout: {error}"))?;
        if self.get_schedule(&id).await?.is_some() { return Err("schedule cleanup failed".into()); }
        Ok(())
    }
    pub async fn list_display_overrides(&self) -> Result<std::collections::HashMap<String, (u32, u32)>, String> {
        self.client.query("SELECT display,width,height FROM display_overrides", &[]).await.map(|rows| rows.into_iter().map(|row| (row.get(0), (row.get::<_, i32>(1) as u32, row.get::<_, i32>(2) as u32))).collect()).map_err(|error| format!("list display overrides: {error}"))
    }

    pub async fn get_display_overrides(&self) -> Result<std::collections::HashMap<String, (u32, u32)>, String> {
        self.list_display_overrides().await
    }

    pub async fn set_display_override(&self, display: &str, width: u32, height: u32) -> Result<(), String> {
        let width = width as i32; let height = height as i32;
        self.client.execute("INSERT INTO display_overrides (display,width,height) VALUES ($1,$2,$3) ON CONFLICT (display) DO UPDATE SET width=EXCLUDED.width,height=EXCLUDED.height", &[&display, &width, &height]).await.map(|_| ()).map_err(|error| format!("set display override: {error}"))
    }

    pub async fn clear_display_override(&self, display: &str) -> Result<(), String> {
        self.client.execute("DELETE FROM display_overrides WHERE display=$1", &[&display]).await.map(|_| ()).map_err(|error| format!("clear display override: {error}"))
    }

    pub async fn list_templates(&self) -> Result<Vec<TemplateMeta>, String> {
        self.client.query("SELECT id,name,category,is_builtin,created_at FROM templates ORDER BY name", &[]).await.map(|rows| rows.into_iter().map(|row| TemplateMeta { id: row.get(0), name: row.get(1), category: row.get(2), is_builtin: row.get(3), created_at: row.get(4) }).collect()).map_err(|error| format!("list templates: {error}"))
    }

    pub async fn get_template(&self, id: &str) -> Result<Option<String>, String> {
        self.client.query_opt("SELECT layout_json FROM templates WHERE id=$1", &[&id]).await.map(|row| row.map(|row| row.get(0))).map_err(|error| format!("get template: {error}"))
    }

    pub async fn save_template(&self, id: &str, name: &str, category: &str, is_builtin: bool, layout_json: &str, created_at: &str) -> Result<(), String> {
        self.client.execute("INSERT INTO templates (id,name,category,is_builtin,layout_json,created_at) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name,category=EXCLUDED.category,is_builtin=EXCLUDED.is_builtin,layout_json=EXCLUDED.layout_json", &[&id, &name, &category, &is_builtin, &layout_json, &created_at]).await.map(|_| ()).map_err(|error| format!("save template: {error}"))
    }

    pub async fn delete_template(&self, id: &str) -> Result<(), String> {
        self.client.execute("DELETE FROM templates WHERE id=$1 AND is_builtin=false", &[&id]).await.map(|_| ()).map_err(|error| format!("delete template: {error}"))
    }

    pub async fn display_override_template_round_trip_probe(&self) -> Result<(), String> {
        let display = format!("__postgres_override_probe_{}", Uuid::new_v4());
        self.set_display_override(&display, 1366, 768).await?;
        if self.list_display_overrides().await?.get(&display) != Some(&(1366, 768)) { return Err("display override round-trip mismatch".into()); }
        if self.get_display_overrides().await?.get(&display) != Some(&(1366, 768)) { return Err("display override compatibility lookup mismatch".into()); }
        self.clear_display_override(&display).await?;
        let template = format!("__postgres_template_probe_{}", Uuid::new_v4());
        self.save_template(&template, "PostgreSQL template probe", "probe", false, "{\"widgets\":[]}", "2026-01-01T00:00:00Z").await?;
        if self.get_template(&template).await?.as_deref() != Some("{\"widgets\":[]}") || !self.list_templates().await?.iter().any(|item| item.id == template) { return Err("template round-trip mismatch".into()); }
        self.delete_template(&template).await?;
        if self.get_template(&template).await?.is_some() { return Err("template cleanup failed".into()); }
        Ok(())
    }
    pub async fn list_widget_presets(&self, widget_type: Option<&str>) -> Result<Vec<WidgetPresetMeta>, String> {
        let rows = match widget_type {
            Some(widget_type) => self.client.query("SELECT id,name,widget_type,is_builtin,created_at,updated_at FROM widget_presets WHERE widget_type=$1 ORDER BY name", &[&widget_type]).await,
            None => self.client.query("SELECT id,name,widget_type,is_builtin,created_at,updated_at FROM widget_presets ORDER BY widget_type,name", &[]).await,
        }.map_err(|error| format!("list widget presets: {error}"))?;
        Ok(rows.into_iter().map(|row| WidgetPresetMeta { id: row.get(0), name: row.get(1), widget_type: row.get(2), is_builtin: row.get(3), created_at: row.get(4), updated_at: row.get(5) }).collect())
    }

    pub async fn get_widget_preset(&self, id: &str) -> Result<Option<WidgetPreset>, String> {
        let row = self.client.query_opt("SELECT id,name,widget_type,props_json,created_at,updated_at FROM widget_presets WHERE id=$1", &[&id]).await.map_err(|error| format!("get widget preset: {error}"))?;
        let Some(row) = row else { return Ok(None); };
        let assets = self.client.query("SELECT id,kind,key,url FROM widget_preset_assets WHERE preset_id=$1 ORDER BY kind,key", &[&id]).await.map_err(|error| format!("get widget preset assets: {error}"))?;
        let mut props: serde_json::Value = serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(3)).unwrap_or(serde_json::json!({}));
        if row.get::<_, String>(2) == "weather" {
            let mut icons = serde_json::Map::new();
            for asset in &assets { if asset.get::<_, String>(1) == "weather_icon" { icons.insert(asset.get(2), serde_json::Value::String(asset.get(3))); } }
            if !icons.is_empty() { props["custom_icons"] = serde_json::Value::Object(icons); }
        }
        Ok(Some(WidgetPreset { id: row.get(0), name: row.get(1), widget_type: row.get(2), props_json: serde_json::to_string(&props).map_err(|error| format!("encode widget preset props: {error}"))?, created_at: row.get(4), updated_at: row.get(5), assets: assets.into_iter().map(|asset| WidgetPresetAsset { id: asset.get(0), preset_id: id.to_string(), kind: asset.get(1), key: asset.get(2), url: asset.get(3) }).collect() }))
    }

    pub async fn create_widget_preset(&self, id: &str, name: &str, widget_type: &str, props_json: &str, now: &str) -> Result<(), String> {
        self.client.execute("INSERT INTO widget_presets (id,name,widget_type,props_json,created_at,updated_at,is_builtin) VALUES ($1,$2,$3,$4,$5,$6,false)", &[&id, &name, &widget_type, &props_json, &now, &now]).await.map(|_| ()).map_err(|error| format!("create widget preset: {error}"))
    }

    pub async fn update_widget_preset(&self, id: &str, name: &str, widget_type: &str, props_json: &str, now: &str) -> Result<(), String> {
        if self.client.execute("UPDATE widget_presets SET name=$1,widget_type=$2,props_json=$3,updated_at=$4 WHERE id=$5", &[&name,&widget_type,&props_json,&now,&id]).await.map_err(|error| format!("update widget preset: {error}"))? == 0 { return Err("Widget preset not found".into()); }
        Ok(())
    }
    pub async fn delete_widget_preset(&self, id: &str) -> Result<(), String> {
        if self.client.execute("DELETE FROM widget_presets WHERE id=$1 AND is_builtin=false", &[&id]).await.map_err(|error| format!("delete widget preset: {error}"))? == 0 { return Err("Preset not found or is built-in".into()); }
        Ok(())
    }

    pub async fn list_widget_preset_assets(&self, preset_id: &str) -> Result<Vec<WidgetPresetAsset>, String> {
        self.client.query("SELECT id,kind,key,url FROM widget_preset_assets WHERE preset_id=$1 ORDER BY kind,key", &[&preset_id]).await.map(|rows| rows.into_iter().map(|row| WidgetPresetAsset { id: row.get(0), preset_id: preset_id.to_string(), kind: row.get(1), key: row.get(2), url: row.get(3) }).collect()).map_err(|error| format!("list widget preset assets: {error}"))
    }

    pub async fn upsert_widget_preset_asset(&self, asset: &WidgetPresetAsset) -> Result<(), String> {
        self.client.execute("INSERT INTO widget_preset_assets (id,preset_id,kind,key,url) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (id) DO UPDATE SET kind=EXCLUDED.kind,key=EXCLUDED.key,url=EXCLUDED.url", &[&asset.id, &asset.preset_id, &asset.kind, &asset.key, &asset.url]).await.map(|_| ()).map_err(|error| format!("upsert widget preset asset: {error}"))
    }

    pub async fn delete_widget_preset_asset(&self, preset_id: &str, id: &str) -> Result<bool, String> {
        self.client.execute("DELETE FROM widget_preset_assets WHERE preset_id=$1 AND id=$2", &[&preset_id, &id]).await.map(|rows| rows > 0).map_err(|error| format!("delete widget preset asset: {error}"))
    }

    pub async fn widget_preset_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_preset_probe_{}", Uuid::new_v4());
        self.create_widget_preset(&id, "PostgreSQL preset probe", "weather", "{\"units\":\"metric\"}", "2026-01-01T00:00:00Z").await?;
        let asset = WidgetPresetAsset { id: format!("{id}-asset"), preset_id: id.clone(), kind: "weather_icon".into(), key: "0".into(), url: "/icons/sunny.svg".into() };
        self.upsert_widget_preset_asset(&asset).await?;
        self.update_widget_preset(&id, "Updated PostgreSQL preset", "weather", "{\"units\":\"imperial\"}", "2026-01-02T00:00:00Z").await?;
        if self.get_widget_preset(&id).await?.ok_or("updated widget preset missing")?.name != "Updated PostgreSQL preset" { return Err("widget preset update mismatch".into()); }
        if !self.delete_widget_preset_asset(&id, &asset.id).await? { return Err("widget preset asset delete failed".into()); }
        self.delete_widget_preset(&id).await?;
        if self.get_widget_preset(&id).await?.is_some() { return Err("widget preset cleanup failed".into()); }
        Ok(())
    }


    pub async fn list_layout_sets(&self) -> Result<Vec<LayoutSetSummary>, String> {
        self.client.query("SELECT ls.id, ls.name, ls.shuffle, ls.dwell_secs, ls.created_at, ls.updated_at, count(lse.id) FROM layout_sets ls LEFT JOIN layout_set_entries lse ON ls.id=lse.layout_set_id GROUP BY ls.id ORDER BY ls.name", &[]).await.map(|rows| rows.into_iter().map(|row| LayoutSetSummary { id: row.get(0), name: row.get(1), shuffle: row.get(2), dwell_secs: row.get::<_, i32>(3) as u32, item_count: row.get::<_, i64>(6) as u32, created_at: row.get(4), updated_at: row.get(5) }).collect()).map_err(|error| format!("list layout sets: {error}"))
    }

    pub async fn get_layout_set(&self, id: &str) -> Result<Option<LayoutSetWithItems>, String> {
        let row = self.client.query_opt("SELECT id,name,shuffle,dwell_secs,created_at,updated_at FROM layout_sets WHERE id=$1", &[&id]).await.map_err(|error| format!("get layout set: {error}"))?;
        let Some(row) = row else { return Ok(None); };
        let entries = self.client.query("SELECT id,layout_id,order_index,interval_secs FROM layout_set_entries WHERE layout_set_id=$1 ORDER BY order_index", &[&id]).await.map_err(|error| format!("get layout set entries: {error}"))?;
        Ok(Some(LayoutSetWithItems { id: row.get(0), name: row.get(1), shuffle: row.get(2), dwell_secs: row.get::<_, i32>(3) as u32, items: entries.into_iter().map(|entry| LayoutSetEntry { id: entry.get(0), layout_set_id: id.to_string(), layout_id: entry.get(1), position: entry.get::<_, i32>(2) as u32, interval_secs: entry.get::<_, Option<i32>>(3).map(|value| value as u32), transition: None }).collect(), created_at: row.get(4), updated_at: row.get(5) }))
    }

    pub async fn create_layout_set(&self, set: &LayoutSetWithItems) -> Result<(), String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin layout set creation: {error}"))?;
        transaction
            .execute(
                "INSERT INTO layout_sets (id,name,shuffle,dwell_secs,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6)",
                &[&set.id, &set.name, &set.shuffle, &(set.dwell_secs as i32), &set.created_at, &set.updated_at],
            )
            .await
            .map_err(|error| format!("create layout set: {error}"))?;
        for (position, item) in set.items.iter().enumerate() {
            let position = position as i32;
            let interval = item.interval_secs.map(|value| value as i32);
            transaction
                .execute(
                    "INSERT INTO layout_set_entries (id,layout_set_id,layout_id,order_index,interval_secs) VALUES ($1,$2,$3,$4,$5)",
                    &[&item.id, &set.id, &item.layout_id, &position, &interval],
                )
                .await
                .map_err(|error| format!("create layout set entry: {error}"))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit layout set creation: {error}"))?;
        Ok(())
    }

    pub async fn update_layout_set(&self, set: &LayoutSetWithItems) -> Result<(), String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin layout set update: {error}"))?;
        if transaction
            .execute(
                "UPDATE layout_sets SET name=$2,shuffle=$3,dwell_secs=$4,updated_at=$5 WHERE id=$1",
                &[&set.id, &set.name, &set.shuffle, &(set.dwell_secs as i32), &set.updated_at],
            )
            .await
            .map_err(|error| format!("update layout set: {error}"))?
            == 0
        {
            transaction
                .rollback()
                .await
                .map_err(|error| format!("rollback missing layout set update: {error}"))?;
            return Err("Layout set not found".into());
        }
        transaction
            .execute("DELETE FROM layout_set_entries WHERE layout_set_id=$1", &[&set.id])
            .await
            .map_err(|error| format!("replace layout set entries: {error}"))?;
        for (position, item) in set.items.iter().enumerate() {
            let position = position as i32;
            let interval = item.interval_secs.map(|value| value as i32);
            transaction
                .execute(
                    "INSERT INTO layout_set_entries (id,layout_set_id,layout_id,order_index,interval_secs) VALUES ($1,$2,$3,$4,$5)",
                    &[&item.id, &set.id, &item.layout_id, &position, &interval],
                )
                .await
                .map_err(|error| format!("update layout set entry: {error}"))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit layout set update: {error}"))?;
        Ok(())
    }

    pub async fn delete_layout_set(&self, id: &str) -> Result<(), String> {
        if self.client.execute("DELETE FROM layout_sets WHERE id=$1", &[&id]).await.map_err(|error| format!("delete layout set: {error}"))? == 0 { return Err("Layout set not found".into()); }
        Ok(())
    }

    pub async fn get_layout_sets_for_layouts(&self, layout_ids: &[String]) -> Result<std::collections::HashMap<String, LayoutSetWithItems>, String> {
        if layout_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let rows = self
            .client
            .query(
                "SELECT ls.id,ls.name,ls.shuffle,ls.dwell_secs,ls.created_at,ls.updated_at,lse.layout_id,lse.order_index,lse.interval_secs FROM layout_sets ls JOIN layout_set_entries lse ON lse.layout_set_id=ls.id WHERE EXISTS (SELECT 1 FROM layout_set_entries assigned WHERE assigned.layout_set_id=ls.id AND assigned.layout_id=ANY($1::text[]))",
                &[&layout_ids],
            )
            .await
            .map_err(|error| format!("get layout sets for layouts: {error}"))?;
        let mut sets = std::collections::HashMap::<String, LayoutSetWithItems>::new();
        for row in rows {
            let set_id: String = row.get(0);
            let layout_id: String = row.get(6);
            let entry = LayoutSetEntry {
                id: format!("{set_id}-{layout_id}"),
                layout_set_id: set_id.clone(),
                layout_id,
                position: row.get::<_, i32>(7) as u32,
                interval_secs: row.get::<_, Option<i32>>(8).map(|value| value as u32),
                transition: None,
            };
            sets.entry(set_id.clone()).or_insert_with(|| LayoutSetWithItems {
                id: set_id,
                name: row.get(1),
                shuffle: row.get(2),
                dwell_secs: row.get::<_, i32>(3) as u32,
                items: Vec::new(),
                created_at: row.get(4),
                updated_at: row.get(5),
            }).items.push(entry);
        }
        let requested: std::collections::HashSet<&str> =
            layout_ids.iter().map(String::as_str).collect();
        let mut result = std::collections::HashMap::new();
        for set in sets.values_mut() {
            set.items.sort_by_key(|entry| entry.position);
            for entry in &set.items {
                if requested.contains(entry.layout_id.as_str()) {
                    result.entry(entry.layout_id.clone()).or_insert_with(|| set.clone());
                }
            }
        }
        Ok(result)
    }

    pub async fn add_layout_set_entry(&self, layout_set_id: &str, entry: &AddLayoutSetEntry) -> Result<(), String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client.transaction().await.map_err(|error| format!("begin layout set entry add: {error}"))?;
        let id = Uuid::new_v4().to_string();
        let position = entry.position as i32;
        let interval = entry.interval_secs.unwrap_or(30) as i32;
        transaction.execute(
            "INSERT INTO layout_set_entries (id,layout_set_id,layout_id,order_index,interval_secs) VALUES ($1,$2,$3,$4,$5)",
            &[&id, &layout_set_id, &entry.layout_id, &position, &interval],
        ).await.map_err(|error| format!("add layout set entry: {error}"))?;
        let now = chrono::Utc::now().to_rfc3339();
        transaction.execute("UPDATE layout_sets SET updated_at=$1 WHERE id=$2", &[&now, &layout_set_id]).await.map_err(|error| format!("update layout set entry timestamp: {error}"))?;
        transaction.commit().await.map_err(|error| format!("commit layout set entry add: {error}"))?;
        Ok(())
    }

    pub async fn update_layout_set_entry(&self, entry_id: &str, update: &UpdateLayoutSetEntry) -> Result<(), String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client.transaction().await.map_err(|error| format!("begin layout set entry update: {error}"))?;
        if let Some(position) = update.position {
            transaction.execute("UPDATE layout_set_entries SET order_index=$1 WHERE id=$2", &[&(position as i32), &entry_id]).await.map_err(|error| format!("update layout set entry position: {error}"))?;
        }
        if let Some(interval_secs) = update.interval_secs {
            transaction.execute("UPDATE layout_set_entries SET interval_secs=$1 WHERE id=$2", &[&(interval_secs as i32), &entry_id]).await.map_err(|error| format!("update layout set entry interval: {error}"))?;
        }
        transaction.commit().await.map_err(|error| format!("commit layout set entry update: {error}"))?;
        Ok(())
    }

    pub async fn delete_layout_set_entry(&self, entry_id: &str) -> Result<(), String> {
        self.client.execute("DELETE FROM layout_set_entries WHERE id=$1", &[&entry_id]).await.map(|_| ()).map_err(|error| format!("delete layout set entry: {error}"))
    }

    pub async fn replace_layout_set_entries(&self, layout_set_id: &str, entries: Vec<LayoutSetEntry>) -> Result<(), String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client.transaction().await.map_err(|error| format!("begin layout set entry replacement: {error}"))?;
        transaction.execute("DELETE FROM layout_set_entries WHERE layout_set_id=$1", &[&layout_set_id]).await.map_err(|error| format!("delete replaced layout set entries: {error}"))?;
        for item in &entries {
            let position = item.position as i32;
            let interval = item.interval_secs.unwrap_or(30) as i32;
            transaction.execute("INSERT INTO layout_set_entries (id,layout_set_id,layout_id,order_index,interval_secs) VALUES ($1,$2,$3,$4,$5)", &[&item.id, &layout_set_id, &item.layout_id, &position, &interval]).await.map_err(|error| format!("insert replacement layout set entry: {error}"))?;
        }
        let now = chrono::Utc::now().to_rfc3339();
        transaction.execute("UPDATE layout_sets SET updated_at=$1 WHERE id=$2", &[&now, &layout_set_id]).await.map_err(|error| format!("update replaced layout set timestamp: {error}"))?;
        transaction.commit().await.map_err(|error| format!("commit layout set entry replacement: {error}"))?;
        Ok(())
    }

    pub async fn layout_set_round_trip_probe(&self) -> Result<(), String> {
        let layout_id = format!("__postgres_set_layout_probe_{}", Uuid::new_v4());
        let layout_id_two = format!("__postgres_set_layout_probe_two_{}", Uuid::new_v4());
        let set_id = format!("__postgres_layout_set_probe_{}", Uuid::new_v4());
        self.client.execute("INSERT INTO layouts (id,name,created_at,updated_at) VALUES ($1,$2,$3,$4)", &[&layout_id, &"Set item probe", &"2026-01-01T00:00:00Z", &"2026-01-01T00:00:00Z"]).await.map_err(|error| format!("create set probe layout: {error}"))?;
        self.client.execute("INSERT INTO layouts (id,name,created_at,updated_at) VALUES ($1,$2,$3,$4)", &[&layout_id_two, &"Set item probe two", &"2026-01-01T00:00:00Z", &"2026-01-01T00:00:00Z"]).await.map_err(|error| format!("create second set probe layout: {error}"))?;
        let set = LayoutSetWithItems { id: set_id.clone(), name: "PostgreSQL layout set probe".into(), shuffle: true, dwell_secs: 45, items: vec![LayoutSetEntry { id: format!("{set_id}-entry"), layout_set_id: set_id.clone(), layout_id: layout_id.clone(), position: 0, interval_secs: Some(12), transition: None }], created_at: "2026-01-01T00:00:00Z".into(), updated_at: "2026-01-01T00:00:00Z".into() };
        self.create_layout_set(&set).await?;
        let stored = self.get_layout_set(&set_id).await?.ok_or("layout set missing")?;
        if stored.name != set.name || !stored.shuffle || stored.dwell_secs != 45 || stored.items.len() != 1 || stored.items[0].interval_secs != Some(12) { return Err("layout set round-trip mismatch".into()); }
        self.add_layout_set_entry(&set_id, &AddLayoutSetEntry { layout_id: layout_id_two.clone(), position: 1, interval_secs: Some(18) }).await?;
        let with_entry = self.get_layout_set(&set_id).await?.ok_or("layout set entry missing")?;
        if with_entry.items.len() != 2 { return Err("layout set entry add mismatch".into()); }
        self.update_layout_set_entry(&with_entry.items[1].id, &UpdateLayoutSetEntry { position: Some(3), interval_secs: Some(20) }).await?;
        let indexed = self.get_layout_set(&set_id).await?.ok_or("updated layout set entry missing")?;
        if indexed.items.iter().find(|item| item.id == with_entry.items[1].id).and_then(|item| item.interval_secs) != Some(20) { return Err("layout set entry update mismatch".into()); }
        let matching = self.get_layout_sets_for_layouts(&[layout_id.clone(), layout_id_two.clone()]).await?;
        if !matching.contains_key(&layout_id) || !matching.contains_key(&layout_id_two) { return Err("layout sets by layout lookup mismatch".into()); }
        self.replace_layout_set_entries(&set_id, vec![indexed.items[0].clone()]).await?;
        if self.get_layout_set(&set_id).await?.ok_or("replaced layout set missing")?.items.len() != 1 { return Err("layout set entry replacement mismatch".into()); }
        let mut updated = stored; updated.name = "Updated set".into(); updated.items[0].interval_secs = Some(20); self.update_layout_set(&updated).await?;
        if self.get_layout_set(&set_id).await?.ok_or("updated set missing")?.name != "Updated set" { return Err("layout set update mismatch".into()); }
        self.delete_layout_set(&set_id).await?;
        self.client.execute("DELETE FROM layouts WHERE id=$1", &[&layout_id]).await.map_err(|error| format!("clean set probe layout: {error}"))?;
        self.client.execute("DELETE FROM layouts WHERE id=$1", &[&layout_id_two]).await.map_err(|error| format!("clean second set probe layout: {error}"))?;
        if !self.list_layout_sets().await?.iter().all(|item| item.id != set_id) { return Err("layout set cleanup failed".into()); }
        Ok(())
    }
    async fn load_layout_widgets(&self, layout_id: &str) -> Result<Vec<crate::models::Widget>, String> {
        let rows = self.client.query("SELECT id, widget_type, x, y, w, h, z, props FROM widgets WHERE layout_id=$1 ORDER BY z ASC, id ASC", &[&layout_id]).await.map_err(|error| format!("load layout widgets: {error}"))?;
        rows.into_iter().map(|row| {
            let widget_type: String = row.get(1);
            let props: serde_json::Value = serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(7)).unwrap_or(serde_json::Value::Object(Default::default()));
            serde_json::from_value(serde_json::json!({"id": row.get::<_, String>(0), "type": widget_type, "x": row.get::<_, f64>(2), "y": row.get::<_, f64>(3), "w": row.get::<_, f64>(4), "h": row.get::<_, f64>(5), "z": row.get::<_, i32>(6), "props": props})).map_err(|error| format!("decode layout widget: {error}"))
        }).collect()
    }

    fn layout_from_row(row: &tokio_postgres::Row) -> Result<Layout, String> {
        let items = serde_json::from_str(&row.get::<_, String>(9)).unwrap_or_default();
        Ok(Layout { id: row.get(0), name: row.get(1), interactive: row.get(2), page_type: row.get(3), timing_mode: row.get(4), duration: row.get::<_, i32>(5).max(1), shuffle: row.get(6), canvas_orientation: row.get(7), folder_id: row.get(8), items, created_at: row.get(10), updated_at: row.get(11), widgets: Vec::new() })
    }

    pub async fn list_layouts(&self) -> Result<Vec<Layout>, String> {
        let rows = self.client.query("SELECT id, name, interactive, page_type, timing_mode, duration, shuffle, canvas_orientation, folder_id, items, created_at, updated_at FROM layouts ORDER BY updated_at DESC", &[]).await.map_err(|error| format!("list layouts: {error}"))?;
        let mut layouts = Vec::with_capacity(rows.len());
        for row in rows {
            let mut layout = Self::layout_from_row(&row)?;
            layout.widgets = self.load_layout_widgets(&layout.id).await?;
            layouts.push(layout);
        }
        Ok(layouts)
    }

    pub async fn get_layout(&self, id: &str) -> Result<Option<Layout>, String> {
        let row = self.client.query_opt("SELECT id, name, interactive, page_type, timing_mode, duration, shuffle, canvas_orientation, folder_id, items, created_at, updated_at FROM layouts WHERE id=$1", &[&id]).await.map_err(|error| format!("get layout: {error}"))?;
        match row {
            Some(row) => { let mut layout = Self::layout_from_row(&row)?; layout.widgets = self.load_layout_widgets(&layout.id).await?; Ok(Some(layout)) }
            None => Ok(None),
        }
    }

    pub async fn save_layout(&self, layout: &Layout) -> Result<(), String> {
        let items = serde_json::to_string(&layout.items).map_err(|error| format!("encode layout items: {error}"))?;
        self.client.execute("INSERT INTO layouts (id,name,interactive,page_type,timing_mode,duration,shuffle,canvas_orientation,folder_id,items,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name, interactive=EXCLUDED.interactive, page_type=EXCLUDED.page_type, timing_mode=EXCLUDED.timing_mode, duration=EXCLUDED.duration, shuffle=EXCLUDED.shuffle, canvas_orientation=EXCLUDED.canvas_orientation, folder_id=EXCLUDED.folder_id, items=EXCLUDED.items, updated_at=EXCLUDED.updated_at", &[&layout.id, &layout.name, &layout.interactive, &layout.page_type, &layout.timing_mode, &layout.duration, &layout.shuffle, &layout.canvas_orientation, &layout.folder_id, &items, &layout.created_at, &layout.updated_at]).await.map_err(|error| format!("save layout: {error}"))?;
        self.client.execute("DELETE FROM widgets WHERE layout_id=$1", &[&layout.id]).await.map_err(|error| format!("replace layout widgets: {error}"))?;
        for widget in &layout.widgets {
            let widget_type = serde_json::to_value(&widget.widget_type).map_err(|error| format!("encode widget type: {error}"))?.as_str().unwrap_or("text").to_string();
            let props = widget.props.to_string();
            let x = widget.x as f64; let y = widget.y as f64; let w = widget.w as f64; let h = widget.h as f64;
            self.client.execute("INSERT INTO widgets (id,layout_id,widget_type,x,y,w,h,z,props) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&widget.id, &layout.id, &widget_type, &x, &y, &w, &h, &widget.z, &props]).await.map_err(|error| format!("save layout widget: {error}"))?;
        }
        Ok(())
    }

    pub async fn publish_layout_revision(&self, layout: &Layout, published_by: Option<&str>) -> Result<LayoutRevision, String> {
        let revision: i64 = self.client
            .query_one("SELECT COALESCE(MAX(revision), 0) + 1 FROM layout_revisions WHERE layout_id=$1", &[&layout.id])
            .await
            .map_err(|error| format!("next layout revision: {error}"))?
            .get(0);
        let created_at = chrono::Utc::now().to_rfc3339();
        let snapshot = serde_json::to_string(layout).map_err(|error| format!("encode layout revision: {error}"))?;
        self.client.execute("INSERT INTO layout_revisions (layout_id, revision, snapshot, created_at, published_by) VALUES ($1,$2,$3,$4,$5)", &[&layout.id, &revision, &snapshot, &created_at, &published_by]).await.map_err(|error| format!("publish layout revision: {error}"))?;
        Ok(LayoutRevision { layout_id: layout.id.clone(), revision, snapshot: layout.clone(), created_at, published_by: published_by.map(str::to_string) })
    }

    pub async fn list_layout_revisions(&self, layout_id: &str) -> Result<Vec<LayoutRevision>, String> {
        let rows = self.client.query("SELECT revision, snapshot, created_at, published_by FROM layout_revisions WHERE layout_id=$1 ORDER BY revision DESC", &[&layout_id]).await.map_err(|error| format!("list layout revisions: {error}"))?;
        rows.into_iter().map(|row| {
            let snapshot: String = row.get(1);
            Ok(LayoutRevision { layout_id: layout_id.to_string(), revision: row.get(0), snapshot: serde_json::from_str(&snapshot).map_err(|error| format!("decode layout revision: {error}"))?, created_at: row.get(2), published_by: row.get(3) })
        }).collect()
    }

    pub async fn get_published_layout(&self, layout_id: &str) -> Result<Option<Layout>, String> {
        let row = self.client.query_opt("SELECT snapshot FROM layout_revisions WHERE layout_id=$1 ORDER BY revision DESC LIMIT 1", &[&layout_id]).await.map_err(|error| format!("get published layout: {error}"))?;
        row.map(|row| serde_json::from_str(row.get::<_, &str>(0)).map_err(|error| format!("decode published layout: {error}"))).transpose()
    }

    pub async fn delete_layout(&self, id: &str) -> Result<(), String> {
        self.client.execute("UPDATE client_displays SET assigned_layout_id=NULL WHERE assigned_layout_id=$1", &[&id]).await.map_err(|error| format!("clear display layout: {error}"))?;
        self.client.execute("UPDATE clients SET active_layout_id=NULL, assigned_layout_id=NULL WHERE active_layout_id=$1 OR assigned_layout_id=$1", &[&id]).await.map_err(|error| format!("clear client layout: {error}"))?;
        if self.client.execute("DELETE FROM layouts WHERE id=$1", &[&id]).await.map_err(|error| format!("delete layout: {error}"))? == 0 { return Err("Layout not found".into()); }
        Ok(())
    }

    pub async fn layout_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_layout_probe_{}", Uuid::new_v4());
        let widget = Widget { id: "probe-widget".into(), widget_type: WidgetType::Text, x: 0.1, y: 0.2, w: 0.5, h: 0.3, z: 1, props: serde_json::json!({"text": "hello"}) };
        let layout = Layout { id: id.clone(), name: "PostgreSQL layout probe".into(), widgets: vec![widget], interactive: true, canvas_orientation: "portrait".into(), folder_id: None, page_type: "single".into(), timing_mode: "global".into(), duration: 15, shuffle: false, items: vec![], created_at: "2026-01-01T00:00:00Z".into(), updated_at: "2026-01-01T00:00:00Z".into() };
        self.save_layout(&layout).await?;
        let stored = self.get_layout(&id).await?.ok_or("layout missing")?;
        if stored.name != layout.name || !stored.interactive || stored.canvas_orientation != "portrait" || stored.widgets.len() != 1 || stored.widgets[0].widget_type != WidgetType::Text || stored.widgets[0].props["text"] != "hello" || !self.list_layouts().await?.iter().any(|item| item.id == id) { return Err("layout round-trip mismatch".into()); }
        self.delete_layout(&id).await?;
        if self.get_layout(&id).await?.is_some() { return Err("layout cleanup failed".into()); }
        Ok(())
    }
    pub async fn list_layout_folders(&self) -> Result<Vec<LayoutFolder>, String> {
        self.client
            .query("SELECT id, name, parent_id, created_at, updated_at FROM layout_folders ORDER BY COALESCE(parent_id, ''), name ASC", &[])
            .await
            .map(|rows| rows.into_iter().map(|row| LayoutFolder { id: row.get(0), name: row.get(1), parent_id: row.get(2), created_at: row.get(3), updated_at: row.get(4) }).collect())
            .map_err(|error| format!("list layout folders: {error}"))
    }

    pub async fn get_layout_folder(&self, id: &str) -> Result<Option<LayoutFolder>, String> {
        self.client
            .query_opt("SELECT id, name, parent_id, created_at, updated_at FROM layout_folders WHERE id=$1", &[&id])
            .await
            .map(|row| row.map(|row| LayoutFolder { id: row.get(0), name: row.get(1), parent_id: row.get(2), created_at: row.get(3), updated_at: row.get(4) }))
            .map_err(|error| format!("get layout folder: {error}"))
    }

    pub async fn save_layout_folder(&self, folder: &LayoutFolder) -> Result<(), String> {
        let name = folder.name.trim();
        if name.is_empty() || name.len() > 120 {
            return Err("Folder name must be between 1 and 120 characters".into());
        }
        if let Some(parent_id) = folder.parent_id.as_deref() {
            if parent_id == folder.id || !self.client.query_one("SELECT EXISTS(SELECT 1 FROM layout_folders WHERE id=$1)", &[&parent_id]).await.map_err(|error| format!("validate parent folder: {error}"))?.get::<_, bool>(0) {
                return Err("Invalid parent folder".into());
            }
        }
        self.client
            .execute("INSERT INTO layout_folders (id,name,parent_id,created_at,updated_at) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name, parent_id=EXCLUDED.parent_id, updated_at=EXCLUDED.updated_at", &[&folder.id, &name, &folder.parent_id, &folder.created_at, &folder.updated_at])
            .await
            .map(|_| ())
            .map_err(|error| format!("save layout folder: {error}"))
    }

    pub async fn delete_layout_folder(&self, id: &str) -> Result<(), String> {
        let counts = self.client.query_one("SELECT (SELECT count(*) FROM layouts WHERE folder_id=$1), (SELECT count(*) FROM layout_folders WHERE parent_id=$1)", &[&id]).await.map_err(|error| format!("check layout folder: {error}"))?;
        if counts.get::<_, i64>(0) > 0 || counts.get::<_, i64>(1) > 0 {
            return Err("Folder must be empty before it can be deleted".into());
        }
        if self.client.execute("DELETE FROM layout_folders WHERE id=$1", &[&id]).await.map_err(|error| format!("delete layout folder: {error}"))? == 0 {
            return Err("Folder not found".into());
        }
        Ok(())
    }

    pub async fn move_layout_to_folder(&self, layout_id: &str, folder_id: Option<&str>) -> Result<(), String> {
        if let Some(folder_id) = folder_id {
            if !self.client.query_one("SELECT EXISTS(SELECT 1 FROM layout_folders WHERE id=$1)", &[&folder_id]).await.map_err(|error| format!("validate layout folder: {error}"))?.get::<_, bool>(0) {
                return Err("Folder not found".into());
            }
        }
        if self.client.execute("UPDATE layouts SET folder_id=$2, updated_at=$3 WHERE id=$1", &[&layout_id, &folder_id, &chrono::Utc::now().to_rfc3339()]).await.map_err(|error| format!("move layout to folder: {error}"))? == 0 {
            return Err("Layout not found".into());
        }
        Ok(())
    }

    pub async fn layout_folder_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_folder_probe_{}", Uuid::new_v4());
        let folder = LayoutFolder { id: id.clone(), name: " Probe Folder ".into(), parent_id: None, created_at: "2026-01-01T00:00:00Z".into(), updated_at: "2026-01-01T00:00:00Z".into() };
        self.save_layout_folder(&folder).await?;
        let stored = self.get_layout_folder(&id).await?.ok_or("layout folder missing")?;
        if stored.name != "Probe Folder" || self.list_layout_folders().await?.iter().all(|item| item.id != id) {
            return Err("layout folder round-trip mismatch".into());
        }
        self.delete_layout_folder(&id).await?;
        if self.get_layout_folder(&id).await?.is_some() {
            return Err("layout folder cleanup failed".into());
        }
        Ok(())
    }

    pub async fn upsert_news_feed(
        &self,
        url: &str,
        title: &str,
        headlines_json: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), String> {
        let now = chrono::Utc::now().to_rfc3339();
        self.client
            .execute(
                "INSERT INTO news_feeds (url, title, headlines_json, last_fetched_at, last_error, etag, last_modified) VALUES ($1,$2,$3,$4,NULL,$5,$6) ON CONFLICT (url) DO UPDATE SET title=EXCLUDED.title, headlines_json=EXCLUDED.headlines_json, last_fetched_at=EXCLUDED.last_fetched_at, last_error=NULL, etag=EXCLUDED.etag, last_modified=EXCLUDED.last_modified",
                &[&url, &title, &headlines_json, &now, &etag, &last_modified],
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("upsert news feed: {error}"))
    }

    pub async fn record_news_feed_error(&self, url: &str, error: &str) -> Result<(), String> {
        let now = chrono::Utc::now().to_rfc3339();
        self.client
            .execute(
                "INSERT INTO news_feeds (url, last_fetched_at, last_error) VALUES ($1,$2,$3) ON CONFLICT (url) DO UPDATE SET last_fetched_at=EXCLUDED.last_fetched_at, last_error=EXCLUDED.last_error",
                &[&url, &now, &error],
            )
            .await
            .map(|_| ())
            .map_err(|db_error| format!("record news feed error: {db_error}"))
    }

    pub async fn get_news_feed(&self, url: &str) -> Result<Option<NewsFeed>, String> {
        self.client
            .query_opt("SELECT url, title, headlines_json, last_fetched_at, last_error, etag, last_modified FROM news_feeds WHERE url=$1", &[&url])
            .await
            .map(|row| row.map(|row| NewsFeed { url: row.get(0), title: row.get(1), headlines_json: row.get(2), last_fetched_at: row.get(3), last_error: row.get(4), etag: row.get(5), last_modified: row.get(6) }))
            .map_err(|error| format!("get news feed: {error}"))
    }

    pub async fn ensure_news_feed_registered(&self, url: &str) -> Result<Option<NewsFeed>, String> {
        self.client.execute("INSERT INTO news_feeds (url,last_fetched_at) VALUES ($1,'') ON CONFLICT (url) DO NOTHING", &[&url]).await.map_err(|error| format!("ensure news feed registered: {error}"))?;
        self.get_news_feed(url).await
    }
    pub async fn list_news_feeds(&self) -> Result<Vec<NewsFeed>, String> {
        self.client
            .query("SELECT url, title, headlines_json, last_fetched_at, last_error, etag, last_modified FROM news_feeds ORDER BY url ASC", &[])
            .await
            .map(|rows| rows.into_iter().map(|row| NewsFeed { url: row.get(0), title: row.get(1), headlines_json: row.get(2), last_fetched_at: row.get(3), last_error: row.get(4), etag: row.get(5), last_modified: row.get(6) }).collect())
            .map_err(|error| format!("list news feeds: {error}"))
    }

    pub async fn mark_news_feed_fetched(&self, url: &str, fetched_at: &str) -> Result<(), String> {
        self.client
            .execute("UPDATE news_feeds SET last_fetched_at=$1, last_error=NULL WHERE url=$2", &[&fetched_at, &url])
            .await
            .map(|_| ())
            .map_err(|error| format!("mark news feed fetched: {error}"))
    }

    pub async fn get_news_feeds_needing_refresh(&self, max_age_secs: i64) -> Result<Vec<NewsFeed>, String> {
        let max_age_secs = max_age_secs as f64;
        self.client
            .query(
                "SELECT url, title, headlines_json, last_fetched_at, last_error, etag, last_modified FROM news_feeds WHERE last_fetched_at='' OR last_fetched_at::timestamptz < now() - ($1::double precision * interval '1 second') ORDER BY url ASC",
                &[&max_age_secs],
            )
            .await
            .map(|rows| rows.into_iter().map(|row| NewsFeed { url: row.get(0), title: row.get(1), headlines_json: row.get(2), last_fetched_at: row.get(3), last_error: row.get(4), etag: row.get(5), last_modified: row.get(6) }).collect())
            .map_err(|error| format!("list news feeds needing refresh: {error}"))
    }

    pub async fn news_feed_round_trip_probe(&self) -> Result<(), String> {
        let url = format!("https://news-probe.invalid/{}.xml", Uuid::new_v4());
        if self.ensure_news_feed_registered(&url).await?.is_none() { return Err("news feed registration failed".into()); }
        self.upsert_news_feed(&url, "Probe feed", "[{\"title\":\"hello\"}]", Some("etag-1"), Some("modified-1")).await?;
        let feed = self.get_news_feed(&url).await?.ok_or("news feed missing")?;
        if feed.title != "Probe feed" || feed.headlines_json != "[{\"title\":\"hello\"}]" || feed.etag.as_deref() != Some("etag-1") || feed.last_error.is_some() {
            return Err("news feed round-trip mismatch".into());
        }
        self.record_news_feed_error(&url, "temporary failure").await?;
        if self.get_news_feed(&url).await?.ok_or("news error feed missing")?.last_error.as_deref() != Some("temporary failure") {
            return Err("news feed error round-trip mismatch".into());
        }
        self.mark_news_feed_fetched(&url, &chrono::Utc::now().to_rfc3339()).await?;
        if !self.get_news_feeds_needing_refresh(3600).await?.iter().all(|feed| feed.url != url) {
            return Err("fresh news feed was incorrectly marked for refresh".into());
        }
        if self.client.execute("DELETE FROM news_feeds WHERE url=$1", &[&url]).await.map_err(|error| format!("clean news feed probe: {error}"))? != 1 {
            return Err("news feed cleanup affected the wrong number of rows".into());
        }
        Ok(())
    }

    pub async fn list_registered_clients(
        &self,
    ) -> Result<
        Vec<(
            String, String, String, String, Option<String>, Option<i64>, Option<f32>, Option<i64>, Option<i64>,
            Option<i64>, Option<i64>, Option<String>, Option<String>, String, i64, Option<String>, Option<String>,
            Option<i64>, Option<i64>, Option<String>, Option<String>, i64,
        )>,
        String,
    > {
        self.client
            .query(
                "SELECT id, name, address, role, version, uptime_seconds, cpu_usage, memory_used, memory_total, disk_used, disk_total, active_layout_id, assigned_layout_id, last_seen, enrolled, last_command_result, last_command_at, max_displays, installer_detected_max_displays, touch_binding, theme, is_virtual FROM clients ORDER BY last_seen DESC",
                &[],
            )
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get(0),
                            row.get(1),
                            row.get(2),
                            row.get(3),
                            row.get(4),
                            row.get(5),
                            row.get::<_, Option<f64>>(6).map(|value| value as f32),
                            row.get(7),
                            row.get(8),
                            row.get(9),
                            row.get(10),
                            row.get(11),
                            row.get(12),
                            row.get(13),
                            if row.get::<_, bool>(14) { 1 } else { 0 },
                            row.get(15),
                            row.get(16),
                            row.get::<_, Option<i32>>(17).map(|value| value as i64),
                            row.get::<_, Option<i32>>(18).map(|value| value as i64),
                            row.get(19),
                            row.get(20),
                            if row.get::<_, bool>(21) { 1 } else { 0 },
                        )
                    })
                    .collect()
            })
            .map_err(|error| format!("list registered clients: {error}"))
    }

    pub async fn registered_clients_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_registered_clients_probe_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: id.clone(), name: "PostgreSQL registered clients probe".into(), address: "127.0.0.1".into(), role: "player".into(),
            version: Some("probe".into()), uptime_seconds: Some(7), cpu_usage: Some(3.5), memory_used: Some(10), memory_total: Some(20),
            disk_used: Some(30), disk_total: Some(40), active_layout_id: Some("active-layout".into()), last_seen: "2026-01-01T00:00:00Z".into(),
            last_command_result: Some("ok".into()), last_command_at: Some("2026-01-01T00:00:00Z".into()), installer_detected_max_displays: Some(2),
            touch_binding: Some("{}".into()), theme: Some("dark".into()), max_displays: Some(2),
            client_auth_token_hash: None,
        };
        self.register_client(&client, &[]).await?;
        self.set_client_assignment(&id, Some("assigned-layout")).await?;
        let rows = self.list_registered_clients().await?;
        let row = rows.into_iter().find(|row| row.0 == id).ok_or("registered client missing from list")?;
        if row.1 != client.name || row.2 != client.address || row.4.as_deref() != Some("probe") || row.5 != Some(7) || row.6 != Some(3.5) || row.11.as_deref() != Some("active-layout") || row.12.as_deref() != Some("assigned-layout") || row.14 != 0 || row.17 != Some(2) || row.18 != Some(2) || row.21 != 0 {
            return Err("registered client list round-trip mismatch".into());
        }
        let deleted = self.client.execute("DELETE FROM clients WHERE id=$1", &[&id]).await
            .map_err(|error| format!("clean registered clients probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one deleted registered clients probe row, got {deleted}"));
        }
        Ok(())
    }

    pub async fn get_display_control(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<Option<(Option<String>, Option<String>, Option<i64>, Option<i64>, Option<f64>, bool, String, String)>, String> {
        let display_index = display_index as i32;
        Ok(self.client
            .query_opt(
                "SELECT connector, label, width, height, refresh_rate, enabled, orientation, control_method FROM client_displays WHERE client_id=$1 AND display_index=$2",
                &[&client_id, &display_index],
            )
            .await
            .map_err(|error| format!("get display control: {error}"))?
            .map(|row| {
                (
                    row.get(0),
                    row.get(1),
                    row.get::<_, Option<i32>>(2).map(|value| value as i64),
                    row.get::<_, Option<i32>>(3).map(|value| value as i64),
                    row.get::<_, Option<f64>>(4),
                    row.get(5),
                    row.get(6),
                    row.get(7),
                )
            }))
    }

    pub async fn set_display_control_method(
        &self,
        client_id: &str,
        display_index: i64,
        method: &str,
    ) -> Result<usize, String> {
        let display_index = display_index as i32;
        self.client
            .execute(
                "UPDATE client_displays SET control_method=$1 WHERE client_id=$2 AND display_index=$3",
                &[&method, &client_id, &display_index],
            )
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("set display control method: {error}"))
    }

    pub async fn set_display_orientation(
        &self,
        client_id: &str,
        display_index: i64,
        orientation: &str,
    ) -> Result<usize, String> {
        let display_index = display_index as i32;
        self.client
            .execute(
                "UPDATE client_displays SET orientation=$1 WHERE client_id=$2 AND display_index=$3",
                &[&orientation, &client_id, &display_index],
            )
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("set display orientation: {error}"))
    }

    pub async fn get_display_assignment_and_last_seen(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<(Option<String>, String), String> {
        let display_index = display_index as i32;
        self.client
            .query_opt(
                "SELECT assigned_layout_id, last_seen FROM client_displays WHERE client_id=$1 AND display_index=$2",
                &[&client_id, &display_index],
            )
            .await
            .map_err(|error| format!("get display assignment: {error}"))?
            .map(|row| (row.get(0), row.get(1)))
            .ok_or_else(|| "Display not found".to_string())
    }

    pub async fn display_control_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_display_control_probe_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: id.clone(), name: "PostgreSQL display control probe".into(), address: "127.0.0.1".into(), role: "player".into(),
            version: None, uptime_seconds: None, cpu_usage: None, memory_used: None, memory_total: None,
            disk_used: None, disk_total: None, active_layout_id: None, last_seen: "1970-01-01T00:00:00Z".into(),
            last_command_result: None, last_command_at: None, installer_detected_max_displays: None,
            touch_binding: None, theme: None, max_displays: None,
            client_auth_token_hash: None,
        };
        self.register_client(&client, &[ClientDisplayRegistration { display_index: 0, connector: Some("HDMI-A-1".into()), label: Some("Probe".into()), width: Some(1920), height: Some(1080), refresh_rate: Some(60.0), enabled: Some(true), last_error: None }]).await?;
        let control = self.get_display_control(&id, 0).await?.ok_or("display control probe row missing")?;
        if control.0.as_deref() != Some("HDMI-A-1") || control.5 != true || control.6 != "normal" || control.7 != "auto" {
            return Err("display control initial state mismatch".into());
        }
        if self.set_display_control_method(&id, 0, "ddc").await? != 1 || self.set_display_orientation(&id, 0, "portrait").await? != 1 {
            return Err("display control update affected the wrong number of rows".into());
        }
        let updated = self.get_display_control(&id, 0).await?.ok_or("updated display control probe row missing")?;
        if updated.6 != "portrait" || updated.7 != "ddc" {
            return Err("display control update round-trip mismatch".into());
        }
        let (_, last_seen) = self.get_display_assignment_and_last_seen(&id, 0).await?;
        if last_seen != "1970-01-01T00:00:00Z" {
            return Err("display last-seen round-trip mismatch".into());
        }
        let deleted = self.client.execute("DELETE FROM clients WHERE id=$1", &[&id]).await
            .map_err(|error| format!("clean display control probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one deleted display control probe row, got {deleted}"));
        }
        Ok(())
    }


    pub async fn delete_client_history(&self, client_id: &str) -> Result<usize, String> {
        self.client
            .execute("DELETE FROM client_heartbeats WHERE client_id=$1", &[&client_id])
            .await
            .map(|rows| rows as usize)
            .map_err(|error| format!("delete client heartbeat history: {error}"))
    }

    pub async fn count_stale_clients(&self, threshold_secs: i64) -> Result<i64, String> {
        let threshold_secs = threshold_secs.max(0) as f64;
        self.client
            .query_one(
                "SELECT count(*) FROM clients WHERE is_virtual=false AND NULLIF(last_seen,'') IS NOT NULL AND NULLIF(last_seen,'')::timestamptz < now() - ($1::double precision * interval '1 second')",
                &[&threshold_secs],
            )
            .await
            .map(|row| row.get(0))
            .map_err(|error| format!("count stale clients: {error}"))
    }

    pub async fn stale_client_count_round_trip_probe(&self) -> Result<(), String> {
        let stale_id = format!("__postgres_stale_probe_{}", Uuid::new_v4());
        let fresh_id = format!("__postgres_fresh_probe_{}", Uuid::new_v4());
        let virtual_id = format!("__postgres_virtual_stale_probe_{}", Uuid::new_v4());
        let fresh_time = chrono::Utc::now().to_rfc3339();
        for (id, name, last_seen, is_virtual) in [
            (&stale_id, "Stale probe", "2020-01-01T00:00:00Z".to_string(), false),
            (&fresh_id, "Fresh probe", fresh_time, false),
            (&virtual_id, "Virtual stale probe", "2020-01-01T00:00:00Z".to_string(), true),
        ] {
            self.client
                .execute(
                    "INSERT INTO clients (id,name,address,role,last_seen,is_virtual) VALUES ($1,$2,$3,$4,$5,$6)",
                    &[id, &name, &"127.0.0.1", &"player", &last_seen, &is_virtual],
                )
                .await
                .map_err(|error| format!("create stale-client probe row: {error}"))?;
        }
        if self.count_stale_clients(300).await? != 1 {
            return Err("stale-client count mismatch".into());
        }
        let deleted = self.client
            .execute("DELETE FROM clients WHERE id IN ($1,$2,$3)", &[&stale_id, &fresh_id, &virtual_id])
            .await
            .map_err(|error| format!("clean stale-client probe: {error}"))?;
        if deleted != 3 {
            return Err(format!("expected three stale-client cleanup rows, got {deleted}"));
        }
        Ok(())
    }

    pub async fn list_client_heartbeats(
        &self,
        client_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, Option<f64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>)>, String> {
        self.client
            .query(
                "SELECT recorded_at, cpu_usage, memory_used, memory_total, disk_used, disk_total, uptime_seconds FROM client_heartbeats WHERE client_id=$1 ORDER BY recorded_at DESC LIMIT $2",
                &[&client_id, &limit],
            )
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get(0), row.get(1), row.get(2), row.get(3), row.get(4), row.get(5), row.get(6),
                        )
                    })
                    .collect()
            })
            .map_err(|error| format!("list client heartbeats: {error}"))
    }

    pub async fn delete_client(&self, client_id: &str) -> Result<bool, String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin client delete: {error}"))?;
        transaction
            .execute("DELETE FROM client_displays WHERE client_id=$1", &[&client_id])
            .await
            .map_err(|error| format!("delete client displays: {error}"))?;
        transaction
            .execute("DELETE FROM client_commands WHERE client_id=$1", &[&client_id])
            .await
            .map_err(|error| format!("delete client commands: {error}"))?;
        transaction
            .execute("DELETE FROM client_command_cooldowns WHERE client_id=$1", &[&client_id])
            .await
            .map_err(|error| format!("delete client command cooldowns: {error}"))?;
        let deleted = transaction
            .execute("DELETE FROM clients WHERE id=$1", &[&client_id])
            .await
            .map_err(|error| format!("delete client: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit client delete: {error}"))?;
        Ok(deleted > 0)
    }

    pub async fn delete_virtual_client(&self, client_id: &str) -> Result<bool, String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin virtual client delete: {error}"))?;
        let is_virtual = transaction
            .query_opt("SELECT is_virtual FROM clients WHERE id=$1", &[&client_id])
            .await
            .map_err(|error| format!("read virtual client flag: {error}"))?
            .map(|row| row.get::<_, bool>(0));
        if is_virtual != Some(true) {
            transaction
                .rollback()
                .await
                .map_err(|error| format!("rollback non-virtual client delete: {error}"))?;
            return Ok(false);
        }
        transaction
            .execute("DELETE FROM client_displays WHERE client_id=$1", &[&client_id])
            .await
            .map_err(|error| format!("delete virtual client displays: {error}"))?;
        transaction
            .execute("DELETE FROM client_commands WHERE client_id=$1", &[&client_id])
            .await
            .map_err(|error| format!("delete virtual client commands: {error}"))?;
        transaction
            .execute("DELETE FROM client_command_cooldowns WHERE client_id=$1", &[&client_id])
            .await
            .map_err(|error| format!("delete virtual client command cooldowns: {error}"))?;
        let deleted = transaction
            .execute("DELETE FROM clients WHERE id=$1", &[&client_id])
            .await
            .map_err(|error| format!("delete virtual client: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit virtual client delete: {error}"))?;
        Ok(deleted > 0)
    }

    pub async fn client_history_lifecycle_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_history_probe_{}", Uuid::new_v4());
        let make_client = |last_seen: &str, cpu: f64| ClientRegistration {
            id: id.clone(), name: "PostgreSQL history probe".into(), address: "127.0.0.1".into(), role: "player".into(),
            version: None, uptime_seconds: Some(1), cpu_usage: Some(cpu), memory_used: Some(10), memory_total: Some(20),
            disk_used: Some(30), disk_total: Some(40), active_layout_id: None, last_seen: last_seen.into(),
            last_command_result: None, last_command_at: None, installer_detected_max_displays: None,
            touch_binding: None, theme: None, max_displays: None,
            client_auth_token_hash: None,
        };
        self.register_client(&make_client("2026-01-01T00:00:00Z", 1.0), &[]).await?;
        self.register_client(&make_client("2026-01-01T00:01:00Z", 2.0), &[]).await?;
        if self.list_client_heartbeats(&id, 1).await?.len() != 1 {
            return Err("heartbeat limit was not applied".into());
        }
        if self.delete_client_history(&id).await? != 2 || !self.list_client_heartbeats(&id, 10).await?.is_empty() {
            return Err("client heartbeat history lifecycle mismatch".into());
        }
        if self.delete_virtual_client(&id).await? {
            return Err("non-virtual client was deleted by virtual delete".into());
        }
        if !self.delete_client(&id).await? || self.client_exists(&id).await? {
            return Err("client deletion lifecycle mismatch".into());
        }
        Ok(())
    }

    pub async fn list_client_displays(
        &self,
        client_id: &str,
    ) -> Result<(bool, Option<String>, Vec<ClientDisplayRecord>), String> {
        let client_row = self
            .client
            .query_opt("SELECT is_virtual, assigned_layout_id FROM clients WHERE id=$1", &[&client_id])
            .await
            .map_err(|error| format!("read client display owner: {error}"))?;
        let (is_virtual, client_layout_id) = client_row
            .map(|row| (row.get::<_, bool>(0), row.get::<_, Option<String>>(1)))
            .unwrap_or((false, None));
        let rows = self
            .client
            .query(
                "SELECT client_id, display_index, connector, label, width, height, refresh_rate, enabled, assigned_layout_id, active_layout_id, last_seen, last_error, orientation, control_method FROM client_displays WHERE client_id=$1 ORDER BY display_index ASC",
                &[&client_id],
            )
            .await
            .map_err(|error| format!("list client displays: {error}"))?;
        let displays = rows
            .into_iter()
            .map(|row| ClientDisplayRecord {
                client_id: row.get(0),
                display_index: row.get::<_, i32>(1) as i64,
                connector: row.get(2),
                label: row.get(3),
                width: row.get::<_, Option<i32>>(4).map(|value| value as i64),
                height: row.get::<_, Option<i32>>(5).map(|value| value as i64),
                refresh_rate: row.get::<_, Option<f64>>(6).map(|value| value as f32),
                enabled: row.get(7),
                assigned_layout_id: row.get::<_, Option<String>>(8).or(client_layout_id.clone()),
                active_layout_id: row.get(9),
                last_seen: row.get(10),
                last_error: row.get(11),
                orientation: row.get(12),
                control_method: row.get(13),
            })
            .collect();
        Ok((is_virtual, client_layout_id, displays))
    }

    pub async fn list_client_admin_users(
        &self,
        client_id: &str,
    ) -> Result<Option<(bool, Vec<ClientAdminUser>)>, String> {
        let Some(row) = self
            .client
            .query_opt("SELECT enrolled FROM clients WHERE id=$1", &[&client_id])
            .await
            .map_err(|error| format!("read client enrollment: {error}"))?
        else {
            return Ok(None);
        };
        let enrolled: bool = row.get(0);
        let rows = self
            .client
            .query(
                "SELECT username, display_name, role FROM users WHERE active=true AND role IN ('admin','layout-manager') ORDER BY role ASC, username ASC",
                &[],
            )
            .await
            .map_err(|error| format!("list client admin users: {error}"))?;
        let users = rows
            .into_iter()
            .map(|row| ClientAdminUser {
                username: row.get(0),
                display_name: row.get(1),
                role: row.get(2),
            })
            .collect();
        Ok(Some((enrolled, users)))
    }

    pub async fn client_admin_users_round_trip_probe(&self) -> Result<(), String> {
        let client_id = format!("__postgres_admin_client_probe_{}", Uuid::new_v4());
        let user_id = format!("__postgres_admin_user_probe_{}", Uuid::new_v4());
        let username = format!("__postgres_admin_probe_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: client_id.clone(), name: "PostgreSQL admin probe".into(), address: "127.0.0.1".into(), role: "player".into(),
            version: None, uptime_seconds: None, cpu_usage: None, memory_used: None, memory_total: None,
            disk_used: None, disk_total: None, active_layout_id: None, last_seen: "1970-01-01T00:00:00Z".into(),
            last_command_result: None, last_command_at: None, installer_detected_max_displays: None,
            touch_binding: None, theme: None, max_displays: None,
            client_auth_token_hash: None,
        };
        self.register_client(&client, &[]).await?;
        self.client
            .execute(
                "INSERT INTO users (id, username, display_name, role, password_hash, active, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,true,$6,$6)",
                &[&user_id, &username, &"Probe Admin", &"admin", &"probe-hash", &"2026-01-01T00:00:00Z"],
            )
            .await
            .map_err(|error| format!("create admin user probe: {error}"))?;
        let listing = self.list_client_admin_users(&client_id).await?.ok_or("admin client listing missing")?;
        if listing.0 || listing.1.len() != 1 || listing.1[0].username != username || listing.1[0].role != "admin" {
            return Err("client admin user listing mismatch".into());
        }
        if !self.set_client_enrolled(&client_id).await? || !self.list_client_admin_users(&client_id).await?.ok_or("enrolled admin listing missing")?.0 {
            return Err("client enrollment was not reflected in admin listing".into());
        }
        if self.list_client_admin_users("missing-admin-client").await?.is_some() {
            return Err("missing admin client returned a listing".into());
        }
        let deleted_user = self.client.execute("DELETE FROM users WHERE id=$1", &[&user_id]).await
            .map_err(|error| format!("clean admin user probe: {error}"))?;
        let deleted_client = self.client.execute("DELETE FROM clients WHERE id=$1", &[&client_id]).await
            .map_err(|error| format!("clean admin client probe: {error}"))?;
        if deleted_user != 1 || deleted_client != 1 {
            return Err(format!("admin probe cleanup mismatch: users={deleted_user} clients={deleted_client}"));
        }
        Ok(())
    }


    pub async fn client_display_listing_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_display_listing_probe_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: id.clone(), name: "PostgreSQL display listing probe".into(), address: "127.0.0.1".into(), role: "player".into(),
            version: None, uptime_seconds: None, cpu_usage: None, memory_used: None, memory_total: None,
            disk_used: None, disk_total: None, active_layout_id: Some("client-layout".into()), last_seen: "1970-01-01T00:00:00Z".into(),
            last_command_result: None, last_command_at: None, installer_detected_max_displays: None,
            touch_binding: None, theme: None, max_displays: None,
            client_auth_token_hash: None,
        };
        self.register_client(&client, &[ClientDisplayRegistration { display_index: 0, connector: Some("HDMI-A-1".into()), label: Some("Probe".into()), width: Some(1920), height: Some(1080), refresh_rate: Some(60.0), enabled: Some(true), last_error: None }]).await?;
        if !self.set_client_assignment(&id, Some("client-layout")).await? {
            return Err("setting display listing client assignment affected no rows".into());
        }
        let (is_virtual, layout, displays) = self.list_client_displays(&id).await?;
        if is_virtual || layout.as_deref() != Some("client-layout") || displays.len() != 1 || displays[0].display_index != 0 || displays[0].width != Some(1920) || displays[0].refresh_rate != Some(60.0) {
            return Err("client display listing round-trip mismatch".into());
        }
        let unknown = self.list_client_displays("missing-client").await?;
        if unknown.2.len() != 0 || unknown.0 {
            return Err("missing client display listing mismatch".into());
        }
        let deleted = self.client.execute("DELETE FROM clients WHERE id=$1", &[&id]).await
            .map_err(|error| format!("clean display listing probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one deleted display listing probe row, got {deleted}"));
        }
        Ok(())
    }

    pub async fn register_client(
        &self,
        client: &ClientRegistration,
        displays: &[ClientDisplayRegistration],
    ) -> Result<(), String> {
        let mut transaction_client = connect().await?;
        let transaction = transaction_client
            .transaction()
            .await
            .map_err(|error| format!("begin client registration: {error}"))?;
        let version = client.version.as_deref();
        let active_layout_id = client.active_layout_id.as_deref();
        let last_command_result = client.last_command_result.as_deref();
        let last_command_at = client.last_command_at.as_deref();
        let touch_binding = client.touch_binding.as_deref();
        let theme = client.theme.as_deref();
        let client_auth_token_hash = client.client_auth_token_hash.as_deref();
        let installer_detected_max_displays = client
            .installer_detected_max_displays
            .map(|value| value as i32);
        let client_params: [&(dyn ToSql + Sync); 19] = [
            &client.id,
            &client.name,
            &client.address,
            &client.role,
            &version,
            &client.uptime_seconds,
            &client.cpu_usage,
            &client.memory_used,
            &client.memory_total,
            &client.disk_used,
            &client.disk_total,
            &active_layout_id,
            &client.last_seen,
            &last_command_result,
            &last_command_at,
            &installer_detected_max_displays,
            &touch_binding,
            &theme,
            &client_auth_token_hash,
        ];
        transaction
            .execute(
                "INSERT INTO clients (id, name, address, role, version, uptime_seconds, cpu_usage, memory_used, memory_total, disk_used, disk_total, active_layout_id, last_seen, enrolled, last_command_result, last_command_at, installer_detected_max_displays, touch_binding, theme, client_auth_token_hash) VALUES ($1,$2,$3,$4,$5::text,$6,$7,$8,$9,$10,$11,$12::text,$13,false,$14::text,$15::text,$16,$17::text,$18::text,$19::text) ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name, address=EXCLUDED.address, role=EXCLUDED.role, version=EXCLUDED.version, uptime_seconds=EXCLUDED.uptime_seconds, cpu_usage=EXCLUDED.cpu_usage, memory_used=EXCLUDED.memory_used, memory_total=EXCLUDED.memory_total, disk_used=EXCLUDED.disk_used, disk_total=EXCLUDED.disk_total, active_layout_id=EXCLUDED.active_layout_id, last_seen=EXCLUDED.last_seen, enrolled=clients.enrolled, last_command_result=COALESCE(EXCLUDED.last_command_result, clients.last_command_result), last_command_at=COALESCE(EXCLUDED.last_command_at, clients.last_command_at), installer_detected_max_displays=COALESCE(EXCLUDED.installer_detected_max_displays, clients.installer_detected_max_displays), touch_binding=COALESCE(EXCLUDED.touch_binding, clients.touch_binding), theme=EXCLUDED.theme, client_auth_token_hash=COALESCE(EXCLUDED.client_auth_token_hash, clients.client_auth_token_hash)",
                &client_params,
            )
            .await
            .map_err(|error| format!("register client: {error}"))?;
        if let Some(max_displays) = client.max_displays.map(|value| value as i32) {
            transaction
                .execute(
                    "UPDATE clients SET max_displays=COALESCE(max_displays,$1) WHERE id=$2",
                    &[
                        &max_displays as &(dyn ToSql + Sync),
                        &client.id as &(dyn ToSql + Sync),
                    ],
                )
                .await
                .map_err(|error| format!("preserve client display limit: {error}"))?;
        }
        let heartbeat_params: [&(dyn ToSql + Sync); 8] = [
            &client.id,
            &client.last_seen,
            &client.cpu_usage,
            &client.memory_used,
            &client.memory_total,
            &client.disk_used,
            &client.disk_total,
            &client.uptime_seconds,
        ];
        transaction
            .execute(
                "INSERT INTO client_heartbeats (client_id, recorded_at, cpu_usage, memory_used, memory_total, disk_used, disk_total, uptime_seconds) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
                &heartbeat_params,
            )
            .await
            .map_err(|error| format!("record client heartbeat: {error}"))?;
        transaction
            .execute(
                "DELETE FROM client_heartbeats WHERE client_id=$1 AND id NOT IN (SELECT id FROM client_heartbeats WHERE client_id=$1 ORDER BY recorded_at DESC LIMIT 200)",
                &[&client.id],
            )
            .await
            .map_err(|error| format!("trim client heartbeats: {error}"))?;
        for display in displays {
            let refresh_rate = display.refresh_rate.map(f64::from);
            let enabled = display.enabled;
            let display_index = display.display_index as i32;
            let width = display.width.map(|value| value as i32);
            let height = display.height.map(|value| value as i32);
            let display_params: [&(dyn ToSql + Sync); 10] = [
                &client.id,
                &display_index,
                &display.connector,
                &display.label,
                &width,
                &height,
                &refresh_rate,
                &enabled,
                &client.last_seen,
                &display.last_error,
            ];
            transaction
                .execute(
                    "INSERT INTO client_displays (client_id, display_index, connector, label, width, height, refresh_rate, enabled, last_seen, last_error) VALUES ($1,$2,$3,$4,$5,$6,$7,COALESCE($8,true),$9,$10) ON CONFLICT (client_id, display_index) DO UPDATE SET connector=EXCLUDED.connector, label=EXCLUDED.label, width=COALESCE(EXCLUDED.width,client_displays.width), height=COALESCE(EXCLUDED.height,client_displays.height), refresh_rate=COALESCE(EXCLUDED.refresh_rate,client_displays.refresh_rate), enabled=COALESCE(EXCLUDED.enabled,client_displays.enabled), last_seen=EXCLUDED.last_seen, last_error=EXCLUDED.last_error",
                    &display_params,
                )
                .await
                .map_err(|error| format!("register client display: {error}"))?;
        }
        transaction
            .batch_execute("COMMIT")
            .await
            .map_err(|error| format!("commit client registration: {error}"))
    }

    pub async fn client_registration_round_trip_probe(&self) -> Result<(), String> {
        let id = format!("__postgres_client_probe_{}", Uuid::new_v4());
        let client = ClientRegistration {
            id: id.clone(),
            name: "PostgreSQL client probe".into(),
            address: "127.0.0.1".into(),
            role: "player".into(),
            version: Some("probe".into()),
            uptime_seconds: Some(42),
            cpu_usage: Some(12.5),
            memory_used: Some(100),
            memory_total: Some(200),
            disk_used: Some(300),
            disk_total: Some(400),
            active_layout_id: None,
            last_seen: "1970-01-01T00:00:00Z".into(),
            last_command_result: None,
            last_command_at: Some("1970-01-01T00:00:00Z".into()),
            installer_detected_max_displays: Some(2),
            touch_binding: None,
            theme: Some("dark".into()),
            max_displays: Some(2),
            client_auth_token_hash: None,
        };
        let display = ClientDisplayRegistration {
            display_index: 0,
            connector: Some("HDMI-A-1".into()),
            label: Some("Probe display".into()),
            width: Some(1920),
            height: Some(1080),
            refresh_rate: Some(60.0),
            enabled: Some(true),
            last_error: None,
        };
        self.register_client(&client, &[display]).await?;
        let row = self
            .client
            .query_one(
                "SELECT c.name, c.enrolled, c.max_displays, (SELECT count(*) FROM client_heartbeats WHERE client_id=c.id), (SELECT count(*) FROM client_displays WHERE client_id=c.id) FROM clients c WHERE c.id=$1",
                &[&id],
            )
            .await
            .map_err(|error| format!("read client registration probe: {error}"))?;
        let name: String = row.get(0);
        let enrolled: bool = row.get(1);
        let max_displays: Option<i32> = row.get(2);
        let heartbeat_count: i64 = row.get(3);
        let display_count: i64 = row.get(4);
        if name != client.name || enrolled || max_displays != Some(2) || heartbeat_count != 1 || display_count != 1 {
            return Err("client registration round-trip mismatch".into());
        }
        let deleted = self
            .client
            .execute("DELETE FROM clients WHERE id=$1", &[&id])
            .await
            .map_err(|error| format!("clean client registration probe: {error}"))?;
        if deleted != 1 {
            return Err(format!("expected one deleted client probe row, got {deleted}"));
        }
        Ok(())
    }

    pub async fn backup_round_trip_probe(&self) -> Result<(), String> {
        let generated_name = format!("PostgreSQL generated backup {}", Uuid::new_v4());
        self.create_backup(&generated_name, "/var/tmp/generated-postgres-probe.tar.gz", 42).await?;
        let generated = self.list_backups().await?.into_iter().find(|item| item.name == generated_name).ok_or("generated backup missing")?;
        if generated.size_bytes != 42 || self.get_backup_file_path(&generated.id).await? != "/var/tmp/generated-postgres-probe.tar.gz" {
            return Err("generated backup round-trip mismatch".into());
        }
        if self.delete_backup(&generated.id).await? != 1 { return Err("generated backup cleanup mismatch".into()); }
        let id = format!("__postgres_probe_{}", Uuid::new_v4());
        let backup = Backup {
            id: id.clone(),
            name: "PostgreSQL probe".into(),
            created_at: "1970-01-01T00:00:00Z".into(),
            file_path: "/var/tmp/postgres-probe.tar.gz".into(),
            size_bytes: 1,
        };
        self.insert_backup(&backup).await?;
        let path = self.get_backup_file_path(&id).await?;
        if path != backup.file_path {
            return Err("backup path round-trip mismatch".into());
        }
        let deleted = self.delete_backup(&id).await?;
        if deleted != 1 {
            return Err(format!("expected one deleted probe row, got {deleted}"));
        }
        Ok(())
    }
}

pub async fn apply_initial_schema() -> Result<(), String> {
    let mut client = connect().await?;
    client
        .batch_execute("CREATE TABLE IF NOT EXISTS schema_migrations (version text PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())")
        .await
        .map_err(|error| format!("PostgreSQL migration ledger failed: {error}"))?;
    let transaction = client
        .transaction()
        .await
        .map_err(|error| format!("begin PostgreSQL migrations: {error}"))?;
    transaction
        .query_one("SELECT pg_advisory_xact_lock(hashtextextended('screentra.schema_migrations', 0))", &[])
        .await
        .map_err(|error| format!("lock PostgreSQL migrations: {error}"))?;
    for (version, sql) in [
        ("0001_initial", INITIAL_SCHEMA),
        ("0002_runtime_columns", RUNTIME_COLUMNS_MIGRATION),
        ("0003_addon_storage", ADDON_STORAGE_MIGRATION),
        ("0004_schedule_display_settings", DISPLAY_SETTINGS_MIGRATION),
    ] {
        let applied = transaction
            .query_opt("SELECT 1 FROM schema_migrations WHERE version=$1", &[&version])
            .await
            .map_err(|error| format!("read migration {version}: {error}"))?
            .is_some();
        if applied {
            continue;
        }
        transaction
            .batch_execute(sql)
            .await
            .map_err(|error| format!("PostgreSQL migration {version} failed: {error}"))?;
        transaction
            .execute("INSERT INTO schema_migrations (version) VALUES ($1) ON CONFLICT (version) DO NOTHING", &[&version])
            .await
            .map_err(|error| format!("record migration {version}: {error}"))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| format!("commit PostgreSQL migrations: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{apply_initial_schema, database_url_from, PostgresStore};

    #[test]
    fn database_url_requires_a_nonempty_value() {
        assert!(database_url_from(None).is_err());
        assert!(database_url_from(Some("   ".into())).is_err());
        assert_eq!(
            database_url_from(Some("postgresql://example.invalid/db".into())).unwrap(),
            "postgresql://example.invalid/db"
        );
    }

    #[tokio::test]
    #[ignore = "requires a disposable PostgreSQL database supplied through SIGNAGE_DATABASE_URL"]
    async fn addon_storage_round_trip_is_namespaced_and_upserted() {
        apply_initial_schema().await.expect("apply PostgreSQL migrations");
        let store = PostgresStore::connect().await.expect("connect to PostgreSQL");
        let addon_id = format!("test.addon.{}", uuid::Uuid::new_v4());
        let first = "{\"enabled\":true}";
        let second = "{\"enabled\":false}";
        store
            .set_addon_storage(&addon_id, "settings", first, "2026-10-15T00:00:00Z")
            .await
            .expect("insert addon storage");
        assert_eq!(
            store
                .get_addon_storage(&addon_id, "settings")
                .await
                .expect("read addon storage"),
            Some(first.to_string())
        );
        store
            .set_addon_storage(&addon_id, "settings", second, "2026-10-15T00:01:00Z")
            .await
            .expect("upsert addon storage");
        store
            .set_addon_storage(&addon_id, "cache", "{\"count\":1}", "2026-10-15T00:02:00Z")
            .await
            .expect("insert second addon storage");
        let other_addon_id = format!("test.other.{}", uuid::Uuid::new_v4());
        store
            .set_addon_storage(&other_addon_id, "settings", "{\"isolated\":true}", "2026-10-15T00:03:00Z")
            .await
            .expect("insert isolated addon storage");
        assert_eq!(
            store
                .get_addon_storage(&addon_id, "settings")
                .await
                .expect("read updated addon storage"),
            Some(second.to_string())
        );
        assert_eq!(
            store
                .get_addon_storage("test.other", "settings")
                .await
                .expect("read isolated addon storage"),
            None
        );
        assert!(
            store
                .delete_addon_storage(&addon_id, "settings")
                .await
                .expect("delete addon storage")
        );
        assert_eq!(
            store
                .get_addon_storage(&other_addon_id, "settings")
                .await
                .expect("read retained addon storage"),
            Some("{\"isolated\":true}".to_string())
        );
        assert_eq!(
            store
                .delete_addon_storage_namespace(&addon_id)
                .await
                .expect("delete addon namespace"),
            1
        );
        assert_eq!(
            store
                .get_addon_storage(&addon_id, "cache")
                .await
                .expect("read deleted addon namespace"),
            None
        );
        assert_eq!(
            store
                .delete_addon_storage_namespace(&addon_id)
                .await
                .expect("delete missing addon namespace"),
            0
        );
    }
}
