//! What the two databases share for the items: the list of the columns, the values of a row, the text of the
//! statements, and the reading of a row. The two drivers differ only in the form of a parameter (`?1` or `$1`) and in
//! the types of a value.

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::db::DbError;
use super::items::{ApprovalRow, ItemRow, Loaded};
use inbox_core::proto::Kind;

/// A value of a column. A null value has its type, because PostgreSQL checks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum V {
    Int(i64),
    Text(String),
    Blob(Vec<u8>),
    NullInt,
    NullText,
    NullBlob,
}

impl V {
    fn int(v: Option<i64>) -> V {
        v.map_or(V::NullInt, V::Int)
    }

    fn text(v: Option<&str>) -> V {
        v.map_or(V::NullText, |t| V::Text(t.to_string()))
    }
}

/// The columns of the table `items`, in the order of [`item_values`].
pub const ITEM_COLUMNS: [&str; 35] = [
    "request_id",
    "kind",
    "chain_id",
    "supersedes",
    "superseded_by",
    "previous_id",
    "requester_key_id",
    "requester_owner",
    "source",
    "interaction",
    "urgency",
    "input_type",
    "state",
    "settle_reason",
    "body_kid",
    "body_size",
    "body_purged_at",
    "created_at",
    "chain_created_at",
    "lease_s",
    "lease_until",
    "until_at",
    "agent_seen_at",
    "last_agent_turn_at",
    "abandoned_at",
    "revivals",
    "last_seq",
    "allow_custom",
    "allow_ask_back",
    "answer_action",
    "answered_at",
    "answered_by",
    "answered_via",
    "final_at",
    "idempotency_key",
];

/// The columns of the table `approvals`, in the order of [`approval_values`].
pub const APPROVAL_COLUMNS: [&str; 14] = [
    "request_id",
    "service",
    "subject",
    "action",
    "resource_path",
    "effect",
    "key_hash",
    "evaluation_id",
    "code",
    "decision",
    "shown_effect",
    "consume_before",
    "consumed_at",
    "lapsed_at",
];

/// The columns that a write of an approval sets: the 14 of [`APPROVAL_COLUMNS`] and `open_hash`.
pub const APPROVAL_WRITE_COLUMNS: [&str; 15] = [
    "request_id",
    "service",
    "subject",
    "action",
    "resource_path",
    "effect",
    "key_hash",
    "evaluation_id",
    "code",
    "decision",
    "shown_effect",
    "consume_before",
    "consumed_at",
    "lapsed_at",
    "open_hash",
];

/// The text of an enum as the database holds it (`snake_case`).
pub fn text_of<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

fn from_text<T: DeserializeOwned>(text: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(text.to_string())).ok()
}

pub fn item_values(i: &ItemRow) -> Vec<V> {
    let flag = |b: bool| V::Int(b as i64);
    vec![
        V::Text(i.request_id.clone()),
        V::Text(i.kind.as_str().to_string()),
        V::Text(i.chain_id.clone()),
        V::text(i.supersedes.as_deref()),
        V::text(i.superseded_by.as_deref()),
        V::text(i.previous_id.as_deref()),
        V::Text(i.requester_key_id.clone()),
        V::Text(i.requester_owner.clone()),
        V::Text(text_of(&i.via)),
        V::Text(text_of(&i.interaction)),
        V::Text(text_of(&i.urgency)),
        V::text(i.input_type.map(|t| text_of(&t)).as_deref()),
        V::Text(text_of(&i.state)),
        V::text(i.settle_reason.as_deref()),
        V::text(i.body_kid.as_deref()),
        V::Int(i.body_size),
        V::int(i.body_purged_at),
        V::Int(i.created_at),
        V::Int(i.chain_created_at),
        V::int(i.lease_s),
        V::int(i.lease_until),
        V::int(i.until_at),
        V::Int(i.agent_seen_at),
        V::int(i.last_agent_turn_at),
        V::int(i.abandoned_at),
        V::Int(i.revivals as i64),
        V::Int(i.last_seq as i64),
        flag(i.allow_custom),
        flag(i.allow_ask_back),
        V::text(i.answer_action.map(|a| text_of(&a)).as_deref()),
        V::int(i.answered_at),
        V::text(i.answered_by.as_deref()),
        V::text(i.answered_via.as_deref()),
        V::int(i.final_at),
        V::Text(i.idempotency_key.clone()),
    ]
}

pub fn approval_values(a: &ApprovalRow) -> Vec<V> {
    vec![
        V::Text(a.request_id.clone()),
        V::Text(a.service.clone()),
        V::Text(a.subject.clone()),
        V::Text(a.action.clone()),
        V::Text(a.resource_path.clone()),
        V::Text(a.effect.clone()),
        V::Text(a.key_hash.clone()),
        V::text(a.evaluation_id.as_deref()),
        V::Text(a.code.clone()),
        V::text(a.decision.map(|d| text_of(&d)).as_deref()),
        V::text(a.shown_effect.as_deref()),
        V::int(a.consume_before),
        V::int(a.consumed_at),
        V::int(a.lapsed_at),
    ]
}

