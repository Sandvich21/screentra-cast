use serde::{Deserialize, Serialize};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

use crate::db::Database;
use crate::models::LayoutSetWithItems;

/// "Every Day" mask (all 7 day-bits set). Stored in the
/// `schedules.day_of_week` column. Other values:
///   - 0 = never
///   - single bit (1 << day) = that day only
///   - bitwise OR of multiple (1<<day) = several days
///   - 62 (Mon-Fri) and 65 (Sun+Sat) are common presets that the
///     frontend uses, but they are not symbolic here.
pub const DAY_EVERY: i32 = -1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEntry {
    pub id: String,
    /// Connector or scope identifier. For layout-switch schedules
    /// this is the display name (e.g. "DP-1") or
    /// "<client_id>:DP-1" form.
    pub display: String,
    /// Layout to show on `start_time`. For set-cycling schedules
    /// this MUST be empty (`layout_set_id` is used instead).
    pub layout_id: String,
    /// Layout set to cycle through during this schedule's active
    /// window. Mutually exclusive with `layout_id` - if set,
    /// `layout_id` should be empty. The scheduler picks the
    /// current entry based on the local time and the entries'
    /// `interval_secs`.
    #[serde(default)]
    pub layout_set_id: Option<String>,
    /// Bitmask of days this schedule is active. See `DAY_EVERY` for
    /// the encoding.
    pub day_of_week: i32,
    pub start_time: String, // HH:MM format
    pub end_time: String,   // HH:MM format
    pub created_at: String,

    /// Action this schedule triggers. Defaults to `switch_layout`
    /// for backward compatibility with rows created before this
    /// column existed. Other valid values: `power_on`, `power_off`.
    #[serde(default = "default_action")]
    pub action: String,

    /// Target client for a client-scoped layout switch or a
    /// `power_on`/`power_off` action. NULL for legacy server-display
    /// layout switches.
    #[serde(default)]
    pub target_client: Option<String>,

    /// For `power_on`/`power_off` actions: the state to send
    /// ("on" / "off"). NULL for layout switches.
    #[serde(default)]
    pub power_state: Option<String>,

    #[serde(default)]
    pub display_name: Option<String>,

    /// For power_on/power_off: which control method to use (auto, ddc, cec, ir).
    #[serde(default)]
    pub control_method: Option<String>,

    /// Display index on the target client for layout or power schedules.
    #[serde(default)]
    pub display_index: Option<i32>,
}

fn default_action() -> String {
    "switch_layout".to_string()
}

/// Background scheduler started from `main.rs`.
///
/// Tick interval: 30 seconds. All times are minute-resolution - a
/// tick can fire up to 30s "late" but the next tick catches it.
///
/// For each active `switch_layout` schedule, this updates the
/// matching `display_state` row and bumps the refresh epoch so
/// connected players reload.
///
/// If the schedule references a `layout_set_id` (and `layout_id`
/// is empty), the scheduler picks the current entry from the set
/// based on the local time and each entry's `interval_secs` -
/// cycling through the entries automatically.
///
/// `power_on` / `power_off` schedules are NOT handled here; they
/// are dispatched via `crate::api::queue_command` in the player
/// command pipeline.
pub async fn run_scheduler(db: Arc<Database>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        interval.tick().await;
        if let Err(e) = apply_active_schedules_at(&db, Local::now()).await {
            log::error!("Scheduler error: {e}");
        }
    }
}

/// Backend-neutral production scheduler. The SQLite-only helper below remains
/// available for focused unit tests while runtime work uses ClientRepository.
pub async fn run_repository_scheduler(repository: crate::api::ClientRepository) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        interval.tick().await;
        if let Err(e) = apply_repository_schedules_at(&repository, Local::now()).await {
            log::error!("Scheduler error: {e}");
        }
    }
}

