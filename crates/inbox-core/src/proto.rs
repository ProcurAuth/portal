//! Wire messages. All are JSON; binary fields are base64.
//!
//! Enrollment (`/v1/enroll`) and sessions (`/v1/session`) run over WebSocket binary
//! frames. Each frame is one Noise message.

use serde::{Deserialize, Serialize};

use crate::b64::serde_bytes;

/// Maximum length of an alert title, in characters.
pub const MAX_TITLE_CHARS: usize = 128;

/// The kind of a message. Protocol 4 knows one kind: `alert`.
///
/// The field `kind` is in each alert frame and in the stored alert. A later protocol
/// version can add kinds. Until then, a parser MUST reject any other value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// One way: a sender tells people. The device sends only a delivery receipt.
    Alert,
}

impl Kind {
    /// The text of the kind, as the wire and the database hold it.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Alert => "alert",
        }
    }

    /// The kind that a text names. `None` for each text that is not a known kind.
    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "alert" => Some(Kind::Alert),
            _ => None,
        }
    }
}

/// Delivery receipt sent by a device.
///
/// Alerts are one-way: there is no human acknowledgement. The only thing a device
/// sends back is this automatic receipt, once the alert is stored durably, so the
/// server knows when to stop retransmitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckStage {
    /// Stored durably on the device. Stops server retransmission.
    Received,
}

impl AckStage {
    pub fn as_str(self) -> &'static str {
        match self {
            AckStage::Received => "received",
        }
    }
}

// ---------------------------------------------------------------- enrollment

/// Payload of enrollment Noise message 1 (device to server).
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollHello {
    pub protocol: u32,
    /// The one-time secret of the enrollment code (16 bytes).
    #[serde(with = "serde_bytes")]
    pub secret: Vec<u8>,
    /// 1..64 bytes, no control characters.
    pub device_name: String,
    pub app_version: String,
}

impl std::fmt::Debug for EnrollHello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollHello")
            .field("protocol", &self.protocol)
            .field("secret", &"<hidden>")
            .field("device_name", &self.device_name)
            .field("app_version", &self.app_version)
            .finish()
    }
}

/// Payload of enrollment Noise message 2 (server to device).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollAccept {
    pub protocol: u32,
    /// 32 lowercase hexadecimal characters.
    pub device_id: String,
    pub server_name: String,
    /// The login of the account. It is the name of the device owner.
    pub user_name: String,
}

// ---------------------------------------------------------------- sessions

/// Payload of Noise message 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHello {
    pub protocol: u32,
    pub device_id: String,
    pub app_version: String,
}

/// Payload of Noise message 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAccept {
    pub protocol: u32,
    pub server_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertMsg {
    /// Globally unique (128 random bits, hex). The device deduplicates on it.
    pub alert_id: String,
    /// The kind of the message. Only `alert` is valid.
    pub kind: Kind,
    pub group: String,
    pub group_name: String,
    /// One line, at most [`MAX_TITLE_CHARS`] characters.
    pub title: String,
    /// Unix milliseconds at which the server accepted the alert.
    pub created_at_ms: i64,
}

/// Device -> server, inside the Noise session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMsg {
    /// First transport frame; proves the device completed the handshake.
    Ready,
    Ack {
        alert_id: String,
        stage: AckStage,
    },
    /// Subscribes to a group. How the device reacts (alarm, ring, notification) is
    /// a local setting the server never knows.
    Subscribe {
        group: String,
    },
    Unsubscribe {
        group: String,
    },
    Ping {
        nonce: u64,
    },
}