/// The values of a write of an approval, in the order of [`APPROVAL_WRITE_COLUMNS`]. `open_hash` is the `key_hash`
/// while the item is open and NULL after: a unique index on it allows one open item for each effect.
pub fn approval_write_values(l: &Loaded, a: &ApprovalRow) -> Vec<V> {
    let mut values = approval_values(a);
    values.push(if l.item.is_open() {
        V::Text(a.key_hash.clone())
    } else {
        V::NullText
    });
    values
}

/// `SELECT` of an item with its approval. The columns of the item come first, then the 14 of the approval.
pub fn select_sql() -> String {
    let items: Vec<String> = ITEM_COLUMNS.iter().map(|c| format!("i.{c}")).collect();
    let approvals: Vec<String> = APPROVAL_COLUMNS.iter().map(|c| format!("a.{c}")).collect();
    format!(
        "SELECT {}, {} FROM items i LEFT JOIN approvals a ON a.request_id = i.request_id",
        items.join(", "),
        approvals.join(", ")
    )
}

/// `INSERT INTO table (columns) VALUES (placeholders)`. `ph(n)` is the placeholder number `n`, from 1.
pub fn insert_sql(table: &str, columns: &[&str], ph: &dyn Fn(usize) -> String) -> String {
    let marks: Vec<String> = (1..=columns.len()).map(ph).collect();
    format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        columns.join(", "),
        marks.join(", ")
    )
}

/// `UPDATE table SET column = placeholder ... WHERE first column = placeholder 1`. The values come in the order of the
/// columns, the key first.
pub fn update_sql(table: &str, columns: &[&str], ph: &dyn Fn(usize) -> String) -> String {
    let sets: Vec<String> = columns
        .iter()
        .enumerate()
        .skip(1)
        .map(|(i, c)| format!("{c} = {}", ph(i + 1)))
        .collect();
    format!(
        "UPDATE {table} SET {} WHERE {} = {}",
        sets.join(", "),
        columns[0],
        ph(1)
    )
}

/// The access to the columns of a row, for each driver.
pub trait Cols {
    fn int(&self, i: usize) -> Result<Option<i64>, DbError>;
    fn text(&self, i: usize) -> Result<Option<String>, DbError>;
}

fn bad(column: &str, value: &str) -> DbError {
    DbError::Pg(format!("items: the column {column} holds {value:?}"))
}

/// Reads an item and its approval from a row of [`select_sql`].
pub fn decode(r: &impl Cols) -> Result<Loaded, DbError> {
    let need_text = |i: usize| -> Result<String, DbError> {
        r.text(i)?
            .ok_or_else(|| bad(ITEM_COLUMNS.get(i).copied().unwrap_or("approval"), "NULL"))
    };
    let need_int = |i: usize| -> Result<i64, DbError> {
        r.int(i)?
            .ok_or_else(|| bad(ITEM_COLUMNS.get(i).copied().unwrap_or("approval"), "NULL"))
    };
    fn parse<T: DeserializeOwned>(column: &str, text: String) -> Result<T, DbError> {
        from_text(&text).ok_or_else(|| bad(column, &text))
    }
    fn opt<T: DeserializeOwned>(column: &str, text: Option<String>) -> Result<Option<T>, DbError> {
        text.map(|t| parse(column, t)).transpose()
    }
    let kind_text = need_text(1)?;
    let kind = Kind::parse(&kind_text).ok_or_else(|| bad("kind", &kind_text))?;
    let item = ItemRow {
        request_id: need_text(0)?,
        kind,
        chain_id: need_text(2)?,
        supersedes: r.text(3)?,
        superseded_by: r.text(4)?,
        previous_id: r.text(5)?,
        requester_key_id: need_text(6)?,
        requester_owner: need_text(7)?,
        via: parse("source", need_text(8)?)?,
        interaction: parse("interaction", need_text(9)?)?,
        urgency: parse("urgency", need_text(10)?)?,
        input_type: opt("input_type", r.text(11)?)?,
        state: parse("state", need_text(12)?)?,
        settle_reason: r.text(13)?,
        body_kid: r.text(14)?,
        body_size: need_int(15)?,
        body_purged_at: r.int(16)?,
        created_at: need_int(17)?,
        chain_created_at: need_int(18)?,
        lease_s: r.int(19)?,
        lease_until: r.int(20)?,
        until_at: r.int(21)?,
        agent_seen_at: need_int(22)?,
        last_agent_turn_at: r.int(23)?,
        abandoned_at: r.int(24)?,
        revivals: need_int(25)? as u32,
        last_seq: need_int(26)? as u64,
        allow_custom: need_int(27)? != 0,
        allow_ask_back: need_int(28)? != 0,
        answer_action: opt("answer_action", r.text(29)?)?,
        answered_at: r.int(30)?,
        answered_by: r.text(31)?,
        answered_via: r.text(32)?,
        final_at: r.int(33)?,
        idempotency_key: need_text(34)?,
    };
    let approval = match r.text(35)? {
        None => None,
        Some(request_id) => Some(ApprovalRow {
            request_id,
            service: need_text(36)?,
            subject: need_text(37)?,
            action: need_text(38)?,
            resource_path: need_text(39)?,
            effect: need_text(40)?,
            key_hash: need_text(41)?,
            evaluation_id: r.text(42)?,
            code: need_text(43)?,
            decision: opt("decision", r.text(44)?)?,
            shown_effect: r.text(45)?,
            consume_before: r.int(46)?,
            consumed_at: r.int(47)?,
            lapsed_at: r.int(48)?,
        }),
    };
    Ok(Loaded { item, approval })
}

