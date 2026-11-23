//! The agents of a key: the model, the rules that need no storage, and the errors.
//!
//! The portal knows a key. Several programs, or several conversations of one model, can share one key. The owner
//! wants to see them apart in the Inbox. The agent cannot name itself in a way that the portal can trust, so the
//! portal **issues** the identity (a token, `agt_...`) and the **owner** gives the name. The text that an agent sends
//! about itself is a hint, never a proof.
//!
//! # What the token is
//!
//! - It is not a credential. The key authenticates. A token with another key, or with another owner, is unknown.
//! - It is not a boundary between the agents of one key: they share the key, so they are one trust domain.
//! - It is an attribution capability: only the holder of the token can put an item in the group of that agent.
//!
//! The database keeps the SHA-256 digest of the token and never the token. The token appears once, in the result
//! of the call that made the agent.
//!
//! # Where the rules are
//!
//! This file holds the types and the pure rules: the token, the name, the text that an agent says, the address
//! range, the hints, and the binding of a call to the agent of an item. `agents_ops.rs` holds the operations that
//! run in one transaction (the resolution, the limits, the human actions). `agents_store.rs` is the API of the engine
//! for the channel, the phones and the web.

// The channel (G1b), the phones (G2) and the web (G3) call this API in the next lots.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use super::items_ops::Sealed;
use super::items_store::ItemError;
use super::util::hex;

/// The prefix of a token. It lets a scanner of secrets find a token in a log.
pub const TOKEN_PREFIX: &str = "agt_";
/// The characters after the prefix: the base64url text of 32 random bytes, with no padding.
pub const TOKEN_CHARS: usize = 43;
/// The longest name that the owner gives, in characters.
pub const NAME_MAX_CHARS: usize = 32;
/// The longest text that an agent says about itself, in characters.
pub const SAYS_MAX_CHARS: usize = 64;
/// The longest `client` hint, in characters.
pub const CLIENT_MAX_CHARS: usize = 64;
/// The longest `user_agent` hint, in characters.
pub const USER_AGENT_MAX_CHARS: usize = 128;
/// The longest `net` hint, in characters. An IPv6 range with a prefix of 48 bits is 43 characters long at most.
pub const NET_MAX_CHARS: usize = 43;
/// The most address ranges that the portal counts for one agent.
pub const NETS_MAX: usize = 8;
/// A call writes the time of its last call at most once in this time.
pub const SEEN_STEP_MS: i64 = 60_000;
/// The limit of the alerts about a flood: one for each key and each hour.
pub const FLOOD_ALERT_MS: i64 = 3_600_000;

// ---------------------------------------------------------------- the types

/// The state of an agent. The owner changes it; the portal never does, except `new` at the first call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    /// Made by a call. The owner got a notice. With `lock_new`, the owner cannot approve its items yet.
    New,
    /// The owner allowed it, or the owner made it.
    Known,
    /// The owner blocked it: its calls get `agent_blocked`.
    Blocked,
    /// The owner merged it into another agent. Its token still works and gives the other agent.
    Merged,
}

impl AgentState {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentState::New => "new",
            AgentState::Known => "known",
            AgentState::Blocked => "blocked",
            AgentState::Merged => "merged",
        }
    }

    pub fn parse(text: &str) -> Option<AgentState> {
        Some(match text {
            "new" => AgentState::New,
            "known" => AgentState::Known,
            "blocked" => AgentState::Blocked,
            "merged" => AgentState::Merged,
            _ => return None,
        })
    }
}

/// An agent, or the bucket of a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    Agent,
    /// The pseudo agent "Unattributed" of a key: the items that no agent claims. It has no token, and the owner
    /// cannot name it, merge it, block it or lock it.
    Bucket,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Agent => "agent",
            AgentKind::Bucket => "bucket",
        }
    }

    pub fn parse(text: &str) -> Option<AgentKind> {
        match text {
            "agent" => Some(AgentKind::Agent),
            "bucket" => Some(AgentKind::Bucket),
            _ => None,
        }
    }
}

/// How the row came to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentOrigin {
    /// An item-making call without a token made it.
    Call,
    /// The owner made it (the mode `enrolled`).
    Owner,
    /// The bucket of a key.
    Key,
}

impl AgentOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentOrigin::Call => "call",
            AgentOrigin::Owner => "owner",
            AgentOrigin::Key => "key",
        }
    }

    pub fn parse(text: &str) -> Option<AgentOrigin> {
        Some(match text {
            "call" => AgentOrigin::Call,
            "owner" => AgentOrigin::Owner,
            "key" => AgentOrigin::Key,
            _ => return None,
        })
    }
}

/// What a key does with the agents. It is an attribute of the key, not of the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentMode {
    /// One agent for the key: its bucket. No token, no notice. This is the behavior of a portal before the agents.
    #[default]
    Key,
    /// An item-making call without a token makes an agent.
    Discover,
    /// The owner makes the agents. A call without a valid token is refused.
    Enrolled,
}

impl AgentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentMode::Key => "key",
            AgentMode::Discover => "discover",
            AgentMode::Enrolled => "enrolled",
        }
    }

    pub fn parse(text: &str) -> Option<AgentMode> {
        Some(match text {
            "key" => AgentMode::Key,
            "discover" => AgentMode::Discover,
            "enrolled" => AgentMode::Enrolled,
            _ => return None,
        })
    }
}