async fn apply_repository_schedules_at(
    repository: &crate::api::ClientRepository,
    now: DateTime<Local>,
) -> Result<(), String> {
    let day = now.weekday().num_days_from_sunday() as i32;
    let current_time = format!("{:02}:{:02}", now.hour(), now.minute());
    let active = repository
        .get_active_schedules_full(&current_time, day)
        .await?;
    let now_rfc = chrono::Utc::now().to_rfc3339();
    for schedule in active {
        let action = if schedule.action.is_empty() {
            "switch_layout"
        } else {
            schedule.action.as_str()
        };
        match action {
            "switch_layout" => {
                let layout_id = resolve_repository_layout(repository, &schedule, now).await?;
                if layout_id.is_empty() {
                    continue;
                }
                // New schedules identify the actual registered client and
                // display index. Older schedules only have `display` and are
                // retained through the legacy server-display path.
                let target_client = schedule.target_client.as_deref().filter(|id| !id.is_empty());
                let changed = if let Some(client_id) = target_client {
                    let display_index = i64::from(schedule.display_index.unwrap_or(0));
                    let current = repository
                        .get_display_assignment_and_last_seen(client_id, display_index)
                        .await?;
                    let changed = current
                        .as_ref()
                        .and_then(|(layout, _)| layout.as_deref())
                        != Some(layout_id.as_str());
                    if changed {
                        let updated = repository
                            .set_display_layout(client_id, display_index, Some(&layout_id))
                            .await?;
                        if updated != 1 {
                            return Err(format!(
                                "layout schedule {} targeted missing client display {client_id}:{display_index}",
                                schedule.id
                            ));
                        }
                    }
                    changed
                } else {
                    let current = repository.get_display_assignment(&schedule.display).await?;
                    let changed = current
                        .as_ref()
                        .and_then(|assignment| assignment.layout_id.as_deref())
                        != Some(layout_id.as_str());
                    if changed {
                        repository
                            .set_display_assignment(&schedule.display, Some(&layout_id))
                            .await?;
                    }
                    changed
                };
                if changed {
                    let _ = repository.bump_refresh_epoch().await;
                    let _ = repository
                        .log_schedule_run(
                            &schedule.id,
                            "switch_layout",
                            &schedule.display,
                            true,
                            None,
                            &now_rfc,
                        )
                        .await;
                }
            }
            "power_on" | "power_off" => {
                apply_repository_power_action(repository, &schedule, action, &now_rfc).await?;
            }
            other => {
                let message = format!("unknown action '{other}'");
                let _ = repository
                    .log_schedule_run(
                        &schedule.id,
                        other,
                        &schedule.display,
                        false,
                        Some(&message),
                        &now_rfc,
                    )
                    .await;
            }
        }
    }
    Ok(())
}

async fn resolve_repository_layout(
    repository: &crate::api::ClientRepository,
    schedule: &ScheduleEntry,
    now: DateTime<Local>,
) -> Result<String, String> {
    if !schedule.layout_id.is_empty() {
        return Ok(schedule.layout_id.clone());
    }
    let Some(set_id) = schedule.layout_set_id.as_deref().filter(|id| !id.is_empty()) else {
        return Ok(String::new());
    };
    let Some(set) = repository.get_layout_set(set_id).await? else {
        return Ok(String::new());
    };
    Ok(pick_entry_from_set(&set, &schedule.start_time, now)
        .map(|entry| entry.layout_id.clone())
        .unwrap_or_default())
}

async fn apply_repository_power_action(
    repository: &crate::api::ClientRepository,
    schedule: &ScheduleEntry,
    action: &str,
    now_rfc: &str,
) -> Result<(), String> {
    let target = schedule.target_client.as_deref().unwrap_or("");
    if target.is_empty() {
        let message = format!("{action} schedule {} has no target_client", schedule.id);
        let _ = repository
            .log_schedule_run(&schedule.id, action, "", false, Some(&message), now_rfc)
            .await;
        return Ok(());
    }
    let method = schedule.control_method.as_deref().unwrap_or("auto");
    let state = schedule
        .power_state
        .as_deref()
        .unwrap_or(if action == "power_on" { "on" } else { "off" });
    let display_index = schedule.display_index.unwrap_or(0);
    let (command, payload) = power_command(method, state, display_index);
    let payload_json = payload.to_string();
    match repository
        .queue_client_command(target, &command, Some(&payload_json), now_rfc, Some("scheduler"))
        .await
    {
        Ok(()) => {
            let detail = format!("action={command}");
            let _ = repository.record_audit_event(
                Some("scheduler"),
                Some("service"),
                "client_command_persist",
                Some(target),
                "success",
                Some(&detail),
                now_rfc,
            );
            let _ = repository.record_audit_event(
                Some("scheduler"),
                Some("service"),
                "client_command_create",
                Some(target),
                "success",
                Some(&detail),
                now_rfc,
            );
            let message = format!("queued {command} via {method}");
            let _ = repository
                .log_schedule_run(
                    &schedule.id,
                    &command,
                    target,
                    true,
                    Some(&message),
                    now_rfc,
                )
                .await;
        }
        Err(error) => {
            let _ = repository
                .log_schedule_run(
                    &schedule.id,
                    &command,
                    target,
                    false,
                    Some(&error),
                    now_rfc,
                )
                .await;
        }
    }
    Ok(())
}

