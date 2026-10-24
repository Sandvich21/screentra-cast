use crate::models::{
    AddLayoutSetEntry, AuditEvent, Backup, CreateLayoutSet, DisplayAssignment, DisplayState, Layout,
    LayoutFolder, LayoutSetEntry, LayoutSetSummary, LayoutSetWithItems, NewsFeed, ScheduleRun, TemplateMeta,
    UpdateLayoutSet, UpdateLayoutSetEntry, Widget, WidgetPreset, WidgetPresetAsset,
    WidgetPresetMeta, WidgetType,
};
use crate::schedule::ScheduleEntry;
use rusqlite::{params, Connection, OptionalExtension};
use std::{sync::Mutex, time::Duration};


/// Rebuild the `schedules` table so that `layout_id` is nullable.
/// SQLite has no `ALTER TABLE ... DROP NOT NULL`, so we:
///   1. CREATE TABLE schedules_new with the same shape but no NOT NULL on layout_id
///   2. INSERT INTO schedules_new SELECT * FROM schedules
///   3. DROP TABLE schedules
///   4. ALTER TABLE schedules_new RENAME TO schedules
/// Recreating indexes is the caller's responsibility (the `CREATE TABLE
/// IF NOT EXISTS` below re-runs them; the migration runs BEFORE that
/// block, so we also re-create them here). The function is a no-op if
/// the column is already nullable.
fn rebuild_schedules_layout_id_nullable(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    let mut columns = std::collections::HashSet::new();
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('schedules')")?;
    for row in stmt.query_map([], |row| row.get::<_, String>(0))? {
        columns.insert(row?);
    }
    if columns.is_empty() {
        return Ok(());
    }

    let required = [
        "layout_id",
        "updated_at",
        "layout_set_id",
        "action",
        "target_client",
        "power_state",
        "display_name",
        "control_method",
        "display_index",
        "brightness",
        "contrast",
        "end_action",
    ];
    let layout_id_notnull: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT [notnull] FROM pragma_table_info('schedules') WHERE name='layout_id'), 0)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if layout_id_notnull == 0 && required.iter().all(|name| columns.contains(*name)) {
        return Ok(());
    }

    let expr = |name: &str, fallback: &str| {
        if columns.contains(name) {
            name.to_string()
        } else {
            fallback.to_string()
        }
    };
    let sql = format!(
        r#"
        DROP TABLE IF EXISTS schedules_new;
        CREATE TABLE schedules_new (
            id              TEXT PRIMARY KEY,
            display         TEXT NOT NULL,
            layout_id       TEXT,
            day_of_week     INTEGER NOT NULL,
            start_time      TEXT NOT NULL,
            end_time        TEXT NOT NULL,
            created_at      TEXT NOT NULL,
            updated_at      TEXT,
            layout_set_id   TEXT,
            action          TEXT NOT NULL DEFAULT 'switch_layout',
            target_client   TEXT,
            power_state     TEXT,
            display_name    TEXT,
            control_method  TEXT NOT NULL DEFAULT 'auto',
            display_index   INTEGER NOT NULL DEFAULT 0,
            brightness      INTEGER,
            contrast        INTEGER,
            end_action      TEXT NOT NULL DEFAULT 'none',
            FOREIGN KEY(layout_id) REFERENCES layouts(id) ON DELETE SET NULL
        );
        INSERT INTO schedules_new
            (id, display, layout_id, day_of_week, start_time, end_time, created_at, updated_at,
             layout_set_id, action, target_client, power_state, display_name, control_method, display_index, brightness, contrast, end_action)
        SELECT {id}, {display}, {layout_id}, {day_of_week}, {start_time}, {end_time}, {created_at},
               {updated_at}, {layout_set_id}, {action}, {target_client}, {power_state}, {display_name},
               {control_method}, {display_index}, {brightness}, {contrast}, {end_action}
        FROM schedules;
        DROP TABLE schedules;
        ALTER TABLE schedules_new RENAME TO schedules;
        CREATE INDEX IF NOT EXISTS idx_schedules_display ON schedules(display);
        CREATE INDEX IF NOT EXISTS idx_schedules_layout_set_id ON schedules(layout_set_id);
        "#,
        id = expr("id", "lower(hex(randomblob(16)))"),
        display = expr("display", "''"),
        layout_id = expr("layout_id", "NULL"),
        day_of_week = expr("day_of_week", "0"),
        start_time = expr("start_time", "'00:00'"),
        end_time = expr("end_time", "'00:00'"),
        created_at = expr("created_at", "CURRENT_TIMESTAMP"),
        updated_at = expr("updated_at", "NULL"),
        layout_set_id = expr("layout_set_id", "NULL"),
        action = expr("action", "'switch_layout'"),
        target_client = expr("target_client", "NULL"),
        power_state = expr("power_state", "NULL"),
        display_name = expr("display_name", "NULL"),
        control_method = expr("control_method", "'auto'"),
        display_index = expr("display_index", "0"),
        brightness = expr("brightness", "NULL"),
        contrast = expr("contrast", "NULL"),
        end_action = expr("end_action", "'none'"),
    );
    conn.execute_batch(&sql)?;
    Ok(())
}

pub struct Database {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct TableColumnRecord {
    pub name: String,
    pub col_type: String,
    pub notnull: bool,
    pub dflt_value: Option<String>,
    pub pk: bool,
}

#[derive(Debug, Clone)]
pub struct TableQueryResult {
    pub columns: Vec<TableColumnRecord>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub total: i64,
}

#[derive(Debug, Clone)]
pub struct TableExportResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClientRegistration {
    pub id: String,
    pub name: String,
    pub address: String,
    pub role: String,
    pub version: Option<String>,
    pub uptime_seconds: Option<i64>,
    pub cpu_usage: Option<f64>,
    pub memory_used: Option<i64>,
    pub memory_total: Option<i64>,
    pub disk_used: Option<i64>,
    pub disk_total: Option<i64>,
    pub active_layout_id: Option<String>,
    pub last_seen: String,
    pub last_command_result: Option<String>,
    pub last_command_at: Option<String>,
    pub installer_detected_max_displays: Option<i64>,
    pub touch_binding: Option<String>,
    pub theme: Option<String>,
    pub max_displays: Option<i64>,
    /// SHA-256 of the player credential. The raw credential never enters the
    /// repository or database.
    pub client_auth_token_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClientDisplayRegistration {
    pub display_index: i64,
    pub connector: Option<String>,
    pub label: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub refresh_rate: Option<f32>,
    pub enabled: Option<bool>,
    pub last_error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ClientAdminUser {
    pub username: String,
    pub display_name: String,
    pub role: String,
}

#[derive(Debug, Clone)]
pub struct ClientDisplayRecord {
    pub client_id: String,
    pub display_index: i64,
    pub connector: Option<String>,
    pub label: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub refresh_rate: Option<f32>,
    pub enabled: bool,
    pub assigned_layout_id: Option<String>,
    pub active_layout_id: Option<String>,
    pub last_seen: String,
    pub last_error: Option<String>,
    pub orientation: String,
    pub control_method: String,
}

impl Database {
    pub(crate) fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|e| e.to_string())
    }

    pub fn list_users(&self) -> Result<Vec<UserRecord>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT id, username, display_name, role, active, created_at, updated_at, last_login_at FROM users ORDER BY active DESC, role ASC, username ASC LIMIT 10000")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(UserRecord {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    display_name: row.get(2)?,
                    role: row.get(3)?,
                    active: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    last_login_at: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn get_user(&self, id: &str) -> Result<Option<UserRecord>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id, username, display_name, role, active, created_at, updated_at, last_login_at FROM users WHERE id=?1",
            [id],
            |row| {
                Ok(UserRecord {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    display_name: row.get(2)?,
                    role: row.get(3)?,
                    active: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    last_login_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn count_active_admins(&self) -> Result<i64, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT COUNT(*) FROM users WHERE role='admin' AND active=1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())
    }

    pub fn create_user(
        &self,
        id: &str,
        username: &str,
        display_name: &str,
        role: &str,
        password_hash: &str,
        active: bool,
        now: &str,
    ) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO users (id, username, display_name, role, password_hash, active, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, username, display_name, role, password_hash, active as i64, now],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    pub fn create_initial_admin(
        &self,
        id: &str,
        username: &str,
        display_name: &str,
        password_hash: &str,
        now: &str,
    ) -> Result<bool, String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let existing: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM users WHERE role='admin' AND active=1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if existing > 0 {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO users (id, username, display_name, role, password_hash, active, created_at, updated_at) VALUES (?1, ?2, ?3, 'admin', ?4, 1, ?5, ?5)",
            params![id, username, display_name, password_hash, now],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }

    pub fn update_user(
        &self,
        id: &str,
        username: &str,
        display_name: &str,
        role: &str,
        password_hash: Option<&str>,
        active: bool,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn()?;
        let changed = if let Some(password_hash) = password_hash {
            conn.execute(
                "UPDATE users SET username=?1, display_name=?2, role=?3, active=?4, password_hash=?5, updated_at=?6 WHERE id=?7",
                params![username, display_name, role, active as i64, password_hash, now, id],
            )
            .map_err(|e| e.to_string())?
        } else {
            conn.execute(
                "UPDATE users SET username=?1, display_name=?2, role=?3, active=?4, updated_at=?5 WHERE id=?6",
                params![username, display_name, role, active as i64, now, id],
            )
            .map_err(|e| e.to_string())?
        };
        Ok(changed > 0)
    }

    pub fn delete_user(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn()?;
        Ok(conn.execute("DELETE FROM users WHERE id=?1", [id]).map_err(|e| e.to_string())? > 0)
    }

    pub fn find_active_user_credentials(
        &self,
        username: &str,
    ) -> Result<Option<(String, String, String)>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT username, role, password_hash FROM users WHERE lower(username)=lower(?1) AND active=1",
            params![username],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn record_user_login(&self, username: &str, now: &str) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE users SET last_login_at=?1, updated_at=?1 WHERE username=?2",
            params![now, username],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    pub fn get_user_account(
        &self,
        username: &str,
    ) -> Result<Option<(String, String, String, bool, Option<String>)>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT username, display_name, role, active, google_email FROM users WHERE lower(username)=lower(?1)",
            params![username],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn get_active_password_hash(&self, username: &str) -> Result<Option<String>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT password_hash FROM users WHERE lower(username)=lower(?1) AND active=1",
            params![username],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn update_user_password(
        &self,
        username: &str,
        password_hash: &str,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE users SET password_hash=?1, updated_at=?2 WHERE lower(username)=lower(?3)",
            params![password_hash, now, username],
        )
        .map(|rows| rows > 0)
        .map_err(|e| e.to_string())
    }

