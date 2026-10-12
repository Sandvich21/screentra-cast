// In-memory screenshot store for client display previews.

use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use std::sync::OnceLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use std::sync::Arc as StdArc;
pub type AppState = StdArc<crate::db::Database>;

static SCREENSHOT_STORE: OnceLock<ScreenshotStore> = OnceLock::new();

/// Maximum bytes per client across all their displays.
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// A cached screenshot for one display.
#[derive(Clone)]
pub struct CachedScreenshot {
    pub data: Vec<u8>,
    pub timestamp: Instant,
}

/// Global screenshot store, cloned into each handler.
#[derive(Clone)]
pub struct ScreenshotStore {
    inner: Arc<RwLock<HashMap<String, HashMap<u32, CachedScreenshot>>>>,
}

impl ScreenshotStore {
    pub fn new(_capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn put(&self, client_id: &str, display: u32, data: Vec<u8>) {
        let mut store = self.inner.write().await;
        let client = store.entry(client_id.to_string()).or_default();
        let used_before = client.values().map(|s| s.data.len()).sum::<usize>();
        client.insert(display, CachedScreenshot { data, timestamp: Instant::now() });
        // Evict oldest frames until we fit within budget
        let used_after = client.values().map(|s| s.data.len()).sum::<usize>();
        if used_after > MAX_BYTES {
            self.evict_to_budget(client, used_after);
        }
        // Also enforce global client budget across all displays
        let mut total = 0;
        let displays: std::collections::HashSet<_> = client.keys().cloned().collect();
        for disp in displays {
            if let Some(scr) = client.get(&disp) {
                total += scr.data.len();
                while total > MAX_BYTES {
                    if let Some((oldest_disp, _)) = client
                        .iter()
                        .min_by_key(|(_, s)| s.timestamp)
                        .map(|(d, s)| (*d, s.data.len()))
                    {
                        if let Some(removed) = client.remove(&oldest_disp) {
                            total -= removed.data.len();
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    }

    fn evict_to_budget(&self, client: &mut HashMap<u32, CachedScreenshot>, used: usize) {
        if used <= MAX_BYTES {
            return;
        }
        // Sort by timestamp ascending and remove oldest until under budget
        let mut entries: Vec<_> = client.iter().map(|(d, s)| (*d, s.timestamp, s.data.len())).collect();
        entries.sort_by_key(|(_, ts, _)| *ts);
        let mut total = used;
        for (disp, _, len) in entries {
            if total <= MAX_BYTES {
                break;
            }
            if let Some(removed) = client.remove(&disp) {
                total -= removed.data.len();
            }
        }
    }

    pub async fn get(&self, client_id: &str, display: u32) -> Option<CachedScreenshot> {
        let store = self.inner.read().await;
        store.get(client_id).and_then(|d| d.get(&display).cloned())
    }

    pub async fn get_with_timestamp(&self, client_id: &str, display: u32) -> Option<(CachedScreenshot, u64)> {
        let store = self.inner.read().await;
        store.get(client_id).and_then(|d| {
            d.get(&display).map(|s| {
                let elapsed = s.timestamp.elapsed().as_secs();
                (s.clone(), elapsed)
            })
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct ScreenshotQuery {
    pub display: Option<u32>,
}

pub(crate) async fn post_screenshot(
    Path(client_id): Path<String>,
    Query(q): Query<ScreenshotQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    let store = SCREENSHOT_STORE.get().unwrap();
    let display = q.display.unwrap_or(0);
    if body.is_empty() {
        return StatusCode::NO_CONTENT;
    }
    store.put(&client_id, display, body.into()).await;
    StatusCode::OK
}

pub(crate) async fn get_screenshot(
    Path(client_id): Path<String>,
    Query(q): Query<ScreenshotQuery>,
) -> Result<Response, StatusCode> {
    let store = SCREENSHOT_STORE.get().unwrap();
    let display = q.display.unwrap_or(0);
    match store.get(&client_id, display).await {
        Some(scr) => {
            let mut res = Response::new(scr.data.into());
            res.headers_mut().insert(
                header::CONTENT_TYPE,
                "image/jpeg".parse().unwrap(),
            );
            // Cache-Control: no-store so dashboards always fetch fresh
            res.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
            Ok(res)
        }
        None => Err(StatusCode::NO_CONTENT),
    }
}

#[derive(Serialize)]
pub struct ScreenshotInfo {
    pub client_id: String,
    pub display: u32,
    pub age_seconds: u64,
}

pub(crate) async fn list_screenshots(
) -> Json<Vec<ScreenshotInfo>> {
    let store = SCREENSHOT_STORE.get().unwrap();
    let data = store.inner.read().await;
    let mut out = Vec::new();
    for (client_id, displays) in data.iter() {
        for (&display, scr) in displays {
            out.push(ScreenshotInfo {
                client_id: client_id.clone(),
                display,
                age_seconds: scr.timestamp.elapsed().as_secs(),
            });
        }
    }
    Json(out)
}

/// Initialize the global screenshot store. Must be called once before use.
pub fn init(max_bytes: usize) {
    SCREENSHOT_STORE.set(ScreenshotStore::new(max_bytes)).ok();
}

pub fn routes(app_state: AppState) -> (axum::routing::MethodRouter<AppState>, axum::routing::MethodRouter<AppState>, axum::routing::MethodRouter<AppState>) {
    (
        post(post_screenshot).with_state(app_state.clone()),
        get(get_screenshot).with_state(app_state.clone()),
        get(list_screenshots).with_state(app_state),
    )
}
