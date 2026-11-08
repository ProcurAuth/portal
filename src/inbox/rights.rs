//! The rights of the inbox: what the inbox asks the portal, and what it does with the answers.
//!
//! The inbox does not read the state of the portal. The portal gives it one object at start, an
//! [`Authorizer`]. The inbox uses it in these places:
//! - A sender that gives a key of the kind `api` as its token. The inbox checks the key, then asks for
//!   the decision on the action `alert.send` on the group, or on the action `alert.send_user` on the
//!   service (see `api.rs`).
//! - A phone that subscribes to a group, and the list of groups in the message `welcome` (see
//!   `device.rs`). The account of the phone needs `inbox.receive` on the group.
//! - A change of the directory of the portal. The task [`run`] then removes the subscriptions that an
//!   account lost.
//! - A phone that opens a session or enrolls. Its account must exist (see [`Standing`]).
//! - An alert about a service (an alert of `inbox.watch`). Each reader of the alert must see the
//!   service (see [`Services`]): the phones at the fan-out and at the delivery, the notifications,
//!   the page of the Inbox and the keys.
//!
//! SAFETY RULE. A subscription is the way to a person that an alert can protect. The inbox
//! removes a subscription only when a loaded directory positively says that the account has no
//! right on the group. A missing authorizer, a directory that is not ready, a directory that is
//! old, and an account that the directory does not know never remove a subscription. The keys of
//! the file (the blocks `api_key`) never depend on the authorizer: they work when the portal is not
//! ready.
//!
//! The one exception is an account that ended outside a delete: an account of the file that left
//! the file, or an old login of the superadmin. Only one instance on SQLite can prove it: it holds
//! the file, the superadmin login and every account of the database, and the file changes only at
//! a restart. Its verdict is [`Subscribable::Absent`], and the inbox ends the state of the login
//! (see `enrollment::end_account`). With a shared database, the file of another instance can still
//! hold the login: the phone gets no session, and the inbox writes nothing.
//!
//! RULE OF THE SERVICES. An alert about a service must not reach a person who may not open the
//! service. Thus the rule goes the other way: when the portal cannot tell, the inbox shows no such
//! alert and makes no delivery of it. A delivery that exists waits, and a loaded directory that
//! says no removes it.

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

/// The door of an agent: the endpoint `/mcp` takes the keys of the kind `mcp`, and the REST routes of the items take the
/// keys of the kind `api`. A key never opens the other door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Door {
    Mcp,
    Rest,
}

/// The facts about a key that the agent channel needs. The owner is the login of the account that holds the key: the
/// superadmin has its current login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentKey {
    pub key_id: String,
    pub owner: String,
    pub interaction: inbox_core::proto::Interaction,
}

/// The result of the check of a bearer token for the door of an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentCheck {
    /// The text has not the form of a key of this door, or the portal turns this kind off.
    NotAKey,
    /// The key does not exist, is expired, or its owner cannot use keys.
    Invalid,
    /// The directory of the portal is not ready. The agent channel answers 503.
    NotReady,
    Valid(AgentKey),
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

/// The groups for which an account has `inbox.receive`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subscribable {
    /// The portal cannot tell: the directory is not ready or is old, or a shared directory does
    /// not know the account. The inbox changes nothing.
    Unknown,
    /// One instance holds every account (SQLite), and none has this login: not the superadmin, not
    /// an account of the file, not an account of the database. The inbox ends the state of the
    /// login.
    Absent,
    /// The directory is loaded and knows the account. These are the groups that it allows.
    Groups(BTreeSet<String>),
}

/// Whether an account has a login, for a phone that opens a session or enrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The superadmin, an account of the file or an account of the database has the login.
    Exists,
    /// One instance holds every account (SQLite), and none has the login. The inbox ends the state
    /// of the login.
    Absent,
    /// A read of the shared database that started after the question found no account, and the
    /// file of this instance has none. The file of another instance can still have one, for
    /// example during a change of the file on each instance in turn. The phone gets no session,
    /// and the inbox writes nothing.
    NotHere,
    /// The portal cannot tell: the directory is not ready, is old, or the database did not answer.
    Unknown,
}

