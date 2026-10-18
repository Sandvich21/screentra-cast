use serde::{Deserialize, Serialize};

/// A widget is a single element placed on a layout (image, text, clock, video, web page).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Widget {
    pub id: String,
    #[serde(alias = "type")]
    pub widget_type: WidgetType,
    /// X position as percentage of display width (0.0 - 1.0)
    pub x: f32,
    /// Y position as percentage of display height (0.0 - 1.0)
    pub y: f32,
    /// Width as percentage of display width (0.0 - 1.0)
    pub w: f32,
    /// Height as percentage of display height (0.0 - 1.0)
    pub h: f32,
    /// Z-index for layering (higher = on top)
    pub z: i32,
    /// Widget-specific properties (e.g. text content, image URL, font size)
    pub props: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WidgetType {
    Text,
    Image,
    Video,
    Clock,
    // Countdown. The editor exposes this widget type and writes it
    // to layouts as `countdown`, but the server has never had the
    // matching enum variant so the JSON deserializer rejected it
    // with "unknown variant" and the layout failed to save. The
    // player runtime now renders it (player.js `widget_type ===
    // 'countdown'` branch) so a Round-trip is safe.
    Countdown,
    Webpage,
    Rectangle,
    DateTime,
    Calendar,
    Weather,
    /// Browser-local camera widget. Capture is opt-in in the renderer and
    /// never uploads frames to the server.
    Camera,
    Traffic,
    Music,
    Sound,
    // News ticker. Added in Phase X so the Widget Templates editor
    // can build custom news layouts. Persisted as the JSON string
    // "news" so it round-trips with the existing db.rs match arm.
    News,
    // Interactive widget types. Added in Phase 4 so the editor can
    // save layouts containing these without serde rejecting unknown
    // variants. The runtime (player.js) handles them via
    // `data-interactive-*` attributes regardless of the variant name.
    Button,
    Dropdown,
    Checkbox,
    // The hyphenated form `checkbox-list` is what the editor and runtime
    // have always used. `rename_all = "lowercase"` on the enum strips
    // hyphens, so override the rename for this variant to keep the JSON
    // form consistent with the editor (`makeLayoutWidget` uses
    // `checkbox-list`) and the runtime (`buildWidgetHTML` checks for
    // `checkbox-list`).
    #[serde(rename = "checkbox-list")]
    CheckboxList,
    Radio,
    Textbox,
    Keyboard,
}

fn default_canvas_orientation() -> String { "landscape".to_string() }

/// A layout is a collection of widgets assigned to a specific display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layout {
    pub id: String,
    pub name: String,
    pub widgets: Vec<Widget>,
    /// True if this is an interactive layout (collects submissions)
    #[serde(default)]
    pub interactive: bool,
    /// Canvas orientation used by the layout editor: "landscape" or "portrait".
    #[serde(default = "default_canvas_orientation")]
    pub canvas_orientation: String,
    #[serde(default)]
    pub folder_id: Option<String>,
    /// Page type: "single", "interactive", or "set"
    #[serde(default)]
    pub page_type: String,
    /// Timing mode: "global" or "per_layout"
    #[serde(default)]
    pub timing_mode: String,
    /// Default duration in seconds for set items
    #[serde(default)]
    pub duration: i32,
    /// Shuffle set items
    #[serde(default)]
    pub shuffle: bool,
    /// Set items: [{layout_id, duration}]
    #[serde(default)]
    pub items: Vec<SetScheduleItem>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutRevision {
    pub layout_id: String,
    pub revision: i64,
    pub snapshot: Layout,
    pub created_at: String,
    #[serde(default)]
    pub published_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetScheduleItem {
    pub layout_id: String,
    #[serde(default)]
    pub duration: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutFolder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSet {
    pub id: String,
    pub name: String,
    pub shuffle: bool,
    pub dwell_secs: u32,
    pub items: Vec<LayoutSetItem>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSetItem {
    pub id: String,
    pub layout_id: String,
    pub position: u32,
    /// Per-item interval override. Defaults to the layout set's `dwell_secs` when 0.
    #[serde(default)]
    pub interval_secs: u32,
}

/// Database-level layout set entry (normalized storage).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSetEntry {
    pub id: String,
    pub layout_set_id: String,
    pub layout_id: String,
    pub position: u32,
    pub interval_secs: Option<u32>,
    pub transition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSetSummary {
    pub id: String,
    pub name: String,
    pub shuffle: bool,
    pub dwell_secs: u32,
    pub item_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

/// Full layout set with resolved items (used in API responses).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSetWithItems {
    pub id: String,
    pub name: String,
    pub shuffle: bool,
    pub dwell_secs: u32,
    pub items: Vec<LayoutSetEntry>,
    pub created_at: String,
    pub updated_at: String,
}


// ── Layout Set Request/Response Types ────────────────────────────────────────

/// Payload for creating a new layout set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLayoutSet {
    pub id: String,
    pub name: String,
    pub shuffle: bool,
    #[serde(default = "default_dwell_secs")]
    pub dwell_secs: u32,
    #[serde(default)]
    pub items: Vec<CreateLayoutSetItem>,
}

fn default_dwell_secs() -> u32 { 30 }

/// Item in create payload — only layout_id and position are needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLayoutSetItem {
    pub layout_id: String,
    pub position: u32,
    #[serde(default)]
    pub interval_secs: Option<u32>,
}

/// Payload for updating an existing layout set (partial update).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateLayoutSet {
    pub name: Option<String>,
    pub shuffle: Option<bool>,
    #[serde(default)]
    pub dwell_secs: Option<u32>,
}