/// A row of `agents`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    /// 32 hex characters. It is public (the frames carry it) and it is never the token.
    pub agent_id: String,
    pub key_id: String,
    pub owner: String,
    pub kind: AgentKind,
    pub state: AgentState,
    pub origin: AgentOrigin,
    /// The number in "<key name> #<ordinal>". Per key, never given twice. For a bucket: the last number given.
    pub ordinal: i64,
    /// Set by the owner only.
    pub name: Option<String>,
    /// The folded form of `name` (see [`fold`]). Unique inside the key.
    pub name_key: Option<String>,
    /// The SHA-256 digest of the token, in hex. `None` for a bucket.
    pub token_digest: Option<String>,
    /// With the state `merged`: the agent that the items went to.
    pub merged_into: Option<String>,
    /// Grows at each change that a device must see: a human action and a change of the hints.
    pub rev: i64,
    pub created_at: i64,
    pub named_at: Option<i64>,
    pub last_seen_at: i64,
    /// The sealed JSON of [`Observed`].
    pub hints: Option<Sealed>,
}

impl AgentRow {
    /// The text of the portal for this agent: the name of the owner, else "<key name> #<ordinal>". For the bucket:
    /// the key name in the mode `key`, else "Unattributed · <key name>". It is portal text, never agent text.
    pub fn label(&self, key_name: &str, mode: AgentMode) -> String {
        match (self.kind, &self.name) {
            (AgentKind::Bucket, _) if mode == AgentMode::Key => key_name.to_string(),
            (AgentKind::Bucket, _) => format!("Unattributed · {key_name}"),
            (AgentKind::Agent, Some(name)) => name.clone(),
            (AgentKind::Agent, None) => format!("{key_name} #{}", self.ordinal),
        }
    }

    pub fn is_bucket(&self) -> bool {
        self.kind == AgentKind::Bucket
    }
}

/// An agent with the number of its open items, for the lists of the screens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSummary {
    pub agent: AgentRow,
    pub open: i64,
    /// No open item, and no call for `inbox.agents.idle_after`: the active views leave it out.
    pub idle: bool,
}

/// A row of `agent_events`. It holds ids and numbers, never a text that an agent sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEventRow {
    pub at: i64,
    pub event: String,
    /// A login, `agent` or `server`.
    pub actor: String,
    pub detail: Option<String>,
}

/// The limits and the lock of the agents, as the rules of the engine read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentRules {
    /// False without the block `inbox.agents`: every key behaves as in the mode `key`.
    pub enabled: bool,
    pub max_per_key: u32,
    pub new_per_hour: u32,
    pub lock_new: bool,
    pub idle_after_ms: i64,
    pub purge_after_ms: i64,
}

impl AgentRules {
    /// The rules of a portal without the block `inbox.agents`.
    pub fn off() -> AgentRules {
        AgentRules {
            enabled: false,
            max_per_key: 0,
            new_per_hour: 0,
            lock_new: false,
            idle_after_ms: 0,
            purge_after_ms: 0,
        }
    }

    pub fn from_config(c: &crate::config::AgentsConfig) -> AgentRules {
        AgentRules {
            enabled: true,
            max_per_key: c.max_per_key,
            new_per_hour: c.new_per_hour,
            lock_new: c.lock_new,
            idle_after_ms: c.idle_after.as_millis() as i64,
            purge_after_ms: c.purge_after.as_millis() as i64,
        }
    }

    /// The mode that applies: without the block, each key is one agent.
    pub fn effective(&self, mode: AgentMode) -> AgentMode {
        if self.enabled {
            mode
        } else {
            AgentMode::Key
        }
    }
}

/// What the call tells the engine about the agent. The channel fills it from the mode of the key, the token
/// and what it saw on the connection. `Default` is a call of a key in the mode `key`.
#[derive(Clone, Default)]
pub struct AgentCall {
    pub mode: AgentMode,
    /// The token of the call, from the header or from the argument.
    pub token: Option<String>,
    pub hints: Hints,
}

impl std::fmt::Debug for AgentCall {
    /// The token never goes to a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCall")
            .field("mode", &self.mode)
            .field("token", &self.token.as_ref().map(|_| "<hidden>"))
            .field("hints", &self.hints)
            .finish()
    }
}

/// The result of the resolution: the agent that the call belongs to.
#[derive(Clone, PartialEq, Eq)]
pub struct Resolved {
    pub agent_id: String,
    pub kind: AgentKind,
    pub state: AgentState,
    /// What `approval_intent` reports as `agent_status`.
    pub status: AgentStatus,
    /// Only when this call made the agent. The portal keeps no copy: this value is the only one.
    pub token: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved")
            .field("agent_id", &self.agent_id)
            .field("kind", &self.kind)
            .field("state", &self.state)
            .field("status", &self.status)
            .field("token", &self.token.as_ref().map(|_| "<hidden>"))
            .finish()
    }
}