fn power_command(
    method: &str,
    state: &str,
    display_index: i32,
) -> (String, serde_json::Value) {
    match (method, state) {
        ("ddc", _) | (_, "sleep") => (
            "ddc_set".to_string(),
            serde_json::json!({ "vcp": 6, "value": if state == "on" { 1 } else { 4 }, "bus": display_index }),
        ),
        ("cec", "off") | ("auto", "off") => (
            "cec_send".to_string(),
            serde_json::json!({ "cmd": "standby 0" }),
        ),
        ("cec", _) | ("auto", "on") => (
            "cec_send".to_string(),
            serde_json::json!({ "cmd": "pow 0" }),
        ),
        ("ir", _) => (
            "ir_blast".to_string(),
            serde_json::json!({ "state": state }),
        ),
        _ => (
            "cec_send".to_string(),
            serde_json::json!({ "cmd": if state == "on" { "pow 0" } else { "standby 0" } }),
        ),
    }
}

/// Apply any schedules active at the given local time. Exposed for
/// tests; production callers should use `run_scheduler`.
pub async fn apply_active_schedules_at(
    db: &Arc<Database>,
    now: DateTime<Local>,
) -> Result<(), String> {
    // We use the local time because the user enters schedule times
    // in their local timezone (e.g. "08:00" means 8 AM on the wall
    // clock), and the OS timezone (e.g. America/New_York) determines
    // what "now" actually is. If we used Utc::now() here, schedules
    // would fire at the wrong wall-clock time.
    // chrono: 0 = Sunday, 1 = Monday, ..., 6 = Saturday
    // We store: bit 0 = Sunday, bit 6 = Saturday
    let day_of_week = now.weekday().num_days_from_sunday() as i32; // 0-6
    let current_time = format!("{:02}:{:02}", now.hour(), now.minute());

    log::debug!("Scheduler tick: day={day_of_week}, time={current_time}");

    let active = db.get_active_schedules_full(&current_time, day_of_week)?;
    for schedule in &active {
        // Dispatch on `schedule.action`. The default is `switch_layout`
        // (backward compat with rows created before the action column
        // existed). New actions: `power_on` and `power_off` queue an
        // `ir_blast` command for `target_client` so the player can
        // power-cycle the attached display.
        let action = if schedule.action.is_empty() {
            "switch_layout"
        } else {
            schedule.action.as_str()
        };
        let now_rfc = chrono::Utc::now().to_rfc3339();
        match action {
            "switch_layout" => {
                apply_switch_layout(db, schedule, now, &current_time, day_of_week)?;
            }
            "power_on" | "power_off" => {
                apply_power_action(db, schedule, action, &now_rfc)?;
            }
            other => {
                log::warn!(
                    "Scheduler: schedule {} has unknown action '{other}', skipping",
                    schedule.id
                );
                let msg = format!("unknown action '{other}'");
                let _ = db.log_schedule_run(
                    &schedule.id, other, &schedule.display, false, Some(&msg), &now_rfc,
                );
            }
        }
    }
    Ok(())
}

/// Default `switch_layout` branch. Resolves the layout_id (single
/// layout or cycling from a layout_set) and writes the matching
/// `display_state` row, bumping the refresh epoch if anything changed.
fn apply_switch_layout(
    db: &Arc<Database>,
    schedule: &ScheduleEntry,
    now: DateTime<Local>,
    current_time: &str,
    day_of_week: i32,
) -> Result<(), String> {
    let display = &schedule.display;
    let layout_id = resolve_target_layout(db, schedule, now)?;
    if layout_id.is_empty() {
        log::debug!(
            "Scheduler: display={display} has active switch_layout but no resolvable layout_id (empty set?)"
        );
        return Ok(());
    }
    let current = db.get_display_assignment(display)?;
    let needs_update = match &current {
        Some(c) => c.layout_id.as_deref() != Some(&layout_id),
        None => true,
    };
    if needs_update {
        log::info!(
            "Schedule applied: display={display} -> layout={layout_id} (day={day_of_week}, time={current_time})"
        );
        db.set_display_assignment(display, Some(&layout_id))?;
        // Bump the refresh epoch so connected players reload.
        let _ = db.bump_refresh_epoch();
        let now_rfc = chrono::Utc::now().to_rfc3339();
        let _ = db.log_schedule_run(
            &schedule.id, "switch_layout", display, true, None, &now_rfc,
        );
    }
    Ok(())
}

