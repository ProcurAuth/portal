//! Wire messages. All are JSON; binary fields are base64.
//!
//! Enrollment runs over WebSocket text frames at `/v1/enroll`. Sessions run over
//! WebSocket binary frames at `/v1/session`, each frame being one Noise message.

use serde::{Deserialize, Serialize};

use crate::b64::serde_bytes;

/// Maximum length of an alert description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 128;

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
    /// Legacy (app 0.1 / 0.2). Treated as `Received`.
    Displayed,
    /// Legacy (app 0.1 / 0.2). Treated as `Received`.
    Acknowledged,
}

impl AckStage {
    pub fn as_str(self) -> &'static str {
        match self {
            AckStage::Received => "received",
            AckStage::Displayed => "displayed",
            AckStage::Acknowledged => "acknowledged",
        }
    }
}

// ---------------------------------------------------------------- enrollment

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnrollClientMsg {
    Start {
        protocol: u32,
        #[serde(with = "serde_bytes")]
        credential_request: Vec<u8>,
    },
    Finish {
        #[serde(with = "serde_bytes")]
        credential_finalization: Vec<u8>,
        device_name: String,
        /// Pseudonym of the person using the device. Absent for apps 0.1 / 0.2.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_name: Option<String>,
        #[serde(with = "serde_bytes")]
        device_public_key: Vec<u8>,
        /// Binds the device key and name to the OPAQUE session.
        #[serde(with = "serde_bytes")]
        mac: Vec<u8>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnrollServerMsg {
    Challenge {
        #[serde(with = "serde_bytes")]
        credential_response: Vec<u8>,
    },
    Accepted {
        device_id: String,
        server_name: String,
        #[serde(with = "serde_bytes")]
        server_public_key: Vec<u8>,
        /// Proves the server key comes from the party holding the credential record.
        #[serde(with = "serde_bytes")]
        mac: Vec<u8>,
    },
    Rejected {
        code: String,
        message: String,
    },
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
    pub group: String,
    pub group_name: String,
    /// Short summary, at most [`MAX_DESCRIPTION_CHARS`] characters.
    pub description: Option<String>,
    /// Longer content, in Markdown.
    pub details: Option<String>,
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
    /// Changes the pseudonym under which this device is tracked.
    SetUserName {
        user_name: String,
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
        /// Pseudonym recorded for this device, if any.
        #[serde(default)]
        user_name: Option<String>,
    },
    /// Confirms a `SetUserName`.
    UserName {
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

/// WebSocket close codes used on `/v1/session`.
pub mod close {
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
            group: "g".into(),
            group_name: "G".into(),
            description: None,
            details: Some("**m**".into()),
            created_at_ms: 1,
        });
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.starts_with(r#"{"t":"alert","alert_id":"ab""#), "{s}");
        let back: ServerMsg = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, ServerMsg::Alert(ref x) if x.details.as_deref() == Some("**m**")));
        assert!(!s.contains("action"), "the server never knows alert levels");
        assert!(
            serde_json::from_str::<ClientMsg>(
                r#"{"t":"ack","alert_id":"a","stage":"received","x":1}"#
            )
            .is_err()
        );
    }
}
