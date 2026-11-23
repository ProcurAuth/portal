//! The notifications of a signed-in person: the alerts that the portal shows in the browser
//! while a page is open.
//!
//! The browser asks a small endpoint every 10 to 15 seconds. The portal keeps no state in memory
//! for it: each answer comes from the shared store, so any instance gives the same answer.
//!
//! A person is notified of the alerts that the person receives. These are the alerts for the
//! groups where the account holds `inbox.receive`, and the alerts for the account itself. The
//! page of the portal gives the groups as a [`Scope`] (see `src/http/inbox_page.rs`). An alert
//! about a service also needs the service in the scope: the rule of the launcher. The store
//! applies the scope in the query, so a row of another account never leaves the database.
//!
//! The store keeps two values for each account: the marker "read up to" and the level of the
//! toasts (see [`Level`]). The marker is a time. It only grows. An alert that is newer than the
//! marker is unread. The Android app has its own state: this marker is the one of the web only.

use super::api::ApiError;
pub use super::rights::Services;
use super::state::App;
use super::target::Target;
pub use super::target::DIRECT_NAME;

/// The most alerts that one answer carries.
pub const ITEMS_MAX: u32 = 10;
/// The count of unread alerts stops here: the page shows "99+".
pub const UNREAD_CAP: u32 = 100;
/// An answer includes the alerts of this long before the cursor. The clocks of two instances can
/// differ, and a transaction can commit after a later one. The browser drops the alerts that it
/// has seen already.
pub const LOOKBACK_MS: i64 = 5_000;

/// What an account wants to see as a toast.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// Each alert gives a toast. This is the default.
    #[default]
    All,
    /// Only the alerts of a critical group and of the other groups give a toast. An alert for the
    /// account itself (level Ring) does not.
    Important,
    /// No toast, no badge and no polling.
    Off,
}

impl Level {
    /// The text of the store and of the forms.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::All => "all",
            Level::Important => "important",
            Level::Off => "off",
        }
    }

    pub fn parse(text: &str) -> Option<Level> {
        match text {
            "all" => Some(Level::All),
            "important" => Some(Level::Important),
            "off" => Some(Level::Off),
            _ => None,
        }
    }
}

/// The level of one alert, as the portal knows it. The phones choose their own action for a
/// group, so the portal knows three levels only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Importance {
    /// An alert for a critical group.
    Critical,
    /// An alert for another group.
    Alert,
    /// An alert for one account. It is never critical.
    Ring,
}

impl Importance {
    pub fn as_str(self) -> &'static str {
        match self {
            Importance::Critical => "critical",
            Importance::Alert => "alert",
            Importance::Ring => "ring",
        }
    }

    /// The level of an alert for `target`. `is_critical` tells the groups that are critical.
    pub fn of(target: &Target, is_critical: impl Fn(&str) -> bool) -> Importance {
        match target {
            Target::Group(group) if is_critical(group) => Importance::Critical,
            Target::Group(_) => Importance::Alert,
            Target::User(_) => Importance::Ring,
        }
    }

    /// Whether a toast of this level shows with the level `level`.
    pub fn toasts_at(self, level: Level) -> bool {
        match level {
            Level::All => true,
            Level::Important => self != Importance::Ring,
            Level::Off => false,
        }
    }
}

/// The alerts that an account receives: the groups where it holds `inbox.receive`, and the
/// alerts that are for it. An alert about a service also needs the service in `services`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// The ids of the groups. `None`: all the groups (the superadmin).
    pub groups: Option<Vec<String>>,
    pub login: String,
    /// The services whose alerts the account may see (see `rights::Services`).
    pub services: Services,
}