/// `power_on` / `power_off` branch. Queues an `ir_blast` command for
/// `schedule.target_client`. The player polls its command endpoint
/// and dispatches the IR (or CEC, or HTTP-to-gateway) to actually
/// toggle power on the attached display. Empty `target_client` is
/// logged as a failure but does not abort other schedules.
fn apply_power_action(
    db: &Arc<Database>,
    schedule: &ScheduleEntry,
    action: &str,
    now_rfc: &str,
) -> Result<(), String> {
    let target = schedule
        .target_client
        .as_deref()
        .unwrap_or("")
        .to_string();
    if target.is_empty() {
        let msg = format!("{action} schedule {} has no target_client", schedule.id);
        log::warn!("Scheduler: {msg}");
        let _ = db.log_schedule_run(
            &schedule.id, action, "", false, Some(&msg), now_rfc,
        );
        return Ok(());
    }
    let control_method = schedule.control_method.as_deref().unwrap_or("auto");
    let display_index = schedule.display_index.unwrap_or(0);
    let power_state = schedule
        .power_state
        .as_deref()
        .unwrap_or(if action == "power_on" { "on" } else { "off" });

    let log_target = format!("{}[{}]", target, display_index);

    let (dispatch_action, payload) = match (control_method, power_state) {
        ("ddc", _) | (_, "sleep") => {
            // DDC/CI VCP D6: 1=wake from sleep, 4=enter sleep (display_sleep / display standby).
            // Also used for "sleep" power_state regardless of method (auto path).
            let vcp_val = if power_state == "on" { 1 } else { 4 };
            let payload = serde_json::json!({ "vcp": 6, "value": vcp_val, "bus": display_index });
            ("ddc_set".to_string(), payload)
        }
        ("cec", "off") => {
            // CEC standby — full power off (display cannot wake via CEC from this state)
            let cmd = "standby 0";
            let payload = serde_json::json!({ "cmd": cmd });
            ("cec_send".to_string(), payload)
        }
        ("cec", "on") | ("cec", _) => {
            // CEC power on
            let cmd = "pow 0";
            let payload = serde_json::json!({ "cmd": cmd });
            ("cec_send".to_string(), payload)
        }
        ("auto", "off") => {
            // Auto + power off → CEC standby
            let cmd = "standby 0";
            let payload = serde_json::json!({ "cmd": cmd });
            ("cec_send".to_string(), payload)
        }
        ("ir", _) => {
            // IR stub — queue ir_blast so it reaches the actor (actor has the stub)
            let payload = serde_json::json!({ "state": power_state });
            ("ir_blast".to_string(), payload)
        }
        _ => {
            // Fallback: auto mode, wake → CEC pow
            let cmd = if power_state == "on" { "pow 0" } else { "standby 0" };
            let payload = serde_json::json!({ "cmd": cmd });
            ("cec_send".to_string(), payload)
        }
    };

    let queued = crate::api::queue_command(
        db,
        &target,
        &dispatch_action,
        Some(&payload),
        Some("scheduler"),
    );
    match queued {
        Ok(_rec) => {
            log::info!(
                "Schedule {action}[{log_target}]: queued {dispatch_action} for client {target} (state={power_state}, method={control_method})"
            );
            let _ = db.log_schedule_run(
                &schedule.id, &dispatch_action, &target, true,
                Some(&format!("queued {dispatch_action} via {control_method}")),
                now_rfc,
            );
        }
        Err(e) => {
            log::warn!("Schedule {action}[{log_target}]: queue_command failed for {target}: {e}");
            let _ = db.log_schedule_run(
                &schedule.id, &dispatch_action, &target, false, Some(&e), now_rfc,
            );
        }
    }
    Ok(())
}

/// Resolve the layout_id that should be shown on `schedule.display`
/// at local time `now`. Returns an empty string when the schedule
/// cannot be satisfied (no layout, no entries, etc.).
///
/// Two cases:
/// - `schedule.layout_id` is set: return it verbatim (single layout).
/// - `schedule.layout_set_id` is set and `layout_id` is empty:
///   fetch the set, pick the entry whose cumulative-interval
///   window contains the current position within the schedule's
///   time window. Return the entry's `layout_id`.
fn resolve_target_layout(
    db: &Arc<Database>,
    schedule: &ScheduleEntry,
    now: DateTime<Local>,
) -> Result<String, String> {
    if !schedule.layout_id.is_empty() {
        return Ok(schedule.layout_id.clone());
    }
    let set_id = match schedule.layout_set_id.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => {
            // Neither layout_id nor layout_set_id is set.
            return Ok(String::new());
        }
    };
    let set = match db.get_layout_set(set_id).map_err(|e| e.to_string())? {
        Some(s) => s,
        None => {
            log::warn!(
                "Scheduler: schedule {} references missing layout_set {set_id}",
                schedule.id
            );
            return Ok(String::new());
        }
    };
    if set.items.is_empty() {
        log::warn!(
            "Scheduler: schedule {} references empty layout_set {set_id}",
            schedule.id
        );
        return Ok(String::new());
    }
    let entry = pick_entry_from_set(&set, &schedule.start_time, now);
    Ok(entry.map(|e| e.layout_id.clone()).unwrap_or_default())
}