/// The member `agent_status` of the result of `approval_intent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    /// The agent is `new`: the call made it now, or the owner did not allow it yet.
    New,
    /// The token is known and the owner allowed the agent.
    Known,
    /// A limit of the key sent the call to the bucket. No token was made.
    Unattributed,
    /// The key is in the mode `key`. There is nothing to keep.
    Key,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::New => "new",
            AgentStatus::Known => "known",
            AgentStatus::Unattributed => "unattributed",
            AgentStatus::Key => "key",
        }
    }
}

// ---------------------------------------------------------------- the errors

/// Why a call cannot get an agent, or cannot touch the item of an agent. Each case has a code of the channel.
#[derive(Debug)]
pub enum AgentError {
    /// The token is unknown, was erased, is of another key or of another owner, or has a bad form. One answer for
    /// each case, so the answer tells nothing about the tokens that exist.
    Unknown,
    /// The owner blocked the agent.
    Blocked,
    /// The mode is `enrolled` and the call has no token, or a write on the item of an agent has no token.
    TokenRequired,
    /// The token is not the agent of the item, or a `previous_id` belongs to another agent.
    Mismatch,
    Storage(ItemError),
}

impl AgentError {
    /// The code of the error of the channel (`agent_unknown` and the others).
    pub fn code(&self) -> &'static str {
        match self {
            AgentError::Unknown => "agent_unknown",
            AgentError::Blocked => "agent_blocked",
            AgentError::TokenRequired => "agent_token_required",
            AgentError::Mismatch => "agent_mismatch",
            AgentError::Storage(_) => "unavailable",
        }
    }

    /// The HTTP status of the code.
    pub fn status(&self) -> u16 {
        match self {
            AgentError::Unknown | AgentError::TokenRequired => 422,
            AgentError::Blocked | AgentError::Mismatch => 403,
            AgentError::Storage(_) => 503,
        }
    }

    /// The label of `portal_agent_refused_total`. `None` for a case that is not a refusal of a call.
    pub fn metric(&self) -> Option<&'static str> {
        match self {
            AgentError::Unknown => Some("unknown"),
            AgentError::Blocked => Some("blocked"),
            AgentError::Mismatch => Some("mismatch"),
            AgentError::TokenRequired | AgentError::Storage(_) => None,
        }
    }
}

impl From<ItemError> for AgentError {
    fn from(e: ItemError) -> Self {
        AgentError::Storage(e)
    }
}

// ---------------------------------------------------------------- the token

/// A new token: `agt_` and the base64url text of 32 random bytes. It is 47 characters long.
pub fn new_token() -> Zeroizing<String> {
    use base64ct::{Base64UrlUnpadded, Encoding};
    let bytes = Zeroizing::new(inbox_core::rng::Rng::os().bytes::<32>());
    Zeroizing::new(format!(
        "{TOKEN_PREFIX}{}",
        Base64UrlUnpadded::encode_string(&*bytes)
    ))
}

