//! Wire messages. All are JSON; binary fields are base64.
//!
//! Enrollment runs over WebSocket text frames at `/v1/enroll`. Sessions run over
//! WebSocket binary frames at `/v1/session`, each frame being one Noise message.

use serde::{Deserialize, Serialize};

use crate::b64::serde_bytes;

/// What the device does when an alert of a subscribed group arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Loops the alarm sound at full alarm volume, overrides silent / vibrate / do
    /// not disturb and shows a full-screen alert until acknowledged.
    Alarm,
    /// Plays one notification sound even when the phone is silent or on vibrate.
    Ring,
    /// Standard notification following the phone sound mode.
    Notify,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Alarm => "alarm",
            Action::Ring => "ring",
            Action::Notify => "notify",
        }
    }

    pub fn parse(text: &str) -> Option<Action> {
        match text {
            "alarm" => Some(Action::Alarm),
            "ring" => Some(Action::Ring),
            "notify" => Some(Action::Notify),
            _ => None,
        }
    }
}

/// Delivery stages acknowledged by a device, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckStage {
    /// Stored durably on the device. Stops server retransmission.
    Received,
    /// Shown to the user (notification posted / alarm started).
    Displayed,
    /// Explicitly acknowledged by a human.
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
pub struct Subscription {
    pub group: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertMsg {
    /// Globally unique (128 random bits, hex). The device deduplicates on it.
    pub alert_id: String,
    pub group: String,
    pub group_name: String,
    pub title: Option<String>,
    pub message: Option<String>,
    /// Unix milliseconds at which the server accepted the alert.
    pub created_at_ms: i64,
    /// Action chosen by this device's subscription when the alert was accepted.
    pub action: Action,
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
    Subscribe {
        group: String,
        action: Action,
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
        subscriptions: Vec<Subscription>,
        heartbeat_interval_ms: u64,
        offline_warning_after_ms: u64,
        server_time_ms: i64,
    },
    Alert(AlertMsg),
    Subscriptions {
        subscriptions: Vec<Subscription>,
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
            title: None,
            message: Some("m".into()),
            created_at_ms: 1,
            action: Action::Alarm,
        });
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.starts_with(r#"{"t":"alert","alert_id":"ab""#), "{s}");
        let back: ServerMsg = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, ServerMsg::Alert(ref x) if x.action == Action::Alarm));
        assert!(
            serde_json::from_str::<ClientMsg>(
                r#"{"t":"ack","alert_id":"a","stage":"received","x":1}"#
            )
            .is_err()
        );
    }
}
