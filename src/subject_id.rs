//! Who the subject of an AuthZEN question is. A subject is a person with a login, or a key. A key
//! has an id with a colon: the colon is the separator between the prefix and the local part
//! (protocol 3.2). No login has a colon, because the program refuses it at start for the
//! superadmin, and the rule of the accounts refuses it for the others. Therefore, no person can
//! have the id of a key. This module is the only place that reads the id. The superadmin check
//! applies only to a person.
//!
//! Only the prefix `key:` exists. An id with a colon and another prefix, or a bad local part, is
//! an unknown subject (protocol 3.7).

/// The prefix of the id of a key. The local part that follows is made by the program at random
/// (portal 4.7).
pub const KEY_PREFIX: &str = "key:";

/// The separator that no login can have.
pub const SEPARATOR: char = ':';

/// The longest local part of a key in an id.
const LOCAL_MAX: usize = 64;

/// What an id names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectId<'a> {
    /// A person. The text is the login.
    User(&'a str),
    /// A key. The text is the local part after `key:`.
    Key(&'a str),
    /// An id with a colon that names no known prefix, or a bad local part.
    Unknown,
}

impl<'a> SubjectId<'a> {
    /// Reads the id of a subject.
    pub fn parse(id: &'a str) -> Self {
        if !id.contains(SEPARATOR) {
            return Self::User(id);
        }
        match id.strip_prefix(KEY_PREFIX) {
            Some(local) if valid_local(local) => Self::Key(local),
            _ => Self::Unknown,
        }
    }
}

/// A local part is 1 to 64 characters from a to z, 0 to 9, underscore and hyphen (protocol 3.2).
fn valid_local(local: &str) -> bool {
    (1..=LOCAL_MAX).contains(&local.len())
        && local
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_without_a_colon_is_a_person() {
        assert_eq!(SubjectId::parse("alice"), SubjectId::User("alice"));
        assert_eq!(SubjectId::parse(""), SubjectId::User(""));
        assert_eq!(SubjectId::parse("a.b_c-d"), SubjectId::User("a.b_c-d"));
    }

    #[test]
    fn a_key_id_has_the_prefix_and_a_valid_local_part() {
        assert_eq!(
            SubjectId::parse("key:9b1c04e2aa71f0c3"),
            SubjectId::Key("9b1c04e2aa71f0c3")
        );
        assert_eq!(SubjectId::parse("key:a-1_b"), SubjectId::Key("a-1_b"));
    }

    #[test]
    fn any_other_id_with_a_colon_is_unknown() {
        for id in [
            "key:",
            "key:UPPER",
            "key:a b",
            "key:a:b",
            "key:é",
            "other:abc",
            "access:abc",
            "Key:abc",
            ":",
            "alice:",
            "~key:x",
        ] {
            assert_eq!(SubjectId::parse(id), SubjectId::Unknown, "{id:?}");
        }
        let long = format!("key:{}", "a".repeat(65));
        assert_eq!(SubjectId::parse(&long), SubjectId::Unknown);
        let longest = format!("key:{}", "a".repeat(64));
        assert!(matches!(SubjectId::parse(&longest), SubjectId::Key(_)));
    }

    #[test]
    fn the_superadmin_login_can_never_look_like_a_key() {
        // `check_login` in the configuration refuses a colon. This test keeps the two rules together.
        assert_eq!(
            SubjectId::parse("root"),
            SubjectId::User("root"),
            "a login without a colon is a person"
        );
        assert_ne!(SubjectId::parse("key:root"), SubjectId::User("key:root"));
    }
}