    pub fn clear_google_binding(&self, username: &str, now: &str) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE users SET google_sub=NULL, google_email=NULL, updated_at=?1 WHERE lower(username)=lower(?2)",
            params![now, username],
        )
        .map(|rows| rows > 0)
        .map_err(|e| e.to_string())
    }

    pub fn bind_google_account(
        &self,
        username: &str,
        google_sub: &str,
        google_email: &str,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE users SET google_sub=?1, google_email=?2, updated_at=?3 WHERE lower(username)=lower(?4)",
            params![google_sub, google_email, now, username],
        )
        .map(|rows| rows > 0)
        .map_err(|e| e.to_string())
    }

    pub fn find_active_google_user(
        &self,
        google_sub: &str,
    ) -> Result<Option<(String, String)>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT username, role FROM users WHERE google_sub=?1 AND active=1",
            params![google_sub],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn get_client_assignment(&self, client_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT assigned_layout_id FROM clients WHERE id = ?1",
            [client_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn set_client_assignment(
        &self,
        client_id: &str,
        layout_id: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE clients SET assigned_layout_id = ?1 WHERE id = ?2",
            params![layout_id, client_id],
        )
        .map(|rows| rows > 0)
        .map_err(|e| e.to_string())
    }

    pub fn bulk_set_client_assignment(
        &self,
        client_ids: &[String],
        layout_id: Option<&str>,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("UPDATE clients SET assigned_layout_id = ?1 WHERE id = ?2")
            .map_err(|e| e.to_string())?;
        let mut count = 0usize;
        for client_id in client_ids {
            count += stmt
                .execute(params![layout_id, client_id])
                .map_err(|e| e.to_string())?;
        }
        Ok(count)
    }

    pub fn get_client_theme(&self, client_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn()?;
        conn.query_row("SELECT theme FROM clients WHERE id = ?1", [client_id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn set_client_theme(&self, client_id: &str, theme: &str) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE clients SET theme = ?1 WHERE id = ?2",
            params![theme, client_id],
        )
        .map(|rows| rows > 0)
        .map_err(|e| e.to_string())
    }

    pub fn set_client_enrolled(&self, client_id: &str) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE clients SET enrolled = 1 WHERE id = ?1 AND is_virtual = 0",
            [client_id],
        )
        .map(|rows| rows > 0)
        .map_err(|e| e.to_string())
    }

    pub fn get_client_virtual_flag(&self, client_id: &str) -> Result<Option<bool>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT is_virtual FROM clients WHERE id = ?1",
            [client_id],
            |row| Ok(row.get::<_, i64>(0)? != 0),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn bulk_set_display_layout(
        &self,
        client_id: &str,
        layout_id: Option<&str>,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE client_displays SET assigned_layout_id = ?1 WHERE client_id = ?2",
            params![layout_id, client_id],
        )
        .map_err(|e| e.to_string())
    }

    pub fn get_display_config(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<(Option<String>, String), String> {
        let conn = self.conn()?;
        let row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT assigned_layout_id, orientation FROM client_displays WHERE client_id = ?1 AND display_index = ?2",
                params![client_id, display_index],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let (display_layout, orientation) = row
            .map(|(layout, orientation)| (layout, orientation.unwrap_or_else(|| "normal".to_string())))
            .unwrap_or((None, "normal".to_string()));
        if display_layout.is_some() {
            return Ok((display_layout, orientation));
        }
        let client_layout = conn
            .query_row(
                "SELECT assigned_layout_id FROM clients WHERE id = ?1",
                [client_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok((client_layout, orientation))
    }

    pub fn set_display_layout(
        &self,
        client_id: &str,
        display_index: i64,
        layout_id: Option<&str>,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE client_displays SET assigned_layout_id = ?1 WHERE client_id = ?2 AND display_index = ?3",
            params![layout_id, client_id, display_index],
        )
        .map_err(|e| e.to_string())
    }

    pub fn get_display_control(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<Option<(Option<String>, Option<String>, Option<i64>, Option<i64>, Option<f64>, bool, String, String)>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT connector, label, width, height, refresh_rate, enabled, orientation, control_method FROM client_displays WHERE client_id = ?1 AND display_index = ?2",
            params![client_id, display_index],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get::<_, i32>(5)? != 0,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn set_display_control_method(
        &self,
        client_id: &str,
        display_index: i64,
        method: &str,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE client_displays SET control_method = ?1 WHERE client_id = ?2 AND display_index = ?3",
            params![method, client_id, display_index],
        )
        .map_err(|e| e.to_string())
    }

    pub fn set_display_orientation(
        &self,
        client_id: &str,
        display_index: i64,
        orientation: &str,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE client_displays SET orientation = ?1 WHERE client_id = ?2 AND display_index = ?3",
            params![orientation, client_id, display_index],
        )
        .map_err(|e| e.to_string())
    }

    pub fn get_display_assignment_and_last_seen(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<Option<(Option<String>, String)>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT assigned_layout_id, last_seen FROM client_displays WHERE client_id = ?1 AND display_index = ?2",
            params![client_id, display_index],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn queue_client_command(
        &self,
        client_id: &str,
        action: &str,
        payload: Option<&str>,
        requested_at: &str,
        requested_by: Option<&str>,
    ) -> Result<(), String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let pending: Option<String> = tx
            .query_row(
                "SELECT action FROM client_commands WHERE client_id = ?1",
                [client_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if pending.is_some() {
            return Err("command_pending".to_string());
        }
        let burst_limit = crate::rate_limit::CLIENT_COMMAND_BURST_LIMIT;
        let cooldown_secs = crate::rate_limit::CLIENT_COMMAND_BURST_COOLDOWN_SECS;
        let now = chrono::Utc::now();
        let previous: Option<(Option<String>, i64, Option<String>)> = tx
            .query_row(
                "SELECT burst_started_at, burst_count, cooldown_until FROM client_command_cooldowns WHERE client_id = ?1 AND action = ?2",
                params![client_id, action],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let (previous_start, previous_count, previous_cooldown) =
            previous.unwrap_or((None, 0, None));
        if let Some(until) = previous_cooldown.as_deref() {
            if let Ok(until) = chrono::DateTime::parse_from_rfc3339(until) {
                let remaining_ms = (until.with_timezone(&chrono::Utc) - now).num_milliseconds();
                if remaining_ms > 0 {
                    let remaining = (remaining_ms + 999) / 1000;
                    return Err(format!("cooldown:{}", remaining.clamp(1, 86_400)));
                }
            }
        }
        let burst_expired = previous_start
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|started| {
                (now - started.with_timezone(&chrono::Utc)).num_milliseconds()
                    >= cooldown_secs * 1_000
            })
            .unwrap_or(true);
        let (burst_started_at, burst_count_before) = if burst_expired {
            (now.to_rfc3339(), 0)
        } else {
            (previous_start.unwrap_or_else(|| now.to_rfc3339()), previous_count)
        };
        if burst_count_before >= burst_limit {
            return Err(format!("cooldown:{cooldown_secs}"));
        }
        let burst_count = burst_count_before + 1;
        let cooldown_until = if burst_count >= burst_limit {
            Some((now + chrono::Duration::seconds(cooldown_secs)).to_rfc3339())
        } else {
            None
        };
        tx.execute(
            "INSERT INTO client_command_cooldowns
                (client_id, action, last_requested_at, burst_started_at, burst_count, cooldown_until)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(client_id, action) DO UPDATE SET
                last_requested_at=excluded.last_requested_at,
                burst_started_at=excluded.burst_started_at,
                burst_count=excluded.burst_count,
                cooldown_until=excluded.cooldown_until",
            params![
                client_id,
                action,
                requested_at,
                burst_started_at,
                burst_count,
                cooldown_until
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO client_commands (client_id, action, payload, requested_at, requested_by)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(client_id) DO UPDATE SET
                action=excluded.action,
                payload=excluded.payload,
                requested_at=excluded.requested_at,
                requested_by=excluded.requested_by",
            params![client_id, action, payload, requested_at, requested_by],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn get_client_command(
        &self,
        client_id: &str,
    ) -> Result<Option<(String, String, Option<String>, String, Option<String>)>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT client_id, action, payload, requested_at, requested_by FROM client_commands WHERE client_id = ?1",
            [client_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn delete_client_command_if_requested_at(
        &self,
        client_id: &str,
        requested_at: &str,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM client_commands WHERE client_id = ?1 AND requested_at = ?2",
            params![client_id, requested_at],
        )
        .map_err(|e| e.to_string())
    }

    pub fn clear_client_command(&self, client_id: &str) -> Result<usize, String> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM client_commands WHERE client_id = ?1",
            [client_id],
        )
        .map_err(|e| e.to_string())
    }
    pub fn delete_client_history(&self, client_id: &str) -> Result<usize, String> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM client_heartbeats WHERE client_id = ?1",
            [client_id],
        )
        .map_err(|e| e.to_string())
    }

    pub fn list_client_heartbeats(
        &self,
        client_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, Option<f64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>)>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT recorded_at, cpu_usage, memory_used, memory_total, disk_used, disk_total, uptime_seconds FROM client_heartbeats WHERE client_id = ?1 ORDER BY recorded_at DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![client_id, limit], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn delete_client(&self, client_id: &str) -> Result<bool, String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM client_displays WHERE client_id = ?1", [client_id])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM client_commands WHERE client_id = ?1", [client_id])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM client_command_cooldowns WHERE client_id = ?1", [client_id])
            .map_err(|e| e.to_string())?;
        let deleted = tx
            .execute("DELETE FROM clients WHERE id = ?1", [client_id])
            .map_err(|e| e.to_string())?
            > 0;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(deleted)
    }

    pub fn delete_virtual_client(&self, client_id: &str) -> Result<bool, String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let is_virtual: Option<i64> = tx
            .query_row(
                "SELECT is_virtual FROM clients WHERE id = ?1",
                [client_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if is_virtual != Some(1) {
            return Ok(false);
        }
        tx.execute("DELETE FROM client_displays WHERE client_id = ?1", [client_id])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM client_commands WHERE client_id = ?1", [client_id])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM client_command_cooldowns WHERE client_id = ?1", [client_id])
            .map_err(|e| e.to_string())?;
        let deleted = tx
            .execute("DELETE FROM clients WHERE id = ?1", [client_id])
            .map_err(|e| e.to_string())?
            > 0;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(deleted)
    }
    pub fn set_client_display_count(
        &self,
        client_id: &str,
        requested: Option<i64>,
    ) -> Result<Option<(i64, i64)>, String> {
        let conn = self.conn()?;
        let installer: Option<i64> = conn
            .query_row(
                "SELECT COALESCE(installer_detected_max_displays, 1) FROM clients WHERE id = ?1",
                [client_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(installer) = installer else {
            return Ok(None);
        };
        let installer = installer.clamp(1, 4);
        let active = requested.map(|v| v.clamp(1, installer)).unwrap_or(installer);
        conn.execute(
            "UPDATE clients SET max_displays = ?1 WHERE id = ?2",
            params![requested.map(|_| active), client_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(Some((installer, active)))
    }

    pub fn client_exists(&self, client_id: &str) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM clients WHERE id = ?1)",
            [client_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())
    }

    pub fn get_client_auth_token_hash(&self, client_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT client_auth_token_hash FROM clients WHERE id = ?1",
            [client_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
        .map(|value: Option<Option<String>>| value.flatten())
    }

    pub fn set_client_touch_binding(
        &self,
        client_id: &str,
        binding_json: &str,
    ) -> Result<bool, String> {
        let conn = self.conn()?;
        Ok(conn
            .execute(
                "UPDATE clients SET touch_binding = ?1 WHERE id = ?2",
                params![binding_json, client_id],
            )
            .map_err(|e| e.to_string())?
            > 0)
    }

    pub fn list_registered_clients(
        &self,
    ) -> Result<
        Vec<(
            String,
            String,
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<f32>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            i64,
        )>,
        String,
    > {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, address, role, version, uptime_seconds, cpu_usage, memory_used, memory_total, disk_used, disk_total, active_layout_id, assigned_layout_id, last_seen, enrolled, last_command_result, last_command_at, max_displays, installer_detected_max_displays, touch_binding, theme, is_virtual FROM clients ORDER BY last_seen DESC LIMIT 10000",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                    row.get(18)?,
                    row.get(19)?,
                    row.get(20)?,
                    row.get(21)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn mark_news_feed_fetched(&self, url: &str, fetched_at: &str) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE news_feeds SET last_fetched_at = ?1, last_error = NULL WHERE url = ?2",
            params![fetched_at, url],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn register_client(
        &self,
        client: &ClientRegistration,
        displays: &[ClientDisplayRegistration],
    ) -> Result<(), String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO clients (id, name, address, role, version, uptime_seconds, cpu_usage, memory_used, memory_total, disk_used, disk_total, active_layout_id, last_seen, enrolled, last_command_result, last_command_at, installer_detected_max_displays, touch_binding, theme, client_auth_token_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0, ?14, ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, address=excluded.address, role=excluded.role,
                version=excluded.version, uptime_seconds=excluded.uptime_seconds,
                cpu_usage=excluded.cpu_usage, memory_used=excluded.memory_used,
                memory_total=excluded.memory_total, disk_used=excluded.disk_used,
                disk_total=excluded.disk_total, active_layout_id=excluded.active_layout_id,
                last_seen=excluded.last_seen, enrolled=enrolled,
                last_command_result=COALESCE(excluded.last_command_result, last_command_result),
                last_command_at=COALESCE(excluded.last_command_at, last_command_at),
                installer_detected_max_displays=COALESCE(excluded.installer_detected_max_displays, installer_detected_max_displays),
                touch_binding=COALESCE(excluded.touch_binding, touch_binding), theme=excluded.theme,
               client_auth_token_hash=COALESCE(excluded.client_auth_token_hash, client_auth_token_hash)",
            params![
                client.id, client.name, client.address, client.role, client.version,
                client.uptime_seconds, client.cpu_usage, client.memory_used,
                client.memory_total, client.disk_used, client.disk_total,
                client.active_layout_id, client.last_seen, client.last_command_result,
                client.last_command_at, client.installer_detected_max_displays,
                client.touch_binding, client.theme, client.client_auth_token_hash,
            ],
        )
        .map_err(|e| e.to_string())?;
        if client.max_displays.is_some() {
            tx.execute(
                "UPDATE clients SET max_displays = COALESCE(max_displays, ?1) WHERE id = ?2",
                params![client.max_displays, client.id],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute(
            "INSERT INTO client_heartbeats (client_id, recorded_at, cpu_usage, memory_used, memory_total, disk_used, disk_total, uptime_seconds) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![client.id, client.last_seen, client.cpu_usage, client.memory_used, client.memory_total, client.disk_used, client.disk_total, client.uptime_seconds],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM client_heartbeats WHERE client_id = ?1 AND id NOT IN (SELECT id FROM client_heartbeats WHERE client_id = ?1 ORDER BY recorded_at DESC LIMIT 200)",
            [client.id.as_str()],
        )
        .map_err(|e| e.to_string())?;
        for display in displays {
            tx.execute(
                "INSERT INTO client_displays (client_id, display_index, connector, label, width, height, refresh_rate, enabled, last_seen, last_error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(client_id, display_index) DO UPDATE SET
                    connector=excluded.connector, label=excluded.label,
                    width=COALESCE(excluded.width, client_displays.width),
                    height=COALESCE(excluded.height, client_displays.height),
                    refresh_rate=COALESCE(excluded.refresh_rate, client_displays.refresh_rate),
                    enabled=COALESCE(excluded.enabled, client_displays.enabled),
                    last_seen=excluded.last_seen, last_error=excluded.last_error",
                params![
                    client.id, display.display_index, display.connector, display.label,
                    display.width, display.height, display.refresh_rate,
                    display.enabled.map(|v| if v { 1 } else { 0 }),
                    client.last_seen, display.last_error,
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn list_table_names(&self) -> Result<Vec<String>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn query_table_rows(
        &self,
        table: &str,
        limit: u32,
        offset: u32,
    ) -> Result<TableQueryResult, String> {
        if !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err("Invalid table name".into());
        }
        let conn = self.conn()?;
        let mut col_stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| e.to_string())?;
        let columns = col_stmt
            .query_map([], |row| {
                Ok(TableColumnRecord {
                    name: row.get(1)?,
                    col_type: row.get(2)?,
                    notnull: row.get::<_, i32>(3).unwrap_or(0) != 0,
                    dflt_value: row.get::<_, Option<String>>(4).unwrap_or(None),
                    pk: row.get::<_, i32>(5).unwrap_or(0) != 0,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let total = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .unwrap_or(0);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT rowid,* FROM {table} ORDER BY rowid DESC LIMIT {limit} OFFSET {offset}"
            ))
            .map_err(|e| e.to_string())?;
        let mut query_rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        while let Some(row) = query_rows.next().map_err(|e| e.to_string())? {
            let mut values = Vec::new();
            for index in 0..columns.len() {
                let value: rusqlite::types::Value = row.get(index).map_err(|e| e.to_string())?;
                values.push(match value {
                    rusqlite::types::Value::Null => serde_json::Value::Null,
                    rusqlite::types::Value::Integer(value) => serde_json::json!(value),
                    rusqlite::types::Value::Real(value) => serde_json::json!(value),
                    rusqlite::types::Value::Text(value) => serde_json::json!(value),
                    rusqlite::types::Value::Blob(value) => {
                        serde_json::json!(base64::encode(value))
                    }
                });
            }
            rows.push(values);
        }
        Ok(TableQueryResult {
            columns,
            rows,
            total,
        })
    }

    pub fn delete_table_rows(&self, table: &str, ids: &[i64]) -> Result<usize, String> {
        if !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err("Invalid table name".into());
        }
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let query = format!("DELETE FROM {table} WHERE rowid IN ({placeholders})");
        let conn = self.conn()?;
        conn.execute(&query, rusqlite::params_from_iter(ids.iter()))
            .map_err(|e| e.to_string())
    }

    pub fn export_table_rows(&self, table: &str) -> Result<TableExportResult, String> {
        if !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err("Invalid table name".into());
        }
        let conn = self.conn()?;
        let mut col_stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| e.to_string())?;
        let columns = col_stmt
            .query_map([], |row| row.get(1))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(&format!("SELECT rowid,* FROM {table} ORDER BY rowid DESC"))
            .map_err(|e| e.to_string())?;
        let mut query_rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        while let Some(row) = query_rows.next().map_err(|e| e.to_string())? {
            let mut values = Vec::new();
            for index in 0..columns.len() {
                let value: rusqlite::types::Value = row.get(index).map_err(|e| e.to_string())?;
                values.push(match value {
                    rusqlite::types::Value::Null => serde_json::Value::Null,
                    rusqlite::types::Value::Integer(value) => serde_json::json!(value),
                    rusqlite::types::Value::Real(value) => serde_json::json!(value),
                    rusqlite::types::Value::Text(value) => serde_json::json!(value),
                    rusqlite::types::Value::Blob(value) => {
                        serde_json::json!(base64::encode(value))
                    }
                });
            }
            rows.push(values);
        }
        Ok(TableExportResult { columns, rows })
    }

    pub fn set_virtual_client_layout(
        &self,
        client_id: &str,
        layout_id: &str,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE clients SET assigned_layout_id = ?1, active_layout_id = NULL WHERE id = ?2 AND is_virtual = 1",
            params![layout_id, client_id],
        )
        .map_err(|e| e.to_string())
    }

    pub fn clear_client_display_command(
        &self,
        client_id: &str,
        display_index: i64,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        let targeted = conn
            .execute(
                "DELETE FROM client_commands WHERE client_id = ?1 AND json_extract(payload, '$.display_index') = ?2",
                params![client_id, display_index],
            )
            .map_err(|e| e.to_string())?;
        if targeted > 0 {
            return Ok(targeted);
        }
        conn.execute("DELETE FROM client_commands WHERE client_id = ?1", [client_id])
            .map_err(|e| e.to_string())
    }

    pub fn list_client_displays(
        &self,
        client_id: &str,
    ) -> Result<(bool, Option<String>, Vec<ClientDisplayRecord>), String> {
        let conn = self.conn()?;
        let (is_virtual, client_layout_id): (bool, Option<String>) = conn
            .query_row(
                "SELECT is_virtual, assigned_layout_id FROM clients WHERE id = ?1",
                [client_id],
                |row| Ok((row.get::<_, i32>(0).unwrap_or(0) != 0, row.get(1).ok())),
            )
            .unwrap_or((false, None));
        let mut stmt = conn
            .prepare("SELECT client_id, display_index, connector, label, width, height, refresh_rate, enabled, assigned_layout_id, active_layout_id, last_seen, last_error, orientation, control_method FROM client_displays WHERE client_id = ?1 ORDER BY display_index ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([client_id], |row| {
                Ok(ClientDisplayRecord {
                    client_id: row.get(0)?,
                    display_index: row.get(1)?,
                    connector: row.get(2)?,
                    label: row.get(3)?,
                    width: row.get(4)?,
                    height: row.get(5)?,
                    refresh_rate: row.get(6)?,
                    enabled: row.get::<_, i64>(7).unwrap_or(1) != 0,
                    assigned_layout_id: row.get(8).ok().or_else(|| client_layout_id.clone()),
                    active_layout_id: row.get(9)?,
                    last_seen: row.get(10)?,
                    last_error: row.get(11)?,
                    orientation: row.get(12).unwrap_or_else(|_| "normal".to_string()),
                    control_method: row.get(13).unwrap_or_else(|_| "auto".to_string()),
                })
            })
            .map_err(|e| e.to_string())?;
        let displays = rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        Ok((is_virtual, client_layout_id, displays))
    }

    pub fn list_client_admin_users(
        &self,
        client_id: &str,
    ) -> Result<Option<(bool, Vec<ClientAdminUser>)>, String> {
        let conn = self.conn()?;
        let Some(enrolled) = conn
            .query_row("SELECT enrolled FROM clients WHERE id = ?1", [client_id], |row| row.get::<_, i64>(0))
            .optional()
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare("SELECT username, display_name, role FROM users WHERE active=1 AND role IN ('admin','layout-manager') ORDER BY role ASC, username ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ClientAdminUser {
                    username: row.get(0)?,
                    display_name: row.get(1)?,
                    role: row.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let users = rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        Ok(Some((enrolled == 1, users)))
    }

    pub fn publish_scene_assignments(
        &self,
        client_id: &str,
        assignments: &[(i64, Option<String>)],
    ) -> Result<usize, String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let mut updated = 0;
        for (display_index, layout_id) in assignments {
            updated += tx
                .execute(
                    "UPDATE client_displays SET assigned_layout_id = ?1 WHERE client_id = ?2 AND display_index = ?3",
                    params![layout_id, client_id, display_index],
                )
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(updated)
    }

    pub fn get_scene(
        &self,
        scene_id: &str,
    ) -> Result<Option<(String, String, Option<String>, String, String, Vec<(i64, Option<String>)>)>, String> {
        let conn = self.conn()?;
        let base: Option<(String, String, Option<String>, String, String)> = conn
            .query_row(
                "SELECT id, name, description, created_at, updated_at FROM scenes WHERE id = ?1",
                [scene_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(base) = base else { return Ok(None); };
        let mut stmt = conn
            .prepare("SELECT display_index, layout_id FROM scene_display_layouts WHERE scene_id = ?1 ORDER BY display_index ASC")
            .map_err(|e| e.to_string())?;
        let assignments = stmt
            .query_map([scene_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(Some((base.0, base.1, base.2, base.3, base.4, assignments)))
    }

    pub fn list_scene_ids(&self) -> Result<Vec<String>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT id FROM scenes ORDER BY updated_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn save_scene(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        timestamp: &str,
        assignments: &[(i64, Option<String>)],
        update: bool,
    ) -> Result<bool, String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let changed = if update {
            tx.execute(
                "UPDATE scenes SET name = ?1, description = ?2, updated_at = ?3 WHERE id = ?4",
                params![name, description, timestamp, id],
            )
            .map_err(|e| e.to_string())?
        } else {
            tx.execute(
                "INSERT INTO scenes (id, name, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, name, description, timestamp, timestamp],
            )
            .map_err(|e| e.to_string())?
        };
        if changed == 0 { return Ok(false); }
        tx.execute("DELETE FROM scene_display_layouts WHERE scene_id = ?1", [id])
            .map_err(|e| e.to_string())?;
        for (display_index, layout_id) in assignments {
            tx.execute(
                "INSERT INTO scene_display_layouts (scene_id, display_index, layout_id) VALUES (?1, ?2, ?3)",
                params![id, display_index, layout_id],
            ).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }

    pub fn delete_scene(&self, scene_id: &str) -> Result<usize, String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM scene_display_layouts WHERE scene_id = ?1", [scene_id])
            .map_err(|e| e.to_string())?;
        let deleted = tx
            .execute("DELETE FROM scenes WHERE id = ?1", [scene_id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(deleted)
    }
    pub fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // Enforce relational integrity on every connection and use WAL for
        // concurrent heartbeat reads while an admin write is in progress.
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        // Phase 5: kiosk → interactive rename. This MUST run before the
        // schema's `CREATE TABLE IF NOT EXISTS interactive_submissions`
        // (below) so that an old DB with a `kiosk_submissions` table can
        // be renamed to the new name. Otherwise the CREATE TABLE will
        // create `interactive_submissions` first and the RENAME will fail
        // with "there is already another table or index with this name".
        // Each step is idempotent (no-op if already applied).
        let old_table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='kiosk_submissions'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        let new_table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='interactive_submissions'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if old_table_exists && !new_table_exists {
            conn.execute(
                "ALTER TABLE kiosk_submissions RENAME TO interactive_submissions",
                [],
            )?;
            let _ = conn.execute(
                "ALTER TABLE interactive_submissions RENAME COLUMN kiosk_page_id TO interactive_layout_id",
                [],
            );
            let _ = conn.execute(
                "ALTER TABLE interactive_submissions RENAME COLUMN kiosk_page_name TO interactive_layout_name",
                [],
            );
        } else if old_table_exists && new_table_exists {
            // Both tables exist. The old `kiosk_submissions` table is from
            // a legacy system whose columns don't match the new schema
            // (e.g. it has `value` and `submitted_at` instead of
            // `payload_json` / `collected_at`). Best-effort: log the row
            // count, then DROP the legacy table. The new code only reads
            // `interactive_submissions`.
            let legacy_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM kiosk_submissions", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap_or(0);
            log::info!(
                "dropping legacy kiosk_submissions table ({} rows, incompatible schema)",
                legacy_count
            );
            conn.execute("DROP TABLE kiosk_submissions", [])?;
        }
        let has_old_column: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('clients') WHERE name='assigned_kiosk_page_id'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if has_old_column {
            conn.execute("ALTER TABLE clients RENAME COLUMN assigned_kiosk_page_id TO assigned_interactive_layout_id", [])
                .map_err(Box::new)?;
        }
        // Phase 5 cleanup: drop any leftover legacy Phase 3 tables. These may
        // be reintroduced by a state-bundle import after the original migration
        // ran (the migration is idempotent for its own writes, but a restored
        // bundle is a separate code path). The Rust code has no readers for
        // these tables — they are dead weight plus a dangling FK target from
        // `clients.assigned_interactive_layout_id` → `kiosk_pages(id)`.
        // Each DROP is idempotent.
        let _ = conn.execute("DROP TABLE IF EXISTS kiosk_pages", []);
        let _ = conn.execute("DROP TABLE IF EXISTS kiosk_submissions", []);
        // Also drop the per-column legacy indexes that survived with
        // `kiosk_submissions` (no longer needed once the table is gone).
        let _ = conn.execute("DROP INDEX IF EXISTS idx_kiosk_submissions_collected", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_kiosk_submissions_status", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_kiosk_submissions_page", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_kiosk_submissions_client", []);
        // Phase 5 cleanup (followup): drop the dead `assigned_interactive_layout_id`
        // column on `clients`. The column carried a dangling FK to `kiosk_pages(id)`
        // (dropped above) and is never read or written by the Rust code — the active
        // column is `assigned_layout_id` (Phase 2). `let _ =` swallows the error on
        // fresh DBs that don't have the column. Requires SQLite >= 3.35.0 (libsqlite3-sys
        // 0.30.1 ships 3.46+).
        let _ = conn.execute(
            "ALTER TABLE clients DROP COLUMN assigned_interactive_layout_id",
            [],
        );
        // Phase 5 (cycling): make `schedules.layout_id` nullable so
        // schedules that use a `layout_set_id` can store NULL
        // instead of an empty string (which would violate the FK to
        // `layouts(id)`). SQLite has no DROP NOT NULL, so we
        // rebuild the table.
        rebuild_schedules_layout_id_nullable(&conn)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS layout_folders (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                parent_id  TEXT REFERENCES layout_folders(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_layout_folders_parent ON layout_folders(parent_id);

            CREATE TABLE IF NOT EXISTS layouts (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                interactive INTEGER NOT NULL DEFAULT 0,
                page_type   TEXT NOT NULL DEFAULT 'single',
                timing_mode TEXT NOT NULL DEFAULT 'global',
                duration    INTEGER NOT NULL DEFAULT 15,
                shuffle     INTEGER NOT NULL DEFAULT 0,
                canvas_orientation TEXT NOT NULL DEFAULT 'landscape',
                folder_id   TEXT REFERENCES layout_folders(id) ON DELETE SET NULL,
                items       TEXT NOT NULL DEFAULT '[]',
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                snap_grid   TEXT
            );

            CREATE TABLE IF NOT EXISTS widgets (
                id           TEXT PRIMARY KEY,
                layout_id    TEXT NOT NULL REFERENCES layouts(id) ON DELETE CASCADE,
                widget_type  TEXT NOT NULL,
                x            REAL NOT NULL,
                y            REAL NOT NULL,
                w            REAL NOT NULL,
                h            REAL NOT NULL,
                z            INTEGER NOT NULL DEFAULT 0,
                props        TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS display_state (
                display     TEXT PRIMARY KEY,
                layout_id   TEXT
            );

            CREATE TABLE IF NOT EXISTS display_overrides (
                display     TEXT PRIMARY KEY,
                width       INTEGER NOT NULL,
                height      INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS templates (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                category    TEXT NOT NULL DEFAULT 'general',
                is_builtin  INTEGER NOT NULL DEFAULT 0,
                layout_json TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                layout_set_id TEXT
            );

            CREATE TABLE IF NOT EXISTS refresh_state (
                id          INTEGER PRIMARY KEY DEFAULT 1,
                epoch       INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS backups (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                file_path   TEXT NOT NULL,
                size_bytes  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                token      TEXT PRIMARY KEY,
                username   TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                revoked    INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS schedule_runs (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                schedule_id TEXT NOT NULL,
                action      TEXT NOT NULL,
                target      TEXT NOT NULL,
                success     INTEGER NOT NULL,
                message     TEXT,
                occurred_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_schedule_runs_schedule_time
                ON schedule_runs(schedule_id, occurred_at);

            CREATE TABLE IF NOT EXISTS audit_events (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                actor       TEXT,
                role        TEXT,
                event_type  TEXT NOT NULL,
                target      TEXT,
                outcome     TEXT NOT NULL,
                detail      TEXT,
                occurred_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_audit_events_occurred_at
                ON audit_events(occurred_at);

            CREATE TABLE IF NOT EXISTS schedules (
                id             TEXT PRIMARY KEY,
                display        TEXT NOT NULL,
                layout_id      TEXT, -- NULL when this schedule uses a layout_set_id instead
                day_of_week    INTEGER NOT NULL, -- -1 for every day, 0-6 for Sun-Sat
                start_time     TEXT NOT NULL,   -- HH:MM
                end_time       TEXT NOT NULL,   -- HH:MM
                created_at     TEXT NOT NULL,
                layout_set_id  TEXT,
                action         TEXT NOT NULL DEFAULT 'switch_layout', -- switch_layout | power_on | power_off
                target_client  TEXT,
                power_state    TEXT,
                updated_at     TEXT,
                display_name   TEXT,
                control_method TEXT NOT NULL DEFAULT 'auto',
                display_index   INTEGER NOT NULL DEFAULT 0,
                brightness     INTEGER,
                contrast       INTEGER,
                end_action     TEXT NOT NULL DEFAULT 'none', -- none | power_on
                FOREIGN KEY(layout_id) REFERENCES layouts(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_schedules_display ON schedules(display);
            CREATE INDEX IF NOT EXISTS idx_schedules_layout_set_id ON schedules(layout_set_id);
            CREATE TABLE IF NOT EXISTS schedule_runtime_state (
                schedule_id TEXT PRIMARY KEY,
                active INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS clients (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                address         TEXT NOT NULL,
                role            TEXT NOT NULL DEFAULT 'player',
                version         TEXT,
                uptime_seconds  INTEGER,
                cpu_usage       REAL,
                memory_used     INTEGER,
                memory_total    INTEGER,
                disk_used       INTEGER,
                disk_total      INTEGER,
                active_layout_id TEXT,
                assigned_layout_id TEXT,
                last_seen       TEXT NOT NULL,
                enrolled        INTEGER NOT NULL DEFAULT 0,
                last_command_result TEXT,
                last_command_at TEXT,
                -- Display count governance. installer_detected_max_displays is
                -- set from the player's heartbeat (count of connectors reported);
                -- max_displays is the operator/server-side override (NULL =
                -- no override, follow installer_detected). The renderer honors
                -- min(installer_detected_max_displays, COALESCE(max_displays,
                -- installer_detected_max_displays)) when deciding how many
                -- interactive-browser instances to launch.
                max_displays                INTEGER,
                installer_detected_max_displays INTEGER,
                touch_binding              TEXT,
                theme                      TEXT,
                is_virtual                 INTEGER NOT NULL DEFAULT 0,
                url                        TEXT,
                client_auth_token_hash     TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_clients_last_seen ON clients(last_seen);
            -- Idempotent migration: ALTER TABLE will fail with duplicate-column if already present; that's fine.
            -- (SQLite has no "ADD COLUMN IF NOT EXISTS" before 3.35.)

            CREATE TABLE IF NOT EXISTS addon_storage (
                addon_id      TEXT NOT NULL,
                storage_key   TEXT NOT NULL,
                value_json    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                PRIMARY KEY (addon_id, storage_key)
            );

CREATE TABLE IF NOT EXISTS client_heartbeats (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id     TEXT NOT NULL,
                recorded_at   TEXT NOT NULL,
                cpu_usage     REAL,
                memory_used   INTEGER,
                memory_total  INTEGER,
                disk_used     INTEGER,
                disk_total    INTEGER,
                uptime_seconds INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_client_heartbeats_client_time ON client_heartbeats(client_id, recorded_at);

CREATE TABLE IF NOT EXISTS layout_sets (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                shuffle     INTEGER NOT NULL DEFAULT 0,
                dwell_secs  INTEGER NOT NULL DEFAULT 30,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

CREATE TABLE IF NOT EXISTS layout_set_entries (
                id              TEXT PRIMARY KEY,
                layout_set_id   TEXT NOT NULL,
                layout_id       TEXT NOT NULL,
                order_index     INTEGER NOT NULL DEFAULT 0,
                interval_secs   INTEGER NOT NULL DEFAULT 30,
                FOREIGN KEY (layout_set_id) REFERENCES layout_sets(id) ON DELETE CASCADE,
                FOREIGN KEY (layout_id) REFERENCES layouts(id) ON DELETE CASCADE
            );

CREATE TABLE IF NOT EXISTS client_displays (
                client_id          TEXT NOT NULL,
                display_index      INTEGER NOT NULL,
                connector          TEXT,
                label              TEXT,
                width              INTEGER,
                height             INTEGER,
                refresh_rate       REAL,
                enabled            INTEGER NOT NULL DEFAULT 1,
                assigned_layout_id TEXT,
                active_layout_id   TEXT,
                last_seen          TEXT NOT NULL,
                last_error         TEXT,
                orientation        TEXT NOT NULL DEFAULT 'normal',
                control_method      TEXT NOT NULL DEFAULT 'auto',
                PRIMARY KEY (client_id, display_index)
            );
CREATE INDEX IF NOT EXISTS idx_client_displays_client ON client_displays(client_id);


CREATE TABLE IF NOT EXISTS client_commands (
                client_id     TEXT PRIMARY KEY,
                action        TEXT NOT NULL,
                payload       TEXT,
                requested_at  TEXT NOT NULL,
                requested_by  TEXT
            );
CREATE TABLE IF NOT EXISTS client_command_cooldowns (
                client_id          TEXT NOT NULL,
                action             TEXT NOT NULL,
                last_requested_at  TEXT NOT NULL,
                burst_started_at   TEXT,
                burst_count        INTEGER NOT NULL DEFAULT 0,
                cooldown_until     TEXT,
                PRIMARY KEY (client_id, action)
            );
CREATE INDEX IF NOT EXISTS idx_client_command_cooldowns_last_requested
    ON client_command_cooldowns(last_requested_at);


CREATE TABLE IF NOT EXISTS users (
                id            TEXT PRIMARY KEY,
                username      TEXT NOT NULL UNIQUE,
                display_name  TEXT NOT NULL,
                role          TEXT NOT NULL DEFAULT 'layout-manager',
                password_hash TEXT NOT NULL,
                active        INTEGER NOT NULL DEFAULT 1,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                last_login_at TEXT,
                google_sub    TEXT,
                google_email  TEXT
            );
CREATE INDEX IF NOT EXISTS idx_users_username ON users(username);
CREATE INDEX IF NOT EXISTS idx_users_role ON users(role);

CREATE TABLE IF NOT EXISTS scenes (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                description TEXT,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
CREATE TABLE IF NOT EXISTS scene_display_layouts (
                scene_id      TEXT NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
                display_index INTEGER NOT NULL,
                layout_id     TEXT,
                PRIMARY KEY (scene_id, display_index)
            );
CREATE INDEX IF NOT EXISTS idx_scene_display_layouts_scene ON scene_display_layouts(scene_id);



            -- Interactive Submissions: data collected from interactive widgets
            -- (textbox, radio, dropdown, checkbox-list, button-with-value, etc.)
            -- POSTed by the player when the user submits, then optionally synced
            -- to a configured Google Sheet (Sheets/Forms/etc.).
            CREATE TABLE IF NOT EXISTS interactive_submissions (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id            TEXT NOT NULL,
                interactive_layout_id        TEXT,
                interactive_layout_name      TEXT,
                widget_id            TEXT NOT NULL,
                widget_type          TEXT NOT NULL,
                widget_label         TEXT,
                payload_json         TEXT NOT NULL,
                client_address       TEXT,
                collected_at         TEXT NOT NULL,
                received_at          TEXT NOT NULL,
                google_sync_status   TEXT NOT NULL DEFAULT 'pending',  -- pending | success | failed | skipped
                google_sync_message  TEXT,
                google_synced_at     TEXT,
                google_sheet_id      TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_interactive_submissions_client ON interactive_submissions(client_id);
            CREATE INDEX IF NOT EXISTS idx_interactive_submissions_layout ON interactive_submissions(interactive_layout_id);
            CREATE INDEX IF NOT EXISTS idx_interactive_submissions_status ON interactive_submissions(google_sync_status);
            CREATE INDEX IF NOT EXISTS idx_interactive_submissions_collected ON interactive_submissions(collected_at);

            -- Interactive Layouts: named layouts that can be served to interactive/player clients.
            -- Widget Presets: a reusable set of default properties for a widget type.
            -- When a user drags a new widget onto the layout, they can choose a preset
            -- to seed the widget's props. Custom assets (e.g. weather icons) live in
            -- widget_preset_assets and are looked up at render time.
            CREATE TABLE IF NOT EXISTS widget_presets (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL,
                widget_type  TEXT NOT NULL,    -- 'text', 'image', 'video', 'clock', 'datetime', 'calendar', 'weather', 'webpage', 'music', etc.
                props_json   TEXT NOT NULL,    -- JSON object of default widget props (font, color, etc.)
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                is_builtin   INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_widget_presets_type ON widget_presets(widget_type);

            -- Custom assets belonging to a widget preset. The primary use case is
            -- overriding the default weather icons with custom images per WMO code.
            CREATE TABLE IF NOT EXISTS widget_preset_assets (
                id          TEXT PRIMARY KEY,
                preset_id   TEXT NOT NULL,
                kind        TEXT NOT NULL,    -- e.g. 'weather_icon'
                key         TEXT NOT NULL,    -- e.g. WMO code as string ("0", "65", "95")
                url         TEXT NOT NULL,    -- URL or relative path to the custom asset
                FOREIGN KEY (preset_id) REFERENCES widget_presets(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_widget_preset_assets_preset ON widget_preset_assets(preset_id);

            -- Cached RSS / Atom feeds for News widgets. The url is the primary
            -- key so multiple News widgets pointing at the same feed share the
            -- same cached headlines. The maintenance loop refreshes rows whose
            -- last_fetched_at is older than the threshold; the player reads the
            -- latest headlines_json via GET /api/news/headlines?url=<url>.
            CREATE TABLE IF NOT EXISTS news_feeds (
                url              TEXT PRIMARY KEY,
                title            TEXT NOT NULL DEFAULT '',
                headlines_json   TEXT NOT NULL DEFAULT '[]',
                last_fetched_at  TEXT NOT NULL DEFAULT '',
                last_error       TEXT,
                etag             TEXT,
                last_modified    TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_news_feeds_last_fetched ON news_feeds(last_fetched_at);

            INSERT OR IGNORE INTO refresh_state (id, epoch) VALUES (1, 0);
            "#
        )?;
        for sql in [
            "ALTER TABLE layouts ADD COLUMN snap_grid TEXT",
            "ALTER TABLE users ADD COLUMN google_sub TEXT",
            "ALTER TABLE users ADD COLUMN google_email TEXT",
            "ALTER TABLE clients ADD COLUMN max_displays INTEGER",
            "ALTER TABLE clients ADD COLUMN installer_detected_max_displays INTEGER",
            "ALTER TABLE clients ADD COLUMN touch_binding TEXT",
            "ALTER TABLE clients ADD COLUMN theme TEXT",
            "ALTER TABLE clients ADD COLUMN is_virtual INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE clients ADD COLUMN url TEXT",
            "ALTER TABLE clients ADD COLUMN client_auth_token_hash TEXT",
            "ALTER TABLE client_displays ADD COLUMN control_method TEXT NOT NULL DEFAULT 'auto'",
            "ALTER TABLE client_command_cooldowns ADD COLUMN burst_started_at TEXT",
            "ALTER TABLE client_command_cooldowns ADD COLUMN burst_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE client_command_cooldowns ADD COLUMN cooldown_until TEXT",
            "ALTER TABLE schedules ADD COLUMN updated_at TEXT",
            "ALTER TABLE schedules ADD COLUMN display_name TEXT",
            "ALTER TABLE schedules ADD COLUMN control_method TEXT NOT NULL DEFAULT 'auto'",
            "ALTER TABLE schedules ADD COLUMN display_index INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE schedules ADD COLUMN brightness INTEGER",
            "ALTER TABLE schedules ADD COLUMN contrast INTEGER",
            "ALTER TABLE layout_sets ADD COLUMN dwell_secs INTEGER NOT NULL DEFAULT 30",
            "ALTER TABLE layouts ADD COLUMN canvas_orientation TEXT NOT NULL DEFAULT 'landscape'",
            "ALTER TABLE layouts ADD COLUMN folder_id TEXT REFERENCES layout_folders(id) ON DELETE SET NULL",
            // (Phase 5: kiosk → interactive rename moved to the top of
            // `open()` so it runs before `CREATE TABLE IF NOT EXISTS
            // interactive_submissions` below.)
        ] {
            if let Err(e) = conn.execute(sql, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column") {
                    return Err(Box::new(e));
                }
            }
        }
        conn.execute(
            "CREATE TABLE IF NOT EXISTS layout_revisions (layout_id TEXT NOT NULL, revision INTEGER NOT NULL, snapshot TEXT NOT NULL, created_at TEXT NOT NULL, published_by TEXT, PRIMARY KEY (layout_id, revision), FOREIGN KEY(layout_id) REFERENCES layouts(id) ON DELETE CASCADE)",
            [],
        )?;
        conn.execute("CREATE INDEX IF NOT EXISTS idx_layout_revisions_layout ON layout_revisions(layout_id, revision DESC)", [])?;
        let _ = conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_users_google_sub ON users(google_sub) WHERE google_sub IS NOT NULL", []);
        // Re-assert the new index names for the migrated table. The RENAME
        // TO above auto-renames indexes, but new indexes named after the
        // old table only get created if the old table was missing.
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_interactive_submissions_client ON interactive_submissions(client_id)", []);
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_interactive_submissions_layout ON interactive_submissions(interactive_layout_id)", []);
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ── Layout CRUD ──────────────────────────────────────────

    pub fn list_layout_folders(&self) -> Result<Vec<LayoutFolder>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, name, parent_id, created_at, updated_at FROM layout_folders ORDER BY COALESCE(parent_id, ''), name COLLATE NOCASE LIMIT 10000")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(LayoutFolder {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    parent_id: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn save_layout_folder(&self, folder: &LayoutFolder) -> Result<(), String> {
        let name = folder.name.trim();
        if name.is_empty() || name.len() > 120 {
            return Err("Folder name must be between 1 and 120 characters".into());
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        if let Some(parent_id) = &folder.parent_id {
            let exists: bool = conn
                .query_row("SELECT EXISTS(SELECT 1 FROM layout_folders WHERE id = ?1)", [parent_id], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            if !exists || parent_id == &folder.id {
                return Err("Invalid parent folder".into());
            }
        }
        conn.execute(
            "INSERT INTO layout_folders (id, name, parent_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(id) DO UPDATE SET name=excluded.name, parent_id=excluded.parent_id, updated_at=excluded.updated_at",
            rusqlite::params![folder.id, name, folder.parent_id, folder.created_at, folder.updated_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_layout_folder(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let layout_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM layouts WHERE folder_id = ?1", [id], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        let child_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM layout_folders WHERE parent_id = ?1", [id], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        if layout_count > 0 || child_count > 0 {
            return Err("Folder must be empty before it can be deleted".into());
        }
        let changed = conn.execute("DELETE FROM layout_folders WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("Folder not found".into());
        }
        Ok(())
    }

    pub fn move_layout_to_folder(&self, layout_id: &str, folder_id: Option<&str>) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        if let Some(folder_id) = folder_id {
            let exists: bool = conn
                .query_row("SELECT EXISTS(SELECT 1 FROM layout_folders WHERE id = ?1)", [folder_id], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            if !exists {
                return Err("Folder not found".into());
            }
        }
        let changed = conn
            .execute("UPDATE layouts SET folder_id = ?2, updated_at = ?3 WHERE id = ?1", rusqlite::params![layout_id, folder_id, chrono::Utc::now().to_rfc3339()])
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("Layout not found".into());
        }
        Ok(())
    }

    pub fn get_layout_folder(&self, id: &str) -> Result<Option<LayoutFolder>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT id, name, parent_id, created_at, updated_at FROM layout_folders WHERE id = ?1",
            [id],
            |row| Ok(LayoutFolder { id: row.get(0)?, name: row.get(1)?, parent_id: row.get(2)?, created_at: row.get(3)?, updated_at: row.get(4)? }),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn list_layouts(&self) -> Result<Vec<Layout>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, name, interactive, page_type, timing_mode, duration, shuffle, canvas_orientation, folder_id, items, created_at, updated_at FROM layouts ORDER BY updated_at DESC LIMIT 10000")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Layout {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    interactive: row.get::<_, i32>(2)? != 0,
                    canvas_orientation: row.get::<_, Option<String>>(7)?.filter(|v| v == "portrait" || v == "landscape").unwrap_or_else(|| "landscape".into()),
                    folder_id: row.get::<_, Option<String>>(8)?,
                    page_type: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "single".into()),
                    timing_mode: row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "global".into()),
                    duration: row.get::<_, i32>(5)?.max(1),
                    shuffle: row.get::<_, i32>(6)? != 0,
                    items: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                    widgets: vec![],
                })
            })
            .map_err(|e| e.to_string())?;
        let mut layouts = Vec::new();
        for row in rows {
            let mut layout = row.map_err(|e| e.to_string())?;
            layout.widgets = self.get_widgets_for_layout(&conn, &layout.id)?;
            layouts.push(layout);
        }
        Ok(layouts)
    }

    pub fn get_layout(&self, id: &str) -> Result<Option<Layout>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, name, interactive, page_type, timing_mode, duration, shuffle, canvas_orientation, folder_id, items, created_at, updated_at FROM layouts WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map([id], |row| {
                Ok(Layout {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    interactive: row.get::<_, i32>(2)? != 0,
                    canvas_orientation: row.get::<_, Option<String>>(7)?.filter(|v| v == "portrait" || v == "landscape").unwrap_or_else(|| "landscape".into()),
                    folder_id: row.get::<_, Option<String>>(8)?,
                    page_type: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "single".into()),
                    timing_mode: row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "global".into()),
                    duration: row.get::<_, i32>(5)?.max(1),
                    shuffle: row.get::<_, i32>(6)? != 0,
                    items: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                    widgets: vec![],
                })
            })
            .map_err(|e| e.to_string())?;
        if let Some(row) = rows.next() {
            let mut layout = row.map_err(|e| e.to_string())?;
            layout.widgets = self.get_widgets_for_layout(&conn, &layout.id)?;
            Ok(Some(layout))
        } else {
            Ok(None)
        }
    }

    pub fn publish_layout_revision(
        &self,
        layout: &Layout,
        published_by: Option<&str>,
    ) -> Result<crate::models::LayoutRevision, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let revision: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(revision), 0) + 1 FROM layout_revisions WHERE layout_id = ?1",
                [&layout.id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let created_at = chrono::Utc::now().to_rfc3339();
        let snapshot = serde_json::to_string(layout).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO layout_revisions (layout_id, revision, snapshot, created_at, published_by) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![layout.id, revision, snapshot, created_at, published_by],
        )
        .map_err(|e| e.to_string())?;
        Ok(crate::models::LayoutRevision {
            layout_id: layout.id.clone(),
            revision,
            snapshot: layout.clone(),
            created_at,
            published_by: published_by.map(str::to_string),
        })
    }

    pub fn list_layout_revisions(&self, layout_id: &str) -> Result<Vec<crate::models::LayoutRevision>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT revision, snapshot, created_at, published_by FROM layout_revisions WHERE layout_id = ?1 ORDER BY revision DESC LIMIT 1000")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([layout_id], |row| {
                let snapshot: String = row.get(1)?;
                let layout: Layout = serde_json::from_str(&snapshot).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                Ok(crate::models::LayoutRevision { layout_id: layout_id.to_string(), revision: row.get(0)?, snapshot: layout, created_at: row.get(2)?, published_by: row.get(3)? })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn get_layout_revision(&self, layout_id: &str, revision: i64) -> Result<Option<Layout>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let snapshot: Option<String> = conn
            .query_row("SELECT snapshot FROM layout_revisions WHERE layout_id = ?1 AND revision = ?2", rusqlite::params![layout_id, revision], |row| row.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        snapshot.map(|value| serde_json::from_str(&value).map_err(|e| e.to_string())).transpose()
    }

    pub fn get_published_layout(&self, layout_id: &str) -> Result<Option<Layout>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let snapshot: Option<String> = conn
            .query_row("SELECT snapshot FROM layout_revisions WHERE layout_id = ?1 ORDER BY revision DESC LIMIT 1", [layout_id], |row| row.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        snapshot
            .map(|value| serde_json::from_str(&value).map_err(|e| e.to_string()))
            .transpose()
    }

    pub fn save_layout(&self, layout: &Layout) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO layouts (id, name, interactive, page_type, timing_mode, duration, shuffle, canvas_orientation, folder_id, items, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                layout.id,
                layout.name,
                layout.interactive as i32,
                layout.page_type,
                layout.timing_mode,
                layout.duration,
                layout.shuffle as i32,
                layout.canvas_orientation,
                layout.folder_id,
                serde_json::to_string(&layout.items).unwrap_or_default(),
                layout.created_at,
                layout.updated_at,
            ],
        )
        .map_err(|e| e.to_string())?;

        // Replace all widgets for this layout
        conn.execute(
            "DELETE FROM widgets WHERE layout_id = ?1",
            [layout.id.clone()],
        )
        .map_err(|e| e.to_string())?;
        for widget in &layout.widgets {
            conn.execute(
                "INSERT INTO widgets (id, layout_id, widget_type, x, y, w, h, z, props) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    widget.id,
                    layout.id,
                    format!("{:?}", widget.widget_type).to_lowercase(),
                    widget.x,
                    widget.y,
                    widget.w,
                    widget.h,
                    widget.z,
                    widget.props.to_string(),
                ],
            )
            .map_err(|e| e.to_string())?;
        }

        // Sync set pages to layout_sets table for the player engine
        if layout.page_type == "set" && !layout.items.is_empty() {
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO layout_sets (id, name, shuffle, dwell_secs, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(id) DO UPDATE SET name=excluded.name, shuffle=excluded.shuffle, dwell_secs=excluded.dwell_secs, updated_at=excluded.updated_at",
                rusqlite::params![
                    layout.id,
                    layout.name,
                    layout.shuffle as i32,
                    layout.duration.max(1) as u32,
                    now,
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "DELETE FROM layout_set_entries WHERE layout_set_id = ?1",
                [&layout.id],
            )
            .map_err(|e| e.to_string())?;
            for (idx, item) in layout.items.iter().enumerate() {
                let entry_id = format!("{}-{}", layout.id, item.layout_id);
                let interval_secs = match layout.timing_mode.as_str() {
                    "per_layout" => item.duration.unwrap_or(layout.duration) as i32,
                    _ => layout.duration as i32,
                };
                conn.execute(
                    "INSERT INTO layout_set_entries (id, layout_set_id, layout_id, order_index, interval_secs) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        entry_id,
                        layout.id,
                        item.layout_id,
                        idx as i32,
                        interval_secs,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
        } else {
            // Remove any stale layout_set entries if page_type changed away from set
            conn.execute(
                "DELETE FROM layout_set_entries WHERE layout_set_id = ?1",
                [&layout.id],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "DELETE FROM layout_sets WHERE id = ?1",
                [&layout.id],
            )
            .map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    pub fn delete_layout(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM widgets WHERE layout_id = ?1", [id])
            .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM layouts WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        // Unassign from any display that was using it
        conn.execute(
            "UPDATE display_state SET layout_id = NULL WHERE layout_id = ?1",
            [id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Map a DB-stored widget_type string back to the enum variant.
    /// Extracted as a free helper so the deserialization match can be
    /// unit-tested without standing up a full Database.
    ///
    /// `checkbox-list` (the JSON form used by the editor and runtime)
    /// and `checkboxlist` (what serde writes after stripping the
    /// hyphen under `rename_all = "lowercase"`) are both accepted.
    pub fn widget_type_from_db_str(s: &str) -> WidgetType {
        match s {
            "text" => WidgetType::Text,
            "image" => WidgetType::Image,
            "video" => WidgetType::Video,
            "clock" => WidgetType::Clock,
            "countdown" => WidgetType::Countdown,
            "webpage" => WidgetType::Webpage,
            "rectangle" => WidgetType::Rectangle,
            "datetime" => WidgetType::DateTime,
            "calendar" => WidgetType::Calendar,
            "weather" => WidgetType::Weather,
            "camera" => WidgetType::Camera,
            "traffic" => WidgetType::Traffic,
            "music" => WidgetType::Music,
            "sound" => WidgetType::Sound,
            "news" => WidgetType::News,
            "button" => WidgetType::Button,
            "dropdown" => WidgetType::Dropdown,
            "checkbox" => WidgetType::Checkbox,
            "checkbox-list" | "checkboxlist" => WidgetType::CheckboxList,
            "radio" => WidgetType::Radio,
            "textbox" => WidgetType::Textbox,
            "keyboard" => WidgetType::Keyboard,
            _ => WidgetType::Text,
        }
    }

    fn get_widgets_for_layout(
        &self,
        conn: &Connection,
        layout_id: &str,
    ) -> Result<Vec<Widget>, String> {
        let mut stmt = conn
            .prepare("SELECT id, widget_type, x, y, w, h, z, props FROM widgets WHERE layout_id = ?1 ORDER BY z ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([layout_id], |row| {
                let wtype_str: String = row.get(1)?;
                let props_str: String = row.get(7)?;
                let widget_type = Self::widget_type_from_db_str(&wtype_str);
                Ok(Widget {
                    id: row.get(0)?,
                    widget_type,
                    x: row.get(2)?,
                    y: row.get(3)?,
                    w: row.get(4)?,
                    h: row.get(5)?,
                    z: row.get(6)?,
                    props: serde_json::from_str(&props_str).unwrap_or(serde_json::Value::Null),
                })
            })
            .map_err(|e| e.to_string())?;
        let mut widgets = Vec::new();
        for row in rows {
            widgets.push(row.map_err(|e| e.to_string())?);
        }
        Ok(widgets)
    }

    // ── Display State ────────────────────────────────────────
    pub fn get_display_state(&self) -> Result<DisplayState, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT display, layout_id FROM display_state")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(DisplayAssignment {
                    display: row.get(0)?,
                    layout_id: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut assignments = Vec::new();
        for row in rows {
            assignments.push(row.map_err(|e| e.to_string())?);
        }
        Ok(DisplayState { assignments })
    }

    pub fn get_display_assignment(
        &self,
        display: &str,
    ) -> Result<Option<DisplayAssignment>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT display, layout_id FROM display_state WHERE display = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map([display], |row| {
                Ok(DisplayAssignment {
                    display: row.get(0)?,
                    layout_id: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?;

        if let Some(row) = rows.next() {
            Ok(Some(row.map_err(|e| e.to_string())?))
        } else {
            Ok(None)
        }
    }

    pub fn set_display_assignment(
        &self,
        display: &str,
        layout_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO display_state (display, layout_id) VALUES (?1, ?2)",
            rusqlite::params![display, layout_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Remove a single `display_state` row by its display name.
    /// Returns the number of rows deleted (0 if the row did not exist).
    /// Useful for clearing stale assignments after a layout is deleted
    /// or a schedule is removed.
    pub fn delete_display_assignment(&self, display: &str) -> Result<usize, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM display_state WHERE display = ?1",
            rusqlite::params![display],
        )
        .map_err(|e| e.to_string())
    }

    // ── Display Size Overrides ───────────────────────────────
    pub fn get_display_overrides(
        &self,
    ) -> Result<std::collections::HashMap<String, (u32, u32)>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT display, width, height FROM display_overrides")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, u32>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (display, width, height) = row.map_err(|e| e.to_string())?;
            map.insert(display, (width, height));
        }
        Ok(map)
    }

    pub fn set_display_override(
        &self,
        display: &str,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO display_overrides (display, width, height) VALUES (?1, ?2, ?3)",
            rusqlite::params![display, width, height],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn clear_display_override(&self, display: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM display_overrides WHERE display = ?1",
            [display],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ── Template CRUD ─────────────────────────────────────────
    pub fn list_templates(&self) -> Result<Vec<TemplateMeta>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, name, category, is_builtin, created_at FROM templates ORDER BY name ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TemplateMeta {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    category: row.get(2)?,
                    is_builtin: row.get::<_, i32>(3)? != 0,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut templates = Vec::new();
        for row in rows {
            templates.push(row.map_err(|e| e.to_string())?);
        }
        Ok(templates)
    }

    pub fn get_template(&self, id: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT layout_json FROM templates WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map([id], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        if let Some(row) = rows.next() {
            Ok(Some(row.map_err(|e| e.to_string())?))
        } else {
            Ok(None)
        }
    }

    pub fn save_template(
        &self,
        id: &str,
        name: &str,
        category: &str,
        is_builtin: bool,
        layout_json: &str,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO templates (id, name, category, is_builtin, layout_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, name, category, if is_builtin { 1 } else { 0 }, layout_json, created_at],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_template(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM templates WHERE id = ?1 AND is_builtin = 0",
            [id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ── Widget Preset CRUD ────────────────────────────────────
    // A widget preset is a reusable set of default properties for a given widget type.
    // When the user creates a new widget, they can pick a preset to pre-fill its props.
    // Custom assets (e.g. weather icons) are stored separately in widget_preset_assets.

    pub fn list_widget_presets(
        &self,
        widget_type: Option<&str>,
    ) -> Result<Vec<WidgetPresetMeta>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let (sql, filter): (&str, Vec<String>) = match widget_type {
            Some(t) => (
                "SELECT id, name, widget_type, is_builtin, created_at, updated_at FROM widget_presets WHERE widget_type = ?1 ORDER BY name ASC",
                vec![t.to_string()],
            ),
            None => (
                "SELECT id, name, widget_type, is_builtin, created_at, updated_at FROM widget_presets ORDER BY widget_type, name",
                vec![],
            ),
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let mapper = |row: &rusqlite::Row| -> rusqlite::Result<WidgetPresetMeta> {
            Ok(WidgetPresetMeta {
                id: row.get(0)?,
                name: row.get(1)?,
                widget_type: row.get(2)?,
                is_builtin: row.get::<_, i32>(3)? != 0,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        };
        let rows = if filter.is_empty() {
            stmt.query_map([], mapper).map_err(|e| e.to_string())?
        } else {
            stmt.query_map([&filter[0]], mapper)
                .map_err(|e| e.to_string())?
        };
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn get_widget_preset(&self, id: &str) -> Result<Option<WidgetPreset>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let meta = conn.query_row(
            "SELECT id, name, widget_type, props_json, created_at, updated_at FROM widget_presets WHERE id = ?1",
            [id],
            |row| {
                Ok(WidgetPreset {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    widget_type: row.get(2)?,
                    props_json: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                    assets: Vec::new(), // populated below
                })
            },
        );
        let preset = match meta {
            Ok(p) => p,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        // Load custom assets for this preset
        let mut stmt = conn
            .prepare("SELECT id, kind, key, url FROM widget_preset_assets WHERE preset_id = ?1 ORDER BY kind, key")
            .map_err(|e| e.to_string())?;
        let asset_rows = stmt
            .query_map([id], |row| {
                Ok(WidgetPresetAsset {
                    id: row.get(0)?,
                    preset_id: id.to_string(),
                    kind: row.get(1)?,
                    key: row.get(2)?,
                    url: row.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut assets = Vec::new();
        for row in asset_rows {
            assets.push(row.map_err(|e| e.to_string())?);
        }
        let mut preset = preset.into_preset_with_assets(assets);
        Self::reconcile_widget_preset_custom_icons(&mut preset)?;
        Ok(Some(preset))
    }

    fn reconcile_widget_preset_custom_icons(preset: &mut WidgetPreset) -> Result<(), String> {
        if preset.widget_type != "weather" {
            return Ok(());
        }
        let icon_rows: Vec<&WidgetPresetAsset> = preset
            .assets
            .iter()
            .filter(|a| {
                a.kind == "weather_icon" && !a.key.trim().is_empty() && !a.url.trim().is_empty()
            })
            .collect();
        let mut props: serde_json::Value =
            serde_json::from_str(&preset.props_json).unwrap_or_else(|_| serde_json::json!({}));
        if !props.is_object() {
            props = serde_json::json!({});
        }
        let obj = props
            .as_object_mut()
            .ok_or_else(|| "preset props_json must be an object".to_string())?;
        if icon_rows.is_empty() {
            obj.remove("custom_icons");
        } else {
            let mut icon_obj = serde_json::Map::new();
            for asset in icon_rows {
                icon_obj.insert(
                    asset.key.clone(),
                    serde_json::Value::String(asset.url.clone()),
                );
            }
            obj.insert(
                "custom_icons".to_string(),
                serde_json::Value::Object(icon_obj),
            );
        }
        preset.props_json = serde_json::to_string(&props).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn create_widget_preset(
        &self,
        id: &str,
        name: &str,
        widget_type: &str,
        props_json: &str,
        now: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO widget_presets (id, name, widget_type, props_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            rusqlite::params![id, name, widget_type, props_json, now],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_widget_preset(
        &self,
        id: &str,
        name: &str,
        widget_type: &str,
        props_json: &str,
        now: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let rows = conn.execute(
            "UPDATE widget_presets SET name = ?1, widget_type = ?2, props_json = ?3, updated_at = ?4 WHERE id = ?5",
            rusqlite::params![name, widget_type, props_json, now, id],
        ).map_err(|e| e.to_string())?;
        if rows == 0 {
            return Err("Widget preset not found".to_string());
        }
        Ok(())
    }

    pub fn delete_widget_preset(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        // ON DELETE CASCADE removes child assets too.
        conn.execute("DELETE FROM widget_presets WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ── Widget preset assets CRUD ─────────────────────────────
    #[allow(dead_code)]
    pub fn list_widget_preset_assets(
        &self,
        preset_id: &str,
    ) -> Result<Vec<WidgetPresetAsset>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, kind, key, url FROM widget_preset_assets WHERE preset_id = ?1 ORDER BY kind, key")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([preset_id], |row| {
                Ok(WidgetPresetAsset {
                    id: row.get(0)?,
                    preset_id: preset_id.to_string(),
                    kind: row.get(1)?,
                    key: row.get(2)?,
                    url: row.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn upsert_widget_preset_asset(
        &self,
        id: &str,
        preset_id: &str,
        kind: &str,
        key: &str,
        url: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        // Upsert: if (preset_id, kind, key) already exists, replace its url; otherwise insert.
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM widget_preset_assets WHERE preset_id = ?1 AND kind = ?2 AND key = ?3",
                rusqlite::params![preset_id, kind, key],
                |row| row.get(0),
            )
            .ok();
        match existing {
            Some(existing_id) => {
                conn.execute(
                    "UPDATE widget_preset_assets SET url = ?1 WHERE id = ?2",
                    rusqlite::params![url, existing_id],
                )
                .map_err(|e| e.to_string())?;
            }
            None => {
                conn.execute(
                    "INSERT INTO widget_preset_assets (id, preset_id, kind, key, url) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, preset_id, kind, key, url],
                ).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    pub fn delete_widget_preset_asset(&self, preset_id: &str, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let rows = conn
            .execute(
                "DELETE FROM widget_preset_assets WHERE preset_id = ?1 AND id = ?2",
                rusqlite::params![preset_id, id],
            )
            .map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }


    // ---- News feed cache (RSS / Atom) ----
    // The maintenance loop fetches feeds and stores them here. The player
    // reads via GET /api/news/headlines?url=<url>. The url is the primary
    // key so multiple News widgets pointing at the same feed share the
    // same cached headlines (and the same fetch cost).

    /// Insert or update a cached news feed. Called by the news fetch
    /// loop and the manual refresh endpoint. Resets `last_error` on
    /// successful update.
    pub fn upsert_news_feed(
        &self,
        url: &str,
        title: &str,
        headlines_json: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO news_feeds (url, title, headlines_json, last_fetched_at, last_error, etag, last_modified)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)
             ON CONFLICT(url) DO UPDATE SET
                 title = excluded.title,
                 headlines_json = excluded.headlines_json,
                 last_fetched_at = excluded.last_fetched_at,
                 last_error = NULL,
                 etag = excluded.etag,
                 last_modified = excluded.last_modified",
            rusqlite::params![url, title, headlines_json, now, etag, last_modified],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Record a fetch error for a feed URL without changing the cached
    /// headlines. Keeps the previous successful payload so a transient
    /// failure doesn't blank the ticker.
    pub fn record_news_feed_error(&self, url: &str, error: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO news_feeds (url, last_fetched_at, last_error)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(url) DO UPDATE SET
                 last_fetched_at = excluded.last_fetched_at,
                 last_error = excluded.last_error",
            rusqlite::params![url, now, error],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_news_feed(&self, url: &str) -> Result<Option<NewsFeed>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT url, title, headlines_json, last_fetched_at, last_error, etag, last_modified
                 FROM news_feeds WHERE url = ?1",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query([url])
            .map_err(|e| e.to_string())?;
        if let Some(row) = rows.next().map_err(|e| e.to_string())? {
            Ok(Some(NewsFeed {
                url: row.get(0).map_err(|e| e.to_string())?,
                title: row.get(1).map_err(|e| e.to_string())?,
                headlines_json: row.get(2).map_err(|e| e.to_string())?,
                last_fetched_at: row.get(3).map_err(|e| e.to_string())?,
                last_error: row.get(4).map_err(|e| e.to_string())?,
                etag: row.get(5).map_err(|e| e.to_string())?,
                last_modified: row.get(6).map_err(|e| e.to_string())?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_news_feeds_needing_refresh(
        &self,
        max_age_secs: i64,
    ) -> Result<Vec<NewsFeed>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        // CAST(... AS INTEGER) because strftime returns TEXT.
        let cutoff = chrono::Utc::now().timestamp() - max_age_secs;
        let mut stmt = conn
            .prepare(
                "SELECT url, title, headlines_json, last_fetched_at, last_error, etag, last_modified
                 FROM news_feeds
                 WHERE last_fetched_at = ''
                    OR CAST(strftime('%s', last_fetched_at) AS INTEGER) < ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([cutoff], |row| {
                Ok(NewsFeed {
                    url: row.get(0)?,
                    title: row.get(1)?,
                    headlines_json: row.get(2)?,
                    last_fetched_at: row.get(3)?,
                    last_error: row.get(4)?,
                    etag: row.get(5)?,
                    last_modified: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn list_news_feeds(&self) -> Result<Vec<NewsFeed>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT url, title, headlines_json, last_fetched_at, last_error, etag, last_modified
                 FROM news_feeds ORDER BY url LIMIT 4096",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(NewsFeed {
                    url: row.get(0)?,
                    title: row.get(1)?,
                    headlines_json: row.get(2)?,
                    last_fetched_at: row.get(3)?,
                    last_error: row.get(4)?,
                    etag: row.get(5)?,
                    last_modified: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Register a feed URL for fetching. Creates an empty row if new,
    /// leaves the existing row alone. Called when a layout is saved:
    /// the api layer scans all news widgets in the saved layout and
    /// ensures their feedUrl is in news_feeds so the next maintenance
    /// pass picks it up.
    pub fn ensure_news_feed_registered(&self, url: &str) -> Result<Option<NewsFeed>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO news_feeds (url, last_fetched_at) VALUES (?1, '')",
            [url],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_news_feed(url)
    }

    // ── Refresh epoch (force player to re-render) ─────────────
    pub fn get_refresh_epoch(&self) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let epoch: i64 = conn
            .query_row("SELECT epoch FROM refresh_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        Ok(epoch)
    }

    pub fn bump_refresh_epoch(&self) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE refresh_state SET epoch = epoch + 1 WHERE id = 1",
            [],
        )
        .map_err(|e| e.to_string())?;
        let epoch: i64 = conn
            .query_row("SELECT epoch FROM refresh_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        Ok(epoch)
    }

    // ── Backup CRUD ───────────────────────────────────────────
    pub fn list_backups(&self) -> Result<Vec<Backup>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, name, created_at, file_path, size_bytes FROM backups ORDER BY created_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Backup {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    file_path: row.get(3)?,
                    size_bytes: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut backups = Vec::new();
        for row in rows {
            backups.push(row.map_err(|e| e.to_string())?);
        }
        Ok(backups)
    }
    pub fn sanitize_imported_db(path: &std::path::Path) -> Result<(), String> {
        let conn = Connection::open(path).map_err(|e| format!("open: {e}"))?;
        let _ = conn.execute("DROP TABLE IF EXISTS kiosk_pages", []);
        let _ = conn.execute("DROP TABLE IF EXISTS kiosk_submissions", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_kiosk_submissions_collected", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_kiosk_submissions_status", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_kiosk_submissions_page", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_kiosk_submissions_client", []);
        Ok(())
    }

    pub fn insert_backup(&self, backup: &Backup) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO backups (id, name, created_at, file_path, size_bytes) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                backup.id,
                backup.name,
                backup.created_at,
                backup.file_path,
                backup.size_bytes as i64,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_backup_file_path(&self, id: &str) -> Result<String, String> {
        let conn = self.conn()?;
        conn.query_row("SELECT file_path FROM backups WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .map_err(|e| e.to_string())
    }

    #[allow(dead_code)]
    pub fn create_backup(
        &self,
        name: &str,
        file_path: &str,
        size_bytes: i64,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO backups (id, name, created_at, file_path, size_bytes) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, name, now, file_path, size_bytes],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_backup(&self, id: &str) -> Result<u64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM backups WHERE id = ?1", [id])
            .map(|rows| rows as u64)
            .map_err(|e| e.to_string())
    }

    // ── Schedule CRUD ──────────────────────────────────────────
    pub fn list_schedules(&self) -> Result<Vec<ScheduleEntry>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, display, layout_id, day_of_week, start_time, end_time, created_at, layout_set_id, action, target_client, power_state, display_name, control_method, display_index, brightness, contrast, end_action FROM schedules ORDER BY start_time ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ScheduleEntry {
                    id: row.get(0)?,
                    display: row.get(1)?,
                    layout_id: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    day_of_week: row.get(3)?,
                    start_time: row.get(4)?,
                    end_time: row.get(5)?,
                    created_at: row.get(6)?,
                    layout_set_id: row.get(7)?,
                    action: row.get(8)?,
                    target_client: row.get(9)?,
                    power_state: row.get(10)?,
                    display_name: row.get(11)?,
                    control_method: row.get::<_, Option<String>>(12)?,
                    display_index: row.get::<_, Option<i32>>(13)?.map(|v| v),
                    brightness: row.get::<_, Option<i32>>(14)?,
                    contrast: row.get::<_, Option<i32>>(15)?,
                    end_action: row.get::<_, String>(16)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| e.to_string())?);
        }
        Ok(entries)
    }

    pub fn create_schedule(&self, entry: &ScheduleEntry) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        // `layout_id` carries a FOREIGN KEY to `layouts(id)`, so it
        // cannot be the empty string. Schedules that use a layout
        // set pass an empty `layout_id`; we store NULL instead so
        // the FK is satisfied.
        let layout_id: Option<&str> = if entry.layout_id.is_empty() {
            None
        } else {
            Some(&entry.layout_id)
        };
        conn.execute(
            "INSERT INTO schedules (id, display, layout_id, day_of_week, start_time, end_time, created_at, layout_set_id, action, target_client, power_state, updated_at, display_name, control_method, display_index, brightness, contrast, end_action) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            rusqlite::params![
                entry.id,
                entry.display,
                layout_id,
                entry.day_of_week,
                entry.start_time,
                entry.end_time,
                entry.created_at,
                entry.layout_set_id.as_deref().unwrap_or(""),
                &entry.action,
                entry.target_client.as_deref().unwrap_or(""),
                entry.power_state.as_deref().unwrap_or(""),
                entry.created_at,
                entry.display_name.as_deref().unwrap_or(""),
                entry.control_method.as_deref().unwrap_or("auto"),
                entry.display_index.unwrap_or(0),
                entry.brightness,
                entry.contrast,
                &entry.end_action,
            ],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM schedule_runtime_state WHERE schedule_id = ?1", [&entry.id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_schedule(&self, id: &str) -> Result<Option<ScheduleEntry>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, display, layout_id, day_of_week, start_time, end_time, created_at, layout_set_id, action, target_client, power_state, display_name, control_method, display_index, brightness, contrast, end_action FROM schedules WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map([id], |row| {
                Ok(ScheduleEntry {
                    id: row.get(0)?,
                    display: row.get(1)?,
                    layout_id: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    day_of_week: row.get(3)?,
                    start_time: row.get(4)?,
                    end_time: row.get(5)?,
                    created_at: row.get(6)?,
                    layout_set_id: row.get(7)?,
                    action: row.get(8)?,
                    target_client: row.get(9)?,
                    power_state: row.get(10)?,
                    display_name: row.get(11)?,
                    control_method: row.get::<_, Option<String>>(12)?,
                    display_index: row.get::<_, Option<i32>>(13)?,
                    brightness: row.get::<_, Option<i32>>(14)?,
                    contrast: row.get::<_, Option<i32>>(15)?,
                    end_action: row.get::<_, String>(16)?,
                })
            })
            .map_err(|e| e.to_string())?;
        if let Some(row) = rows.next() {
            Ok(Some(row.map_err(|e| e.to_string())?))
        } else {
            Ok(None)
        }
    }

    pub fn update_schedule(&self, entry: &ScheduleEntry) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let updated_at = chrono::Utc::now().to_rfc3339();
        // See create_schedule: empty layout_id becomes NULL to
        // satisfy the FK.
        let layout_id: Option<&str> = if entry.layout_id.is_empty() {
            None
        } else {
            Some(&entry.layout_id)
        };
        conn.execute(
            "INSERT INTO schedules (id, display, layout_id, day_of_week, start_time, end_time, created_at, layout_set_id, action, target_client, power_state, updated_at, display_name, control_method, display_index, brightness, contrast, end_action) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18) ON CONFLICT(id) DO UPDATE SET display=excluded.display, layout_id=excluded.layout_id, day_of_week=excluded.day_of_week, start_time=excluded.start_time, end_time=excluded.end_time, layout_set_id=excluded.layout_set_id, action=excluded.action, target_client=excluded.target_client, power_state=excluded.power_state, updated_at=excluded.updated_at, display_name=COALESCE(NULLIF(excluded.display_name, ''), schedules.display_name), control_method=excluded.control_method, display_index=excluded.display_index, brightness=excluded.brightness, contrast=excluded.contrast, end_action=excluded.end_action",
            rusqlite::params![
                entry.id,
                entry.display,
                layout_id,
                entry.day_of_week,
                entry.start_time,
                entry.end_time,
                entry.created_at,
                entry.layout_set_id.as_deref().unwrap_or(""),
                &entry.action,
                entry.target_client.as_deref().unwrap_or(""),
                entry.power_state.as_deref().unwrap_or(""),
                updated_at,
                entry.display_name.as_deref().unwrap_or(""),
                entry.control_method.as_deref().unwrap_or("auto"),
                entry.display_index.unwrap_or(0),
                entry.brightness,
                entry.contrast,
                &entry.end_action,
            ],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM schedule_runtime_state WHERE schedule_id = ?1", [&entry.id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_schedule(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM schedules WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM schedule_runtime_state WHERE schedule_id = ?1", [id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_schedule_runtime_active(&self, id: &str) -> Result<Option<bool>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT active FROM schedule_runtime_state WHERE schedule_id = ?1",
            [id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.map(|active| active != 0))
        .map_err(|e| e.to_string())
    }

    pub fn set_schedule_runtime_active(&self, id: &str, active: bool) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO schedule_runtime_state (schedule_id, active, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(schedule_id) DO UPDATE SET active=excluded.active, updated_at=excluded.updated_at",
            rusqlite::params![id, if active { 1 } else { 0 }, chrono::Utc::now().to_rfc3339()],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    pub fn get_active_schedules_full(
        &self,
        current_time: &str,
        day_of_week: i32,
    ) -> Result<Vec<ScheduleEntry>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        // day_of_week_mask is a bitmask: bit 0 = Sunday, bit 1 = Monday, ... bit 6 = Saturday.
        // A schedule is active if its mask contains the current day:
        //   -1 (DAY_EVERY) matches all days
        //   otherwise check (mask & (1 << day)) != 0
        let day_bit = 1i32 << day_of_week;
        let previous_day_bit = 1i32 << ((day_of_week + 6) % 7);
        let mut stmt = conn
            .prepare("SELECT id, display, layout_id, day_of_week, start_time, end_time, created_at, layout_set_id, action, target_client, power_state, display_name, control_method, display_index, brightness, contrast, end_action FROM schedules WHERE (start_time <= end_time AND (day_of_week = -1 OR (day_of_week & ?1) != 0) AND start_time <= ?3 AND end_time >= ?3) OR (start_time > end_time AND (((day_of_week = -1 OR (day_of_week & ?1) != 0) AND start_time <= ?3) OR ((day_of_week = -1 OR (day_of_week & ?2) != 0) AND end_time >= ?3)))")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([day_bit.to_string(), previous_day_bit.to_string(), current_time.to_string()], |row| {
                Ok(ScheduleEntry {
                    id: row.get(0)?,
                    display: row.get(1)?,
                    // layout_id is nullable (NULL when a schedule uses a
                    // layout_set_id instead). Convert to the empty string
                    // so the rest of the code can keep using String.
                    layout_id: row
                        .get::<_, Option<String>>(2)?
                        .unwrap_or_default(),
                    day_of_week: row.get(3)?,
                    start_time: row.get(4)?,
                    end_time: row.get(5)?,
                    created_at: row.get(6)?,
                    layout_set_id: row.get(7)?,
                    action: row.get(8)?,
                    target_client: row.get(9)?,
                    power_state: row.get(10)?,
                    display_name: row.get(11)?,
                    control_method: row.get::<_, Option<String>>(12)?,
                    display_index: row.get::<_, Option<i32>>(13)?,
                    brightness: row.get::<_, Option<i32>>(14)?,
                    contrast: row.get::<_, Option<i32>>(15)?,
                    end_action: row.get::<_, String>(16)?,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut active = Vec::new();
        for row in rows {
            active.push(row.map_err(|e| e.to_string())?);
        }
        Ok(active)
    }

    // ── Interactive Submissions ────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub fn insert_interactive_submission(
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
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let received_at = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO interactive_submissions (client_id, client_address, interactive_layout_id, interactive_layout_name, widget_id, widget_type, widget_label, payload_json, collected_at, received_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                client_id,
                client_address,
                interactive_layout_id,
                interactive_layout_name,
                widget_id,
                widget_type,
                widget_label,
                payload_json,
                collected_at,
                received_at,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_interactive_submissions(
        &self,
        limit: i64,
        client_filter: Option<&str>,
        page_filter: Option<&str>,
        status_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut sql = String::from(
            "SELECT id, client_id, client_address, interactive_layout_id, interactive_layout_name, widget_id, widget_type, widget_label, payload_json, collected_at, received_at, google_sync_status, google_sync_message, google_synced_at, google_sheet_id FROM interactive_submissions WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(c) = client_filter {
            sql.push_str(" AND client_id = ?");
            params.push(Box::new(c.to_string()));
        }
        if let Some(p) = page_filter {
            sql.push_str(" AND interactive_layout_id = ?");
            params.push(Box::new(p.to_string()));
        }
        if let Some(s) = status_filter {
            sql.push_str(" AND google_sync_status = ?");
            params.push(Box::new(s.to_string()));
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        params.push(Box::new(limit));
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_refs), |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "client_id": r.get::<_, String>(1)?,
                    "client_address": r.get::<_, Option<String>>(2)?,
                    "interactive_layout_id": r.get::<_, Option<String>>(3)?,
                    "interactive_layout_name": r.get::<_, Option<String>>(4)?,
                    "widget_id": r.get::<_, String>(5)?,
                    "widget_type": r.get::<_, String>(6)?,
                    "widget_label": r.get::<_, Option<String>>(7)?,
                    "payload_json": r.get::<_, String>(8)?,
                    "collected_at": r.get::<_, String>(9)?,
                    "received_at": r.get::<_, String>(10)?,
                    "google_sync_status": r.get::<_, String>(11)?,
                    "google_sync_message": r.get::<_, Option<String>>(12)?,
                    "google_synced_at": r.get::<_, Option<String>>(13)?,
                    "google_sheet_id": r.get::<_, Option<String>>(14)?,
                }))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn update_interactive_submission_sync_status(
        &self,
        ids: &[i64],
        status: &str,
        message: Option<&str>,
        sheet_id: Option<&str>,
    ) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let synced_at = chrono::Utc::now().to_rfc3339();
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "UPDATE interactive_submissions SET google_sync_status = ?1, google_sync_message = ?2, google_synced_at = ?3, google_sheet_id = ?4 WHERE id IN ({})",
            placeholders
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(4 + ids.len());
        params.push(Box::new(status.to_string()));
        params.push(Box::new(message.unwrap_or("").to_string()));
        params.push(Box::new(synced_at));
        params.push(Box::new(sheet_id.unwrap_or("").to_string()));
        for id in ids {
            params.push(Box::new(*id));
        }
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let updated = conn
            .execute(&sql, rusqlite::params_from_iter(param_refs))
            .map_err(|e| e.to_string())?;
        Ok(updated)
    }

    pub fn count_interactive_submissions(&self, status_filter: Option<&str>) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let (sql, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(s) = status_filter
        {
            (
                "SELECT COUNT(*) FROM interactive_submissions WHERE google_sync_status = ?".to_string(),
                vec![Box::new(s.to_string())],
            )
        } else {
            ("SELECT COUNT(*) FROM interactive_submissions".to_string(), vec![])
        };
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let n: i64 = conn
            .query_row(&sql, rusqlite::params_from_iter(param_refs), |r| r.get(0))
            .map_err(|e| e.to_string())?;
        Ok(n)
    }

    #[allow(dead_code)]
    pub fn get_interactive_submission(&self, id: i64) -> Result<Option<serde_json::Value>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT id, client_id, client_address, interactive_layout_id, interactive_layout_name, widget_id, widget_type, widget_label, payload_json, collected_at, received_at, google_sync_status, google_sync_message, google_synced_at, google_sheet_id FROM interactive_submissions WHERE id = ?")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map([id], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "client_id": r.get::<_, String>(1)?,
                    "client_address": r.get::<_, Option<String>>(2)?,
                    "interactive_layout_id": r.get::<_, Option<String>>(3)?,
                    "interactive_layout_name": r.get::<_, Option<String>>(4)?,
                    "widget_id": r.get::<_, String>(5)?,
                    "widget_type": r.get::<_, String>(6)?,
                    "widget_label": r.get::<_, Option<String>>(7)?,
                    "payload_json": r.get::<_, String>(8)?,
                    "collected_at": r.get::<_, String>(9)?,
                    "received_at": r.get::<_, String>(10)?,
                    "google_sync_status": r.get::<_, String>(11)?,
                    "google_sync_message": r.get::<_, Option<String>>(12)?,
                    "google_synced_at": r.get::<_, Option<String>>(13)?,
                    "google_sheet_id": r.get::<_, Option<String>>(14)?,
                }))
            })
            .map_err(|e| e.to_string())?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| e.to_string())?)),
            None => Ok(None),
        }
    }

    pub fn delete_interactive_submissions(&self, ids: &[i64]) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM interactive_submissions WHERE id IN ({})",
            placeholders
        );
        let n = conn
            .execute(&sql, rusqlite::params_from_iter(ids.iter()))
            .map_err(|e| e.to_string())?;
        Ok(n)
    }

    // ── Virtual Clients ─────────────────────────────────────────────────

    pub fn create_virtual_client(&self, id: &str, name: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO clients (id, name, address, role, is_virtual, last_seen) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            rusqlite::params![id, name, "virtual", "player", now],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO client_displays (client_id, display_index, connector, label, width, height, refresh_rate, enabled, last_seen) VALUES (?1, 0, ?2, ?3, 1920, 1080, 60.0, 1, ?4)",
            rusqlite::params![id, "Virtual-1", format!("{} display", name), now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_virtual_client(&self, id: &str) -> Result<Option<(String, bool)>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT name, is_virtual FROM clients WHERE id = ?1 AND is_virtual = 1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query([id])
            .map_err(|e| e.to_string())?;
        if let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let name: String = row.get(0).map_err(|e| e.to_string())?;
            let is_virtual: bool = row.get::<_, i32>(1).map_err(|e| e.to_string())? != 0;
            Ok(Some((name, is_virtual)))
        } else {
            Ok(None)
        }
    }

    pub fn update_virtual_client_name(&self, id: &str, name: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let updated = conn
            .execute(
                "UPDATE clients SET name = ?1 WHERE id = ?2 AND is_virtual = 1",
                rusqlite::params![name, id],
            )
            .map_err(|e| e.to_string())?;
        Ok(updated > 0)
    }

    pub fn is_virtual_client(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let is_virtual: i32 = conn
            .query_row(
                "SELECT is_virtual FROM clients WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(is_virtual != 0)
    }

    // ── Persistent sessions ─────────────────────────────────────────────────

    /// Returns the current session's revoked flag, or `None` when it is
    /// missing or expired. The lookup is atomic under the SQLite connection
    /// mutex so authentication does not need two separate queries.
    pub fn session_state(&self, token: &str) -> Result<Option<bool>, String> {
        let conn = self.conn().map_err(|error| error.to_string())?;
        let now = chrono::Utc::now().timestamp();
        conn.query_row(
            "SELECT revoked FROM sessions WHERE token = ?1 AND expires_at > ?2",
            params![token, now],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.map(|revoked| revoked != 0))
        .map_err(|error| error.to_string())
    }

    pub fn session_principal(
        &self,
        token: &str,
    ) -> Result<Option<(String, Option<String>, Option<bool>)>, String> {
        let conn = self.conn().map_err(|error| error.to_string())?;
        let now = chrono::Utc::now().timestamp();
        conn.query_row(
            "SELECT s.username, u.role, u.active FROM sessions s LEFT JOIN users u ON lower(u.username)=lower(s.username) WHERE s.token=?1 AND s.expires_at>?2 AND s.revoked=0",
            params![token, now],
            |row| {
                let active = row.get::<_, Option<i64>>(2)?.map(|value| value != 0);
                Ok((row.get(0)?, row.get(1)?, active))
            },
        )
        .optional()
        .map_err(|error| error.to_string())
    }

    pub fn is_session_revoked(&self, token: &str) -> Option<bool> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare("SELECT revoked FROM sessions WHERE token = ?1 AND expires_at > ?2")
            .ok()?;
        let now = chrono::Utc::now().timestamp();
        let mut rows = stmt.query([token, &now.to_string()]).ok()?;
        rows.next().ok().flatten().and_then(|row| row.get::<_, i32>(0).ok()).map(|v| v == 1)
    }

    pub fn session_exists(&self, token: &str) -> Option<bool> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare("SELECT 1 FROM sessions WHERE token = ?1 AND expires_at > ?2 AND revoked = 0")
            .ok()?;
        let now = chrono::Utc::now().timestamp();
        let mut rows = stmt.query([token, &now.to_string()]).ok()?;
        Some(rows.next().ok().flatten().is_some())
    }

    pub fn revoke_session(&self, token: &str) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "UPDATE sessions SET revoked = 1 WHERE token = ?1",
                [token],
            );
        }
    }

    /// Delete all `sessions` rows whose `expires_at` is in the past.
    /// Returns the number of rows deleted. Idempotent and safe to call
    /// repeatedly. The caller may want to VACUUM afterwards to reclaim
    /// disk space.
    pub fn purge_expired_sessions(&self) -> Result<usize, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let purged = conn
            .execute(
                "DELETE FROM sessions WHERE expires_at < strftime('%s','now')",
                [],
            )
            .map_err(|e| e.to_string())?;
        Ok(purged)
    }

    /// Record that a schedule fired at `occurred_at`. `target` is the
    /// display name (for switch_layout) or the target client id (for
    /// power_on / power_off). Returns the new row id. Used by the
    /// scheduler to provide an audit trail of every schedule action.
    pub fn log_schedule_run(
        &self,
        schedule_id: &str,
        action: &str,
        target: &str,
        success: bool,
        message: Option<&str>,
        occurred_at: &str,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let success_int: i64 = if success { 1 } else { 0 };
        conn.execute(
            "INSERT INTO schedule_runs (schedule_id, action, target, success, message, occurred_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![schedule_id, action, target, success_int, message, occurred_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    /// Return the most recent schedule_runs rows, newest first.
    /// `limit` is clamped to [1, 500] by the caller.
    pub fn list_schedule_runs(&self, limit: i64) -> Result<Vec<ScheduleRun>, String> {
        let cap = limit.clamp(1, 500);
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, schedule_id, action, target, success, message, occurred_at
                 FROM schedule_runs ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![cap], |row| {
                Ok(ScheduleRun {
                    id: row.get::<_, i64>(0)?,
                    schedule_id: row.get(1)?,
                    action: row.get(2)?,
                    target: row.get(3)?,
                    success: row.get::<_, i64>(4)? != 0,
                    message: row.get(5)?,
                    occurred_at: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Store a sanitized audit row. Audit callers pass identifiers and fixed
    /// outcome labels, never request bodies or credentials.
    pub fn record_audit_event(
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
        let actor = actor.map(|v| bounded(v, 128));
        let role = role.map(|v| bounded(v, 64));
        let event_type = bounded(event_type, 96);
        let target = target.map(|v| bounded(v, 256));
        let outcome = bounded(outcome, 32);
        let detail = detail.map(|v| bounded(v, 512));
        let occurred_at = bounded(occurred_at, 64);
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO audit_events (actor, role, event_type, target, outcome, detail, occurred_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![actor, role, event_type, target, outcome, detail, occurred_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    /// Return recent audit rows, newest first. The hard cap prevents an
    /// administrative listing from becoming an unbounded data export.
    pub fn list_audit_events(&self, limit: i64) -> Result<Vec<AuditEvent>, String> {
        let cap = limit.clamp(1, 500);
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, actor, role, event_type, target, outcome, detail, occurred_at
                 FROM audit_events ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![cap], |row| {
                Ok(AuditEvent {
                    id: row.get(0)?,
                    actor: row.get(1)?,
                    role: row.get(2)?,
                    event_type: row.get(3)?,
                    target: row.get(4)?,
                    outcome: row.get(5)?,
                    detail: row.get(6)?,
                    occurred_at: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// seconds. The caller surfaces this as a "stale / offline" count.
    /// The heartbeat write path updates `last_seen` on every ping, so
    /// the count is a lower bound on offline players.
    ///
    /// Note: `strftime('%s', last_seen)` returns TEXT. SQLite will do
    /// text comparison against a numeric parameter unless we CAST, so
    /// the cast on both sides is required to get a numeric <.
    pub fn count_stale_clients(&self, threshold_secs: i64) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let cutoff = chrono::Utc::now().timestamp() - threshold_secs;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clients
                 WHERE CAST(strftime('%s', last_seen) AS INTEGER) < ?1
                   AND is_virtual = 0",
                rusqlite::params![cutoff],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(n)
    }

    pub fn create_session(&self, token: &str, username: &str, expires_at: i64) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sessions (token, username, expires_at, revoked) VALUES (?1, ?2, ?3, 0)",
            rusqlite::params![token, username, expires_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ── Layout Sets ────────────────────────────────────────────

    pub fn list_layout_sets(&self) -> Result<Vec<LayoutSetSummary>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT ls.id, ls.name, ls.shuffle, ls.dwell_secs, ls.created_at, ls.updated_at, COUNT(lse.id) as item_count FROM layout_sets ls LEFT JOIN layout_set_entries lse ON ls.id = lse.layout_set_id GROUP BY ls.id ORDER BY ls.name")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut results = Vec::new();
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            results.push(LayoutSetSummary {
                id: row.get(0).map_err(|e| e.to_string())?,
                name: row.get(1).map_err(|e| e.to_string())?,
                shuffle: row.get::<_, i32>(2).map_err(|e| e.to_string())? != 0,
                dwell_secs: row.get::<_, u32>(3).map_err(|e| e.to_string())?,
                item_count: row.get::<_, u32>(6).map_err(|e| e.to_string())?,
                created_at: row.get(4).map_err(|e| e.to_string())?,
                updated_at: row.get(5).map_err(|e| e.to_string())?,
            });
        }
        Ok(results)
    }

    pub fn get_layout_set(&self, id: &str) -> Result<Option<LayoutSetWithItems>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;

        // Get set metadata
        let mut stmt = conn
            .prepare("SELECT id, name, shuffle, dwell_secs, created_at, updated_at FROM layout_sets WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([id]).map_err(|e| e.to_string())?;
        let row = rows.next().map_err(|e| e.to_string())?;
        let row = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        let set = LayoutSetWithItems {
            id: row.get(0).map_err(|e| e.to_string())?,
            name: row.get(1).map_err(|e| e.to_string())?,
            shuffle: row.get::<_, i32>(2).map_err(|e| e.to_string())? != 0,
            dwell_secs: row.get::<_, u32>(3).map_err(|e| e.to_string())?,
            items: Vec::new(),
            created_at: row.get(4).map_err(|e| e.to_string())?,
            updated_at: row.get(5).map_err(|e| e.to_string())?,
        };

        // Get entries
        let mut stmt = conn
            .prepare("SELECT id, layout_id, order_index, interval_secs FROM layout_set_entries WHERE layout_set_id = ?1 ORDER BY order_index")
            .map_err(|e| e.to_string())?;
        let mut entries = stmt.query([id]).map_err(|e| e.to_string())?;
        let mut items = Vec::new();
        while let Some(row) = entries.next().map_err(|e| e.to_string())? {
            items.push(LayoutSetEntry {
                id: row.get(0).map_err(|e| e.to_string())?,
                layout_set_id: id.to_string(),
                layout_id: row.get(1).map_err(|e| e.to_string())?,
                position: row.get::<_, u32>(2).map_err(|e| e.to_string())?,
                interval_secs: Some(row.get::<_, u32>(3).map_err(|e| e.to_string())?),
                transition: None,
            });
        }

        Ok(Some(LayoutSetWithItems { items, ..set }))
    }

    pub fn create_layout_set(&self, set: &LayoutSetWithItems) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO layout_sets (id, name, shuffle, dwell_secs, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                set.id,
                set.name,
                set.shuffle as i32,
                set.dwell_secs,
                set.created_at,
                set.updated_at
            ],
        )
        .map_err(|e| e.to_string())?;
        for (i, item) in set.items.iter().enumerate() {
            tx.execute(
                "INSERT INTO layout_set_entries (id, layout_set_id, layout_id, order_index, interval_secs) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![item.id, set.id, item.layout_id, i as u32, item.interval_secs.or(Some(30))],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn update_layout_set(&self, set: &LayoutSetWithItems) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE layout_sets SET name = ?2, shuffle = ?3, dwell_secs = ?4, updated_at = ?5 WHERE id = ?1",
            rusqlite::params![set.id, set.name, set.shuffle as i32, set.dwell_secs, set.updated_at],
        )
        .map_err(|e| e.to_string())?;

        tx.execute(
            "DELETE FROM layout_set_entries WHERE layout_set_id = ?1",
            [&set.id],
        )
        .map_err(|e| e.to_string())?;
        for (i, item) in set.items.iter().enumerate() {
            tx.execute(
                "INSERT INTO layout_set_entries (id, layout_set_id, layout_id, order_index, interval_secs) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![item.id, set.id, item.layout_id, i as u32, item.interval_secs.or(Some(30))],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn delete_layout_set(&self, id: &str) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM layout_set_entries WHERE layout_set_id = ?1",
            [id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM layout_sets WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Fetch all layout sets whose id appears in the given layout_ids (via layout_set_entries).
    /// Returns a map of layout_id -> LayoutSetWithItems.
    pub fn get_layout_sets_for_layouts(
        &self,
        layout_ids: &[String],
    ) -> Result<std::collections::HashMap<String, LayoutSetWithItems>, String> {
        if layout_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let placeholders: Vec<String> = layout_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT ls.id, ls.name, ls.shuffle, ls.dwell_secs, ls.created_at, ls.updated_at,
                    lse.layout_id, lse.order_index, lse.interval_secs
             FROM layout_sets ls
             JOIN layout_set_entries lse ON lse.layout_set_id = ls.id
             WHERE EXISTS (
                 SELECT 1 FROM layout_set_entries assigned
                 WHERE assigned.layout_set_id = ls.id
                   AND assigned.layout_id IN ({})
             )",
            placeholders.join(",")
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let params: Vec<&dyn rusqlite::ToSql> = layout_ids
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,   // set_id
                    row.get::<_, String>(1)?,   // name
                    row.get::<_, i32>(2)? != 0, // shuffle
                    row.get::<_, u32>(3)?,      // dwell_secs
                    row.get::<_, String>(4)?,   // created_at
                    row.get::<_, String>(5)?,   // updated_at
                    row.get::<_, String>(6)?,   // layout_id
                    row.get::<_, i32>(7)?,      // order_index
                    row.get::<_, Option<i32>>(8)?, // interval_secs
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut sets_map: std::collections::HashMap<String, LayoutSetWithItems> =
            std::collections::HashMap::new();
        let mut set_entries: std::collections::HashMap<String, Vec<LayoutSetEntry>> =
            std::collections::HashMap::new();
        for row in rows {
            let (set_id, name, shuffle, dwell_secs, created_at, updated_at, layout_id, position, interval_secs) =
                row.map_err(|e| e.to_string())?;
            let entry = LayoutSetEntry {
                id: format!("{}-{}", set_id, layout_id),
                layout_set_id: set_id.clone(),
                layout_id,
                position: position as u32,
                interval_secs: interval_secs.map(|value| value as u32),
                transition: None,
            };
            set_entries
                .entry(set_id.clone())
                .or_default()
                .push(entry);
            if !sets_map.contains_key(&set_id) {
                sets_map.insert(
                    set_id.clone(),
                    LayoutSetWithItems {
                        id: set_id,
                        name,
                        shuffle,
                        dwell_secs,
                        items: vec![],
                        created_at,
                        updated_at,
                    },
                );
            }
        }
        for (set_id, entries) in set_entries {
            if let Some(set) = sets_map.get_mut(&set_id) {
                set.items = entries;
                set.items.sort_by_key(|entry| entry.position);
            }
        }
        let requested: std::collections::HashSet<&str> =
            layout_ids.iter().map(String::as_str).collect();
        let mut result = std::collections::HashMap::new();
        for set in sets_map.values() {
            for entry in &set.items {
                if requested.contains(entry.layout_id.as_str()) {
                    result
                        .entry(entry.layout_id.clone())
                        .or_insert_with(|| set.clone());
                }
            }
        }
        Ok(result)
    }

    pub fn add_layout_set_entry(
        &self,
        layout_set_id: &str,
        entry: &AddLayoutSetEntry,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let id = uuid::Uuid::new_v4().to_string();
        let id_ref: &str = &id;
        let lsid_ref: &str = layout_set_id;
        let lid_ref: &str = &entry.layout_id;
        conn.execute(
            "INSERT INTO layout_set_entries (id, layout_set_id, layout_id, order_index, interval_secs) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id_ref, lsid_ref, lid_ref, entry.position, entry.interval_secs.or(Some(30))],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE layout_sets SET updated_at = ?1 WHERE id = ?2",
            params![chrono::Utc::now().to_rfc3339(), lsid_ref],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_layout_set_entry(
        &self,
        entry_id: &str,
        update: &UpdateLayoutSetEntry,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let eid_ref: &str = entry_id;
        if let Some(position) = update.position {
            conn.execute(
                "UPDATE layout_set_entries SET order_index = ?1 WHERE id = ?2",
                params![position, eid_ref],
            )
            .map_err(|e| e.to_string())?;
        }
        if let Some(interval_secs) = update.interval_secs {
            conn.execute(
                "UPDATE layout_set_entries SET interval_secs = ?1 WHERE id = ?2",
                params![interval_secs, eid_ref],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn delete_layout_set_entry(&self, entry_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let eid_ref: &str = entry_id;
        conn.execute("DELETE FROM layout_set_entries WHERE id = ?1", [eid_ref])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn replace_layout_set_entries(
        &self,
        layout_set_id: &str,
        entries: Vec<LayoutSetEntry>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let lsid_ref: &str = layout_set_id;
        conn.execute(
            "DELETE FROM layout_set_entries WHERE layout_set_id = ?1",
            [lsid_ref],
        )
        .map_err(|e| e.to_string())?;
        for item in &entries {
            let id_ref: &str = &item.id;
            let lid_ref: &str = &item.layout_id;
            let interval = item.interval_secs.unwrap_or(30);
            conn.execute(
                "INSERT INTO layout_set_entries (id, layout_set_id, layout_id, order_index, interval_secs) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id_ref, lsid_ref, lid_ref, item.position, interval],
            )
            .map_err(|e| e.to_string())?;
        }
        conn.execute(
            "UPDATE layout_sets SET updated_at = ?1 WHERE id = ?2",
            params![chrono::Utc::now().to_rfc3339(), lsid_ref],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
        }

    pub fn set_addon_storage(
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
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            r#"INSERT INTO addon_storage (addon_id, storage_key, value_json, updated_at)
               VALUES (?1, ?2, ?3, ?4)
               ON CONFLICT(addon_id, storage_key) DO UPDATE SET
               value_json = excluded.value_json, updated_at = excluded.updated_at"#,
            params![addon_id, storage_key, value_json, updated_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_addon_storage(
        &self,
        addon_id: &str,
        storage_key: &str,
    ) -> Result<Option<String>, String> {
        crate::addons::validate_addon_storage_namespace(addon_id)?;
        crate::addons::validate_addon_storage_key(storage_key)?;
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT value_json FROM addon_storage WHERE addon_id = ?1 AND storage_key = ?2",
            params![addon_id, storage_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn list_addon_storage(
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
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT storage_key, value_json FROM addon_storage
                 WHERE addon_id = ?1 AND storage_key LIKE ?2 ESCAPE '\\'
                 ORDER BY storage_key LIMIT ?3",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![addon_id, pattern, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn delete_addon_storage(&self, addon_id: &str, storage_key: &str) -> Result<bool, String> {
        crate::addons::validate_addon_storage_namespace(addon_id)?;
        crate::addons::validate_addon_storage_key(storage_key)?;
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let deleted = conn
            .execute(
                "DELETE FROM addon_storage WHERE addon_id = ?1 AND storage_key = ?2",
                params![addon_id, storage_key],
            )
            .map_err(|e| e.to_string())?;
        Ok(deleted > 0)
    }

    pub fn delete_addon_storage_namespace(&self, addon_id: &str) -> Result<usize, String> {
        crate::addons::validate_addon_storage_namespace(addon_id)?;
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM addon_storage WHERE addon_id = ?1",
            params![addon_id],
        )
        .map_err(|e| e.to_string())
    }

        /// Run SQLite VACUUM to reclaim space from dropped tables and deleted rows.
        /// VACUUM cannot run inside a transaction, so we wrap the lock acquisition
        /// in `tokio::task::spawn_blocking` to avoid holding the runtime's executor
        /// while the DB file is being rewritten.
        pub fn vacuum(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        // VACUUM cannot be executed within a transaction. SQLite opens an
        // implicit one for the statement, but we need to make sure no other
        // writer is active. The lock above serializes against other handlers.
        conn.execute("VACUUM", [])
        .map_err(|e| format!("vacuum: {e}"))?;
        Ok(())
        }
        }


#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_path(label: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let pid = std::process::id();
        std::env::temp_dir()
            .join(format!("ripley-{label}-{pid}-{n}.db"))
            .to_str()
            .unwrap()
            .to_string()
    }


    use rusqlite::Connection;
    use std::path::PathBuf;

    /// Helper: write a DB with the Phase 3 schema (kiosk_pages +
    /// kiosk_submissions) so we can simulate an old deploy being migrated.
    fn seed_legacy_db(path: &PathBuf) {
        let conn = Connection::open(path).expect("open");
        conn.execute_batch(
            "CREATE TABLE kiosk_pages (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                html_content TEXT NOT NULL DEFAULT '',
                is_default INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE kiosk_submissions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id TEXT NOT NULL,
                kiosk_page_id TEXT,
                kiosk_page_name TEXT,
                widget_id TEXT NOT NULL,
                widget_type TEXT NOT NULL,
                widget_label TEXT,
                payload_json TEXT NOT NULL,
                collected_at TEXT NOT NULL,
                received_at TEXT NOT NULL,
                google_sync_status TEXT NOT NULL DEFAULT 'pending',
                google_sync_message TEXT,
                google_synced_at TEXT,
                google_sheet_id TEXT
            );
            CREATE INDEX idx_kiosk_submissions_collected ON kiosk_submissions(collected_at);
            CREATE INDEX idx_kiosk_submissions_status ON kiosk_submissions(google_sync_status);
            CREATE INDEX idx_kiosk_submissions_page ON kiosk_submissions(kiosk_page_id);
            CREATE INDEX idx_kiosk_submissions_client ON kiosk_submissions(client_id);
            INSERT INTO kiosk_submissions
                (client_id, kiosk_page_id, kiosk_page_name, widget_id, widget_type,
                 payload_json, collected_at, received_at)
                VALUES ('legacy-client', 'legacy-page', 'Legacy Page', 'w1', 'button',
                        '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
        )
        .expect("seed");
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
        let q = format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name='{}'",
            table, column
        );
        conn.query_row(&q, [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    #[test]
    fn schedule_display_settings_round_trip_persists_brightness_and_contrast() {
        let path = unique_temp_path("display-settings");
        let db = Database::open(&path).expect("open");
        let entry = ScheduleEntry {
            id: "settings-1".into(),
            display: "client-a:DP-1".into(),
            layout_id: String::new(),
            layout_set_id: None,
            day_of_week: crate::schedule::DAY_EVERY,
            start_time: "08:00".into(),
            end_time: "17:00".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            action: "display_settings".into(),
            target_client: Some("client-a".into()),
            power_state: None,
            display_name: Some("Display 1".into()),
            control_method: Some("ddc".into()),
            display_index: Some(1),
            brightness: Some(35),
            contrast: Some(80),
            end_action: "none".into(),
        };
        db.create_schedule(&entry).expect("create");
        let loaded = db.get_schedule("settings-1").expect("get").expect("row");
        assert_eq!(loaded.brightness, Some(35));
        assert_eq!(loaded.contrast, Some(80));
        assert_eq!(loaded.display_index, Some(1));
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn migration_handles_only_old_table() {
        let path = std::env::temp_dir().join("ripley-mig-only-old.db");
        let _ = std::fs::remove_file(&path);
        seed_legacy_db(&path);
        // Run Database::open — should rename the table + columns.
        let _db = Database::open(path.to_str().unwrap()).expect("open");
        let conn = Connection::open(&path).expect("reopen");
        assert!(!table_exists(&conn, "kiosk_submissions"));
        assert!(!table_exists(&conn, "kiosk_pages"));
        assert!(table_exists(&conn, "interactive_submissions"));
        assert!(column_exists(&conn, "interactive_submissions", "interactive_layout_id"));
        assert!(column_exists(&conn, "interactive_submissions", "interactive_layout_name"));
        assert!(!column_exists(&conn, "interactive_submissions", "kiosk_page_id"));
        // The legacy row should be preserved under the new column names.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM interactive_submissions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "legacy row should be migrated, not dropped");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn addon_storage_round_trip_is_namespaced_and_upserted() {
        let path = unique_temp_path("addon-storage");
        let db = Database::open(&path).expect("open db");
        db.set_addon_storage("example.one", "settings", "{\"enabled\":true}", "2026-10-15T00:00:00Z")
            .expect("insert addon storage");
        assert_eq!(
            db.get_addon_storage("example.one", "settings")
                .expect("read addon storage"),
            Some("{\"enabled\":true}".to_string())
        );
        assert_eq!(
            db.get_addon_storage("example.two", "settings")
                .expect("read other addon storage"),
            None
        );
        db.set_addon_storage("example.one", "settings", "{\"enabled\":false}", "2026-10-15T00:01:00Z")
            .expect("upsert addon storage");
        assert_eq!(
            db.get_addon_storage("example.one", "settings")
                .expect("read updated addon storage"),
            Some("{\"enabled\":false}".to_string())
        );
        assert!(db.delete_addon_storage("example.one", "settings").expect("delete addon storage"));
        assert!(!db.delete_addon_storage("example.one", "settings").expect("delete missing addon storage"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn addon_storage_namespace_cleanup_removes_only_target_addon() {
        let path = unique_temp_path("addon-storage-cleanup");
        let db = Database::open(&path).expect("open db");
        db.set_addon_storage("example.one", "settings", "{\"enabled\":true}", "2026-10-15T00:00:00Z")
            .expect("insert first addon storage");
        db.set_addon_storage("example.one", "cache", "{\"count\":1}", "2026-10-15T00:00:00Z")
            .expect("insert second addon storage");
        db.set_addon_storage("example.two", "settings", "{\"enabled\":false}", "2026-10-15T00:00:00Z")
            .expect("insert other addon storage");

        assert_eq!(
            db.delete_addon_storage_namespace("example.one")
                .expect("delete addon namespace"),
            2
        );
        assert_eq!(
            db.get_addon_storage("example.one", "settings")
                .expect("read deleted addon storage"),
            None
        );
        assert_eq!(
            db.get_addon_storage("example.two", "settings")
                .expect("read retained addon storage"),
            Some("{\"enabled\":false}".to_string())
        );
        assert_eq!(
            db.delete_addon_storage_namespace("example.one")
                .expect("delete missing addon namespace"),
            0
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migration_handles_both_tables() {
        let path = std::env::temp_dir().join("ripley-mig-both.db");
        let _ = std::fs::remove_file(&path);
        // Schema-incompatible legacy data is dropped when both tables exist.
        // This branch is only reachable via state-bundle import (the
        // migration itself can't create both tables because the second
        // branch drops kiosk_submissions). The import sanitizer now prevents
        // legacy tables from being restored, so this branch should never
        // run in production — the test documents the fall-through behavior.
        seed_legacy_db(&path);
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "CREATE TABLE interactive_submissions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id TEXT NOT NULL,
                interactive_layout_id TEXT,
                interactive_layout_name TEXT,
                widget_id TEXT NOT NULL,
                widget_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                collected_at TEXT NOT NULL,
                received_at TEXT NOT NULL,
                google_sync_status TEXT NOT NULL DEFAULT 'pending'
            );
            INSERT INTO interactive_submissions
                (client_id, interactive_layout_id, widget_id, widget_type,
                 payload_json, collected_at, received_at)
                VALUES ('new-client', 'new-page', 'w2', 'textbox',
                        '{}', '2026-02-01T00:00:00Z', '2026-02-01T00:00:00Z');",
        )
        .expect("seed new");
        drop(conn);
        let _db = Database::open(path.to_str().unwrap()).expect("open");
        let conn = Connection::open(&path).expect("reopen");
        assert!(!table_exists(&conn, "kiosk_submissions"));
        assert!(!table_exists(&conn, "kiosk_pages"));
        assert!(table_exists(&conn, "interactive_submissions"));
        // The new row survives (new schema is canonical). The legacy row
        // is dropped because the schemas are incompatible.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM interactive_submissions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "only the new-schema row survives");
        let client_id: String = conn
            .query_row("SELECT client_id FROM interactive_submissions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(client_id, "new-client");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn migration_is_idempotent_on_clean_db() {
        // A fresh DB (no legacy tables) should open cleanly with no errors
        // and no leftover state. The startup runs the migration even though
        // there is nothing to migrate.
        let path = std::env::temp_dir().join("ripley-mig-clean.db");
        let _ = std::fs::remove_file(&path);
        let _db = Database::open(path.to_str().unwrap()).expect("open");
        let conn = Connection::open(&path).expect("reopen");
        assert!(!table_exists(&conn, "kiosk_submissions"));
        assert!(!table_exists(&conn, "kiosk_pages"));
        assert!(table_exists(&conn, "interactive_submissions"));
        // Opening again should be a no-op (idempotent).
        let _db2 = Database::open(path.to_str().unwrap()).expect("reopen");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn migration_renames_then_drops_clients_kiosk_page_id_column() {
        // The migration renames assigned_kiosk_page_id to
        // assigned_interactive_layout_id (Phase 5 transition), then drops
        // the renamed column on the next pass (Phase 5 cleanup followup).
        // The active column is assigned_layout_id (Phase 2) and the
        // renamed column is never read by the Rust code.
        let path = std::env::temp_dir().join("ripley-mig-clients-col.db");
        let _ = std::fs::remove_file(&path);
        // Seed the clients table with the Phase 2 column name.
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "CREATE TABLE clients (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                address TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'player',
                enrolled INTEGER NOT NULL DEFAULT 1,
                last_seen TEXT NOT NULL,
                assigned_kiosk_page_id TEXT,
                assigned_layout_id TEXT,
                active_layout_id TEXT
            );
            INSERT INTO clients (id, name, address, last_seen, assigned_kiosk_page_id)
                VALUES ('c1', 'test', 'addr', '2026-01-01T00:00:00Z', 'old-page');",
        )
        .expect("seed");
        drop(conn);
        let _db = Database::open(path.to_str().unwrap()).expect("open");
        let conn = Connection::open(&path).expect("reopen");
        // After migration: old name gone, intermediate name gone, only
        // the canonical Phase 2 name remains.
        assert!(!column_exists(&conn, "clients", "assigned_kiosk_page_id"));
        assert!(!column_exists(
            &conn,
            "clients",
            "assigned_interactive_layout_id"
        ));
        assert!(column_exists(&conn, "clients", "assigned_layout_id"));
        // The Phase 2 column is still NULL (the seeded value lived on
        // assigned_kiosk_page_id, which was renamed then dropped).
        let v: Option<String> = conn
            .query_row(
                "SELECT assigned_layout_id FROM clients WHERE id='c1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, None);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn migration_handles_db_with_dangling_fk_to_dropped_table() {
        // Simulate a DB whose clients table carries a FK column pointing
        // at a now-dropped `kiosk_pages` table. The migration must:
        //   1. Drop the `kiosk_pages` and `kiosk_submissions` tables.
        //   2. Drop the `assigned_interactive_layout_id` column on
        //      `clients` (the FK target is gone, the column is dead).
        // This test pins down the order — without (1), the ALTER would
        // see a real FK target; without (2), the schema would still
        // advertise a dangling reference.
        let path = std::env::temp_dir().join("ripley-mig-dangling-fk.db");
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).expect("open");
        // The columns here match what the production code's Phase 3
        // schema would have created; the migration expects them when
        // renaming kiosk_submissions -> interactive_submissions.
        conn.execute_batch(
            "CREATE TABLE clients (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                address TEXT NOT NULL,
                last_seen TEXT NOT NULL,
                assigned_layout_id TEXT,
                assigned_interactive_layout_id TEXT
            );
            CREATE TABLE kiosk_pages (
                id TEXT PRIMARY KEY
            );
            CREATE TABLE kiosk_submissions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id TEXT NOT NULL,
                kiosk_page_id TEXT,
                kiosk_page_name TEXT,
                widget_id TEXT NOT NULL,
                widget_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                collected_at TEXT NOT NULL,
                received_at TEXT NOT NULL,
                google_sync_status TEXT NOT NULL DEFAULT 'pending',
                google_sync_message TEXT,
                google_synced_at TEXT,
                google_sheet_id TEXT
            );",
        )
        .expect("seed");
        drop(conn);
        let _db = Database::open(path.to_str().unwrap()).expect("open");
        let conn = Connection::open(&path).expect("reopen");
        assert!(!table_exists(&conn, "kiosk_pages"));
        assert!(!table_exists(&conn, "kiosk_submissions"));
        assert!(!column_exists(&conn, "clients", "assigned_interactive_layout_id"));
        // The CREATE statement no longer mentions the dead column or its FK.
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name='clients'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!sql.contains("REFERENCES kiosk_pages"));
        assert!(!sql.contains("assigned_interactive_layout_id"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn delete_display_assignment_removes_targeted_row() {
        let path = unique_temp_path("display-del");
        let db = Database::open(&path).expect("open");
        db.set_display_assignment("DP-1", Some("layout-A"))
            .expect("set");
        db.set_display_assignment("DP-2", Some("layout-B"))
            .expect("set");
        let deleted = db.delete_display_assignment("DP-1").expect("delete");
        assert_eq!(deleted, 1, "exactly one row should be deleted");
        let state = db.get_display_state().expect("get state");
        let displays: Vec<&str> = state
            .assignments
            .iter()
            .map(|a| a.display.as_str())
            .collect();
        assert!(
            displays.contains(&"DP-2"),
            "DP-2 must survive a targeted delete; got {:?}",
            displays
        );
        assert!(
            !displays.contains(&"DP-1"),
            "DP-1 must be gone; got {:?}",
            displays
        );
        let deleted_again = db.delete_display_assignment("DP-1").expect("delete");
        assert_eq!(deleted_again, 0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn purge_expired_sessions_removes_only_expired() {
        let path = unique_temp_path("purge-sess");
        let db = Database::open(&path).expect("open");
        let now = chrono::Utc::now().timestamp();
        db.create_session("expired-token", "user1", now - 3600)
            .expect("create expired");
        db.create_session("valid-token", "user2", now + 3600)
            .expect("create valid");
        let purged = db.purge_expired_sessions().expect("purge");
        assert_eq!(purged, 1, "exactly one row should be purged");
        assert!(
            db.session_exists("valid-token").unwrap_or(false),
            "valid session must survive"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn log_schedule_run_roundtrips_via_list() {
        let path = unique_temp_path("sched-run-rt");
        let db = Database::open(&path).expect("open");
        let now = "2026-09-12T22:00:00Z";
        let id1 = db
            .log_schedule_run("sched-1", "switch_layout", "DP-1", true, None, now)
            .expect("log 1");
        let id2 = db
            .log_schedule_run("sched-1", "power_on", "client-42", true, Some("queued"), now)
            .expect("log 2");
        let id3 = db
            .log_schedule_run("sched-2", "power_off", "", false, Some("no target"), now)
            .expect("log 3");
        assert!(id2 > id1, "ids should be monotonic");
        assert!(id3 > id2);
        let rows = db.list_schedule_runs(10).expect("list");
        // Newest first
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].schedule_id, "sched-2");
        assert!(!rows[0].success);
        assert_eq!(rows[0].message.as_deref(), Some("no target"));
        assert_eq!(rows[1].action, "power_on");
        assert_eq!(rows[1].target, "client-42");
        assert_eq!(rows[2].action, "switch_layout");
        assert_eq!(rows[2].target, "DP-1");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_schedule_runs_clamps_limit() {
        let path = unique_temp_path("sched-run-clamp");
        let db = Database::open(&path).expect("open");
        for i in 0..5 {
            let _ = db
                .log_schedule_run(
                    "sched",
                    "switch_layout",
                    &format!("DP-{i}"),
                    true,
                    None,
                    "2026-09-12T22:00:00Z",
                )
                .expect("log");
        }
        // 0 should be clamped up to 1, huge value should be capped to 500
        let one = db.list_schedule_runs(0).expect("list 0");
        assert_eq!(one.len(), 1);
        let big = db.list_schedule_runs(10_000).expect("list big");
        assert_eq!(big.len(), 5, "only 5 rows exist; limit cap should not fabricate");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn count_stale_clients_uses_last_seen_recency() {
        let path = unique_temp_path("stale-clients");
        let db = Database::open(&path).expect("open");
        // Insert via Database's own connection so we don't fight
        // with SQLite's locking across separate Connection handles.
        let conn = db.conn.lock().unwrap();
        // Two stale rows (old last_seen): one real, one virtual.
        // One fresh row (just now): real, should not be counted.
        let old = "2020-01-01T00:00:00Z";
        let fresh = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO clients (id, name, address, role, version, last_seen, enrolled, is_virtual) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 0)",
            rusqlite::params!["stale-1", "stale", "x", "player", "v1", old],
        ).expect("insert stale");
        conn.execute(
            "INSERT INTO clients (id, name, address, role, version, last_seen, enrolled, is_virtual) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 1)",
            rusqlite::params!["virtual-1", "vclient", "x", "player", "v1", old],
        ).expect("insert virtual");
        conn.execute(
            "INSERT INTO clients (id, name, address, role, version, last_seen, enrolled, is_virtual) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 0)",
            rusqlite::params!["fresh-1", "fresh", "x", "player", "v1", fresh],
        ).expect("insert fresh");
        drop(conn);
        // Threshold = 60s. Only the "stale-1" row should be counted:
        // virtual-1 is filtered by is_virtual=0, fresh-1 is filtered
        // by the recency threshold.
        let n = db.count_stale_clients(60).expect("count");
        assert_eq!(n, 1, "only stale-1 should be counted as stale");
        // A much wider threshold (years) catches only the old rows
        // and still excludes the fresh one.
        let n2 = db.count_stale_clients(60 * 60 * 24 * 365).expect("count wide");
        assert_eq!(n2, 1, "wide threshold still excludes virtual and fresh");
        std::fs::remove_file(&path).ok();
    }

    /// News ticker: round-trip the string "news" through the DB layer's
    /// widget_type deserializer. Before the News variant was added, the
    /// match arm fell through to _ => Text, so a News widget saved to a
    /// News feed cache: register a feed URL, upsert headlines, query,
    /// then check the "needing refresh" filter. Pins the schema and
    /// the time-window logic.
    #[test]
    fn news_feed_round_trip_and_staleness() {
        use crate::models::NewsHeadline;
        let path = unique_temp_path("news-rt");
        let db = Database::open(&path).expect("open db");

        // 1) Register a brand new URL. No cached headlines yet, last_fetched_at = "".
        let before = db.get_news_feed("https://example.com/feed.rss").expect("get");
        assert!(before.is_none(), "should be absent before registration");
        let after_reg = db
            .ensure_news_feed_registered("https://example.com/feed.rss")
            .expect("register");
        assert!(after_reg.is_some(), "register should create the row");
        let empty = after_reg.unwrap();
        assert_eq!(empty.url, "https://example.com/feed.rss");
        assert_eq!(empty.headlines_json, "[]");
        assert_eq!(empty.last_fetched_at, "");

        // 2) A feed with last_fetched_at = "" is always stale.
        let stale = db.get_news_feeds_needing_refresh(3600).expect("stale");
        assert_eq!(stale.len(), 1, "empty feed should be stale");
        assert_eq!(stale[0].url, "https://example.com/feed.rss");

        // 3) Upsert some headlines. last_fetched_at is now set.
        let headlines = vec![
            NewsHeadline { title: "H1".into(), link: "https://x/1".into(), pub_date: "".into(), source: "S".into() },
            NewsHeadline { title: "H2".into(), link: "https://x/2".into(), pub_date: "".into(), source: "S".into() },
        ];
        let json = serde_json::to_string(&headlines).expect("ser");
        db.upsert_news_feed("https://example.com/feed.rss", "Example", &json, Some("etag-1"), Some("Mon, 01 Jan 2024 00:00:00 GMT"))
            .expect("upsert");
        let got = db.get_news_feed("https://example.com/feed.rss").expect("get2").unwrap();
        assert_eq!(got.title, "Example");
        assert_eq!(got.headlines_json, json);
        assert_eq!(got.etag.as_deref(), Some("etag-1"));
        assert!(!got.last_fetched_at.is_empty());

        // 4) Right after upsert, with max_age = 1 hour, the feed is NOT stale.
        let stale_after = db.get_news_feeds_needing_refresh(3600).expect("stale2");
        assert_eq!(stale_after.len(), 0, "freshly fetched feed should not be stale");

        // 5) A just-upserted row is FRESH. With max_age=1s (which means
        //    "older than 1 second"), a 0-second-old row is NOT stale.
        //    This is the correct behavior: maintenance loop wants to
        //    refresh feeds that are older than the threshold, not
        //    feeds that were just fetched.
        let not_stale = db.get_news_feeds_needing_refresh(1).expect("stale3");
        assert_eq!(not_stale.len(), 0, "1-second-old threshold should NOT mark a just-upserted row as stale");
        // Sanity: a very lax threshold still should not mark it stale.
        let not_stale_lax = db.get_news_feeds_needing_refresh(86400).expect("stale4");
        assert_eq!(not_stale_lax.len(), 0, "lax threshold should not mark the fresh row as stale");

        // 5b) Backdate the row so it's older than the threshold, then
        //     verify the staleness filter catches it. This proves the
        //     < comparison works in the real direction (vs. the
        //     right-after-upsert case above which exercises the "not
        //     stale" branch).
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE news_feeds SET last_fetched_at = ?1 WHERE url = ?2",
                rusqlite::params!["2020-01-01T00:00:00+00:00", "https://example.com/feed.rss"],
            )
            .unwrap();
        }
        let stale_now = db.get_news_feeds_needing_refresh(60).expect("stale5");
        assert_eq!(stale_now.len(), 1, "backdated row should be stale with any sensible threshold");
        assert_eq!(stale_now[0].url, "https://example.com/feed.rss");

        // 6) record_news_feed_error should keep the old headlines but
        //    update last_fetched_at and set last_error.
        db.record_news_feed_error("https://example.com/feed.rss", "503 service unavailable")
            .expect("record error");
        let with_err = db.get_news_feed("https://example.com/feed.rss").expect("get3").unwrap();
        assert_eq!(with_err.headlines_json, json, "old headlines should survive an error");
        assert_eq!(with_err.last_error.as_deref(), Some("503 service unavailable"));

        // 7) list_news_feeds should include ours.
        let all = db.list_news_feeds().expect("list");
        assert_eq!(all.len(), 1);
        std::fs::remove_file(&path).ok();
    }

    /// News ticker: round-trip the string "news" through the DB layer's
    /// widget_type deserializer. Before the News variant was added, the
    /// match arm fell through to _ => Text, so a News widget saved to a
    /// layout would come back as Text. This test pins the new behavior.
    #[test]
    fn news_widget_type_round_trip() {
        use crate::models::WidgetType;
        // Forward direction: DB string -> enum variant.
        let parsed = Database::widget_type_from_db_str("news");
        assert!(
            matches!(parsed, WidgetType::News),
            "expected WidgetType::News, got {:?}",
            parsed
        );
        // Reverse direction: enum variant -> JSON string. The enum has
        // `rename_all = "lowercase"` so the JSON form is "news".
        let s = serde_json::to_string(&WidgetType::News).expect("serialize");
        assert_eq!(s, "\"news\"");
    }

    #[test]
    fn camera_widget_type_round_trip() {
        use crate::models::WidgetType;

        assert!(matches!(
            Database::widget_type_from_db_str("camera"),
            WidgetType::Camera
        ));
        assert_eq!(
            serde_json::to_string(&WidgetType::Camera).expect("serialize camera"),
            "\"camera\""
        );
    }

    #[test]
    fn user_repository_credentials_and_login_round_trip() {
        let path = unique_temp_path("user-repository");
        let db = Database::open(&path).expect("open db");
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO users (id, username, display_name, role, password_hash, active, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                rusqlite::params![
                    "user-1",
                    "Admin.User",
                    "Admin User",
                    "admin",
                    "hash-value",
                    1,
                    "2026-09-24T00:00:00Z",
                ],
            )
            .expect("insert user");
        }
        let user = db
            .find_active_user_credentials("admin.user")
            .expect("find credentials")
            .expect("active user");
        assert_eq!(user.0, "Admin.User");
        assert_eq!(user.1, "admin");
        assert_eq!(user.2, "hash-value");

        db.record_user_login("Admin.User", "2026-09-24T01:02:03Z")
            .expect("record login");
        let updated: (String, String) = db
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT last_login_at, updated_at FROM users WHERE id=?1",
                ["user-1"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read login timestamps");
        assert_eq!(updated.0, "2026-09-24T01:02:03Z");
        assert_eq!(updated.1, "2026-09-24T01:02:03Z");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn initial_admin_creation_is_atomic_and_one_time() {
        let path = unique_temp_path("initial-admin");
        let db = Database::open(&path).expect("open db");
        assert_eq!(
            db.create_initial_admin(
                "admin-1",
                "owner",
                "Owner",
                "hash-1",
                "2026-09-24T00:00:00Z",
            )
            .expect("create initial admin"),
            true
        );
        assert_eq!(db.count_active_admins().expect("count admins"), 1);
        assert_eq!(
            db.create_initial_admin(
                "admin-2",
                "second-owner",
                "Second Owner",
                "hash-2",
                "2026-09-24T00:00:01Z",
            )
            .expect("repeat initial admin"),
            false
        );
        let count: i64 = db
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("count users");
        assert_eq!(count, 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn table_query_repository_preserves_metadata_and_values() {
        let path = unique_temp_path("table-query");
        let db = Database::open(&path).expect("open db");
        {
            let conn = db.conn.lock().unwrap();
            conn.execute_batch(
                "CREATE TABLE table_test (id INTEGER PRIMARY KEY, name TEXT NOT NULL, payload BLOB);",
            )
            .expect("create table");
            conn.execute(
                "INSERT INTO table_test (name, payload) VALUES (?1, ?2)",
                rusqlite::params!["Comma, value", vec![0_u8, 255_u8]],
            )
            .expect("insert row");
        }

        let result = db
            .query_table_rows("table_test", 100, 0)
            .expect("query table");
        assert_eq!(result.total, 1);
        assert_eq!(
            result
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "name", "payload"]
        );
        assert_eq!(result.columns[1].col_type, "TEXT");
        assert!(result.columns[1].notnull);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][1], serde_json::json!(1));
        assert_eq!(result.rows[0][2], serde_json::json!("Comma, value"));
        assert_eq!(result.rows[0][0], serde_json::json!(1));
        let export = db.export_table_rows("table_test").expect("export table");
        assert_eq!(export.columns, vec!["id", "name", "payload"]);
        assert_eq!(export.rows.len(), 1);
        assert_eq!(export.rows[0][2], serde_json::json!("Comma, value"));
        assert!(db.query_table_rows("bad-name", 100, 0).is_err());
        assert_eq!(db.delete_table_rows("table_test", &[1, 999]), Ok(1));
        assert_eq!(db.delete_table_rows("table_test", &[]), Ok(0));
        assert_eq!(db.query_table_rows("table_test", 100, 0).unwrap().total, 0);
        assert!(db.delete_table_rows("bad-name", &[1]).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn backup_repository_round_trip() {
        let path = unique_temp_path("backup-repository");
        let db = Database::open(&path).expect("open db");
        let backup = Backup {
            id: "backup-1".into(),
            name: "Backup 1".into(),
            created_at: "2026-09-24T00:00:00Z".into(),
            file_path: "/tmp/backup-1.tar.gz".into(),
            size_bytes: 42,
        };
        db.insert_backup(&backup).expect("insert backup");
        assert_eq!(
            db.get_backup_file_path("backup-1").expect("get path"),
            backup.file_path
        );
        let listed = db.list_backups().expect("list backups");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, backup.id);
        assert_eq!(db.delete_backup("backup-1").expect("delete backup"), 1);
        assert_eq!(db.delete_backup("backup-1").expect("delete missing backup"), 0);
        assert!(db.get_backup_file_path("backup-1").is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn audit_events_store_actor_and_bounded_detail() {
        let path = unique_temp_path("audit-events");
        let db = Database::open(&path).expect("open db");
        let id = db
            .record_audit_event(
                Some("operator@example.invalid"),
                Some("operator"),
                "client_command",
                Some("client-1"),
                "success",
                Some(&"x".repeat(2000)),
                "2026-10-01T18:00:00Z",
            )
            .expect("record audit event");
        assert!(id > 0);
        let rows = db.list_audit_events(10).expect("list audit events");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].actor.as_deref(), Some("operator@example.invalid"));
        assert_eq!(rows[0].event_type, "client_command");
        assert_eq!(rows[0].target.as_deref(), Some("client-1"));
        assert!(rows[0].detail.as_deref().unwrap_or_default().len() <= 512);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn client_command_burst_allows_ten_then_cools_down_per_client_and_action() {
        let path = unique_temp_path("client-command-cooldown");
        let db = Database::open(&path).expect("open db");
        let now = chrono::Utc::now().to_rfc3339();
        for index in 1..=10 {
            db.queue_client_command(
                "client-1",
                "restart_client",
                Some(&index.to_string()),
                &now,
                Some("test"),
            )
            .unwrap_or_else(|error| panic!("command {index} should queue: {error}"));
            // A client can have only one pending command. Remove the probe
            // command before queuing the next burst member so this test
            // exercises the cooldown counter rather than overwrite handling.
            db.clear_client_command("client-1")
                .expect("clear probe command");
        }
        let blocked = db.queue_client_command("client-1", "restart_client", None, &now, Some("test"));
        assert!(blocked.is_err());
        assert!(blocked.unwrap_err().starts_with("cooldown:"));
        db.queue_client_command("client-1", "refresh_cache", None, &now, Some("test"))
            .expect("different action should have an independent burst");
        db.queue_client_command("client-2", "restart_client", None, &now, Some("test"))
            .expect("different client should have an independent burst");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_layouts_is_bounded() {
        let path = unique_temp_path("layout-list-bound");
        let db = Database::open(&path).expect("open db");
        {
            let conn = db.conn.lock().expect("lock db");
            let tx = conn.unchecked_transaction().expect("begin transaction");
            for index in 0..10_001 {
                let id = format!("layout-{index:05}");
                tx.execute(
                    "INSERT INTO layouts (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                    rusqlite::params![id, "bounded layout", "2026-10-01T00:00:00Z"],
                )
                .expect("insert layout");
            }
            tx.commit().expect("commit layouts");
        }

        let layouts = db.list_layouts().expect("list layouts");
        assert_eq!(layouts.len(), 10_000);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_layout_revisions_is_bounded() {
        let path = unique_temp_path("layout-revision-list-bound");
        let db = Database::open(&path).expect("open db");
        let layout = Layout {
            id: "revision-layout".into(),
            name: "Revision layout".into(),
            widgets: vec![],
            interactive: false,
            canvas_orientation: "landscape".into(),
            folder_id: None,
            page_type: "single".into(),
            timing_mode: "global".into(),
            duration: 15,
            shuffle: false,
            items: vec![],
            created_at: "2026-10-01T00:00:00Z".into(),
            updated_at: "2026-10-01T00:00:00Z".into(),
        };
        let snapshot = serde_json::to_string(&layout).expect("encode snapshot");
        {
            let conn = db.conn.lock().expect("lock db");
            let tx = conn.unchecked_transaction().expect("begin transaction");
            tx.execute(
                "INSERT INTO layouts (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                rusqlite::params![layout.id, layout.name, layout.created_at],
            )
            .expect("insert layout");
            for revision in 1..=1_001 {
                tx.execute(
                    "INSERT INTO layout_revisions (layout_id, revision, snapshot, created_at) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![layout.id, revision, snapshot, "2026-10-01T00:00:00Z"],
                )
                .expect("insert revision");
            }
            tx.commit().expect("commit revisions");
        }

        let revisions = db
            .list_layout_revisions(&layout.id)
            .expect("list revisions");
        assert_eq!(revisions.len(), 1_000);
        assert_eq!(revisions[0].revision, 1_001);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_layout_folders_is_bounded() {
        let path = unique_temp_path("layout-folder-list-bound");
        let db = Database::open(&path).expect("open db");
        {
            let conn = db.conn.lock().expect("lock db");
            let tx = conn.unchecked_transaction().expect("begin transaction");
            for index in 0..10_001 {
                tx.execute(
                    "INSERT INTO layout_folders (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                    rusqlite::params![format!("folder-{index:05}"), "bounded folder", "2026-10-01T00:00:00Z"],
                )
                .expect("insert folder");
            }
            tx.commit().expect("commit folders");
        }

        let folders = db.list_layout_folders().expect("list folders");
        assert_eq!(folders.len(), 10_000);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_users_is_bounded() {
        let path = unique_temp_path("user-list-bound");
        let db = Database::open(&path).expect("open db");
        {
            let conn = db.conn.lock().expect("lock db");
            let tx = conn.unchecked_transaction().expect("begin transaction");
            for index in 0..10_001 {
                let id = format!("user-{index:05}");
                let username = format!("user-{index:05}");
                tx.execute(
                    "INSERT INTO users (id, username, display_name, password_hash, created_at, updated_at) VALUES (?1, ?2, ?2, ?3, ?4, ?4)",
                    rusqlite::params![id, username, "[REDACTED]", "2026-10-01T00:00:00Z"],
                )
                .expect("insert user");
            }
            tx.commit().expect("commit users");
        }

        let users = db.list_users().expect("list users");
        assert_eq!(users.len(), 10_000);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_registered_clients_is_bounded() {
        let path = unique_temp_path("client-list-bound");
        let db = Database::open(&path).expect("open db");
        {
            let conn = db.conn.lock().expect("lock db");
            let tx = conn.unchecked_transaction().expect("begin transaction");
            for index in 0..10_001 {
                let id = format!("client-{index:05}");
                tx.execute(
                    "INSERT INTO clients (id, name, address, last_seen) VALUES (?1, ?2, ?3, ?3)",
                    rusqlite::params![id, "bounded client", "2026-10-01T00:00:00Z"],
                )
                .expect("insert client");
            }
            tx.commit().expect("commit clients");
        }

        let clients = db.list_registered_clients().expect("list clients");
        assert_eq!(clients.len(), 10_000);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_news_feeds_is_bounded() {
        let path = unique_temp_path("news-feed-list-bound");
        let db = Database::open(&path).expect("open db");
        {
            let conn = db.conn.lock().expect("lock db");
            let tx = conn.unchecked_transaction().expect("begin transaction");
            for index in 0..4_097 {
                tx.execute(
                    "INSERT INTO news_feeds (url) VALUES (?1)",
                    rusqlite::params![format!("https://example.invalid/feed-{index:04}.xml")],
                )
                .expect("insert news feed");
            }
            tx.commit().expect("commit news feeds");
        }

        let feeds = db.list_news_feeds().expect("list news feeds");
        assert_eq!(feeds.len(), 4_096);
        std::fs::remove_file(&path).ok();
    }
}