/// The services whose alerts a reader may see. An alert of the portal about a service (an alert
/// of `inbox.watch`) names the service. The rule is the rule of the launcher: a reader sees such
/// an alert only when the reader may open the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Services {
    /// Every service: the superadmin.
    All,
    /// The services where the account holds `access`.
    Only(BTreeSet<String>),
}

impl Services {
    /// The services of a person with `grants`: the services that the launcher shows to the person
    /// (`access::may_open`). `None` is the superadmin. This is the one place that turns grants into
    /// services: the portal and the keys of the file use it.
    pub fn of_grants(grants: Option<&[crate::grants::Grant]>) -> Services {
        match grants {
            None => Services::All,
            Some(held) => Services::Only(
                held.iter()
                    .map(|grant| grant.service.as_str())
                    .filter(|service| crate::access::may_open(grants, service))
                    .map(str::to_string)
                    .collect(),
            ),
        }
    }

    /// Whether the reader may see an alert about `service`. An alert about no service has no
    /// such limit. Every reader of an alert uses this rule. The stores apply the same rule in
    /// their queries, and a test keeps the two in agreement.
    pub fn sees(&self, service: Option<&str>) -> bool {
        match (self, service) {
            (_, None) | (Services::All, Some(_)) => true,
            (Services::Only(open), Some(service)) => open.contains(service),
        }
    }
}

/// The services whose alerts the account `login` may see. `None`: the inbox cannot tell, and the
/// reader then sees no alert about a service.
pub fn services_of(app: &App, login: &str) -> Option<Services> {
    app.authorizer()?.services(login)
}

/// The logins that may see the alerts about `service`. Without an authorizer the set is empty:
/// an alert about a service then reaches no phone.
pub fn audience(app: &App, service: &str) -> BTreeSet<String> {
    app.authorizer()
        .map(|authorizer| authorizer.audience(service))
        .unwrap_or_default()
}

/// What the portal gives to the inbox. The calls read memory and, for a use that counts, one row
/// of the database. They block for a short time: the inbox calls them from async code.
pub trait Authorizer: Send + Sync {
    /// Checks the text of a key. It reads memory only.
    fn check_key(&self, token: &str) -> KeyCheck;

    /// Checks the text of a key for the door of an agent, and gives the owner and the interaction of the key. It reads
    /// memory only. The default answer is `NotAKey`.
    fn check_agent_key(&self, _token: &str, _door: Door) -> AgentCheck {
        AgentCheck::NotAKey
    }

    /// Decides whether the key may do `action` on the group `group`. When `count_use` is true, a
    /// decision `Allowed` has taken one unit from each limit of the key and of its owner.
    fn decide(&self, key_id: &str, action: &str, group: &str, count_use: bool) -> Decision;

    /// The same decision for an action on the service `portal` itself, with no group: the action
    /// `alert.send_user` is flat.
    fn decide_service(&self, key_id: &str, action: &str, count_use: bool) -> Decision;

    /// Whether an account with this login exists, the superadmin included. `None`: the portal
    /// cannot tell, because the directory is not ready or is old.
    fn user_exists(&self, login: &str) -> Option<bool>;

    /// The groups that the account may subscribe to, among the groups of the inbox. It reads
    /// memory only.
    fn subscribable(&self, login: &str) -> Subscribable;

    /// Whether an account has this login. With a shared database, a login that the directory does
    /// not know costs one read of the database.
    fn standing(&self, login: &str) -> Standing;

    /// A number that changes when the directory changes. `None`: the directory is not ready.
    fn directory_stamp(&self) -> Option<u64>;

    /// The services whose alerts the account may see (see [`Services`]). `None`: the portal cannot
    /// tell, because the directory is not ready, is old, or does not know the account. It reads
    /// memory only.
    fn services(&self, login: &str) -> Option<Services>;