#[cfg(test)]
mod tests {
    use super::*;
    use inbox_core::proto::ItemState;

    #[test]
    fn the_columns_and_the_values_have_the_same_length_and_order() {
        let item = super::super::items::tests::row(ItemState::Pending);
        assert_eq!(item_values(&item).len(), ITEM_COLUMNS.len());
        let ph = |n: usize| format!("?{n}");
        let sql = update_sql("items", &ITEM_COLUMNS, &ph);
        assert!(
            sql.starts_with("UPDATE items SET kind = ?2, chain_id = ?3"),
            "{sql}"
        );
        assert!(sql.ends_with("WHERE request_id = ?1"), "{sql}");
        assert_eq!(
            insert_sql("t", &["a", "b"], &ph),
            "INSERT INTO t (a, b) VALUES (?1, ?2)"
        );
        // The join names one more column of the item: `i.request_id`.
        assert_eq!(select_sql().matches("i.").count(), ITEM_COLUMNS.len() + 1);
    }

    #[test]
    fn enums_use_the_text_of_the_protocol() {
        assert_eq!(text_of(&ItemState::Pending), "pending");
        assert_eq!(
            text_of(&inbox_core::proto::Interaction::OneShot),
            "one_shot"
        );
        assert_eq!(text_of(&inbox_core::proto::Via::Mcp), "mcp");
        assert_eq!(
            from_text::<inbox_core::proto::Decision>("deny"),
            Some(inbox_core::proto::Decision::Deny)
        );
        assert_eq!(from_text::<ItemState>("nope"), None);
    }
}

/// The columns of a row of `account_prefs`. The two databases read and write the same values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrefsRow {
    pub digest_mode: Option<String>,
    pub digest_at: Option<String>,
    pub digest_tz: Option<String>,
    pub digest_every_h: Option<i64>,
    pub quiet_from: Option<String>,
    pub quiet_to: Option<String>,
    pub quiet_tz: Option<String>,
    pub quiet_high_rings: Option<i64>,
}

impl PrefsRow {
    /// The columns of the preferences of a phone. An absent block gives null columns.
    pub fn from_prefs(p: &inbox_core::proto::Prefs) -> PrefsRow {
        use inbox_core::proto::DigestMode;
        let mut r = PrefsRow::default();
        if let Some(q) = &p.quiet {
            r.quiet_from = Some(q.from.clone());
            r.quiet_to = Some(q.to.clone());
            r.quiet_tz = Some(q.tz.clone());
            r.quiet_high_rings = Some(i64::from(q.high_rings));
        }
        if let Some(d) = &p.digest {
            r.digest_mode = Some(
                match d.mode {
                    DigestMode::Off => "off",
                    DigestMode::Daily => "daily",
                    DigestMode::Every => "every",
                }
                .to_string(),
            );
            r.digest_at = d.at.clone();
            r.digest_tz = d.tz.clone();
            r.digest_every_h = d.every_h.map(i64::from);
        }
        r
    }

    /// The preferences of the columns. A row that no longer holds a valid block gives no block.
    pub fn into_prefs(self) -> inbox_core::proto::Prefs {
        use inbox_core::proto::{DigestMode, DigestPrefs, Prefs, QuietPrefs};
        let quiet = match (self.quiet_from, self.quiet_to, self.quiet_tz) {
            (Some(from), Some(to), Some(tz)) => Some(QuietPrefs {
                from,
                to,
                tz,
                high_rings: self.quiet_high_rings.unwrap_or(0) != 0,
            }),
            _ => None,
        };
        let mode = match self.digest_mode.as_deref() {
            Some("off") => Some(DigestMode::Off),
            Some("daily") => Some(DigestMode::Daily),
            Some("every") => Some(DigestMode::Every),
            _ => None,
        };
        let digest = mode.map(|mode| DigestPrefs {
            mode,
            at: self.digest_at,
            tz: self.digest_tz,
            every_h: self.digest_every_h.and_then(|h| u16::try_from(h).ok()),
        });
        Prefs { quiet, digest }
    }
}
