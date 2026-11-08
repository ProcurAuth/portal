//! The approval gate of the PDP (protocol 17.1, 17.2, 17.8; portal 3.7).
//!
//! A grant with `confirm` makes an action need the approval of a person. The question carries the effect of the
//! operation in `context.approval.effect`. The gate looks at the newest approval for the same service, subject,
//! action, resource and effect, and it answers as protocol 17.1 says:
//!
//! - no approval, or an approval that was consumed: `false` with `approval_required` and an `evaluation_id`;
//! - the owner has not decided: `false` with `approval_pending` and an `evaluation_id`;
//! - the owner declined, and the deny time has not ended: `false` with `approval_denied`;
//! - the approval is void: `false` with `approval_expired` for a use, and `approval_required` for a question
//!   without `usage`;
//! - the approval can be used: `true`. A use consumes it first (compare-and-set in the inbox database), then
//!   counts the unit of the budgets. When the count refuses or fails, the gate gives the approval back (P7). A crash
//!   between the two steps loses one approval and never gives two executions.
//!
//! A `false` answer with one of these reasons takes no unit. The answer holds the reason and the `evaluation_id`
//! and nothing else: no owner, no device, no comment, no time of the answer (protocol 17.12).
//!
//! The `evaluation_id` is stateless: it holds the id of a content key, the time of issue and an HMAC over the
//! service, the subject, the action, the resource, the effect and the time. Each instance that holds the content
//! keys can check it.

use std::future::Future;

use base64ct::{Base64UrlUnpadded, Encoding};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};

use super::*;
use crate::inbox::content_box::ContentBox;
use crate::inbox::{self, Clock, Consumed};

pub(super) const APPROVAL_REQUIRED: &str = "approval_required";
pub(super) const APPROVAL_PENDING: &str = "approval_pending";
pub(super) const APPROVAL_DENIED: &str = "approval_denied";
pub(super) const APPROVAL_EXPIRED: &str = "approval_expired";

/// The values of the metric `portal_pdp_approval_total{reason}`, in the order of [`Stats::approvals`].
pub(super) const GATE_REASONS: [&str; 7] = [
    APPROVAL_REQUIRED,
    APPROVAL_PENDING,
    APPROVAL_DENIED,
    APPROVAL_EXPIRED,
    "consumed",
    "released",
    "not_enabled",
];
const REQUIRED: usize = 0;
const PENDING: usize = 1;
const DENIED: usize = 2;
const EXPIRED: usize = 3;
const CONSUMED: usize = 4;
const RELEASED: usize = 5;
const NOT_ENABLED: usize = 6;

/// The members `procurauth_approval_*` of the metadata: the values that protocol 17 says the PDP publishes (the names
/// are those of the reference PDP of the protocol). `None` without the block `inbox.approvals`.
pub(super) fn metadata(config: &Config) -> Option<Value> {
    let inbox = config.inbox.as_ref()?;
    let (items, approvals) = (inbox.items.as_ref()?, inbox.approvals.as_ref()?);
    Some(json!({
        "procurauth_approval_lease_min_seconds": items.lease.min.as_secs(),
        "procurauth_approval_lease_max_seconds": items.lease.max.as_secs(),
        "procurauth_approval_grace_seconds": items.lease.grace.as_secs(),
        "procurauth_approval_revive_seconds": items.lease.revive_window.as_secs(),
        "procurauth_approval_max_revivals": items.lease.max_revivals,
        "procurauth_approval_max_life_seconds": items.max_lifetime.as_secs(),
        "procurauth_approval_wait_max_seconds": items.wait_max.as_secs(),
        "procurauth_approval_consume_seconds": approvals.consume_within.as_secs(),
        "procurauth_approval_deny_seconds": approvals.deny_cooldown.as_secs(),
        "procurauth_approval_evaluation_ttl_seconds": approvals.evaluation_ttl.as_secs(),
        "procurauth_approval_ref_ttl_seconds": approvals.intent_ttl.as_secs(),
        "procurauth_approval_max_command_bytes": approvals.max_command_bytes,
    }))
}

/// What a question says about the approval.
pub(super) struct Ask<'a> {
    /// The question has `context.usage`: it is a use.
    pub usage: bool,
    /// `context.approval.effect`.
    pub effect: Option<&'a str>,
    /// The server that asks has `approvals = true`.
    pub enabled: bool,
    /// The subject of the question, as the request gives it. The approval is bound to it.
    pub subject: &'a str,
}

