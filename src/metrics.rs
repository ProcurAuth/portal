use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DURATION_BUCKETS_US: [u64; 11] = [
    1_000, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_500_000,
    5_000_000,
];

#[derive(Debug)]
struct DurationHistogram {
    buckets: Mutex<[u64; DURATION_BUCKETS_US.len()]>,
    count: AtomicU64,
    sum_micros: AtomicU64,
}

impl DurationHistogram {
    fn new() -> Self {
        Self {
            buckets: Mutex::new([0; DURATION_BUCKETS_US.len()]),
            count: AtomicU64::new(0),
            sum_micros: AtomicU64::new(0),
        }
    }

    fn observe(&self, duration: Duration) {
        let micros = duration.as_micros().min(u64::MAX as u128) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
        let mut buckets = self
            .buckets
            .lock()
            .expect("metrics histogram mutex poisoned");
        for (index, upper) in DURATION_BUCKETS_US.iter().enumerate() {
            if micros <= *upper {
                buckets[index] = buckets[index].saturating_add(1);
            }
        }
    }
}

#[derive(Debug)]
pub struct Metrics {
    started_at: Instant,
    http_requests: Mutex<HashMap<(String, String, u16), u64>>,
    http_duration: DurationHistogram,

    login_success: AtomicU64,
    login_failure: AtomicU64,
    login_mfa_failure: AtomicU64,
    login_rate_limited_ip: AtomicU64,
    login_rate_limited_global: AtomicU64,
    login_invalid_request: AtomicU64,
    login_csrf_failure: AtomicU64,
    login_form_check_failure: AtomicU64,
    login_origin_failure: AtomicU64,
    login_internal_error: AtomicU64,
    logout_origin_failure: AtomicU64,
    users_origin_failure: AtomicU64,
    /// The counts by `(action, result)` of the account administration routes.
    user_admin: Mutex<HashMap<(&'static str, &'static str), u64>>,

    verify_allowed: AtomicU64,
    verify_denied: AtomicU64,
    verify_excluded: AtomicU64,
    verify_redirected: AtomicU64,
    verify_bad_request: AtomicU64,
    verify_internal_error: AtomicU64,

    session_valid: AtomicU64,
    session_missing: AtomicU64,
    session_invalid: AtomicU64,
    sessions_issued: AtomicU64,
    session_capacity_rejections: AtomicU64,
    logouts: AtomicU64,

    password_checks_total: AtomicU64,
    password_checks_in_flight: AtomicUsize,
    rate_limit_blocks_ip: AtomicU64,
    rate_limit_blocks_global: AtomicU64,

