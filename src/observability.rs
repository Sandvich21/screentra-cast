use axum::{
    extract::Request,
    http::{header::HeaderValue, HeaderName, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;
use uuid::Uuid;

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    started_at: Instant,
    requests_total: AtomicU64,
    responses_2xx: AtomicU64,
    responses_3xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
    in_flight: AtomicU64,
    database_ready: AtomicU64,
    readiness_checks_total: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

impl Metrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner {
                started_at: Instant::now(),
                requests_total: AtomicU64::new(0),
                responses_2xx: AtomicU64::new(0),
                responses_3xx: AtomicU64::new(0),
                responses_4xx: AtomicU64::new(0),
                responses_5xx: AtomicU64::new(0),
                in_flight: AtomicU64::new(0),
                database_ready: AtomicU64::new(0),
                readiness_checks_total: AtomicU64::new(0),
            }),
        }
    }

    pub fn render_prometheus(&self) -> String {
        let uptime = self.inner.started_at.elapsed().as_secs_f64();
        format!(
            "# HELP screentra_cast_uptime_seconds Process uptime in seconds.\n\
             # TYPE screentra_cast_uptime_seconds gauge\n\
             screentra_cast_uptime_seconds {uptime:.3}\n\
             # HELP screentra_cast_http_requests_total Total HTTP requests received.\n\
             # TYPE screentra_cast_http_requests_total counter\n\
             screentra_cast_http_requests_total {requests}\n\
             # HELP screentra_cast_http_responses_total HTTP responses by status class.\n\
             # TYPE screentra_cast_http_responses_total counter\n\
             screentra_cast_http_responses_total{{class=\"2xx\"}} {responses_2xx}\n\
             screentra_cast_http_responses_total{{class=\"3xx\"}} {responses_3xx}\n\
             screentra_cast_http_responses_total{{class=\"4xx\"}} {responses_4xx}\n\
             screentra_cast_http_responses_total{{class=\"5xx\"}} {responses_5xx}\n\
             # HELP screentra_cast_http_in_flight Current HTTP requests being handled.\n\
             # TYPE screentra_cast_http_in_flight gauge\n\
             screentra_cast_http_in_flight {in_flight}\n\
             # HELP screentra_cast_database_ready Whether the last readiness check reached the database.\n\
             # TYPE screentra_cast_database_ready gauge\n\
             screentra_cast_database_ready {database_ready}\n\
             # HELP screentra_cast_readiness_checks_total Total readiness probes.\n\
             # TYPE screentra_cast_readiness_checks_total counter\n\
             screentra_cast_readiness_checks_total {readiness_checks}\n",
            uptime = uptime,
            requests = self.inner.requests_total.load(Ordering::Relaxed),
            responses_2xx = self.inner.responses_2xx.load(Ordering::Relaxed),
            responses_3xx = self.inner.responses_3xx.load(Ordering::Relaxed),
            responses_4xx = self.inner.responses_4xx.load(Ordering::Relaxed),
            responses_5xx = self.inner.responses_5xx.load(Ordering::Relaxed),
            in_flight = self.inner.in_flight.load(Ordering::Relaxed),
            database_ready = self.inner.database_ready.load(Ordering::Relaxed),
            readiness_checks = self.inner.readiness_checks_total.load(Ordering::Relaxed),
        )
    }

    pub fn record_readiness(&self, ready: bool) {
        self.inner
            .database_ready
            .store(u64::from(ready), Ordering::Relaxed);
        self.inner
            .readiness_checks_total
            .fetch_add(1, Ordering::Relaxed);
    }

    fn request_started(&self) {
        self.inner.requests_total.fetch_add(1, Ordering::Relaxed);
        self.inner.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    fn request_finished(&self, status: StatusCode) {
        let counter = match status.as_u16() {
            200..=299 => &self.inner.responses_2xx,
            300..=399 => &self.inner.responses_3xx,
            400..=499 => &self.inner.responses_4xx,
            _ => &self.inner.responses_5xx,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        self.inner.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub async fn request_layer(
    axum::extract::State(metrics): axum::extract::State<Metrics>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_request_id(value))
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    request.extensions_mut().insert(RequestId(request_id.clone()));
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    metrics.request_started();
    let mut response = next.run(request).await;
    let status = response.status();
    let elapsed = started.elapsed();
    metrics.request_finished(status);
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    log::info!(
        "http request_id={} method={} path={} status={} duration_ms={} ",
        request_id,
        method,
        path,
        status.as_u16(),
        elapsed.as_secs_f64() * 1000.0,
    );
    response
}