impl Ask<'_> {
    /// A question of the portal for itself: no effect, and no approval is possible.
    pub(super) fn internal(usage: bool) -> Ask<'static> {
        Ask {
            usage,
            effect: None,
            enabled: false,
            subject: "",
        }
    }
}

/// An approval that the gate consumed. `release` gives it back.
pub(super) struct Held {
    request_id: String,
    consumed_at: i64,
}

/// A fault that a test asks the gate to play. The values are in `AppState::pdp_fault` and exist in tests only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Fault {
    None,
    /// The process stops after the consumption: no count, no give-back.
    CrashAfterConsume,
    /// The count of the budgets fails.
    CountFails,
}

#[cfg(test)]
pub(super) fn fault(state: &AppState) -> Fault {
    match state.pdp_fault.load(std::sync::atomic::Ordering::SeqCst) {
        1 => Fault::CrashAfterConsume,
        2 => Fault::CountFails,
        _ => Fault::None,
    }
}

#[cfg(not(test))]
pub(super) fn fault(_state: &AppState) -> Fault {
    Fault::None
}

/// The effect of `context.approval`. `Ok(None)` without it. A value that is not `{"effect": "sha256:<64 hex>"}`
/// gets HTTP 400 (protocol 17.1).
pub(super) fn read_effect(context: Option<&Value>) -> Result<Option<String>, String> {
    let Some(approval) = context.and_then(|c| c.get("approval")) else {
        return Ok(None);
    };
    let bad = || {
        "context.approval must be {\"effect\": \"sha256:\" and 64 lower-case hexadecimal digits}"
            .to_string()
    };
    let object = approval
        .as_object()
        .filter(|o| o.len() == 1)
        .ok_or_else(bad)?;
    let effect = object
        .get("effect")
        .and_then(Value::as_str)
        .ok_or_else(bad)?;
    match effect.strip_prefix("sha256:") {
        Some(hex)
            if hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Ok(Some(effect.to_string()))
        }
        _ => Err(bad()),
    }
}

/// The path of the resource without the service, as the effect and the approval keep it: `type:id` for each
/// segment, the ids percent-encoded, joined with `/`. The service itself has the empty text.
pub(super) fn resource_path(chain: &[Segment]) -> String {
    chain
        .iter()
        .map(|s| format!("{}:{}", s.kind, encode_id(&s.id)))
        .collect::<Vec<_>>()
        .join("/")
}

/// Runs a call of the inbox from the synchronous code of the PDP. The PDP already runs its blocking calls this way
/// (see [`crate::util::blocking`]).
fn run<T>(work: impl Future<Output = T>) -> T {
    let handle = tokio::runtime::Handle::current();
    crate::util::blocking(|| handle.block_on(work))
}

/// The gate of one question whose verdict asks for an approval. `Ok(None)`: the action can run, and nothing was
/// consumed. `Ok(Some(held))`: the action can run, and the use consumed this approval. `Err`: the answer.
pub(super) fn gate(
    state: &AppState,
    q: &Question<'_>,
    ask: &Ask<'_>,
) -> Result<Option<Held>, Outcome> {
    if !ask.enabled {
        state.pdp_stats.approval(NOT_ENABLED);
        return Err(Outcome::deny(
            "the server has no approvals: fail closed",
            None,
        ));
    }
    let Some(effect) = ask.effect else {
        // The PDP cannot approve an operation that has no effect, and it issues no `evaluation_id` for it.
        return Err(refuse(
            state,
            REQUIRED,
            "approval required, no effect",
            None,
        ));
    };
    let Some(app) = state.inbox.app() else {
        return Err(unavailable("the inbox is not running"));
    };
    let Some(items) = app.items() else {
        return Err(unavailable("the inbox has no items"));
    };
    let path = resource_path(q.chain);
    let key_hash = inbox::approval_key_hash(q.service, ask.subject, q.action, &path, effect);
    let found = if ask.usage {
        run(items.consume(&key_hash))
    } else {
        run(items.status_for_pdp(&key_hash))
    };
    let found = match found {
        Ok(found) => found,
        Err(error) => {
            inbox::log::warn!(event = "pdp_approval_store_error", error = %error);
            return Err(unavailable("the approval store failed"));
        }
    };
    let now = app.clock.now_ms();
    let bound = Bound {
        service: q.service,
        subject: ask.subject,
        action: q.action,
        resource: &path,
        effect,
    };
    let id = || {
        app.secrets
            .content
            .as_ref()
            .and_then(|content| evaluation_id(content, now, &bound).ok())
    };
    let cooldown_ms = app
        .config
        .approvals
        .as_ref()
        .map_or(0, |a| a.deny_cooldown.as_millis() as i64);
    match found {
        Consumed::Used(l) => {
            state.pdp_stats.approval(CONSUMED);
            let consumed_at = l.approval.as_ref().and_then(|a| a.consumed_at);
            match consumed_at {
                Some(consumed_at) => Ok(Some(Held {
                    request_id: l.item.request_id.clone(),
                    consumed_at,
                })),
                None => Err(unavailable("the consumed approval has no time")),
            }
        }
        Consumed::Usable(_) => Ok(None),
        Consumed::NoApproval => Err(refuse(state, REQUIRED, "approval required", id())),
        Consumed::Pending(_) => Err(refuse(state, PENDING, "approval pending", id())),
        Consumed::Denied(l) => {
            let answered = l.item.answered_at.unwrap_or(0);
            // The deny time ends at `answered_at + deny_cooldown`: the same effect then asks again.
            if now < answered + cooldown_ms {
                Err(refuse(state, DENIED, "approval denied", None))
            } else {
                Err(refuse(state, REQUIRED, "approval required", id()))
            }
        }
        Consumed::Gone(l) => {
            let consumed = l.approval.as_ref().is_some_and(|a| a.consumed_at.is_some());
            if ask.usage && !consumed {
                Err(refuse(state, EXPIRED, "approval expired", None))
            } else {
                Err(refuse(state, REQUIRED, "approval required", id()))
            }
        }
    }
}