    connections_active: AtomicUsize,
    connections_accepted_total: AtomicU64,
    connections_rejected_total: AtomicU64,
    login_body_rejected_total: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            http_requests: Mutex::new(HashMap::new()),
            http_duration: DurationHistogram::new(),
            login_success: AtomicU64::new(0),
            login_failure: AtomicU64::new(0),
            login_mfa_failure: AtomicU64::new(0),
            login_rate_limited_ip: AtomicU64::new(0),
            login_rate_limited_global: AtomicU64::new(0),
            login_invalid_request: AtomicU64::new(0),
            login_csrf_failure: AtomicU64::new(0),
            login_form_check_failure: AtomicU64::new(0),
            login_origin_failure: AtomicU64::new(0),
            login_internal_error: AtomicU64::new(0),
            logout_origin_failure: AtomicU64::new(0),
            users_origin_failure: AtomicU64::new(0),
            user_admin: Mutex::new(HashMap::new()),
            verify_allowed: AtomicU64::new(0),
            verify_denied: AtomicU64::new(0),
            verify_excluded: AtomicU64::new(0),
            verify_redirected: AtomicU64::new(0),
            verify_bad_request: AtomicU64::new(0),
            verify_internal_error: AtomicU64::new(0),
            session_valid: AtomicU64::new(0),
            session_missing: AtomicU64::new(0),
            session_invalid: AtomicU64::new(0),
            sessions_issued: AtomicU64::new(0),
            session_capacity_rejections: AtomicU64::new(0),
            logouts: AtomicU64::new(0),
            password_checks_total: AtomicU64::new(0),
            password_checks_in_flight: AtomicUsize::new(0),
            rate_limit_blocks_ip: AtomicU64::new(0),
            rate_limit_blocks_global: AtomicU64::new(0),
            connections_active: AtomicUsize::new(0),
            connections_accepted_total: AtomicU64::new(0),
            connections_rejected_total: AtomicU64::new(0),
            login_body_rejected_total: AtomicU64::new(0),
        }
    }

    pub fn observe_http_request(&self, method: &str, route: &str, status: u16, duration: Duration) {
        let mut requests = self
            .http_requests
            .lock()
            .expect("metrics request mutex poisoned");
        let key = (method.to_string(), route.to_string(), status);
        let counter = requests.entry(key).or_insert(0);
        *counter = counter.saturating_add(1);
        drop(requests);
        self.http_duration.observe(duration);
    }

    pub fn verify_denied(&self) {
        self.verify_denied.fetch_add(1, Ordering::Relaxed);
    }

    pub fn users_origin_failure(&self) {
        self.users_origin_failure.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one call of an account administration route.
    /// The `action` is add, update, delete, impersonate, impersonate_end or end_sessions.
    /// The `result` is ok, rejected (bad input) or forbidden (not the superadmin).
    pub fn user_admin(&self, action: &'static str, result: &'static str) {
        let mut counters = self
            .user_admin
            .lock()
            .expect("metrics admin mutex poisoned");
        let counter = counters.entry((action, result)).or_insert(0);
        *counter = counter.saturating_add(1);
    }

    pub fn login_success(&self) {
        self.login_success.fetch_add(1, Ordering::Relaxed);
    }

    pub fn login_failure(&self) {
        self.login_failure.fetch_add(1, Ordering::Relaxed);
    }

    /// A wrong second-factor code, or a code that a limiter refused, for an account whose password was right.
    pub fn login_mfa_failure(&self) {
        self.login_mfa_failure.fetch_add(1, Ordering::Relaxed);
    }

    /// The sign-ins that the portal refused since the start: a wrong password, an unknown login,
    /// a wrong second-factor code and a sign-in that a limiter blocked. The alerts of the portal read it.
    pub fn refused_sign_ins(&self) -> u64 {
        [
            &self.login_failure,
            &self.login_mfa_failure,
            &self.login_rate_limited_ip,
            &self.login_rate_limited_global,
        ]
        .iter()
        .fold(0u64, |sum, counter| {
            sum.saturating_add(counter.load(Ordering::Relaxed))
        })
    }

    pub fn login_rate_limited_ip(&self) {
        self.login_rate_limited_ip.fetch_add(1, Ordering::Relaxed);
        self.rate_limit_blocks_ip.fetch_add(1, Ordering::Relaxed);
    }

    pub fn login_rate_limited_global(&self) {
        self.login_rate_limited_global
            .fetch_add(1, Ordering::Relaxed);
        self.rate_limit_blocks_global
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn login_invalid_request(&self) {
        self.login_invalid_request.fetch_add(1, Ordering::Relaxed);
    }

    pub fn login_csrf_failure(&self) {
        self.login_csrf_failure.fetch_add(1, Ordering::Relaxed);
    }

    /// A sign-in that failed the anti-bot check of the form. The sign-in also counts as a
    /// `failure` of `portal_login_attempts_total`: the answer is the one of a wrong password.
    pub fn login_form_check_failure(&self) {
        self.login_form_check_failure
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn login_origin_failure(&self) {
        self.login_origin_failure.fetch_add(1, Ordering::Relaxed);
    }

    pub fn login_internal_error(&self) {
        self.login_internal_error.fetch_add(1, Ordering::Relaxed);
    }

    pub fn logout_origin_failure(&self) {
        self.logout_origin_failure.fetch_add(1, Ordering::Relaxed);
    }

    pub fn verify_allowed(&self) {
        self.verify_allowed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn verify_excluded(&self) {
        self.verify_excluded.fetch_add(1, Ordering::Relaxed);
    }

    pub fn verify_redirected(&self) {
        self.verify_redirected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn verify_bad_request(&self) {
        self.verify_bad_request.fetch_add(1, Ordering::Relaxed);
    }

    pub fn verify_internal_error(&self) {
        self.verify_internal_error.fetch_add(1, Ordering::Relaxed);
    }

    pub fn session_valid(&self) {
        self.session_valid.fetch_add(1, Ordering::Relaxed);
    }

    pub fn session_missing(&self) {
        self.session_missing.fetch_add(1, Ordering::Relaxed);
    }

    pub fn session_invalid(&self) {
        self.session_invalid.fetch_add(1, Ordering::Relaxed);
    }

    pub fn session_issued(&self) {
        self.sessions_issued.fetch_add(1, Ordering::Relaxed);
    }

    pub fn session_capacity_rejected(&self) {
        self.session_capacity_rejections
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn logout(&self) {
        self.logouts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_accepted(&self) {
        self.connections_accepted_total
            .fetch_add(1, Ordering::Relaxed);
        self.connections_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_closed(&self) {
        self.connections_active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn connection_rejected(&self) {
        self.connections_rejected_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn login_body_rejected(&self) {
        self.login_body_rejected_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn password_check_started(&self) -> PasswordCheckMetricGuard<'_> {
        self.password_checks_total.fetch_add(1, Ordering::Relaxed);
        self.password_checks_in_flight
            .fetch_add(1, Ordering::Relaxed);
        PasswordCheckMetricGuard { metrics: self }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        version: &str,
        ip_rate_limit_entries: usize,
        global_rate_limit_entries: usize,
        ip_limit: usize,
        global_limit: usize,
        workers: usize,
        max_concurrent_password_checks: usize,
        active_sessions: usize,
        session_capacity: usize,
    ) -> String {
        let mut out = String::with_capacity(8192);
        out.push_str("# HELP portal_build_info Build information.\n");
        out.push_str("# TYPE portal_build_info gauge\n");
        out.push_str(&format!(
            "portal_build_info{{version=\"{}\"}} 1\n",
            prometheus_escape(version)
        ));

        out.push_str("# HELP portal_uptime_seconds Process uptime in seconds.\n");
        out.push_str("# TYPE portal_uptime_seconds gauge\n");
        out.push_str(&format!(
            "portal_uptime_seconds {:.3}\n",
            self.started_at.elapsed().as_secs_f64()
        ));

        out.push_str("# HELP portal_http_requests_total HTTP requests handled by route, method and status.\n");
        out.push_str("# TYPE portal_http_requests_total counter\n");
        let mut http_rows: Vec<_> = self
            .http_requests
            .lock()
            .expect("metrics request mutex poisoned")
            .iter()
            .map(|((method, route, status), count)| {
                (method.clone(), route.clone(), *status, *count)
            })
            .collect();
        http_rows.sort();
        for (method, route, status, count) in http_rows {
            out.push_str(&format!(
                "portal_http_requests_total{{method=\"{}\",route=\"{}\",status=\"{}\"}} {}\n",
                prometheus_escape(&method),
                prometheus_escape(&route),
                status,
                count
            ));
        }

        out.push_str(
            "# HELP portal_http_request_duration_seconds HTTP request duration histogram.\n",
        );
        out.push_str("# TYPE portal_http_request_duration_seconds histogram\n");
        let bucket_counts = *self
            .http_duration
            .buckets
            .lock()
            .expect("metrics histogram mutex poisoned");
        for (upper_us, count) in DURATION_BUCKETS_US.iter().zip(bucket_counts.iter()) {
            out.push_str(&format!(
                "portal_http_request_duration_seconds_bucket{{le=\"{}\"}} {}\n",
                format_seconds(*upper_us),
                count
            ));
        }
        let duration_count = self.http_duration.count.load(Ordering::Relaxed);
        let duration_sum_us = self.http_duration.sum_micros.load(Ordering::Relaxed);
        out.push_str(&format!(
            "portal_http_request_duration_seconds_bucket{{le=\"+Inf\"}} {duration_count}\n"
        ));
        out.push_str(&format!(
            "portal_http_request_duration_seconds_sum {:.6}\n",
            duration_sum_us as f64 / 1_000_000.0
        ));
        out.push_str(&format!(
            "portal_http_request_duration_seconds_count {duration_count}\n"
        ));

        out.push_str("# HELP portal_login_attempts_total Login POST results.\n");
        out.push_str("# TYPE portal_login_attempts_total counter\n");
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "success",
            &self.login_success,
        );
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "failure",
            &self.login_failure,
        );
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "mfa_failure",
            &self.login_mfa_failure,
        );
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "rate_limited_ip",
            &self.login_rate_limited_ip,
        );
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "rate_limited_global",
            &self.login_rate_limited_global,
        );
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "invalid_request",
            &self.login_invalid_request,
        );
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "csrf_failure",
            &self.login_csrf_failure,
        );
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "origin_failure",
            &self.login_origin_failure,
        );
        write_counter(
            &mut out,
            "portal_login_attempts_total",
            "result",
            "internal_error",
            &self.login_internal_error,
        );

        out.push_str("# HELP portal_login_form_check_failures_total Sign-ins that failed the anti-bot check of the form. Each one also counts as a failure in portal_login_attempts_total.\n");
        out.push_str("# TYPE portal_login_form_check_failures_total counter\n");
        out.push_str(&format!(
            "portal_login_form_check_failures_total {}\n",
            self.login_form_check_failure.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP portal_origin_rejections_total POST requests rejected by same-origin validation.\n");
        out.push_str("# TYPE portal_origin_rejections_total counter\n");
        write_counter(
            &mut out,
            "portal_origin_rejections_total",
            "route",
            "login",
            &self.login_origin_failure,
        );
        write_counter(
            &mut out,
            "portal_origin_rejections_total",
            "route",
            "logout",
            &self.logout_origin_failure,
        );
        write_counter(
            &mut out,
            "portal_origin_rejections_total",
            "route",
            "users",
            &self.users_origin_failure,
        );

        out.push_str(
            "# HELP portal_forward_auth_requests_total ForwardAuth verification results.\n",
        );
        out.push_str("# TYPE portal_forward_auth_requests_total counter\n");
        write_counter(
            &mut out,
            "portal_forward_auth_requests_total",
            "result",
            "allowed",
            &self.verify_allowed,
        );
        write_counter(
            &mut out,
            "portal_forward_auth_requests_total",
            "result",
            "denied",
            &self.verify_denied,
        );
        write_counter(
            &mut out,
            "portal_forward_auth_requests_total",
            "result",
            "excluded",
            &self.verify_excluded,
        );
        write_counter(
            &mut out,
            "portal_forward_auth_requests_total",
            "result",
            "redirected",
            &self.verify_redirected,
        );
        write_counter(
            &mut out,
            "portal_forward_auth_requests_total",
            "result",
            "bad_request",
            &self.verify_bad_request,
        );
        write_counter(
            &mut out,
            "portal_forward_auth_requests_total",
            "result",
            "internal_error",
            &self.verify_internal_error,
        );

        out.push_str("# HELP portal_session_checks_total Session cookie validation results.\n");
        out.push_str("# TYPE portal_session_checks_total counter\n");
        write_counter(
            &mut out,
            "portal_session_checks_total",
            "result",
            "valid",
            &self.session_valid,
        );
        write_counter(
            &mut out,
            "portal_session_checks_total",
            "result",
            "missing",
            &self.session_missing,
        );
        write_counter(
            &mut out,
            "portal_session_checks_total",
            "result",
            "invalid",
            &self.session_invalid,
        );

        out.push_str("# HELP portal_sessions_issued_total Authentication sessions issued.\n");
        out.push_str("# TYPE portal_sessions_issued_total counter\n");
        out.push_str(&format!(
            "portal_sessions_issued_total {}\n",
            self.sessions_issued.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP portal_sessions_active Current active opaque sessions.\n");
        out.push_str("# TYPE portal_sessions_active gauge\n");
        out.push_str(&format!("portal_sessions_active {active_sessions}\n"));

        out.push_str("# HELP portal_session_capacity Maximum active opaque sessions.\n");
        out.push_str("# TYPE portal_session_capacity gauge\n");
        out.push_str(&format!("portal_session_capacity {session_capacity}\n"));

        out.push_str("# HELP portal_session_capacity_rejections_total Sessions refused because the in-memory session store was full.\n");
        out.push_str("# TYPE portal_session_capacity_rejections_total counter\n");
        out.push_str(&format!(
            "portal_session_capacity_rejections_total {}\n",
            self.session_capacity_rejections.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP portal_logouts_total Successful logout requests.\n");
        out.push_str("# TYPE portal_logouts_total counter\n");
        out.push_str(&format!(
            "portal_logouts_total {}\n",
            self.logouts.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP portal_password_checks_total Argon2 password checks executed.\n");
        out.push_str("# TYPE portal_password_checks_total counter\n");
        out.push_str(&format!(
            "portal_password_checks_total {}\n",
            self.password_checks_total.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP portal_password_checks_in_flight Current Argon2 password checks in flight.\n",
        );
        out.push_str("# TYPE portal_password_checks_in_flight gauge\n");
        out.push_str(&format!(
            "portal_password_checks_in_flight {}\n",
            self.password_checks_in_flight.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP portal_rate_limit_blocks_total Requests blocked by login rate limiting.\n",
        );
        out.push_str("# TYPE portal_rate_limit_blocks_total counter\n");
        write_counter(
            &mut out,
            "portal_rate_limit_blocks_total",
            "scope",
            "ip",
            &self.rate_limit_blocks_ip,
        );
        write_counter(
            &mut out,
            "portal_rate_limit_blocks_total",
            "scope",
            "global",
            &self.rate_limit_blocks_global,
        );

        out.push_str("# HELP portal_rate_limit_entries Current in-memory rate-limit entries.\n");
        out.push_str("# TYPE portal_rate_limit_entries gauge\n");
        out.push_str(&format!(
            "portal_rate_limit_entries{{scope=\"ip\"}} {ip_rate_limit_entries}\n"
        ));
        out.push_str(&format!(
            "portal_rate_limit_entries{{scope=\"global\"}} {global_rate_limit_entries}\n"
        ));

        out.push_str("# HELP portal_rate_limit_threshold Configured failed-login threshold.\n");
        out.push_str("# TYPE portal_rate_limit_threshold gauge\n");
        out.push_str(&format!(
            "portal_rate_limit_threshold{{scope=\"ip\"}} {ip_limit}\n"
        ));
        out.push_str(&format!(
            "portal_rate_limit_threshold{{scope=\"global\"}} {global_limit}\n"
        ));

        out.push_str("# HELP portal_workers Configured HTTP worker threads.\n");
        out.push_str("# TYPE portal_workers gauge\n");
        out.push_str(&format!("portal_workers {workers}\n"));

        out.push_str("# HELP portal_connections_active Current accepted HTTP connections.\n");
        out.push_str("# TYPE portal_connections_active gauge\n");
        out.push_str(&format!(
            "portal_connections_active {}\n",
            self.connections_active.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP portal_connections_total Accepted HTTP connections.\n");
        out.push_str("# TYPE portal_connections_total counter\n");
        out.push_str(&format!(
            "portal_connections_total {}\n",
            self.connections_accepted_total.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP portal_connections_rejected_total Connections rejected because max_connections was reached.\n");
        out.push_str("# TYPE portal_connections_rejected_total counter\n");
        out.push_str(&format!(
            "portal_connections_rejected_total {}\n",
            self.connections_rejected_total.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP portal_login_body_rejected_total Sign-in requests refused because server.max_login_body_reads bodies were already being read.\n");
        out.push_str("# TYPE portal_login_body_rejected_total counter\n");
        out.push_str(&format!(
            "portal_login_body_rejected_total {}\n",
            self.login_body_rejected_total.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP portal_password_check_concurrency_limit Configured maximum concurrent Argon2 checks.\n");
        out.push_str("# TYPE portal_password_check_concurrency_limit gauge\n");
        out.push_str(&format!(
            "portal_password_check_concurrency_limit {max_concurrent_password_checks}\n"
        ));

        out
    }
}

impl Metrics {
    /// Series about the accounts and the history.
    /// No login, address or user agent ever appears in a label.
    /// The detail is kept in the database only.
    pub fn render_accounts(
        &self,
        users: usize,
        tracking_dropped: u64,
        tracking_errors: u64,
    ) -> String {
        let mut out = String::new();
        out.push_str(
            "# HELP portal_users Accounts in the database (the superadmin is not counted).\n",
        );
        out.push_str("# TYPE portal_users gauge\n");
        out.push_str(&format!("portal_users {users}\n"));
        out.push_str("# HELP portal_user_admin_total Account administration requests by action and result.\n");
        out.push_str("# TYPE portal_user_admin_total counter\n");
        let mut rows: Vec<_> = self
            .user_admin
            .lock()
            .expect("metrics admin mutex poisoned")
            .iter()
            .map(|(key, value)| (*key, *value))
            .collect();
        rows.sort();
        for ((action, result), value) in rows {
            out.push_str(&format!(
                "portal_user_admin_total{{action=\"{action}\",result=\"{result}\"}} {value}\n"
            ));
        }
        out.push_str("# HELP portal_tracking_dropped_total History events lost because the writer queue was full.\n");
        out.push_str("# TYPE portal_tracking_dropped_total counter\n");
        out.push_str(&format!(
            "portal_tracking_dropped_total {tracking_dropped}\n"
        ));
        out.push_str("# HELP portal_tracking_flush_errors_total History flushes that failed.\n");
        out.push_str("# TYPE portal_tracking_flush_errors_total counter\n");
        out.push_str(&format!(
            "portal_tracking_flush_errors_total {tracking_errors}\n"
        ));
        out
    }
}

impl Metrics {
    /// Series that exist only with a shared database.
    pub fn render_shared(
        &self,
        session_errors: u64,
        directory_errors: u64,
        directory_age: Duration,
    ) -> String {
        let mut out = String::new();
        out.push_str("# HELP portal_shared_store_errors_total Calls to the shared database that failed, by what they were for.\n");
        out.push_str("# TYPE portal_shared_store_errors_total counter\n");
        out.push_str(&format!(
            "portal_shared_store_errors_total{{use=\"sessions\"}} {session_errors}\n"
        ));
        out.push_str(&format!(
            "portal_shared_store_errors_total{{use=\"directory\"}} {directory_errors}\n"
        ));
        out.push_str("# HELP portal_directory_age_seconds Seconds since the database last answered a read of the accounts.\n");
        out.push_str("# TYPE portal_directory_age_seconds gauge\n");
        out.push_str(&format!(
            "portal_directory_age_seconds {:.3}\n",
            directory_age.as_secs_f64()
        ));
        out
    }
}

pub struct PasswordCheckMetricGuard<'a> {
    metrics: &'a Metrics,
}

impl Drop for PasswordCheckMetricGuard<'_> {
    fn drop(&mut self) {
        self.metrics
            .password_checks_in_flight
            .fetch_sub(1, Ordering::Relaxed);
    }
}

fn write_counter(out: &mut String, metric: &str, label: &str, value: &str, counter: &AtomicU64) {
    out.push_str(&format!(
        "{}{{{}=\"{}\"}} {}\n",
        metric,
        label,
        value,
        counter.load(Ordering::Relaxed)
    ));
}

fn format_seconds(micros: u64) -> String {
    if micros.is_multiple_of(1_000_000) {
        format!("{}", micros / 1_000_000)
    } else {
        let value = micros as f64 / 1_000_000.0;
        let mut s = format!("{value:.6}");
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
        s
    }
}

fn prometheus_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_expected_series() {
        let metrics = Metrics::new();
        metrics.login_success();
        metrics.login_failure();
        metrics.observe_http_request("GET", "login", 200, Duration::from_millis(2));
        let rendered = metrics.render("1.2.3", 1, 1, 5, 100, 4, 1, 2, 4096);
        assert!(rendered.contains("portal_login_attempts_total{result=\"success\"} 1"));
        assert!(rendered.contains("portal_login_attempts_total{result=\"failure\"} 1"));
        assert!(rendered.contains("portal_http_request_duration_seconds_bucket"));
        assert!(rendered.contains("portal_forward_auth_requests_total{result=\"denied\"} 0"));
        assert!(rendered.contains("portal_origin_rejections_total{route=\"users\"} 0"));
    }

    #[test]
    fn account_series_carry_counts_only() {
        let metrics = Metrics::new();
        metrics.verify_denied();
        metrics.user_admin("add", "ok");
        metrics.user_admin("add", "ok");
        metrics.user_admin("delete", "forbidden");
        let shared = metrics.render_shared(4, 5, Duration::from_millis(12_500));
        assert!(shared.contains("portal_shared_store_errors_total{use=\"sessions\"} 4"));
        assert!(shared.contains("portal_shared_store_errors_total{use=\"directory\"} 5"));
        assert!(shared.contains("portal_directory_age_seconds 12.500"));
        let rendered = metrics.render_accounts(3, 2, 1);
        assert!(rendered.contains("portal_users 3"));
        assert!(rendered.contains("portal_user_admin_total{action=\"add\",result=\"ok\"} 2"));
        assert!(
            rendered.contains("portal_user_admin_total{action=\"delete\",result=\"forbidden\"} 1")
        );
        assert!(rendered.contains("portal_tracking_dropped_total 2"));
        assert!(rendered.contains("portal_tracking_flush_errors_total 1"));
    }
}
