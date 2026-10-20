//! Who the subject of an AuthZEN question is. A subject is a person with a login, or a machine
//! principal. A machine principal has an id with a colon: the colon is the separator between the
//! kind and the name. No login has a colon, because the program refuses it at start for the
//! superadmin, and the rule of the accounts refuses it for the others. Therefore, no person can
//! have the id of a machine principal. This module is the only place that reads the id. The
//! superadmin check applies only to a person.
//!
//! Only the `access:` kind exists, and nothing makes an access yet. The decision for such an id is
//! the same as for an unknown login.

/// The prefix of the id of an access. The name that follows is made by the program at random.
pub const ACCESS_PREFIX: &str = "access:";

/// The separator that no login can have.
pub const SEPARATOR: char = ':';

/// The longest name of an access in an id.
const NAME_MAX: usize = 64;

/// What an id names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectId<'a> {
    /// A person. The text is the login.
    User(&'a str),
    /// An access. The text is the name after `access:`.
    Access(&'a str),
    /// An id with a colon that names no known kind, or a bad name.
    Unknown,
}

impl<'a> SubjectId<'a> {
    /// Reads the id of a subject.
    pub fn parse(id: &'a str) -> Self {
        if !id.contains(SEPARATOR) {
            return Self::User(id);
        }
        match id.strip_prefix(ACCESS_PREFIX) {
            Some(name) if valid_name(name) => Self::Access(name),
            _ => Self::Unknown,
        }
    }
}

/// A name is 1 to 64 characters from a to z, 0 to 9, underscore and hyphen.
fn valid_name(name: &str) -> bool {
    (1..=NAME_MAX).contains(&name.len())
        && name
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
    fn an_access_id_has_the_prefix_and_a_valid_name() {
        assert_eq!(
            SubjectId::parse("access:acc_9b1c04e2aa71"),
            SubjectId::Access("acc_9b1c04e2aa71")
        );
        assert_eq!(SubjectId::parse("access:a-1"), SubjectId::Access("a-1"));
    }

    #[test]
    fn any_other_id_with_a_colon_is_unknown() {
        for id in [
            "access:",
            "access:UPPER",
            "access:a b",
            "access:a:b",
            "access:é",
            "key:abc",
            "Access:abc",
            ":",
            "alice:",
            "~acc:x",
        ] {
            assert_eq!(SubjectId::parse(id), SubjectId::Unknown, "{id:?}");
        }
        let long = format!("access:{}", "a".repeat(65));
        assert_eq!(SubjectId::parse(&long), SubjectId::Unknown);
        let longest = format!("access:{}", "a".repeat(64));
        assert!(matches!(SubjectId::parse(&longest), SubjectId::Access(_)));
    }

    #[test]
    fn the_superadmin_login_can_never_look_like_a_machine_principal() {
        // `check_login` in the configuration refuses a colon. This test keeps the two rules together.
        assert_eq!(
            SubjectId::parse("root"),
            SubjectId::User("root"),
            "a login without a colon is a person"
        );
        assert_ne!(
            SubjectId::parse("access:root"),
            SubjectId::User("access:root")
        );
    }
}
