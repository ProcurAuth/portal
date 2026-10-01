# Inbox configuration.
#
# Every attribute is mandatory: Inbox refuses to start if one is missing,
# misspelled or out of range. Validate with:  inbox check-config --config <file>
# Durations accept ms, s, m, h and d suffixes.

server {
  # Shown in the app and the admin front.
  name = "Inbox"
  # Devices (/v1/enroll, /v1/session), producer API, webhooks, /healthz, /readyz.
  listen = "0.0.0.0:8080"
  # Admin front and admin API. It has NO password: bind it to localhost or a
  # private network, or put an authenticating reverse proxy in front of it.
  admin_listen = "127.0.0.1:8081"
  # Peers whose X-Forwarded-For / X-Real-IP headers are trusted. Empty: none.
  trusted_proxies = ["127.0.0.1/32"]
  shutdown_grace  = "10s"
  # Generated once with `inbox keygen`. Replacing it invalidates every device.
  server_key_file = "/run/secrets/inbox_server_key"
}

storage {
  path = "/var/lib/inbox/inbox.db"
  # Alerts delivered to every target device are purged after this.
  # Alerts still waiting for a device are never purged.
  alert_retention          = "30d"
  connection_log_retention = "90d"
}

devices {
  # Shared device password, from `inbox credential`.
  credential_file       = "/run/secrets/inbox_device_credential"
  heartbeat_interval    = "20s"
  session_timeout       = "60s"
  resend_after          = "15s"
  max_inflight          = 32
  unreachable_after     = "2m"
  offline_warning_after = "3m"
  max_devices           = 1000
}

admin {
  # No password: anyone who reaches server.admin_listen can send alerts and
  # revoke devices. Host headers accepted by the admin listener; any other host
  # is refused (protection against DNS rebinding from a malicious web page).
  allowed_hosts = ["localhost:8081", "127.0.0.1:8081"]
}

security {
  # Device password attempts per client address and window.
  max_auth_failures   = 5
  auth_failure_window = "10m"
}

limits {
  max_title_bytes   = 200
  max_message_bytes = 4000
  max_request_bytes = 65536
}

logging {
  level  = "info"
  format = "json"
}

telemetry {
  enabled         = true
  otlp_endpoint   = "http://otel-collector:4318"
  service_name    = "inbox"
  export_interval = "15s"
}

group "infra" {
  name        = "Infra prod"
  description = "Production outages"
}

group "oncall" {
  name        = "Astreinte"
  description = "Pages for the on-call engineer"
}

# Optional: alert sources other than the admin front.
producer "grafana" {
  # From `inbox token`.
  token_file            = "/run/secrets/inbox_grafana_token"
  groups                = ["infra"]
  webhook_send_resolved = false
}
