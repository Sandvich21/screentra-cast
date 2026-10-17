//! Per-IP rate limiting for sensitive endpoints.
//!
//! Two distinct buckets:
//!   - "auth_login": 5 attempts per 5 minutes per IP (prevents password brute force)
//!   - "files":      60 requests per minute per IP (prevents abusive file scans)
//!
//! Both are fixed-window counters keyed by `(ip, bucket)`. The buckets are
//! pure functions returning `Allow` or `Deny { retry_after_secs }`, so the
//! logic is unit-testable in isolation. The mutex state is the only impure
//! piece and is owned by the middleware.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Map of bucket key -> (window_start, count).
pub type BucketState = HashMap<String, (Instant, u32)>;

/// Outcome of a rate limit check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { retry_after_secs: u64 },
}

/// Bucket policy: max N requests per window.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub max_requests: u32,
    pub window: Duration,
}

impl Policy {
    pub const fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            max_requests,
            window,
        }
    }
}

pub const POLICY_AUTH_LOGIN: Policy = Policy::new(5, Duration::from_secs(300));
pub const POLICY_FILES: Policy = Policy::new(60, Duration::from_secs(60));
pub const POLICY_REGISTER: Policy = Policy::new(120, Duration::from_secs(60));
/// Per-IP cap for authenticated client-control mutations. This is deliberately
/// tighter than the general client bucket because each accepted request can
/// change physical-display state or queue a player action.
pub const POLICY_CLIENT_CONTROL: Policy = Policy::new(30, Duration::from_secs(60));
/// Per-IP cap for server update export/import. Update operations are expensive
/// and high impact, so a short burst must not become an update loop.
pub const POLICY_SERVER_UPDATE: Policy = Policy::new(3, Duration::from_secs(300));
/// Per-IP cap for interactive submissions. The endpoint is open (no admin token),
/// so this is the only abuse control besides the 8 KiB body cap inside the
/// handler. 60 req/min/IP keeps one interactive browser saturated while still
/// preventing disk-fill attacks from a single source.
pub const POLICY_INTERACTIVE_SUBMIT: Policy = Policy::new(60, Duration::from_secs(60));
/// Provider-backed traffic proxy requests have a separate quota bucket.
pub const POLICY_TRAFFIC: Policy = Policy::new(6, Duration::from_secs(300));

/// Maximum number of accepted commands in one burst per client/action.
pub const CLIENT_COMMAND_BURST_LIMIT: i64 = 10;
/// Cooldown applied after a burst is consumed.
pub const CLIENT_COMMAND_BURST_COOLDOWN_SECS: i64 = 300;

/// Legacy per-action spacing retained for callers that need the action-specific
/// safety interval. The durable command queue uses the burst policy above.
pub fn client_command_cooldown_secs(action: &str) -> u64 {
    match action {
        "restart_client" | "restart_kiosk" | "restart_renderer" | "update_client"
        | "reboot" | "reboot_client" => 300,
        "ddc_set" | "display_power" | "cec_send" | "ir_send" | "audio_set_default" | "audio_set_volume" => 10,
        "bluetooth_power" | "bluetooth_pair" | "bluetooth_remove" => 30,
        "reload_layout" | "refresh_cache" => 10,
        "set_display_count" | "set_touch_binding" => 60,
        _ => 15,
    }
}

/// Pure function: given the current state, the key, the policy, and the
/// current time, return the new state and the decision. This is the testable
/// core of the rate limiter.
///
/// The function mutates a single bucket entry; everything else is read-only.
pub fn check(state: &mut BucketState, key: &str, policy: Policy, now: Instant) -> Decision {
    let entry = state.entry(key.to_string()).or_insert((now, 0));
    let (window_start, count) = entry;

    // If the window has expired, reset.
    if now.duration_since(*window_start) >= policy.window {
        *window_start = now;
        *count = 0;
    }

    if *count >= policy.max_requests {
        let elapsed = now.duration_since(*window_start);
        let retry_after = policy.window.saturating_sub(elapsed).as_secs().max(1);
        return Decision::Deny {
            retry_after_secs: retry_after,
        };
    }

    *count += 1;
    Decision::Allow
}