    /// The same for the owner of a key of the database. `None`: the portal cannot tell, or the key
    /// does not exist.
    fn key_services(&self, key_id: &str) -> Option<Services>;

    /// The logins that may see the alerts about `service`: each person whose [`Authorizer::services`]
    /// sees it. A person that the portal cannot judge is not in the set. It reads memory only.
    fn audience(&self, service: &str) -> BTreeSet<String>;
}

/// The groups that a phone of `login` may subscribe to. `None`: the inbox cannot tell, so it
/// shows and changes nothing. A login that no account has may subscribe to no group.
pub fn subscribable_groups(app: &App, login: &str) -> Option<BTreeSet<String>> {
    let authorizer = app.authorizer()?;
    match authorizer.subscribable(login) {
        Subscribable::Groups(groups) => Some(groups),
        Subscribable::Absent => Some(BTreeSet::new()),
        Subscribable::Unknown => None,
    }
}

/// Whether an account has `login`. Without an authorizer, the inbox cannot tell.
pub fn standing(app: &App, login: &str) -> Standing {
    match app.authorizer() {
        Some(authorizer) => authorizer.standing(login),
        None => Standing::Unknown,
    }
}

/// A subscription that an account lost.
#[derive(Debug, PartialEq, Eq)]
pub struct Lost {
    pub device_id: String,
    pub login: String,
    pub group: String,
}

/// What a look at the rights changes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The subscriptions to remove.
    pub lost: Vec<Lost>,
    /// The logins that have a device that is not revoked, and no account (SQLite only). The inbox
    /// ends their state.
    pub absent: BTreeSet<String>,
}

/// What to change. A device that is revoked and an account that the authorizer cannot judge give
/// nothing. A subscription goes only when the authorizer lists the groups of the account and the
/// group is not among them. A login that no account has goes as a whole.
pub fn plan(devices: &[DeviceView], authorizer: &dyn Authorizer) -> Plan {
    let mut verdicts: HashMap<&str, Subscribable> = HashMap::new();
    let mut out = Plan::default();
    for device in devices {
        if device.revoked_at.is_some() {
            continue;
        }
        let verdict = verdicts
            .entry(device.user_name.as_str())
            .or_insert_with(|| authorizer.subscribable(&device.user_name));
        let allowed = match verdict {
            Subscribable::Groups(allowed) => allowed,
            Subscribable::Absent => {
                out.absent.insert(device.user_name.clone());
                continue;
            }
            Subscribable::Unknown => continue,
        };
        for group in device
            .subscriptions
            .iter()
            .filter(|g| !allowed.contains(*g))
        {
            out.lost.push(Lost {
                device_id: device.id.clone(),
                login: device.user_name.clone(),
                group: group.clone(),
            });
        }
    }
    out
}

