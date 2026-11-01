//! The log of the inbox: one JSON line on stderr for each event, as the log of the portal.
//!
//! The macros `error!`, `warn!`, `info!` and `debug!` take the fields in this form:
//! `info!(event = "name", count = 3, error = %e)`. A name can have dots: `alert.id = %id`. A name
//! alone is a shorthand: `epoch` stands for `epoch = epoch`. A last string literal is the
//! field `message`. Every value is written as a JSON string.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::LogLevel;
use crate::util::json_escape;

static LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

/// Sets the lowest level that the inbox logs. The value is `logging.level` of the portal.
pub fn set_level(level: LogLevel) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

/// Tells whether an event of this level is logged.
pub fn enabled(level: LogLevel) -> bool {
    level as u8 <= LEVEL.load(Ordering::Relaxed)
}

/// Writes one event.
pub fn write(level: LogLevel, fields: &[(String, String)]) {
    let name = match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .unwrap_or(0);
    let mut line = format!("{{\"ts\":{ts},\"level\":\"{name}\"");
    for (key, value) in fields {
        line.push_str(&format!(
            ",\"{}\":\"{}\"",
            json_escape(key),
            json_escape(value)
        ));
    }
    line.push('}');
    eprintln!("{line}");
}

/// Collects the fields of one event. The macros call it.
#[macro_export]
#[doc(hidden)]
macro_rules! inbox_fields {
    ($f:ident;) => {};
    ($f:ident; $msg:literal $(,)?) => {
        $f.push(("message".to_string(), $msg.to_string()));
    };
    ($f:ident; $($k:ident).+ = % $v:expr $(, $($rest:tt)*)?) => {
        $f.push(([$(stringify!($k)),+].join("."), format!("{}", $v)));
        $crate::inbox_fields!($f; $($($rest)*)?);
    };
    ($f:ident; $($k:ident).+ = $v:expr $(, $($rest:tt)*)?) => {
        $f.push(([$(stringify!($k)),+].join("."), format!("{}", $v)));
        $crate::inbox_fields!($f; $($($rest)*)?);
    };
    ($f:ident; $k:ident $(, $($rest:tt)*)?) => {
        $f.push((stringify!($k).to_string(), format!("{}", $k)));
        $crate::inbox_fields!($f; $($($rest)*)?);
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! inbox_event {
    ($level:expr, $($rest:tt)*) => {{
        if $crate::inbox::log::enabled($level) {
            #[allow(unused_mut)]
            let mut fields: Vec<(String, String)> = Vec::new();
            $crate::inbox_fields!(fields; $($rest)*);
            $crate::inbox::log::write($level, &fields);
        }
    }};
}

macro_rules! inbox_error {
    ($($rest:tt)*) => { $crate::inbox_event!($crate::config::LogLevel::Error, $($rest)*) };
}
macro_rules! inbox_warn {
    ($($rest:tt)*) => { $crate::inbox_event!($crate::config::LogLevel::Warn, $($rest)*) };
}
macro_rules! inbox_info {
    ($($rest:tt)*) => { $crate::inbox_event!($crate::config::LogLevel::Info, $($rest)*) };
}
macro_rules! inbox_debug {
    ($($rest:tt)*) => { $crate::inbox_event!($crate::config::LogLevel::Debug, $($rest)*) };
}

pub(crate) use inbox_debug as debug;
pub(crate) use inbox_error as error;
pub(crate) use inbox_info as info;
pub(crate) use inbox_warn as warn;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_macros_collect_the_fields_in_the_order_of_the_call() {
        let epoch = 4u64;
        let id = "abc";
        let mut fields: Vec<(String, String)> = Vec::new();
        crate::inbox_fields!(fields; event = "x", epoch, alert.id = %id, count = 3, "text");
        let names: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["event", "epoch", "alert.id", "count", "message"]);
        assert_eq!(fields[2].1, "abc");
        assert_eq!(fields[1].1, "4");
    }

    #[test]
    fn the_level_filters_the_events() {
        set_level(LogLevel::Warn);
        assert!(enabled(LogLevel::Error));
        assert!(enabled(LogLevel::Warn));
        assert!(!enabled(LogLevel::Info));
        set_level(LogLevel::Info);
    }
}
