//! The rights of the inbox: what the inbox asks the portal, and what it does with the answers.
//!
//! The inbox does not read the state of the portal. The portal gives it one object at start, an
//! [`Authorizer`]. The inbox uses it in three places:
//! - A producer that sends a key of the portal as its token. The inbox checks the key, then asks for
//!   the decision on the action `alert.send` on the group (see `api.rs`).
//! - A phone that subscribes to a group, and the list of groups in the message `welcome` (see
//!   `device.rs`). The account of the phone needs `group.subscribe` on the group.
//! - A change of the directory of the portal. The task [`run`] then removes the subscriptions that an
//!   account lost.
//!
//! SAFETY RULE. A subscription is the way to a person that an alert can protect. The inbox
//! removes a subscription only when a loaded directory positively says that the account has no
//! right on the group. A missing authorizer, a directory that is not ready, a directory that is
//! old, and an account that the directory does not know never remove a subscription. HCL tokens
//! never depend on the authorizer: they work when the portal is not ready.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use super::db::DeviceView;
use super::log;
use super::state::App;

/// The result of the check of a bearer token as a key of the portal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyCheck {
    /// The text has not the form of a key, or the portal accepts no key.
    NotAKey,
    /// The key does not exist, is expired, or its owner cannot use keys.
    Invalid,
    /// The directory of the portal is not ready. The inbox answers 503.
    NotReady,
    /// A good key. `key_id` is the local part of its subject id.
    Valid { key_id: String },
}

/// The decision of the portal for one action of a key on one group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// The decision is no. `reason` is a short code for the answer 403.
    Refused {
        reason: &'static str,
    },
    /// A limit has no room. `retry_after` is a whole number of seconds, at least 1.
    Limited {
        retry_after: u64,
    },
    /// The directory is not ready. The inbox answers 503.
    NotReady,
    /// The count of the use failed. The inbox answers 503.
    Unavailable,
}

/// The groups for which an account has `group.subscribe`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subscribable {
    /// The portal cannot tell: the directory is not ready or is old, or the account is unknown.
    /// The inbox changes nothing.
    Unknown,
    /// The directory is loaded and knows the account. These are the groups that it allows.
    Groups(BTreeSet<String>),
}

/// What the portal gives to the inbox. The calls read memory and, for a use that counts, one row
/// of the database. They block for a short time: the inbox calls them from async code.
pub trait Authorizer: Send + Sync {
    /// Checks the text of a key. It reads memory only.
    fn check_key(&self, token: &str) -> KeyCheck;

    /// Decides whether the key may do `action` on the group `group`. When `count_use` is true, a
    /// decision `Allowed` has taken one unit from each limit of the key and of its owner.
    fn decide(&self, key_id: &str, action: &str, group: &str, count_use: bool) -> Decision;

    /// The groups that the account may subscribe to, among the groups of the inbox.
    fn subscribable(&self, login: &str) -> Subscribable;

    /// A number that changes when the directory changes. `None`: the directory is not ready.
    fn directory_stamp(&self) -> Option<u64>;
}

/// The groups that a phone of `login` may subscribe to. `None`: the inbox cannot tell, so it
/// shows and changes nothing.
pub fn subscribable_groups(app: &App, login: &str) -> Option<BTreeSet<String>> {
    let authorizer = app.authorizer()?;
    match authorizer.subscribable(login) {
        Subscribable::Groups(groups) => Some(groups),
        Subscribable::Unknown => None,
    }
}

/// A subscription that an account lost.
#[derive(Debug, PartialEq, Eq)]
pub struct Lost {
    pub device_id: String,
    pub login: String,
    pub group: String,
}

/// The subscriptions to remove. A device that is revoked, a device without subscription and an
/// account that the authorizer cannot judge give nothing. A subscription goes only when the
/// authorizer lists the groups of the account and the group is not among them.
pub fn plan(devices: &[DeviceView], authorizer: &dyn Authorizer) -> Vec<Lost> {
    let mut verdicts: HashMap<&str, Subscribable> = HashMap::new();
    let mut out = Vec::new();
    for device in devices {
        if device.revoked_at.is_some() || device.subscriptions.is_empty() {
            continue;
        }
        let verdict = verdicts
            .entry(device.user_name.as_str())
            .or_insert_with(|| authorizer.subscribable(&device.user_name));
        let Subscribable::Groups(allowed) = verdict else {
            continue;
        };
        for group in device
            .subscriptions
            .iter()
            .filter(|g| !allowed.contains(*g))
        {
            out.push(Lost {
                device_id: device.id.clone(),
                login: device.user_name.clone(),
                group: group.clone(),
            });
        }
    }
    out
}