/// Gives a consumed approval back after the count of the budgets refused or failed. When the give-back fails, the
/// approval stays consumed: the agent asks again. This never gives a second execution.
pub(super) fn release(state: &AppState, held: &Held) {
    let Some(app) = state.inbox.app() else { return };
    let Some(items) = app.items() else { return };
    match run(items.release(&held.request_id, held.consumed_at)) {
        Ok(true) => state.pdp_stats.approval(RELEASED),
        Ok(false) => {}
        Err(error) => inbox::log::error!(
            event = "pdp_approval_release_failed",
            request.id = %held.request_id,
            error = %error
        ),
    }
}

fn refuse(state: &AppState, which: usize, why: &'static str, id: Option<String>) -> Outcome {
    state.pdp_stats.approval(which);
    let mut outcome = Outcome::deny(why, Some(GATE_REASONS[which]));
    outcome.evaluation_id = id;
    outcome
}

fn unavailable(why: &'static str) -> Outcome {
    inbox::log::warn!(event = "pdp_approval_unavailable", detail = why);
    let mut outcome = Outcome::deny(why, None);
    outcome.unavailable = Some(Unavailable::StoreError);
    outcome
}

// ---------------------------------------------------------------- the evaluation_id

/// The version of the text that the HMAC covers.
const ID_CONTEXT: &[u8] = b"portal-evaluation-id/v1";
const MAC_LEN: usize = 32;

/// The facts that an `evaluation_id` binds.
pub(super) struct Bound<'a> {
    pub service: &'a str,
    pub subject: &'a str,
    pub action: &'a str,
    pub resource: &'a str,
    pub effect: &'a str,
}

fn mac_of(key: &[u8; 32], bound: &Bound<'_>, issued_at: u64) -> Hmac<sha2::Sha256> {
    let mut mac =
        <Hmac<sha2::Sha256> as Mac>::new_from_slice(key).expect("an HMAC takes any key length");
    let mut part = |bytes: &[u8]| {
        mac.update(&(bytes.len() as u32).to_be_bytes());
        mac.update(bytes);
    };
    part(ID_CONTEXT);
    part(bound.service.as_bytes());
    part(bound.subject.as_bytes());
    part(bound.action.as_bytes());
    part(bound.resource.as_bytes());
    part(bound.effect.as_bytes());
    part(&issued_at.to_be_bytes());
    mac
}

/// The facts of a question that an `evaluation_id` carries, sealed. The submission API has the service of the token
/// and the effect, but not the subject, the action and the resource: it reads them from the id.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    pub subject: String,
    pub action: String,
    pub resource: String,
}

/// The text that the seal of the facts covers besides the facts: the version and the tag of the HMAC, so the sealed
/// block cannot move to another id.
fn facts_aad(tag: &[u8]) -> Vec<u8> {
    let mut aad = ID_CONTEXT.to_vec();
    aad.extend_from_slice(b"/facts");
    aad.extend_from_slice(tag);
    aad
}