/// Pick the layout-set entry that should be active at local time
/// `now`. The entry's `interval_secs` defines how long it should
/// show; entries are visited in `position` order (shuffled
/// deterministically per-day if `set.shuffle` is true). The current
/// entry is the one whose cumulative interval window contains the
/// seconds-into-schedule-window value.
pub fn pick_entry_from_set<'a>(
    set: &'a LayoutSetWithItems,
    schedule_start_hhmm: &str,
    now: DateTime<Local>,
) -> Option<&'a crate::models::LayoutSetEntry> {
    if set.items.is_empty() {
        return None;
    }
    let mut entries: Vec<&crate::models::LayoutSetEntry> = set.items.iter().collect();
    // Stable sort by position so shuffle is deterministic when applied.
    entries.sort_by_key(|e| e.position);

    // Total cycle length = sum of each entry's interval_secs (falling
    // back to the set's `dwell_secs` per entry).
    let total_cycle: u64 = entries
        .iter()
        .map(|e| {
            e.interval_secs
                .map(|i| i as u64)
                .unwrap_or(set.dwell_secs as u64)
        })
        .sum();
    if total_cycle == 0 {
        // All entries have 0 interval. Fall back to the first entry.
        return entries.first().copied();
    }

    // Compute seconds-into-schedule-window.
    let (sh, sm) = parse_hhmm(schedule_start_hhmm);
    let start_seconds_today = sh * 3600 + sm * 60;
    let now_seconds_today = now.hour() * 3600 + now.minute() * 60 + now.second() as u32;
    // The schedule might cross midnight; if now is before start on the
    // wall clock, treat it as yesterday.
    let seconds_into = if now_seconds_today >= start_seconds_today {
        (now_seconds_today - start_seconds_today) as u64
    } else {
        // Crossed midnight: now is on the next day.
        let secs_today_remaining = 24 * 3600 - start_seconds_today;
        secs_today_remaining as u64 + now_seconds_today as u64
    };
    let cycle_pos = seconds_into % total_cycle;

    // Optionally shuffle (deterministic per-day).
    if set.shuffle {
        let day_seed = now.format("%Y%m%d").to_string();
        let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
        for byte in day_seed.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
        }
        // Fisher-Yates with our seeded RNG.
        for i in (1..entries.len()).rev() {
            hash = hash.wrapping_mul(0x100000001b3) ^ (hash >> 13);
            let j = (hash as usize) % (i + 1);
            entries.swap(i, j);
        }
    }

    // Walk the cumulative sum and pick the entry whose window
    // contains cycle_pos.
    let mut cum: u64 = 0;
    for entry in &entries {
        let dur = entry
            .interval_secs
            .map(|i| i as u64)
            .unwrap_or(set.dwell_secs as u64);
        if cycle_pos < cum + dur {
            return Some(*entry);
        }
        cum += dur;
    }
    entries.first().copied()
}