/// The digest that the database keeps: SHA-256 of the token, in hex.
pub fn token_digest(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// Whether a text has the form of a token. A text of another form is unknown at once, with no read of the database.
pub fn token_has_form(token: &str) -> bool {
    token.strip_prefix(TOKEN_PREFIX).is_some_and(|rest| {
        rest.len() == TOKEN_CHARS
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// The full tokens that a text could hold, as candidates for a check against the stored digests.
///
/// A run is a series of characters of the token alphabet (`A-Z a-z 0-9 - _`). Each window of 43 characters in a run is
/// a candidate: its token is `agt_` and the window. This covers a token with its prefix and a bare token, also when
/// other characters of the alphabet touch it. A text with no run of 43 characters has no candidate. The portal compares
/// the digests of the candidates with the digests of its agents: a candidate that is not a token matches nothing. The
/// result has no duplicate.
pub fn token_candidates(text: &str) -> Vec<String> {
    let alphabet = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let bytes = text.as_bytes();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if !alphabet(bytes[start]) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && alphabet(bytes[end]) {
            end += 1;
        }
        // The run is ASCII, so each index is a boundary of a character.
        let run = &text[start..end];
        if run.len() >= TOKEN_CHARS {
            for at in 0..=(run.len() - TOKEN_CHARS) {
                let candidate = format!("{TOKEN_PREFIX}{}", &run[at..at + TOKEN_CHARS]);
                if seen.insert(candidate.clone()) {
                    out.push(candidate);
                }
            }
        }
        start = end;
    }
    out
}

// ---------------------------------------------------------------- names and texts

/// Why the portal refuses a name or a text. It never cleans the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextError {
    /// Empty, or longer than the limit.
    Length,
    /// A space or another white space at the start or at the end.
    Untrimmed,
    /// A control character, such as a tab or a line end.
    Control,
    /// A format character: a bidirectional control, a zero-width character and the others of the class Cf.
    Format,
    /// The line separator U+2028 or the paragraph separator U+2029.
    LineSeparator,
}

impl TextError {
    pub fn as_str(self) -> &'static str {
        match self {
            TextError::Length => "length",
            TextError::Untrimmed => "untrimmed",
            TextError::Control => "control",
            TextError::Format => "format",
            TextError::LineSeparator => "line_separator",
        }
    }
}

// The format characters (the class Cf of Unicode) are the same for the checks of the portal and for the frames of the
// phones: the core crate holds the one list, so a name that the portal accepts is a name that a frame can carry.
pub use inbox_core::proto::is_format_char;

/// Checks a line of text of 1 to `max` characters: no control, no format character, no line separator, no white space
/// at the ends. The text is never changed: a bad text is refused.
pub fn check_line(text: &str, max: usize) -> Result<(), TextError> {
    let count = text.chars().count();
    if count == 0 || count > max {
        return Err(TextError::Length);
    }
    for c in text.chars() {
        if c.is_control() {
            return Err(TextError::Control);
        }
        if is_format_char(c) {
            return Err(TextError::Format);
        }
        if c == '\u{2028}' || c == '\u{2029}' {
            return Err(TextError::LineSeparator);
        }
    }
    if text.trim() != text {
        return Err(TextError::Untrimmed);
    }
    Ok(())
}

/// Checks the name that the owner gives: 1 to 32 characters.
pub fn check_name(name: &str) -> Result<(), TextError> {
    check_line(name, NAME_MAX_CHARS)
}

/// Checks the text that an agent says about itself (`agent_label`): 1 to 64 characters.
pub fn check_says(text: &str) -> Result<(), TextError> {
    check_line(text, SAYS_MAX_CHARS)
}

/// The folded form of a text: NFKC, then lower case, then each run of white space as one space. Two names that look
/// the same fold to one value. The name key and the check of the text of an agent use it.
pub fn fold(text: &str) -> String {
    let normal: String = text.nfkc().collect();
    normal
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether what an agent says folds to the name of a known agent. The screens warn the owner: the text can be a copy.
pub fn says_equals_name(says: &str, name_key: &str) -> bool {
    !name_key.is_empty() && fold(says) == name_key
}

// ---------------------------------------------------------------- the hints

/// What a call shows about the agent. All values are text that a client sent. They are a help for the owner, never
/// a proof: anyone who holds the key can send the same text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    /// `agent_label`. The channel checks it with [`check_says`] before it gets here.
    pub says: Option<String>,
    /// `clientInfo` of the call if it has one, else the `User-Agent`.
    pub client: Option<String>,
    pub user_agent: Option<String>,
    /// The address of the client, cut to /24 (IPv4) or /48 (IPv6). See [`net_of`].
    pub net: Option<String>,
}

impl Hints {
    pub fn is_empty(&self) -> bool {
        self.says.is_none()
            && self.client.is_none()
            && self.user_agent.is_none()
            && self.net.is_none()
    }
}

/// What the portal remembers of the hints of one agent. It is sealed in the row. It holds client text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observed {
    /// The last text that the agent said about itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub says: Option<String>,
    /// The first text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub says_first: Option<String>,
    /// How many times the text changed.
    #[serde(default)]
    pub says_changes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// The distinct address ranges seen with this token, at most [`NETS_MAX`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nets: Vec<String>,
}

impl Observed {
    /// The number of distinct ranges seen with this token, from 0 to 8.
    pub fn nets_seen(&self) -> u8 {
        self.nets.len().min(NETS_MAX) as u8
    }
}

/// Cuts a hint to `max` characters at a boundary of a character. The hints come from headers that the portal does
/// not control, so it bounds them. It never cuts a name or a text that a person wrote.
fn bound(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Updates the observed record with what a call showed. The result is the new record and the events to write: `says_changed`
/// when the text of the agent changed, `net_changed` when a new address range shows up after the first.
pub fn merge_hints(old: Option<&Observed>, seen: &Hints) -> (Observed, Vec<&'static str>) {
    let mut now = old.cloned().unwrap_or_default();
    let mut events = Vec::new();
    // A text with a hidden character is not kept: the screens escape it, but the portal does not store what it refuses.
    if let Some(says) = seen.says.as_deref().filter(|s| check_says(s).is_ok()) {
        match &now.says {
            None => {
                now.says = Some(says.to_string());
                now.says_first = Some(says.to_string());
            }
            Some(before) if before != says => {
                now.says_changes = now.says_changes.saturating_add(1);
                now.says = Some(says.to_string());
                events.push("says_changed");
            }
            Some(_) => {}
        }
    }
    if let Some(client) = &seen.client {
        now.client = Some(bound(client, CLIENT_MAX_CHARS));
    }
    if let Some(agent) = &seen.user_agent {
        now.user_agent = Some(bound(agent, USER_AGENT_MAX_CHARS));
    }
    if let Some(net) = seen.net.as_deref().map(|n| bound(n, NET_MAX_CHARS)) {
        if !now.nets.contains(&net) {
            if !now.nets.is_empty() {
                events.push("net_changed");
            }
            if now.nets.len() < NETS_MAX {
                now.nets.push(net.clone());
            }
        }
        now.net = Some(net);
    }
    (now, events)
}

/// The address range of a client: the first 24 bits of an IPv4 address and the first 48 bits of an IPv6 address, in
/// the form of a network (`203.0.113.0/24`). The portal never keeps the whole address in the hints.
pub fn net_of(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        std::net::IpAddr::V6(v6) => {
            // A mapped IPv4 address is an IPv4 client.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return net_of(std::net::IpAddr::V4(v4));
            }
            let s = v6.segments();
            std::net::Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0).to_string() + "/48"
        }
    }
}

// ---------------------------------------------------------------- the hint "similar"

/// Why a new agent looks like a known agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimilarWhy {
    /// The two say the same text, after the fold of [`fold`].
    Says,
    /// The two have the same client and the same address range.
    ClientAndNet,
}