impl UpdateLayoutSet {
    /// Apply this partial update to an existing LayoutSetWithItems, returning a new one.
    pub fn apply_to(&self, mut set: LayoutSetWithItems) -> LayoutSetWithItems {
        if let Some(name) = &self.name {
            set.name = name.clone();
        }
        if let Some(shuffle) = self.shuffle {
            set.shuffle = shuffle;
        }
        if let Some(dwell_secs) = self.dwell_secs {
            set.dwell_secs = dwell_secs;
        }
        set.updated_at = chrono::Utc::now().to_rfc3339();
        set
    }
}

/// Payload for adding a layout to a set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddLayoutSetEntry {
    pub layout_id: String,
    pub position: u32,
    #[serde(default)]
    pub interval_secs: Option<u32>,
}

/// Payload for updating a single entry (partial update).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateLayoutSetEntry {
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub interval_secs: Option<u32>,
}

/// A detected physical display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub connector: String, // e.g. "DP-1"
    pub status: String,    // "connected" or "disconnected"
    pub enabled: bool,
    pub width_mm: u32,           // Physical width in mm
    pub height_mm: u32,          // Physical height in mm
    pub modes: Vec<DisplayMode>, // Supported resolutions
    pub current_mode: Option<DisplayMode>,
    pub label: String, // Human-friendly label
    // User-forced override dimensions (for adapters that report wrong EDID)
    pub override_width: Option<u32>,
    pub override_height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh_rate: f32,
}

/// System status for the management page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub hostname: String,
    pub os_name: String,
    pub os_version: String,
    pub uptime_seconds: u64,
    pub cpu_usage: f32,
    pub cpu_count: usize,
    pub memory_total: u64,
    pub memory_used: u64,
    pub disk_total: u64,
    pub disk_used: u64,
    pub cpu_temp: Option<f32>,
    pub displays: Vec<DisplayInfo>,
    pub service_status: String,
}

/// What display a layout should be shown on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayAssignment {
    pub display: String,
    pub layout_id: Option<String>,
}

/// Active display state — which layout is currently showing on which display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayState {
    pub assignments: Vec<DisplayAssignment>,
}

/// Metadata for templates stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMeta {
    pub id: String,
    pub name: String,
    pub category: String,
    pub is_builtin: bool,
    pub created_at: String,
}

/// Represents a system backup/snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub file_path: String,
    pub size_bytes: u64,
}

// ── Widget Presets ──────────────────────────────────────────
// A "widget preset" is a reusable bundle of default properties for a widget type.
// When a user drags a new widget onto the layout, they can pick a preset to seed
// the widget's props. Custom assets (e.g. weather icon overrides) are stored
// separately and looked up at render time.

/// Metadata for a widget preset (no props/assets — used in list views).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetPresetMeta {
    pub id: String,
    pub name: String,
    pub widget_type: String,
    #[serde(default)]
    pub is_builtin: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A custom asset (e.g. weather icon) belonging to a widget preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetPresetAsset {
    pub id: String,
    pub preset_id: String,
    /// e.g. "weather_icon"
    pub kind: String,
    /// e.g. WMO code as a string ("0", "65", "95")
    pub key: String,
    /// URL or relative path to the custom asset
    pub url: String,
}

/// A full widget preset including its custom assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetPreset {
    pub id: String,
    pub name: String,
    pub widget_type: String,
    /// JSON object of default widget props (e.g. `{ "color": "#fff", "font_size": 24 }`)
    pub props_json: String,
    pub created_at: String,
    pub updated_at: String,
    /// Custom assets (weather icons, etc.)
    #[serde(default)]
    pub assets: Vec<WidgetPresetAsset>,
}

impl WidgetPreset {
    /// Attach assets to a preset that was loaded from the db.
    /// The db layer builds a `WidgetPreset` without assets, then calls this to merge.
    pub fn into_preset_with_assets(mut self, assets: Vec<WidgetPresetAsset>) -> WidgetPreset {
        self.assets = assets;
        self
    }
}
/// One row from the `schedule_runs` audit log. Written by the
/// scheduler every time a schedule fires. The `target` field is
/// the display name for `switch_layout` actions, or the target
/// client id for `power_on` / `power_off`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScheduleRun {
    pub id: i64,
    pub schedule_id: String,
    pub action: String,
    pub target: String,
    pub success: bool,
    pub message: Option<String>,
    pub occurred_at: String,
}

/// One security-relevant administrative or operator action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: i64,
    pub actor: Option<String>,
    pub role: Option<String>,
    pub event_type: String,
    pub target: Option<String>,
    pub outcome: String,
    pub detail: Option<String>,
    pub occurred_at: String,
}

/// A cached RSS / Atom feed for a News widget.
///
/// `url` is the primary key. The maintenance loop refreshes any feed
/// whose `last_fetched_at` is older than the configured threshold
/// (default 5 minutes). The latest `headlines_json` is what the
/// player / news-template.html reads via GET /api/news/headlines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsFeed {
    pub url: String,
    #[serde(default)]
    pub title: String,
    /// JSON array of NewsHeadline objects.
    pub headlines_json: String,
    pub last_fetched_at: String,
    #[serde(default)]
    pub last_error: Option<String>,
    /// HTTP ETag from the last successful fetch (for conditional GETs).
    #[serde(default)]
    pub etag: Option<String>,
    /// Last-Modified header value from the last successful fetch.
    #[serde(default)]
    pub last_modified: Option<String>,
}

/// One headline parsed from an RSS / Atom feed. Just the fields the
/// ticker needs; the rest of the feed is ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsHeadline {
    pub title: String,
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub source: String,
}