/// Removes the subscriptions that the accounts lost and ends the logins that no account has (see
/// [`plan`]). The alerts that the inbox made before stay deliverable to the other phones. The
/// result is the number of subscriptions that it removed, or `None` when the storage did not
/// answer: the caller tries again.
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
    let Plan { lost, absent } = plan(&devices, authorizer.as_ref());
    for login in &absent {
        // An account of the file that left the file, or an old login of the superadmin.
        match super::enrollment::end_account(app, login).await {
            Ok(count) => log::warn!(
                event = "absent_account_ended",
                user.name = %login,
                devices = count
            ),
            Err(e) => {
                log::warn!(event = "absent_account_end_failed", user.name = %login, error = %e);
                return None;
            }
        }
    }
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
        /// The services that each login sees. A login that is not in the map is unknown.
        pub services: Mutex<HashMap<String, Services>>,
        pub stamp: Mutex<Option<u64>>,
    }

    impl Fixed {
        pub fn new() -> Arc<Fixed> {
            Arc::new(Fixed {
                answers: Mutex::new(HashMap::new()),
                services: Mutex::new(HashMap::new()),
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
        fn decide_service(&self, _key: &str, _action: &str, _count: bool) -> Decision {
            Decision::NotReady
        }
        fn user_exists(&self, _login: &str) -> Option<bool> {
            None
        }
        fn subscribable(&self, login: &str) -> Subscribable {
            self.answers
                .lock()
                .unwrap()
                .get(login)
                .cloned()
                .unwrap_or(Subscribable::Unknown)
        }
        fn standing(&self, login: &str) -> Standing {
            match self.subscribable(login) {
                Subscribable::Groups(_) => Standing::Exists,
                Subscribable::Absent => Standing::Absent,
                Subscribable::Unknown => Standing::Unknown,
            }
        }
        fn directory_stamp(&self) -> Option<u64> {
            *self.stamp.lock().unwrap()
        }
        fn services(&self, login: &str) -> Option<Services> {
            self.services.lock().unwrap().get(login).cloned()
        }
        fn key_services(&self, _key_id: &str) -> Option<Services> {
            None
        }
        fn audience(&self, service: &str) -> BTreeSet<String> {
            self.services
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, seen)| seen.sees(Some(service)))
                .map(|(login, _)| login.clone())
                .collect()
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
        let lost = plan(&devices, authorizer.as_ref()).lost;
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
        assert_eq!(plan(&devices, authorizer.as_ref()), Plan::default());
        // An account that has no answer at all is unknown too.
        let empty = Fixed::new();
        assert_eq!(plan(&devices, empty.as_ref()), Plan::default());
    }

    /// The audit finding portal/inbox/phones-of-removed-file-account-stay-active: a login that no
    /// account has goes as a whole, with or without a subscription. A revoked device gives nothing.
    #[test]
    fn a_login_without_an_account_goes_as_a_whole() {
        let authorizer = Fixed::new();
        authorizer.set("dave", Subscribable::Absent);
        authorizer.set("alice", groups(&["infra"]));
        let devices = vec![
            device("d1", "dave", &["infra"], false),
            device("d2", "dave", &[], false),
            device("d3", "old", &["infra"], true),
            device("a1", "alice", &["infra"], false),
        ];
        authorizer.set("old", Subscribable::Absent);
        let planned = plan(&devices, authorizer.as_ref());
        assert_eq!(planned.absent, BTreeSet::from(["dave".to_string()]));
        assert!(planned.lost.is_empty());
    }

    /// The audit finding portal/watch/group-alert-names-services-hidden-from-receivers: the
    /// services of a reader are the services that the launcher shows to the reader.
    #[test]
    fn a_reader_sees_an_alert_about_a_service_only_when_the_launcher_shows_the_service() {
        let grant = |text: &str| crate::grants::Grant::parse(text).unwrap();
        let carol = [
            grant("inbox.receive on service:portal/inbox_group:infra"),
            grant("access on service:notes"),
            // A grant below a service does not open the service.
            grant("files.read on service:s3/bucket:b"),
        ];
        let dave = [grant("inbox.receive on service:portal/inbox_group:infra")];
        let seen = Services::of_grants(Some(&carol));
        assert_eq!(seen, Services::Only(BTreeSet::from(["notes".to_string()])));
        for service in ["notes", "s3", "portal", "other"] {
            assert_eq!(
                seen.sees(Some(service)),
                crate::access::may_open(Some(&carol), service),
                "{service}"
            );
            assert!(!Services::of_grants(Some(&dave)).sees(Some(service)));
            assert!(
                Services::of_grants(None).sees(Some(service)),
                "the superadmin"
            );
        }
        // An alert about no service has no such limit.
        assert!(Services::of_grants(Some(&dave)).sees(None));
        assert!(Services::of_grants(Some(&[])).sees(None));
    }

    #[test]
    fn a_revoked_device_and_a_device_without_subscription_give_nothing() {
        let authorizer = Fixed::new();
        authorizer.set("alice", groups(&[]));
        let devices = vec![
            device("a1", "alice", &["infra"], true),
            device("a2", "alice", &[], false),
        ];
        assert_eq!(plan(&devices, authorizer.as_ref()), Plan::default());
    }
}