impl Scope {
    /// Whether the account receives an alert for `target` about `service`. The store applies the
    /// same rule in its queries. A test keeps the two in agreement.
    #[cfg(test)]
    pub fn receives(&self, target: &Target, service: Option<&str>) -> bool {
        let to_it = match target {
            Target::Group(group) => self
                .groups
                .as_ref()
                .is_none_or(|groups| groups.iter().any(|g| g == group)),
            Target::User(login) => *login == self.login,
        };
        to_it && self.services.sees(service)
    }
}

/// One alert that the account receives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub target: Target,
    pub title: String,
    pub created_at: i64,
}

/// What an account stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    pub read_up_to: i64,
    pub level: Level,
}

/// What a call asks of the store.
#[derive(Clone, Copy, Debug)]
pub struct Ask {
    /// Only the alerts created after this time (milliseconds).
    pub since_ms: i64,
    /// The most alerts to give, newest first.
    pub limit: u32,
    /// With the level `Off`, the store reads nothing but the preferences.
    pub skip_if_off: bool,
}

/// The answer of the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub prefs: Prefs,
    /// The alerts newer than the marker, up to [`UNREAD_CAP`].
    pub unread: u32,
    /// Newest first.
    pub items: Vec<Item>,
}

/// The alerts that the account receives, and its preferences.
pub async fn view(app: &App, scope: &Scope, ask: Ask) -> Result<View, ApiError> {
    app.db
        .notify_view(scope, ask)
        .await
        .map_err(ApiError::storage)
}

/// Moves the marker "read up to" of the account to its newest alert. The result is the marker.
pub async fn mark_read(app: &App, scope: &Scope) -> Result<i64, ApiError> {
    app.db
        .notify_mark_read(scope)
        .await
        .map_err(ApiError::storage)
}

/// Sets the level of the toasts of the account.
pub async fn set_level(app: &App, login: &str, level: Level) -> Result<(), ApiError> {
    app.db
        .notify_set_level(login, level)
        .await
        .map_err(ApiError::storage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_has_a_text_that_parses_back() {
        for level in [Level::All, Level::Important, Level::Off] {
            assert_eq!(Level::parse(level.as_str()), Some(level));
        }
        assert_eq!(Level::parse("ALL"), None);
        assert_eq!(Level::default(), Level::All);
    }

    #[test]
    fn the_level_of_an_alert_follows_its_target() {
        let critical = |g: &str| g == "oncall";
        let group = |g: &str| Target::Group(g.into());
        assert_eq!(
            Importance::of(&group("oncall"), critical),
            Importance::Critical
        );
        assert_eq!(Importance::of(&group("infra"), critical), Importance::Alert);
        assert_eq!(
            Importance::of(&Target::User("alice".into()), critical),
            Importance::Ring
        );
    }

    #[test]
    fn the_preference_decides_which_toasts_show() {
        use Importance::*;
        for (level, shown) in [
            (Level::All, [true, true, true]),
            (Level::Important, [true, true, false]),
            (Level::Off, [false, false, false]),
        ] {
            let got = [Critical, Alert, Ring].map(|i| i.toasts_at(level));
            assert_eq!(got, shown, "{level:?}");
        }
    }

    #[test]
    fn a_member_receives_the_alerts_of_its_groups_and_its_own() {
        let scope = Scope {
            groups: Some(vec!["infra".into()]),
            login: "alice".into(),
            services: Services::Only(["notes".to_string()].into()),
        };
        let infra = Target::Group("infra".into());
        assert!(scope.receives(&infra, None));
        assert!(!scope.receives(&Target::Group("other".into()), None));
        assert!(scope.receives(&Target::User("alice".into()), None));
        assert!(!scope.receives(&Target::User("bob".into()), None));
        // An alert about a service needs the service too.
        assert!(scope.receives(&infra, Some("notes")));
        assert!(!scope.receives(&infra, Some("vault")));
        let all = Scope {
            groups: None,
            login: "root".into(),
            services: Services::All,
        };
        assert!(all.receives(&Target::Group("any".into()), Some("vault")));
        assert!(!all.receives(&Target::User("bob".into()), None));
    }
}