/// The known agent of the same key that a `new` agent looks like. The rule is a hint for the owner
/// and nothing else: the hints are text that a client sent, and anyone who holds the key can copy them. The portal never
/// merges by itself.
///
/// An agent is a candidate when it has the same key and the same owner, it is not the agent itself, it is an agent and not
/// a bucket, and its state is `known`. It matches when it says the same text as `agent` (after [`fold`]), or when it has
/// the same client and the same address range. A text match wins over a client match. Among equal matches, the agent that
/// called last wins, then the lower id, so the answer does not depend on the order of `others`. A `new` agent only gets a
/// hint: for each other state the result is `None`.
pub fn find_similar<'a>(
    agent: &AgentRow,
    mine: Option<&Observed>,
    others: impl IntoIterator<Item = (&'a AgentRow, Option<Observed>)>,
) -> Option<(&'a AgentRow, SimilarWhy)> {
    if agent.kind != AgentKind::Agent || agent.state != AgentState::New {
        return None;
    }
    let mine = mine?;
    let says = mine.says.as_deref().map(fold).filter(|s| !s.is_empty());
    let same = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
    let mut best: Option<(&AgentRow, SimilarWhy)> = None;
    for (other, theirs) in others {
        if other.agent_id == agent.agent_id
            || other.key_id != agent.key_id
            || other.owner != agent.owner
            || other.kind != AgentKind::Agent
            || other.state != AgentState::Known
        {
            continue;
        }
        let Some(theirs) = theirs else { continue };
        let why = if says.is_some() && theirs.says.as_deref().map(fold) == says {
            SimilarWhy::Says
        } else if same(&mine.client, &theirs.client) && same(&mine.net, &theirs.net) {
            SimilarWhy::ClientAndNet
        } else {
            continue;
        };
        let rank = |w: SimilarWhy| u8::from(w == SimilarWhy::ClientAndNet);
        let better = match &best {
            None => true,
            Some((b, bw)) => {
                (
                    rank(why),
                    std::cmp::Reverse(other.last_seen_at),
                    &other.agent_id,
                ) < (rank(*bw), std::cmp::Reverse(b.last_seen_at), &b.agent_id)
            }
        };
        if better {
            best = Some((other, why));
        }
    }
    best
}

// ---------------------------------------------------------------- the binding of a call to an item