/// Thin wrapper that holds the mutex state and exposes a thread-safe check.
#[derive(Debug, Default)]
pub struct RateLimiter {
    state: Mutex<BucketState>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(BucketState::new()),
        }
    }

    pub fn check(&self, key: &str, policy: Policy) -> Decision {
        let now = Instant::now();
        let mut state = self.state.lock().expect("rate-limit mutex poisoned");
        check(&mut state, key, policy, now)
    }

    /// Remove one token from a key (call on successful authentication so the
    /// failed-attempt counter does not include the successful login).
    #[allow(dead_code)]
    pub fn evict(&self, key: &str) {
        let now = Instant::now();
        let mut state = self.state.lock().expect("rate-limit mutex poisoned");
        if let Some((window_start, count)) = state.get_mut(key) {
            if *count > 0 {
                *count -= 1;
            }
            // Remove the bucket if count is zero (no point keeping an empty bucket)
            // or if the window is so old it's effectively expired (≥1 hour).
            if *count == 0
                || now
                    .duration_since(*window_start)
                    .as_secs()
                    >= 3600
            {
                state.remove(key);
            }
        }
    }

    /// For tests: construct from a pre-populated state.
    #[allow(dead_code)]
    pub fn from_state(state: BucketState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }

    /// For tests: dump the state.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> BucketState {
        self.state
            .lock()
            .expect("rate-limit mutex poisoned")
            .clone()
    }

    /// For tests: time-travel by calling check at a fixed Instant.
    #[allow(dead_code)]
    pub fn check_at(&self, key: &str, policy: Policy, now: Instant) -> Decision {
        let mut state = self.state.lock().expect("rate-limit mutex poisoned");
        check(&mut state, key, policy, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t(secs: u64) -> Instant {
        Instant::now() + Duration::from_secs(secs)
    }

    #[test]
    fn allows_under_limit() {
        let mut s = BucketState::new();
        let policy = Policy::new(3, Duration::from_secs(60));
        assert_eq!(check(&mut s, "1.2.3.4", policy, t(0)), Decision::Allow);
        assert_eq!(check(&mut s, "1.2.3.4", policy, t(1)), Decision::Allow);
        assert_eq!(check(&mut s, "1.2.3.4", policy, t(2)), Decision::Allow);
    }

    #[test]
    fn denies_at_limit_with_retry_after() {
        let mut s = BucketState::new();
        let policy = Policy::new(2, Duration::from_secs(60));
        let _ = check(&mut s, "1.2.3.4", policy, t(0));
        let _ = check(&mut s, "1.2.3.4", policy, t(1));
        match check(&mut s, "1.2.3.4", policy, t(2)) {
            Decision::Deny { retry_after_secs } => {
                assert!(
                    retry_after_secs > 0 && retry_after_secs <= 60,
                    "retry_after_secs={}",
                    retry_after_secs
                );
            }
            other => panic!("expected Deny, got {:?}", other),
        }
    }

    #[test]
    fn denies_subsequent_requests_until_window_resets() {
        let mut s = BucketState::new();
        let policy = Policy::new(2, Duration::from_secs(60));
        let _ = check(&mut s, "1.2.3.4", policy, t(0));
        let _ = check(&mut s, "1.2.3.4", policy, t(1));
        assert!(matches!(
            check(&mut s, "1.2.3.4", policy, t(2)),
            Decision::Deny { .. }
        ));
        assert!(matches!(
            check(&mut s, "1.2.3.4", policy, t(30)),
            Decision::Deny { .. }
        ));
        // After 60s, window resets.
        assert_eq!(check(&mut s, "1.2.3.4", policy, t(61)), Decision::Allow);
    }

    #[test]
    fn separate_keys_have_independent_buckets() {
        let mut s = BucketState::new();
        let policy = Policy::new(1, Duration::from_secs(60));
        assert_eq!(check(&mut s, "1.2.3.4", policy, t(0)), Decision::Allow);
        assert!(matches!(
            check(&mut s, "1.2.3.4", policy, t(1)),
            Decision::Deny { .. }
        ));
        // Different IP is unaffected.
        assert_eq!(check(&mut s, "5.6.7.8", policy, t(1)), Decision::Allow);
    }

    #[test]
    fn login_policy_matches_documented_budget() {
        // 5 per 5 minutes is the default; smoke-check the constants.
        assert_eq!(POLICY_AUTH_LOGIN.max_requests, 5);
        assert_eq!(POLICY_AUTH_LOGIN.window, Duration::from_secs(300));
        assert_eq!(POLICY_FILES.max_requests, 60);
        assert_eq!(POLICY_FILES.window, Duration::from_secs(60));
        assert_eq!(POLICY_REGISTER.max_requests, 120);
        assert_eq!(POLICY_REGISTER.window, Duration::from_secs(60));
        assert_eq!(POLICY_CLIENT_CONTROL.max_requests, 30);
        assert_eq!(POLICY_CLIENT_CONTROL.window, Duration::from_secs(60));
        assert_eq!(POLICY_SERVER_UPDATE.max_requests, 3);
        assert_eq!(POLICY_SERVER_UPDATE.window, Duration::from_secs(300));
    }

    #[test]
    fn retry_after_decreases_as_window_progresses() {
        let mut s = BucketState::new();
        let policy = Policy::new(1, Duration::from_secs(60));
        let _ = check(&mut s, "1.2.3.4", policy, t(0));
        let r1 = check(&mut s, "1.2.3.4", policy, t(0));
        let r2 = check(&mut s, "1.2.3.4", policy, t(30));
        let r3 = check(&mut s, "1.2.3.4", policy, t(59));
        let n = |d: Decision| {
            if let Decision::Deny { retry_after_secs } = d {
                retry_after_secs
            } else {
                panic!("not deny")
            }
        };
        let n1 = n(r1);
        let n2 = n(r2);
        let n3 = n(r3);
        assert!(
            n1 > n2,
            "retry_after should decrease over time: {} > {}",
            n1,
            n2
        );
        assert!(
            n2 > n3,
            "retry_after should decrease over time: {} > {}",
            n2,
            n3
        );
    }

    #[test]
    fn thread_safe_wraper() {
        let lim = RateLimiter::new();
        let policy = Policy::new(2, Duration::from_secs(60));
        assert_eq!(lim.check("1.2.3.4", policy), Decision::Allow);
        assert_eq!(lim.check("1.2.3.4", policy), Decision::Allow);
        assert!(matches!(
            lim.check("1.2.3.4", policy),
            Decision::Deny { .. }
        ));
    }
}