/// Server -> device, inside the Noise session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMsg {
    Welcome {
        server_name: String,
        groups: Vec<GroupInfo>,
        /// Ids of the groups this device is subscribed to.
        subscriptions: Vec<String>,
        heartbeat_interval_ms: u64,
        offline_warning_after_ms: u64,
        server_time_ms: i64,
        /// Login of the account of this device. Read-only.
        user_name: String,
    },
    Alert(AlertMsg),
    Subscriptions {
        subscriptions: Vec<String>,
    },
    Pong {
        nonce: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

/// WebSocket close codes used on `/v1/enroll` and `/v1/session`.
pub mod close {
    /// Enrollment: the code is unknown, already used or expired, or the account is revoked.
    pub const ENROLL_REFUSED: u16 = 4410;
    /// The device id is unknown or its key does not match: enroll again.
    pub const UNKNOWN_DEVICE: u16 = 4401;
    /// The device was revoked from the admin front: enroll again.
    pub const REVOKED: u16 = 4403;
    /// Incompatible protocol version: update the app.
    pub const PROTOCOL: u16 = 4426;
    /// Another session of the same device took over.
    pub const REPLACED: u16 = 4409;
    /// Too many failed attempts from this address.
    pub const RATE_LIMITED: u16 = 4429;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shapes_are_stable() {
        let m = ClientMsg::Ack { alert_id: "ab".into(), stage: AckStage::Received };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"t":"ack","alert_id":"ab","stage":"received"}"#
        );
        let a = ServerMsg::Alert(AlertMsg {
            alert_id: "ab".into(),
            kind: Kind::Alert,
            group: "g".into(),
            group_name: "G".into(),
            title: "disk full".into(),
            created_at_ms: 1,
        });
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.starts_with(r#"{"t":"alert","alert_id":"ab","kind":"alert","group":"g""#), "{s}");
        let back: ServerMsg = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, ServerMsg::Alert(ref x) if x.title == "disk full"));
        assert!(!s.contains("action"), "the server never knows alert levels");
        assert!(
            serde_json::from_str::<ClientMsg>(
                r#"{"t":"ack","alert_id":"a","stage":"received","x":1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn the_kind_of_an_alert_is_alert_and_nothing_else() {
        let frame = |kind: &str| {
            format!(
                r#"{{"t":"alert","alert_id":"ab","kind":{kind},"group":"g","group_name":"G","title":"x","created_at_ms":1}}"#
            )
        };
        assert!(serde_json::from_str::<ServerMsg>(&frame(r#""alert""#)).is_ok());
        for bad in [r#""question""#, r#""approval""#, r#""Alert""#, r#""""#, "null", "1"] {
            assert!(serde_json::from_str::<ServerMsg>(&frame(bad)).is_err(), "{bad}");
        }
        // The field is mandatory.
        let none = r#"{"t":"alert","alert_id":"ab","group":"g","group_name":"G","title":"x","created_at_ms":1}"#;
        assert!(serde_json::from_str::<ServerMsg>(none).is_err());
        assert_eq!(Kind::parse("alert"), Some(Kind::Alert));
        assert_eq!(Kind::parse("question"), None);
        assert_eq!(Kind::Alert.as_str(), "alert");
    }

    #[test]
    fn user_name_is_read_only() {
        // The device cannot change the login of the account.
        assert!(
            serde_json::from_str::<ClientMsg>(r#"{"t":"set_user_name","user_name":"x"}"#).is_err()
        );
        assert!(serde_json::from_str::<ServerMsg>(r#"{"t":"user_name","user_name":"x"}"#).is_err());
        let w = r#"{"t":"welcome","server_name":"S","groups":[],"subscriptions":[],"heartbeat_interval_ms":1,"offline_warning_after_ms":2,"server_time_ms":3,"user_name":"alice"}"#;
        assert!(matches!(
            serde_json::from_str::<ServerMsg>(w).unwrap(),
            ServerMsg::Welcome { ref user_name, .. } if user_name == "alice"
        ));
    }

    #[test]
    fn close_codes_are_stable() {
        assert_eq!(close::ENROLL_REFUSED, 4410);
        assert_eq!(close::PROTOCOL, 4426);
        assert_eq!(close::RATE_LIMITED, 4429);
    }
}
