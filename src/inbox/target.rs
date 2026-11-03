//! The target of a message: a group or one account.
//!
//! A group has subscribers. An account has phones: a message for an account goes to each phone of
//! the account that is not revoked, whatever its subscriptions. The store keeps the target as two
//! columns: the kind (`group` or `user`) and the id (the id of the group, or the login).
//!
//! The protocol of the phones has no target: the frame of an alert carries a group id and a group
//! name. A message for an account carries the fixed id [`DIRECT_ID`] and the fixed name
//! [`DIRECT_NAME`]. A group id of the file has only the characters `a-z`, `0-9`, `-` and `_`, so
//! the id of a direct message never equals the id of a group. The level of a direct message is
//! never critical: the phone applies its own level for this id.

use serde::{Deserialize, Serialize};

/// The group id that the frame of a message for an account carries.
pub const DIRECT_ID: &str = "@direct";
/// The group name that the frame of a message for an account carries.
pub const DIRECT_NAME: &str = "Direct";

/// The kind of a target, as the store writes it.
pub const KIND_GROUP: &str = "group";
pub const KIND_USER: &str = "user";

/// Who receives a message.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "lowercase",
    deny_unknown_fields
)]
pub enum Target {
    /// The phones that subscribe to the group.
    Group(String),
    /// The phones of the account with this login that are not revoked.
    User(String),
}

impl Target {
    /// The kind as the store writes it: `group` or `user`.
    pub fn kind(&self) -> &'static str {
        match self {
            Target::Group(_) => KIND_GROUP,
            Target::User(_) => KIND_USER,
        }
    }

    /// The id of the group, or the login of the account.
    pub fn id(&self) -> &str {
        match self {
            Target::Group(id) | Target::User(id) => id,
        }
    }

    /// The target that two stored columns name. `None`: the kind is not `group` or `user`.
    pub fn from_columns(kind: &str, id: String) -> Option<Target> {
        match kind {
            KIND_GROUP => Some(Target::Group(id)),
            KIND_USER => Some(Target::User(id)),
            _ => None,
        }
    }

    /// The id of the group. `None` for an account.
    pub fn group(&self) -> Option<&str> {
        match self {
            Target::Group(id) => Some(id),
            Target::User(_) => None,
        }
    }

    /// The group id that the frame of the phone carries, and the name that goes with it. The
    /// function `group_name` gives the name of a group of the file.
    pub fn frame(&self, group_name: impl FnOnce(&str) -> String) -> (String, String) {
        match self {
            Target::Group(id) => (id.clone(), group_name(id)),
            Target::User(_) => (DIRECT_ID.to_string(), DIRECT_NAME.to_string()),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind(), self.id())
    }
}

/// The account that sent an alert, when the `source` of the alert tells it: `admin:<login>` for
/// the form of the page. A key gives its source `producer:key:<id>` or `producer:<name>`: the
/// portal finds the owner of the key itself.
pub fn account_of_source(source: &str) -> Option<&str> {
    source.strip_prefix("admin:").filter(|l| !l.is_empty())
}

/// The id of the key of the database that sent an alert, when the `source` tells it.
pub fn key_of_source(source: &str) -> Option<&str> {
    let name = source.split_once(':').map(|(_, name)| name)?;
    name.strip_prefix("key:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_has_a_kind_and_an_id() {
        let group = Target::Group("infra".into());
        let user = Target::User("alice".into());
        assert_eq!((group.kind(), group.id()), ("group", "infra"));
        assert_eq!((user.kind(), user.id()), ("user", "alice"));
        assert_eq!((group.group(), user.group()), (Some("infra"), None));
        assert_eq!(user.to_string(), "user:alice");
    }

    #[test]
    fn the_columns_give_the_target_back() {
        for target in [Target::Group("a".into()), Target::User("b".into())] {
            assert_eq!(
                Target::from_columns(target.kind(), target.id().to_string()),
                Some(target)
            );
        }
        assert_eq!(Target::from_columns("team", "a".into()), None);
    }

    #[test]
    fn the_wire_form_names_the_kind() {
        let text = serde_json::to_string(&Target::User("alice".into())).unwrap();
        assert_eq!(text, r#"{"kind":"user","id":"alice"}"#);
        let back: Target = serde_json::from_str(&text).unwrap();
        assert_eq!(back, Target::User("alice".into()));
        assert!(serde_json::from_str::<Target>(r#"{"kind":"team","id":"a"}"#).is_err());
    }

    #[test]
    fn the_frame_of_a_direct_message_never_names_a_group() {
        let name = |id: &str| format!("name of {id}");
        assert_eq!(
            Target::Group("infra".into()).frame(name),
            ("infra".to_string(), "name of infra".to_string())
        );
        let (id, label) = Target::User("alice".into()).frame(|_| unreachable!());
        assert_eq!((id.as_str(), label.as_str()), (DIRECT_ID, DIRECT_NAME));
        assert!(crate::config::keys::check_inbox_id(DIRECT_ID).is_err());
    }

    #[test]
    fn the_source_tells_the_sender() {
        assert_eq!(account_of_source("admin:alice"), Some("alice"));
        assert_eq!(account_of_source("admin"), None);
        assert_eq!(account_of_source("producer:key:k1"), None);
        assert_eq!(key_of_source("producer:key:k1"), Some("k1"));
        assert_eq!(key_of_source("producer:ci"), None);
        assert_eq!(key_of_source("portal"), None);
    }
}
