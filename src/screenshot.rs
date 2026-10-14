// In-memory screenshot store for client display previews.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    Json,
};
use std::sync::OnceLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;


static SCREENSHOT_STORE: OnceLock<ScreenshotStore> = OnceLock::new();

/// A cached screenshot for one display.
#[derive(Clone)]
pub struct CachedScreenshot {
    pub data: Vec<u8>,
    pub content_type: String,
    pub timestamp: Instant,
}

/// Global screenshot store, cloned into each handler.
#[derive(Clone)]
pub struct ScreenshotStore {
    inner: Arc<RwLock<HashMap<String, HashMap<u32, CachedScreenshot>>>>,
    max_bytes: usize,
}

impl ScreenshotStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            max_bytes: capacity.max(1),
        }
    }

    pub async fn put(&self, client_id: &str, display: u32, data: Vec<u8>, content_type: String) {
        let mut store = self.inner.write().await;
        store.entry(client_id.to_string()).or_default().insert(
            display,
            CachedScreenshot { data, content_type, timestamp: Instant::now() },
        );

        // The configured budget is global, not merely per client. Evict the
        // oldest frames across all clients so a fleet cannot exhaust memory.
        let mut total: usize = store
            .values()
            .flat_map(|displays| displays.values())
            .map(|s| s.data.len())
            .sum();
        while total > self.max_bytes {
            let oldest = store.iter()
                .flat_map(|(client, displays)| displays.iter().map(move |(display, frame)| {
                    (client.clone(), *display, frame.timestamp, frame.data.len())
                }))
                .min_by_key(|(_, _, timestamp, _)| *timestamp);
            let Some((client, display, _, length)) = oldest else { break };
            if let Some(displays) = store.get_mut(&client) {
                displays.remove(&display);
                if displays.is_empty() { store.remove(&client); }
            }
            total = total.saturating_sub(length);
        }
    }

    pub async fn get(&self, client_id: &str, display: u32) -> Option<CachedScreenshot> {
        let store = self.inner.read().await;
        store.get(client_id).and_then(|d| d.get(&display).cloned())
    }

    pub async fn clear(&self, client_id: &str, display: u32) {
        let mut store = self.inner.write().await;
        if let Some(displays) = store.get_mut(client_id) {
            displays.remove(&display);
            if displays.is_empty() {
                store.remove(client_id);
            }
        }
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
    State(repository): axum::extract::State<crate::api::ClientRepository>,
    Path(client_id): Path<String>,
    Query(q): Query<ScreenshotQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    if crate::api::require_client_command_token(&repository, &client_id, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED;
    }
    let store = SCREENSHOT_STORE.get().unwrap();
    let display = q.display.unwrap_or(0);
    if body.is_empty() {
        return StatusCode::NO_CONTENT;
    }
    const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
    if body.len() > MAX_FRAME_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| matches!(*value, "image/png" | "image/jpeg" | "image/webp"))
        .unwrap_or("image/png")
        .to_string();
    store.put(&client_id, display, body.into(), content_type).await;
    StatusCode::OK
}

pub(crate) async fn clear_screenshot(client_id: &str, display: u32) {
    if let Some(store) = SCREENSHOT_STORE.get() {
        store.clear(client_id, display).await;
    }
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
                scr.content_type.parse().unwrap(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn evicts_oldest_frames_until_global_budget_is_met() {
        let store = ScreenshotStore::new(5);
        store.put("client-a", 0, vec![1, 2, 3], "image/png".to_string()).await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        store.put("client-b", 0, vec![4, 5, 6], "image/png".to_string()).await;
        assert!(store.get("client-a", 0).await.is_none());
        assert_eq!(store.get("client-b", 0).await.unwrap().data, vec![4, 5, 6]);
    }

    #[tokio::test]
    async fn replacing_a_frame_does_not_count_the_old_bytes() {
        let store = ScreenshotStore::new(5);
        store.put("client", 0, vec![1, 2, 3, 4], "image/png".to_string()).await;
        store.put("client", 0, vec![9, 8], "image/png".to_string()).await;
        store.put("other", 0, vec![7, 6, 5], "image/png".to_string()).await;
        assert_eq!(store.get("client", 0).await.unwrap().data, vec![9, 8]);
        assert_eq!(store.get("other", 0).await.unwrap().data, vec![7, 6, 5]);
    }
}