/// Removes the subscriptions that the accounts lost (see [`plan`]). The alerts that the inbox
/// made before stay deliverable. The result is the number of subscriptions that it removed, or
/// `None` when the storage did not answer: the caller tries again.
pub async fn reconcile(app: &App) -> Option<usize> {
    let Some(authorizer) = app.authorizer() else {
        return Some(0);
    };
    let (devices, _) = match app.db.overview(app.delivery_cutoff()).await {
        Ok(read) => read,
        Err(e) => {
            log::warn!(event = "rights_read_failed", error = %e);
            return None;
        }
    };
    let lost = plan(&devices, authorizer.as_ref());
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for item in &lost {
        match app.db.unsubscribe(&item.device_id, &item.group).await {
            Ok(_) => {
                touched.insert(item.device_id.clone());
                log::info!(
                    event = "subscription_removed_by_rights",
                    device.id = %item.device_id,
                    user.name = %item.login,
                    group = %item.group
                );
            }
            Err(e) => {
                log::warn!(event = "rights_unsubscribe_failed", error = %e);
                return None;
            }
        }
    }
    for id in &touched {
        app.hub.subscriptions_changed(id);
    }
    Some(lost.len())
}

/// The time between two looks at the stamp of the directory.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// The task of the rights: when the stamp of the directory changes, it runs [`reconcile`]. It does
/// not run for an alert. A failed round is tried again at the next look.
pub async fn run(app: Arc<App>) {
    let mut seen: Option<u64> = None;
    let mut tick = tokio::time::interval(LOOK_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = app.shutdown.wait() => return,
        }
        let Some(stamp) = app.authorizer().and_then(|a| a.directory_stamp()) else {
            continue;
        };
        if seen == Some(stamp) {
            continue;
        }
        if let Some(removed) = reconcile(&app).await {
            if removed > 0 {
                log::warn!(event = "subscriptions_removed_by_rights", count = removed);
            }
            seen = Some(stamp);
        }
    }
}

#[cfg(test)]
pub mod testing {
    //! An authorizer for the tests of the inbox.

    use super::*;
    use std::sync::Mutex;

    /// A fixed answer for each login, with a stamp that a test can change.
    pub struct Fixed {
        pub answers: Mutex<HashMap<String, Subscribable>>,
        pub stamp: Mutex<Option<u64>>,
    }

    impl Fixed {
        pub fn new() -> Arc<Fixed> {
            Arc::new(Fixed {
                answers: Mutex::new(HashMap::new()),
                stamp: Mutex::new(Some(1)),
            })
        }

        pub fn set(&self, login: &str, answer: Subscribable) {
            self.answers
                .lock()
                .unwrap()
                .insert(login.to_string(), answer);
            let mut stamp = self.stamp.lock().unwrap();
            *stamp = stamp.map(|s| s + 1);
        }
    }

    impl Authorizer for Fixed {
        fn check_key(&self, _token: &str) -> KeyCheck {
            KeyCheck::NotAKey
        }
        fn decide(&self, _key: &str, _action: &str, _group: &str, _count: bool) -> Decision {
            Decision::NotReady
        }
        fn subscribable(&self, login: &str) -> Subscribable {
            self.answers
                .lock()
                .unwrap()
                .get(login)
                .cloned()
                .unwrap_or(Subscribable::Unknown)
        }
        fn directory_stamp(&self) -> Option<u64> {
            *self.stamp.lock().unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Fixed;
    use super::*;

    fn device(id: &str, login: &str, groups: &[&str], revoked: bool) -> DeviceView {
        DeviceView {
            id: id.into(),
            name: id.into(),
            user_name: login.into(),
            enrolled_at: 0,
            enrolled_ip: String::new(),
            app_version: None,
            last_connected_at: None,
            last_seen_at: None,
            last_ip: None,
            revoked_at: revoked.then_some(1),
            subscriptions: groups.iter().map(|g| g.to_string()).collect(),
            pending: 0,
        }
    }

    fn groups(ids: &[&str]) -> Subscribable {
        Subscribable::Groups(ids.iter().map(|g| g.to_string()).collect())
    }

    #[test]
    fn a_subscription_goes_only_when_a_loaded_directory_says_no() {
        let authorizer = Fixed::new();
        authorizer.set("alice", groups(&["infra"]));
        authorizer.set("bob", groups(&[]));
        let devices = vec![
            device("a1", "alice", &["infra", "oncall"], false),
            device("b1", "bob", &["infra"], false),
        ];
        let lost = plan(&devices, authorizer.as_ref());
        assert_eq!(
            lost,
            [
                Lost {
                    device_id: "a1".into(),
                    login: "alice".into(),
                    group: "oncall".into()
                },
                Lost {
                    device_id: "b1".into(),
                    login: "bob".into(),
                    group: "infra".into()
                },
            ]
        );
    }

    #[test]
    fn a_missing_stale_or_unknown_directory_removes_nothing() {
        let authorizer = Fixed::new();
        // The authorizer cannot judge the account: it is unknown, or the directory is not ready.
        authorizer.set("alice", Subscribable::Unknown);
        let devices = vec![
            device("a1", "alice", &["infra", "oncall"], false),
            device("c1", "carol", &["infra"], false),
        ];
        assert!(plan(&devices, authorizer.as_ref()).is_empty());
        // An account that has no answer at all is unknown too.
        let empty = Fixed::new();
        assert!(plan(&devices, empty.as_ref()).is_empty());
    }

    #[test]
    fn a_revoked_device_and_a_device_without_subscription_give_nothing() {
        let authorizer = Fixed::new();
        authorizer.set("alice", groups(&[]));
        let devices = vec![
            device("a1", "alice", &["infra"], true),
            device("a2", "alice", &[], false),
        ];
        assert!(plan(&devices, authorizer.as_ref()).is_empty());
    }
}