/// Makes the `evaluation_id` of a question: base64url of the key id (with its length byte), the time of issue in
/// seconds (8 bytes), the HMAC (32 bytes) and the facts of the question (subject, action, resource), sealed with the
/// content key. The id shows no fact to the service that holds it.
pub(super) fn evaluation_id(
    content: &ContentBox,
    now_ms: i64,
    bound: &Bound<'_>,
) -> Result<String, String> {
    let (kid, key) = content.evaluation_key();
    let issued_at = (now_ms / 1000).max(0) as u64;
    let tag = mac_of(key, bound, issued_at).finalize().into_bytes();
    let facts = json!([bound.subject, bound.action, bound.resource]).to_string();
    let sealed = content.seal(facts.as_bytes(), &facts_aad(&tag))?;
    let mut raw = Vec::with_capacity(1 + kid.len() + 8 + MAC_LEN + sealed.len());
    raw.push(kid.len() as u8);
    raw.extend_from_slice(kid.as_bytes());
    raw.extend_from_slice(&issued_at.to_be_bytes());
    raw.extend_from_slice(&tag);
    raw.extend_from_slice(&sealed);
    Ok(Base64UrlUnpadded::encode_string(&raw))
}

/// Why an `evaluation_id` is not valid. The submission API answers `invalid_evaluation` for each cause.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InvalidId {
    Malformed,
    UnknownKey,
    /// The HMAC does not match: another service or effect, or a forged value.
    Mismatch,
    /// The time of issue is in the future, or older than `evaluation_ttl`.
    Late,
}

/// The parts of an `evaluation_id`: the key id, the time of issue, the HMAC and the sealed facts.
struct Parts<'a> {
    kid: &'a str,
    issued_at: u64,
    tag: &'a [u8],
    sealed: &'a [u8],
}

fn split<'a>(raw: &'a [u8]) -> Result<Parts<'a>, InvalidId> {
    let (&kid_len, rest) = raw.split_first().ok_or(InvalidId::Malformed)?;
    let kid_len = kid_len as usize;
    if rest.len() <= kid_len + 8 + MAC_LEN {
        return Err(InvalidId::Malformed);
    }
    let (kid, rest) = rest.split_at(kid_len);
    let (issued, rest) = rest.split_at(8);
    let (tag, sealed) = rest.split_at(MAC_LEN);
    Ok(Parts {
        kid: std::str::from_utf8(kid).map_err(|_| InvalidId::Malformed)?,
        issued_at: u64::from_be_bytes(issued.try_into().map_err(|_| InvalidId::Malformed)?),
        tag,
        sealed,
    })
}

/// Checks the HMAC of an `evaluation_id` against the facts of a submission, and the time. The check is
/// constant-time on the HMAC.
fn check_mac(
    content: &ContentBox,
    now_ms: i64,
    ttl_s: u64,
    parts: &Parts<'_>,
    bound: &Bound<'_>,
) -> Result<(), InvalidId> {
    let key = content
        .evaluation_key_of(parts.kid)
        .ok_or(InvalidId::UnknownKey)?;
    mac_of(key, bound, parts.issued_at)
        .verify_slice(parts.tag)
        .map_err(|_| InvalidId::Mismatch)?;
    let now = (now_ms / 1000).max(0) as u64;
    if parts.issued_at > now + 1 || now > parts.issued_at.saturating_add(ttl_s) {
        return Err(InvalidId::Late);
    }
    Ok(())
}

/// Checks the HMAC and the time of an `evaluation_id` against all the facts of a question, without a read of the
/// sealed facts.
#[cfg(test)]
pub(super) fn check_evaluation_id(
    content: &ContentBox,
    now_ms: i64,
    ttl_s: u64,
    id: &str,
    bound: &Bound<'_>,
) -> Result<(), InvalidId> {
    let raw = Base64UrlUnpadded::decode_vec(id).map_err(|_| InvalidId::Malformed)?;
    check_mac(content, now_ms, ttl_s, &split(&raw)?, bound)
}