/// The rule of section 4.4 for a call that names an item: a read goes by the key, a write goes by the agent.
///
/// - A **read** (`get_answer`, `keepalive`) needs the key only. A token that is present must still be the agent of the
///   item. A call with no token is allowed: a lost token must not stop a read.
/// - A **write** (`say`, `cancel`, `supersede`) on the item of a real agent needs the token, and it must be the agent of
///   the item. An item of the bucket needs the key only.
///
/// `resolved` is the id of the agent that the token gave, or `None` for a call with no token.
pub fn check_binding(
    item_agent_id: &str,
    item_agent_is_bucket: bool,
    resolved: Option<&str>,
    write: bool,
) -> Result<(), AgentError> {
    match (resolved, write) {
        (None, false) => Ok(()),
        (Some(id), false) if id == item_agent_id => Ok(()),
        (Some(_), false) => Err(AgentError::Mismatch),
        (_, true) if item_agent_is_bucket => Ok(()),
        (None, true) => Err(AgentError::TokenRequired),
        (Some(id), true) if id == item_agent_id => Ok(()),
        (Some(_), true) => Err(AgentError::Mismatch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_has_prefix_and_43_chars_and_only_digest_is_stored() {
        let token = new_token();
        assert!(token.starts_with("agt_"), "{}", *token);
        assert_eq!(token.len(), 4 + 43);
        assert!(token_has_form(&token));
        // Two tokens differ.
        assert_ne!(*token, *new_token());
        // The digest is 64 hex characters and it does not hold the token.
        let digest = token_digest(&token);
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!digest.contains(&token[4..]));
        assert_eq!(digest, token_digest(&token));
        // The debug text of a call and of a result never holds the token.
        let call = AgentCall {
            token: Some(token.to_string()),
            ..Default::default()
        };
        assert!(!format!("{call:?}").contains(&token[4..]));
        let resolved = Resolved {
            agent_id: "a".into(),
            kind: AgentKind::Agent,
            state: AgentState::New,
            status: AgentStatus::New,
            token: Some(token.clone()),
        };
        assert!(!format!("{resolved:?}").contains(&token[4..]));
    }

    #[test]
    fn the_candidates_of_a_text_are_the_windows_of_43_characters_of_the_alphabet() {
        let token = new_token();
        let bare = &token[4..];
        // A token with its prefix, a bare token and a token that other characters of the alphabet touch.
        for text in [
            format!("use {}", *token),
            format!("bare {bare}."),
            format!("x{bare}y"),
            format!("a-{}", *token),
            format!("{}\n{}", "words", *token),
        ] {
            assert!(token_candidates(&text).contains(&*token), "{text}");
        }
        // No run of 43 characters: no candidate and no work.
        assert!(token_candidates("").is_empty());
        assert!(token_candidates("Run the migration on the staging table.").is_empty());
        assert!(token_candidates(&format!("{} {}", &bare[..42], &bare[..42])).is_empty());
        // A space or a character outside the alphabet cuts the run.
        assert!(token_candidates(&format!("{} {}", &bare[..30], &bare[30..])).is_empty());
        // Each window is a candidate once. A run of 50 characters has 8 windows.
        let run = "a".repeat(50);
        assert_eq!(token_candidates(&run).len(), 1, "equal windows count once");
        let run: String = (0..50u8).map(|n| char::from(b'A' + n % 26)).collect();
        assert_eq!(token_candidates(&run).len(), 8);
        // A text with a non-ASCII character never breaks a boundary.
        assert!(token_candidates("é".repeat(100).as_str()).is_empty());
        assert!(token_candidates(&format!("é{bare}é")).contains(&*token));
    }

    fn row(id: &str, state: AgentState, seen: i64) -> AgentRow {
        AgentRow {
            agent_id: id.into(),
            key_id: "k".into(),
            owner: "alice".into(),
            kind: AgentKind::Agent,
            state,
            origin: AgentOrigin::Call,
            ordinal: 1,
            name: None,
            name_key: None,
            token_digest: Some(id.into()),
            merged_into: None,
            rev: 1,
            created_at: 0,
            named_at: None,
            last_seen_at: seen,
            hints: None,
        }
    }

    fn seen(says: Option<&str>, client: Option<&str>, net: Option<&str>) -> Observed {
        Observed {
            says: says.map(str::to_string),
            client: client.map(str::to_string),
            net: net.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn similar_names_a_known_agent_with_the_same_says_or_the_same_client_and_net() {
        let new = row("n", AgentState::New, 0);
        let known = row("a", AgentState::Known, 10);
        let by = |mine: &Observed, others: &[(&AgentRow, Observed)]| {
            find_similar(
                &new,
                Some(mine),
                others.iter().map(|(a, o)| (*a, Some(o.clone()))),
            )
            .map(|(a, why)| (a.agent_id.clone(), why))
        };
        // The same text, after the fold (NFKC, lower case, one space).
        let mine = seen(Some("Billing  Bot"), None, None);
        let theirs = seen(Some("ｂｉｌｌｉｎｇ bot"), None, None);
        assert_eq!(
            by(&mine, &[(&known, theirs)]),
            Some(("a".into(), SimilarWhy::Says))
        );
        // The same client and the same range. The client alone, or the range alone, is not enough.
        let mine = seen(None, Some("claude-code"), Some("203.0.113.0/24"));
        assert_eq!(
            by(&mine, &[(&known, mine.clone())]),
            Some(("a".into(), SimilarWhy::ClientAndNet))
        );
        for other in [
            seen(None, Some("claude-code"), Some("198.51.100.0/24")),
            seen(None, Some("curl"), Some("203.0.113.0/24")),
            seen(None, None, None),
        ] {
            assert_eq!(by(&mine, &[(&known, other)]), None);
        }
        // A text that is not the same, and no text at all, never match.
        let mine = seen(Some("billing-bot"), None, None);
        assert_eq!(
            by(&mine, &[(&known, seen(Some("billing bot"), None, None))]),
            None
        );
        assert_eq!(
            by(&seen(None, None, None), &[(&known, seen(None, None, None))]),
            None
        );
        // A text match wins over a client match. Then the agent that called last wins, then the lower id.
        let text = seen(Some("x"), Some("c"), Some("n"));
        let by_client = row("b", AgentState::Known, 99);
        let by_text = row("c", AgentState::Known, 1);
        let only_client = seen(None, Some("c"), Some("n"));
        assert_eq!(
            by(
                &text,
                &[(&by_client, only_client.clone()), (&by_text, text.clone())]
            ),
            Some(("c".into(), SimilarWhy::Says))
        );
        let late = row("z", AgentState::Known, 50);
        let early = row("y", AgentState::Known, 5);
        for order in [
            [(&late, &text), (&early, &text)],
            [(&early, &text), (&late, &text)],
        ] {
            let others: Vec<_> = order.iter().map(|(a, o)| (*a, (*o).clone())).collect();
            assert_eq!(by(&text, &others).unwrap().0, "z");
        }
        let tie_a = row("e", AgentState::Known, 5);
        let tie_b = row("d", AgentState::Known, 5);
        assert_eq!(
            by(&text, &[(&tie_a, text.clone()), (&tie_b, text.clone())])
                .unwrap()
                .0,
            "d"
        );
        // Only a known agent of the same key and owner, and never the agent itself.
        let mut other_key = row("k2", AgentState::Known, 1);
        other_key.key_id = "other".into();
        let mut other_owner = row("o2", AgentState::Known, 1);
        other_owner.owner = "erin".into();
        let blocked = row("b2", AgentState::Blocked, 1);
        let merged = row("m2", AgentState::Merged, 1);
        let fresh = row("f2", AgentState::New, 1);
        let mut bucket = row("u2", AgentState::Known, 1);
        bucket.kind = AgentKind::Bucket;
        let itself = row("n", AgentState::Known, 1);
        for r in [
            &other_key,
            &other_owner,
            &blocked,
            &merged,
            &fresh,
            &bucket,
            &itself,
        ] {
            assert_eq!(by(&text, &[(r, text.clone())]), None, "{}", r.agent_id);
        }
        // Only a `new` agent gets a hint.
        let known_one = row("n", AgentState::Known, 0);
        assert!(find_similar(&known_one, Some(&text), [(&known, Some(text.clone()))]).is_none());
        let mut bucket_one = row("n", AgentState::New, 0);
        bucket_one.kind = AgentKind::Bucket;
        assert!(find_similar(&bucket_one, Some(&text), [(&known, Some(text.clone()))]).is_none());
        // An agent with no hints gets none, and another agent with no hints is no candidate.
        assert!(find_similar(&new, None, [(&known, Some(text.clone()))]).is_none());
        assert!(find_similar(&new, Some(&text), [(&known, None)]).is_none());
    }

    #[test]
    fn a_text_of_another_form_is_not_a_token() {
        for bad in [
            "",
            "agt_",
            "agt_short",
            "AGT_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "mcp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "agt_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "agt_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa=",
            "agt_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ",
        ] {
            assert!(!token_has_form(bad), "{bad:?}");
        }
        assert!(token_has_form(
            "agt_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }

    #[test]
    fn name_rules_refuse_control_bidi_zero_width_and_never_clean() {
        assert_eq!(check_name("Billing bot"), Ok(()));
        assert_eq!(check_name("Rapport du soir"), Ok(()));
        assert_eq!(check_name(&"a".repeat(32)), Ok(()));
        for (name, why) in [
            ("", TextError::Length),
            (&"a".repeat(33)[..], TextError::Length),
            ("bot\n", TextError::Control),
            ("a\tb", TextError::Control),
            ("a\u{0007}b", TextError::Control),
            ("a\u{202E}b", TextError::Format),
            ("a\u{2066}b", TextError::Format),
            ("a\u{200B}b", TextError::Format),
            ("a\u{200D}b", TextError::Format),
            ("a\u{FEFF}b", TextError::Format),
            ("a\u{00AD}b", TextError::Format),
            ("a\u{2028}b", TextError::LineSeparator),
            ("a\u{2029}b", TextError::LineSeparator),
            (" bot", TextError::Untrimmed),
            ("bot ", TextError::Untrimmed),
            ("bot\u{00A0}", TextError::Untrimmed),
        ] {
            assert_eq!(check_name(name), Err(why), "{name:?}");
        }
        // 32 characters, not 32 bytes.
        assert_eq!(check_name(&"é".repeat(32)), Ok(()));
        assert_eq!(check_name(&"é".repeat(33)), Err(TextError::Length));
    }

    #[test]
    fn says_with_hidden_characters_is_refused_and_equal_to_a_name_is_flagged() {
        assert_eq!(check_says("billing-bot"), Ok(()));
        assert_eq!(
            check_says("billing\u{202E}tob-gnillib"),
            Err(TextError::Format)
        );
        assert_eq!(check_says("a\u{200B}"), Err(TextError::Format));
        assert_eq!(check_says(&"x".repeat(65)), Err(TextError::Length));
        assert_eq!(check_says("two\nlines"), Err(TextError::Control));
        // The text of the agent that folds to the name of a known agent is flagged.
        let known = fold("Billing Bot");
        assert!(says_equals_name("billing bot", &known));
        assert!(says_equals_name("BILLING  BOT", &known));
        // A full-width text and a ligature fold to the same value (NFKC).
        assert!(says_equals_name("ｂｉｌｌｉｎｇ ｂｏｔ", &known));
        assert!(!says_equals_name("billing-bot", &known));
        assert!(!says_equals_name("anything", ""));
        // A merge of the hints drops a text that the check refuses.
        let (kept, _) = merge_hints(
            None,
            &Hints {
                says: Some("a\u{202E}b".into()),
                ..Default::default()
            },
        );
        assert_eq!(kept.says, None);
    }

    #[test]
    fn fold_is_nfkc_then_lower_case_then_one_space() {
        assert_eq!(fold("Night  ETL"), "night etl");
        assert_eq!(fold("ＮＩＧＨＴ ＥＴＬ"), "night etl");
        assert_eq!(fold("ﬁle"), "file");
        assert_eq!(fold("Ä"), fold("a\u{0308}"));
        assert_ne!(fold("Billing bot"), fold("Billing-bot"));
    }

    #[test]
    fn the_hints_merge_keeps_first_and_last_and_counts_the_changes() {
        let a = Hints {
            says: Some("billing-bot".into()),
            client: Some("claude-code/2.1".into()),
            user_agent: Some("node".into()),
            net: Some("203.0.113.0/24".into()),
        };
        let (one, events) = merge_hints(None, &a);
        assert!(events.is_empty(), "the first values raise no event");
        assert_eq!(one.says_first.as_deref(), Some("billing-bot"));
        assert_eq!(one.nets_seen(), 1);
        // The same hints again change nothing.
        let (same, events) = merge_hints(Some(&one), &a);
        assert_eq!((&same, events.is_empty()), (&one, true));
        // Another text and another range raise one event each.
        let b = Hints {
            says: Some("night-etl".into()),
            net: Some("198.51.100.0/24".into()),
            ..Default::default()
        };
        let (two, events) = merge_hints(Some(&one), &b);
        assert_eq!(events, ["says_changed", "net_changed"]);
        assert_eq!(two.says.as_deref(), Some("night-etl"));
        assert_eq!(two.says_first.as_deref(), Some("billing-bot"));
        assert_eq!(two.says_changes, 1);
        assert_eq!(two.nets_seen(), 2);
        assert_eq!(two.client.as_deref(), Some("claude-code/2.1"));
        // A range that was seen before is not a new range.
        let (back, events) = merge_hints(Some(&two), &a);
        assert_eq!(events, ["says_changed"]);
        assert_eq!(back.nets_seen(), 2);
        // The count stops at 8, and the values are bounded.
        let mut o = back;
        for n in 0..20 {
            let h = Hints {
                net: Some(format!("10.0.{n}.0/24")),
                user_agent: Some("u".repeat(500)),
                ..Default::default()
            };
            o = merge_hints(Some(&o), &h).0;
        }
        assert_eq!(o.nets_seen(), 8);
        assert_eq!(
            o.user_agent.as_deref().map(|u| u.chars().count()),
            Some(128)
        );
        // The record keeps its form through JSON, and the form is closed.
        let json = serde_json::to_vec(&o).unwrap();
        assert_eq!(serde_json::from_slice::<Observed>(&json).unwrap(), o);
        assert!(serde_json::from_str::<Observed>(r#"{"other":1}"#).is_err());
    }

    #[test]
    fn the_address_range_is_cut_to_24_bits_or_48_bits() {
        let net = |t: &str| net_of(t.parse().unwrap());
        assert_eq!(net("203.0.113.77"), "203.0.113.0/24");
        assert_eq!(net("2001:db8:abcd:1234:5678::1"), "2001:db8:abcd::/48");
        assert_eq!(net("::ffff:203.0.113.77"), "203.0.113.0/24");
        assert!(net("2001:db8:abcd:1234:5678::1").len() <= NET_MAX_CHARS);
    }

    #[test]
    fn binding_a_read_goes_by_the_key_and_a_write_goes_by_the_agent() {
        // Reads: no token is allowed, a token must match.
        assert!(check_binding("a", false, None, false).is_ok());
        assert!(check_binding("a", false, Some("a"), false).is_ok());
        assert!(matches!(
            check_binding("a", false, Some("b"), false),
            Err(AgentError::Mismatch)
        ));
        assert!(check_binding("bucket", true, None, false).is_ok());
        assert!(matches!(
            check_binding("bucket", true, Some("b"), false),
            Err(AgentError::Mismatch)
        ));
        // Writes on the item of a real agent: the token is required and must match.
        assert!(matches!(
            check_binding("a", false, None, true),
            Err(AgentError::TokenRequired)
        ));
        assert!(check_binding("a", false, Some("a"), true).is_ok());
        assert!(matches!(
            check_binding("a", false, Some("b"), true),
            Err(AgentError::Mismatch)
        ));
        // Writes on an item of the bucket: the key only.
        assert!(check_binding("bucket", true, None, true).is_ok());
        assert!(check_binding("bucket", true, Some("b"), true).is_ok());
    }

    #[test]
    fn the_codes_and_the_statuses_of_the_errors_are_the_table_of_the_channel() {
        let table = [
            (AgentError::Unknown, "agent_unknown", 422),
            (AgentError::Blocked, "agent_blocked", 403),
            (AgentError::TokenRequired, "agent_token_required", 422),
            (AgentError::Mismatch, "agent_mismatch", 403),
        ];
        for (e, code, status) in table {
            assert_eq!((e.code(), e.status()), (code, status));
        }
    }

    #[test]
    fn the_label_is_portal_text() {
        let mut row = AgentRow {
            agent_id: "a".into(),
            key_id: "k".into(),
            owner: "alice".into(),
            kind: AgentKind::Agent,
            state: AgentState::New,
            origin: AgentOrigin::Call,
            ordinal: 4,
            name: None,
            name_key: None,
            token_digest: Some("d".into()),
            merged_into: None,
            rev: 1,
            created_at: 0,
            named_at: None,
            last_seen_at: 0,
            hints: None,
        };
        assert_eq!(row.label("ops-key", AgentMode::Discover), "ops-key #4");
        row.name = Some("Billing bot".into());
        assert_eq!(row.label("ops-key", AgentMode::Discover), "Billing bot");
        row.kind = AgentKind::Bucket;
        assert_eq!(row.label("ops-key", AgentMode::Key), "ops-key");
        assert_eq!(
            row.label("ops-key", AgentMode::Discover),
            "Unattributed · ops-key"
        );
    }

    #[test]
    fn modes_and_states_have_one_text_each() {
        for m in [AgentMode::Key, AgentMode::Discover, AgentMode::Enrolled] {
            assert_eq!(AgentMode::parse(m.as_str()), Some(m));
        }
        for s in [
            AgentState::New,
            AgentState::Known,
            AgentState::Blocked,
            AgentState::Merged,
        ] {
            assert_eq!(AgentState::parse(s.as_str()), Some(s));
        }
        assert_eq!(AgentMode::parse("Key"), None);
        assert_eq!(AgentMode::default(), AgentMode::Key);
        // Without the block of the file, each mode is the mode `key`.
        assert_eq!(
            AgentRules::off().effective(AgentMode::Discover),
            AgentMode::Key
        );
    }
}