fn parse_hhmm(s: &str) -> (u32, u32) {
    let mut it = s.split(':');
    let h = it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    let m = it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    (h, m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::models::LayoutSetEntry;
    use rusqlite::Connection;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::runtime::Runtime;

    /// Build a `Database` open on a unique temp file. Each test
    /// uses its own path so they can run in parallel.
    fn fresh_db() -> (Arc<Database>, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let path = std::env::temp_dir()
            .join(format!("ripley-sched-test-{pid}-{n}.db"));
        let _ = std::fs::remove_file(&path);
        let db = Database::open(path.to_str().unwrap()).expect("open");
        (Arc::new(db), path)
    }

    fn monday_afternoon() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 14, 14, 30, 0).unwrap()
    }

    /// Insert a real `layouts` row so the FK from `schedules.layout_id`
    /// is satisfied. Returns the new layout's id.
    fn insert_layout(db: &Database, id: &str) -> String {
        use crate::models::SetScheduleItem;
        let layout = crate::models::Layout {
            id: id.to_string(),
            name: id.to_string(),
            widgets: Vec::new(),
            interactive: false,
            page_type: "single".to_string(),
            timing_mode: "global".to_string(),
            duration: 15,
            shuffle: false,
            items: Vec::<SetScheduleItem>::new(),
            canvas_orientation: "landscape".to_string(),
            folder_id: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        db.save_layout(&layout).expect("create layout");
        id.to_string()
    }

    fn active_schedule(id: &str, display: &str, layout_id: &str) -> ScheduleEntry {
        ScheduleEntry {
            id: id.to_string(),
            display: display.to_string(),
            layout_id: layout_id.to_string(),
            layout_set_id: None,
            day_of_week: DAY_EVERY,
            start_time: "00:00".to_string(),
            end_time: "23:59".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            action: "switch_layout".to_string(),
            target_client: None,
            power_state: None,
            display_name: Some(display.to_string()),
            control_method: None,
            display_index: None,
        }
    }

    #[test]
    fn schedule_entry_default_action_is_switch_layout() {
        assert_eq!(default_action(), "switch_layout");
    }

    #[test]
    fn schedule_entry_serializes_round_trip() {
        let entry = ScheduleEntry {
            id: "test-1".to_string(),
            display: "DP-1".to_string(),
            layout_id: "layout-A".to_string(),
            layout_set_id: None,
            day_of_week: DAY_EVERY,
            start_time: "08:00".to_string(),
            end_time: "17:00".to_string(),
            created_at: "2026-09-12T00:00:00Z".to_string(),
            action: "switch_layout".to_string(),
            target_client: None,
            power_state: None,
            display_name: Some("Test Display".to_string()),
            control_method: None,
            display_index: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: ScheduleEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "test-1");
        assert_eq!(back.layout_id, "layout-A");
    }

    #[test]
    fn schedule_entry_action_defaults_when_missing() {
        let json = r#"{
            "id": "x", "display": "DP-1", "layout_id": "L",
            "day_of_week": -1, "start_time": "00:00", "end_time": "23:59",
            "created_at": "2026-01-01T00:00:00Z"
        }"#;
        let entry: ScheduleEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.action, "switch_layout");
    }

    #[test]
    fn scheduler_applies_active_switch_layout_schedule() {
        let rt = Runtime::new().expect("runtime");
        rt.block_on(async {
            let sched = active_schedule("s1", "TestDisplay", "layout-A");
            let (db, _path) = fresh_db();
            insert_layout(&db, "layout-A");
            db.create_schedule(&sched).expect("create_schedule");
            assert!(db.get_display_assignment("TestDisplay").unwrap().is_none());
            apply_active_schedules_at(&db, monday_afternoon())
                .await
                .expect("tick");
            let assignment = db
                .get_display_assignment("TestDisplay")
                .unwrap()
                .expect("assignment should now exist");
            assert_eq!(assignment.layout_id.as_deref(), Some("layout-A"));
        });
    }

    #[test]
    fn scheduler_idempotent_when_assignment_already_matches() {
        let rt = Runtime::new().expect("runtime");
        rt.block_on(async {
            let sched = active_schedule("s1", "TestDisplay", "layout-A");
            let (db, _path) = fresh_db();
            insert_layout(&db, "layout-A");
            db.create_schedule(&sched).expect("create_schedule");
            let now = monday_afternoon();
            apply_active_schedules_at(&db, now).await.expect("first tick");
            let epoch_after_first = db.get_refresh_epoch().unwrap();
            assert!(epoch_after_first > 0);
            apply_active_schedules_at(&db, now).await.expect("second tick");
            let epoch_after_second = db.get_refresh_epoch().unwrap();
            assert_eq!(epoch_after_first, epoch_after_second);
        });
    }

    #[test]
    fn scheduler_skips_inactive_schedule_by_day_mask() {
        let rt = Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut sched = active_schedule("s1", "TestDisplay", "layout-A");
            sched.day_of_week = 1 << 6; // Saturday only
            let (db, _path) = fresh_db();
            insert_layout(&db, "layout-A");
            db.create_schedule(&sched).expect("create_schedule");
            apply_active_schedules_at(&db, monday_afternoon())
                .await
                .expect("tick");
            assert!(db.get_display_assignment("TestDisplay").unwrap().is_none());
        });
    }

    #[test]
    fn scheduler_skips_inactive_schedule_by_time_window() {
        let rt = Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut sched = active_schedule("s1", "TestDisplay", "layout-A");
            sched.start_time = "22:00".to_string();
            sched.end_time = "23:00".to_string();
            let (db, _path) = fresh_db();
            insert_layout(&db, "layout-A");
            db.create_schedule(&sched).expect("create_schedule");
            apply_active_schedules_at(&db, monday_afternoon())
                .await
                .expect("tick");
            assert!(db.get_display_assignment("TestDisplay").unwrap().is_none());
        });
    }

    // ── Layout-set cycling tests ─────────────────────────────────

    fn make_set_with_items(
        id: &str,
        items: Vec<(&'static str, u32, u32)>, // (layout_id, position, interval_secs)
    ) -> LayoutSetWithItems {
        LayoutSetWithItems {
            id: id.to_string(),
            name: id.to_string(),
            shuffle: false,
            dwell_secs: 30,
            items: items
                .into_iter()
                .map(|(layout_id, position, interval_secs)| LayoutSetEntry {
                    id: format!("{id}-{layout_id}"),
                    layout_set_id: id.to_string(),
                    layout_id: layout_id.to_string(),
                    position,
                    interval_secs: Some(interval_secs),
                    transition: None,
                })
                .collect(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn pick_entry_first_when_at_schedule_start() {
        // At the start of the schedule, we should pick the first
        // entry.
        let set = make_set_with_items(
            "set",
            vec![("A", 0, 60), ("B", 1, 60), ("C", 2, 60)],
        );
        let entry = pick_entry_from_set(&set, "14:00", monday_afternoon()).unwrap();
        assert_eq!(entry.layout_id, "A");
    }

    #[test]
    fn pick_entry_advances_within_cycle() {
        // cycle = 180s (60+60+60). At 14:01:30 (90s into), we
        // should be in the second entry (B).
        let set = make_set_with_items(
            "set",
            vec![("A", 0, 60), ("B", 1, 60), ("C", 2, 60)],
        );
        let t = Local.with_ymd_and_hms(2026, 9, 14, 14, 1, 30).unwrap();
        let entry = pick_entry_from_set(&set, "14:00", t).unwrap();
        assert_eq!(entry.layout_id, "B");
    }

    #[test]
    fn pick_entry_loops_within_cycle() {
        // cycle = 180s. At 14:02:30 (150s into), we should be in
        // the third entry (C).
        let set = make_set_with_items(
            "set",
            vec![("A", 0, 60), ("B", 1, 60), ("C", 2, 60)],
        );
        let t = Local.with_ymd_and_hms(2026, 9, 14, 14, 2, 30).unwrap();
        let entry = pick_entry_from_set(&set, "14:00", t).unwrap();
        assert_eq!(entry.layout_id, "C");
    }

    #[test]
    fn pick_entry_wraps_to_next_cycle() {
        // cycle = 180s. At 14:00:30 + 180s = 14:03:30, we're back
        // to entry A (180 mod 180 == 0).
        let set = make_set_with_items(
            "set",
            vec![("A", 0, 60), ("B", 1, 60), ("C", 2, 60)],
        );
        let t = Local.with_ymd_and_hms(2026, 9, 14, 14, 3, 30).unwrap();
        let entry = pick_entry_from_set(&set, "14:00", t).unwrap();
        assert_eq!(entry.layout_id, "A");
    }

    #[test]
    fn pick_entry_handles_unequal_intervals() {
        // cycle = 360s (60 + 120 + 180). At 14:01:30 we're 90s
        // in: A is [0, 60), B is [60, 180), so we should be in B.
        let set = make_set_with_items(
            "set",
            vec![("A", 0, 60), ("B", 1, 120), ("C", 2, 180)],
        );
        let t = Local.with_ymd_and_hms(2026, 9, 14, 14, 1, 30).unwrap();
        let entry = pick_entry_from_set(&set, "14:00", t).unwrap();
        assert_eq!(entry.layout_id, "B");
    }

    #[test]
    fn pick_entry_with_shuffle_is_deterministic_per_day() {
        let mut set = make_set_with_items(
            "set",
            vec![("A", 0, 60), ("B", 1, 60), ("C", 2, 60), ("D", 3, 60)],
        );
        set.shuffle = true;
        let t = Local.with_ymd_and_hms(2026, 9, 14, 14, 0, 0).unwrap();
        let first = pick_entry_from_set(&set, "14:00", t).unwrap().layout_id.clone();
        let second = pick_entry_from_set(&set, "14:00", t).unwrap().layout_id.clone();
        assert_eq!(first, second, "shuffle must be deterministic for the same time");
    }

    #[test]
    fn pick_entry_with_shuffle_can_vary_across_days() {
        // With 4 items and shuffle, the day-seed should produce
        // different orderings on different days for at least some
        // day pair.
        let mut set = make_set_with_items(
            "set",
            vec![("A", 0, 60), ("B", 1, 60), ("C", 2, 60), ("D", 3, 60)],
        );
        set.shuffle = true;
        let t1 = Local.with_ymd_and_hms(2026, 9, 14, 14, 0, 0).unwrap();
        let t2 = Local.with_ymd_and_hms(2026, 9, 15, 14, 0, 0).unwrap();
        let first1 = pick_entry_from_set(&set, "14:00", t1).unwrap().layout_id.clone();
        let first2 = pick_entry_from_set(&set, "14:00", t2).unwrap().layout_id.clone();
        // Statistically with 4 items, different days should yield
        // different orderings with overwhelming probability. If
        // this ever flakes, increase the item count.
        assert_ne!(first1, first2, "shuffle order should differ between days");
    }

    #[test]
    fn pick_entry_empty_set_returns_none() {
        let set = make_set_with_items("set", vec![]);
        assert!(pick_entry_from_set(&set, "14:00", monday_afternoon()).is_none());
    }

    #[test]
    fn pick_entry_falls_back_to_first_when_all_intervals_zero() {
        // If all entries have interval_secs = 0, total_cycle = 0;
        // we should fall back to the first entry rather than
        // dividing by zero.
        let set = make_set_with_items(
            "set",
            vec![("A", 0, 0), ("B", 1, 0), ("C", 2, 0)],
        );
        let entry = pick_entry_from_set(&set, "14:00", monday_afternoon()).unwrap();
        assert_eq!(entry.layout_id, "A");
    }

    #[test]
    fn scheduler_cycles_through_layout_set() {
        // End-to-end: create a schedule with layout_set_id and
        // verify the scheduler picks the current entry from the
        // set and writes it to display_state.
        let rt = Runtime::new().expect("runtime");
        rt.block_on(async {
            let (db, _path) = fresh_db();
            // The `layout_set_entries` table has a FK to
            // `layouts(id)`, so we must create the layouts before
            // creating the set. We need a raw Connection for this
            // since the `Database` struct doesn't expose
            // layout creation helpers.
            {
                let conn = Connection::open(_path.to_str().unwrap()).unwrap();
                for lid in &["layout-A", "layout-B", "layout-C"] {
                    conn.execute(
                        "INSERT INTO layouts                          (id, name, page_type, timing_mode, duration,                           shuffle, interactive, items, created_at, updated_at)                          VALUES (?1, ?1, 'single', 'global', 15, 0, 0, '[]',                          '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                        rusqlite::params![lid],
                    )
                    .unwrap();
                }
            }
            // Create a layout set with 3 entries, each 60s long.
            let set = LayoutSetWithItems {
                id: "test-set".to_string(),
                name: "test set".to_string(),
                shuffle: false,
                dwell_secs: 30,
                items: vec![
                    LayoutSetEntry {
                        id: "test-set-A".to_string(),
                        layout_set_id: "test-set".to_string(),
                        layout_id: "layout-A".to_string(),
                        position: 0,
                        interval_secs: Some(60),
                        transition: None,
                    },
                    LayoutSetEntry {
                        id: "test-set-B".to_string(),
                        layout_set_id: "test-set".to_string(),
                        layout_id: "layout-B".to_string(),
                        position: 1,
                        interval_secs: Some(60),
                        transition: None,
                    },
                    LayoutSetEntry {
                        id: "test-set-C".to_string(),
                        layout_set_id: "test-set".to_string(),
                        layout_id: "layout-C".to_string(),
                        position: 2,
                        interval_secs: Some(60),
                        transition: None,
                    },
                ],
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            };
            db.create_layout_set(&set).expect("create set");
            // Create a schedule that uses the set.
            let sched = ScheduleEntry {
                id: "cycle-sched".to_string(),
                display: "CycleDisplay".to_string(),
                layout_id: "".to_string(), // empty - use the set
                layout_set_id: Some("test-set".to_string()),
                day_of_week: DAY_EVERY,
                start_time: "00:00".to_string(),
                end_time: "23:59".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                action: "switch_layout".to_string(),
                target_client: None,
                power_state: None,
                display_name: Some("Cycle Display".to_string()),
                control_method: None,
                display_index: None,
            };
            db.create_schedule(&sched).expect("create schedule");

            // Tick at 00:00:30 - should be in layout-A.
            let t1 = Local.with_ymd_and_hms(2026, 9, 14, 0, 0, 30).unwrap();
            apply_active_schedules_at(&db, t1).await.expect("tick 1");
            let a1 = db.get_display_assignment("CycleDisplay").unwrap();
            assert_eq!(a1.unwrap().layout_id.as_deref(), Some("layout-A"));

            // Tick at 00:01:30 - should advance to layout-B.
            let t2 = Local.with_ymd_and_hms(2026, 9, 14, 0, 1, 30).unwrap();
            apply_active_schedules_at(&db, t2).await.expect("tick 2");
            let a2 = db.get_display_assignment("CycleDisplay").unwrap();
            assert_eq!(a2.unwrap().layout_id.as_deref(), Some("layout-B"));
        });
    }
}