/// Opens an `evaluation_id` for a submission: the service is the one of the token and the effect is the one of the
/// body. The result holds the subject, the action and the resource that the PDP saw in the question. Every other
/// cause is [`InvalidId`].
pub(crate) fn open_evaluation_id(
    content: &ContentBox,
    now_ms: i64,
    ttl_s: u64,
    id: &str,
    service: &str,
    effect: &str,
) -> Result<Facts, InvalidId> {
    let raw = Base64UrlUnpadded::decode_vec(id).map_err(|_| InvalidId::Malformed)?;
    let parts = split(&raw)?;
    let plain = content
        .open(parts.sealed, &facts_aad(parts.tag))
        .ok_or(InvalidId::Mismatch)?;
    let [subject, action, resource]: [String; 3] =
        serde_json::from_slice(&plain).map_err(|_| InvalidId::Malformed)?;
    let bound = Bound {
        service,
        subject: &subject,
        action: &action,
        resource: &resource,
        effect,
    };
    check_mac(content, now_ms, ttl_s, &parts, &bound)?;
    Ok(Facts {
        subject,
        action,
        resource,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EFFECT: &str = "sha256:2e7b47b1726241459949b2d1f67bb47f20f15b0c4995a6e486ebd4ae964a5aab";

    #[test]
    fn the_effect_of_the_context_is_read_and_a_bad_one_is_refused() {
        let ctx = |v: Value| Some(json!({ "approval": v }));
        assert_eq!(read_effect(None), Ok(None));
        assert_eq!(read_effect(Some(&json!({}))), Ok(None));
        assert_eq!(
            read_effect(ctx(json!({"effect": EFFECT})).as_ref()),
            Ok(Some(EFFECT.to_string()))
        );
        for bad in [
            json!("sha256:00"),
            json!({}),
            json!({"effect": 1}),
            json!({"effect": "sha256:ABCDEF"}),
            json!({"effect": EFFECT.to_uppercase()}),
            json!({"effect": format!("{EFFECT}0")}),
            json!({"effect": EFFECT, "op": {}}),
            json!({"effect": EFFECT.replace("sha256:", "md5:")}),
        ] {
            assert!(read_effect(ctx(bad.clone()).as_ref()).is_err(), "{bad}");
        }
    }

    fn content(ids: &[(&str, u8)], active: &str) -> ContentBox {
        let keys: Vec<_> = ids
            .iter()
            .map(|(id, seed)| {
                let mut key = zeroize::Zeroizing::new([0u8; 32]);
                for (i, b) in key.iter_mut().enumerate() {
                    *b = seed.wrapping_add((i as u8).wrapping_mul(7));
                }
                (id.to_string(), key)
            })
            .collect();
        ContentBox::new(&keys, active).unwrap()
    }

    fn bound<'a>() -> Bound<'a> {
        Bound {
            service: "datastore",
            subject: "key:conf0001",
            action: "data.write",
            resource: "host:prod/database:analytics",
            effect: EFFECT,
        }
    }

    const T: i64 = 1_792_569_648_000;

    #[test]
    fn the_evaluation_id_is_stateless_valid_on_another_instance_for_the_ttl_and_binds_each_fact() {
        let first = content(&[("k1", 1)], "k1");
        let second = content(&[("k1", 1)], "k1");
        let id = evaluation_id(&first, T, &bound()).unwrap();
        assert!(
            !id.contains("conf0001") && !id.contains("analytics"),
            "no clear fact in the id"
        );
        // Another instance with the same keys accepts it, at the time of issue and up to the end of the time to live.
        for (at, ttl) in [(T, 300), (T + 299_000, 300), (T + 300_000, 300)] {
            assert_eq!(
                check_evaluation_id(&second, at, ttl, &id, &bound()),
                Ok(()),
                "{at}"
            );
        }
        assert_eq!(
            check_evaluation_id(&second, T + 301_000, 300, &id, &bound()),
            Err(InvalidId::Late)
        );
        assert_eq!(
            check_evaluation_id(&second, T - 5_000, 300, &id, &bound()),
            Err(InvalidId::Late),
            "an id from the future"
        );
        // Each fact is bound: the service, the subject, the action, the resource and the effect.
        let other = EFFECT.replace("2e7b", "2e7c");
        for changed in [
            Bound {
                service: "other",
                ..bound()
            },
            Bound {
                subject: "key:conf0002",
                ..bound()
            },
            Bound {
                action: "data.read",
                ..bound()
            },
            Bound {
                resource: "host:prod/database:staging",
                ..bound()
            },
            Bound {
                effect: &other,
                ..bound()
            },
        ] {
            assert_eq!(
                check_evaluation_id(&second, T, 300, &id, &changed),
                Err(InvalidId::Mismatch)
            );
        }
        // A key that the file does not hold, text that is not an id, and one changed bit.
        let stranger = content(&[("k9", 1)], "k9");
        assert_eq!(
            check_evaluation_id(&stranger, T, 300, &id, &bound()),
            Err(InvalidId::UnknownKey)
        );
        let forged = content(&[("k1", 2)], "k1");
        assert_eq!(
            check_evaluation_id(&forged, T, 300, &id, &bound()),
            Err(InvalidId::Mismatch),
            "the same id with another secret"
        );
        for bad in ["", "AA", "not base64 !"] {
            assert_eq!(
                check_evaluation_id(&second, T, 300, bad, &bound()),
                Err(InvalidId::Malformed),
                "{bad}"
            );
        }
        let mut raw = Base64UrlUnpadded::decode_vec(&id).unwrap();
        // One changed bit in the HMAC (after the key id, its length byte and the time of issue).
        let in_mac = 1 + "k1".len() + 8 + 3;
        raw[in_mac] ^= 1;
        assert_eq!(
            check_evaluation_id(
                &second,
                T,
                300,
                &Base64UrlUnpadded::encode_string(&raw),
                &bound()
            ),
            Err(InvalidId::Mismatch)
        );
    }

    #[test]
    fn an_evaluation_id_survives_a_rotation_of_the_content_key_while_the_old_key_is_in_the_file() {
        let old = content(&[("old", 1)], "old");
        let id = evaluation_id(&old, T, &bound()).unwrap();
        let rotated = content(&[("old", 1), ("new", 2)], "new");
        assert_eq!(check_evaluation_id(&rotated, T, 300, &id, &bound()), Ok(()));
        let fresh = evaluation_id(&rotated, T, &bound()).unwrap();
        assert_ne!(fresh, id, "the new key signs");
        let dropped = content(&[("new", 2)], "new");
        assert_eq!(
            check_evaluation_id(&dropped, T, 300, &id, &bound()),
            Err(InvalidId::UnknownKey)
        );
        assert_eq!(
            check_evaluation_id(&dropped, T, 300, &fresh, &bound()),
            Ok(())
        );
    }

    #[test]
    fn a_submission_opens_the_id_with_the_service_and_the_effect_and_gets_the_sealed_facts() {
        let cb = content(&[("k1", 1)], "k1");
        let id = evaluation_id(&cb, T, &bound()).unwrap();
        let b = bound();
        let opened = open_evaluation_id(&cb, T, 300, &id, b.service, b.effect).unwrap();
        assert_eq!(
            opened,
            Facts {
                subject: b.subject.into(),
                action: b.action.into(),
                resource: b.resource.into()
            }
        );
        // Another service or another effect: the HMAC does not match.
        assert_eq!(
            open_evaluation_id(&cb, T, 300, &id, "other", b.effect),
            Err(InvalidId::Mismatch)
        );
        let other = EFFECT.replace("2e7b", "2e7c");
        assert_eq!(
            open_evaluation_id(&cb, T, 300, &id, b.service, &other),
            Err(InvalidId::Mismatch)
        );
        assert_eq!(
            open_evaluation_id(&cb, T + 301_000, 300, &id, b.service, b.effect),
            Err(InvalidId::Late)
        );
        // A change of the sealed facts, or a cut of them, does not open.
        let mut raw = Base64UrlUnpadded::decode_vec(&id).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert_eq!(
            open_evaluation_id(
                &cb,
                T,
                300,
                &Base64UrlUnpadded::encode_string(&raw),
                b.service,
                b.effect
            ),
            Err(InvalidId::Mismatch)
        );
        assert_eq!(
            open_evaluation_id(&cb, T, 300, &id[..id.len() - 4], b.service, b.effect),
            Err(InvalidId::Mismatch)
        );
        for bad in ["", "AA", "not base64 !"] {
            assert_eq!(
                open_evaluation_id(&cb, T, 300, bad, b.service, b.effect),
                Err(InvalidId::Malformed),
                "{bad}"
            );
        }
        // A key that the file does not hold cannot open the facts.
        let stranger = content(&[("k9", 1)], "k9");
        assert_eq!(
            open_evaluation_id(&stranger, T, 300, &id, b.service, b.effect),
            Err(InvalidId::Mismatch)
        );
    }

    #[test]
    fn the_path_of_a_resource_has_no_service_and_encodes_the_ids() {
        let seg = |k: &str, i: &str| Segment::new(k, i).unwrap();
        assert_eq!(resource_path(&[]), "");
        assert_eq!(
            resource_path(&[seg("host", "prod"), seg("database", "a/b")]),
            "host:prod/database:a%2Fb"
        );
    }
}
