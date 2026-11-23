//! The options of a node: the children that a service lists for the forms of grants (protocol 5.6 to 5.9).
//!
//! A person who gives a grant on a resource does not know the resources of the service. The
//! forms ask the service for the children of a node, and the person picks one. The request is
//! `GET /.well-known/procurauth/resources` at the origin of the action document. It carries the
//! identity token of the person who fills the form, so the service lists only what this person can
//! see. The portal keeps each answer for 60 seconds, in memory, with a bound on the number of
//! entries and on their size.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::util::percent_encode_query_component as encode;

/// The number of items that the portal asks for in one request.
pub const PAGE_ITEMS: usize = 50;
/// The largest answer that the portal reads (protocol 5.8).
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// The longest id of an item, in bytes, and the longest label, in characters (protocol 5.8).
const MAX_ID_BYTES: usize = 256;
const MAX_LABEL_CHARS: usize = 60;
/// How long the portal keeps an answer.
const KEEP: Duration = Duration::from_secs(60);
/// The bounds of the cache.
const MAX_ENTRIES: usize = 5_000;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;

/// One child of a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub has_children: bool,
}

/// One page of children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub items: Vec<Item>,
    /// The cursor of the next page. `None`: the last page.
    pub next: Option<String>,
}

impl Answer {
    /// An approximation of the memory that the answer takes, for the bound of the cache.
    fn size(&self) -> usize {
        64 + self
            .items
            .iter()
            .map(|i| 48 + i.id.len() + i.label.len())
            .sum::<usize>()
            + self.next.as_ref().map_or(0, String::len)
    }
}

/// What a request asks: the service, the person, the node and the type of its children.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Query {
    pub service: String,
    pub subject: String,
    /// The path of the node in its text form. Empty: the children of the service.
    pub parent: String,
    pub kind: String,
    pub q: String,
    pub after: String,
}

impl Query {
    /// The query string of protocol 5.7. The path in `parent` is already encoded as protocol 5.2
    /// says, and the query encodes it one more time.
    fn query_string(&self) -> String {
        let mut out = String::new();
        if !self.parent.is_empty() {
            out.push_str(&format!("parent={}&", encode(&self.parent)));
        }
        out.push_str(&format!("type={}", encode(&self.kind)));
        if !self.q.is_empty() {
            out.push_str(&format!("&q={}", encode(&self.q)));
        }
        out.push_str(&format!("&limit={PAGE_ITEMS}"));
        if !self.after.is_empty() {
            out.push_str(&format!("&after={}", encode(&self.after)));
        }
        out
    }
}

#[derive(Default)]
struct Entries {
    map: HashMap<Query, (Instant, std::sync::Arc<Answer>, usize)>,
    /// The order of insertion: the oldest entry is at the front.
    order: VecDeque<Query>,
    bytes: usize,
}

/// The client of the options, with its cache.
pub struct Options {
    client: reqwest::Client,
    entries: Mutex<Entries>,
}

impl Options {
    /// A client that waits `timeout` for an answer.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        Ok(Self {
            client: crate::discovery::http_client(timeout)?,
            entries: Mutex::new(Entries::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn kept(&self, query: &Query, now: Instant) -> Option<std::sync::Arc<Answer>> {
        let entries = self.lock();
        let (at, answer, _) = entries.map.get(query)?;
        (now.saturating_duration_since(*at) < KEEP).then(|| std::sync::Arc::clone(answer))
    }

    fn keep(&self, query: Query, answer: std::sync::Arc<Answer>, now: Instant) {
        let size = answer.size();
        let mut entries = self.lock();
        if let Some((_, _, old)) = entries.map.remove(&query) {
            entries.bytes -= old;
            entries.order.retain(|q| q != &query);
        }
        entries.bytes += size;
        entries.order.push_back(query.clone());
        entries.map.insert(query, (now, answer, size));
        while entries.map.len() > MAX_ENTRIES || entries.bytes > MAX_CACHE_BYTES {
            let Some(oldest) = entries.order.pop_front() else {
                break;
            };
            if let Some((_, _, size)) = entries.map.remove(&oldest) {
                entries.bytes -= size;
            }
        }
    }

    /// The number of entries, for the tests.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().map.len()
    }

    /// The children that the service lists. `origin` is the origin of the action document, and
    /// `header` is the name of the header of the identity token. An answer that is not good is an
    /// error: the form then offers no option, and the person can still type an id.
    pub async fn children(
        &self,
        origin: &str,
        header: &str,
        token: &str,
        query: &Query,
    ) -> Result<std::sync::Arc<Answer>, String> {
        let now = Instant::now();
        if let Some(answer) = self.kept(query, now) {
            return Ok(answer);
        }
        let url = format!(
            "{}/.well-known/procurauth/resources?{}",
            origin.trim_end_matches('/'),
            query.query_string()
        );
        let response = self
            .client
            .get(&url)
            .header("accept", "application/json")
            .header(header, token)
            .send()
            .await
            .map_err(|e| format!("cannot read the options: {e}"))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("the options gave the status {status}"));
        }
        let body = crate::discovery::read_capped(response, MAX_ANSWER_BYTES).await?;
        let answer = std::sync::Arc::new(parse(&body)?);
        self.keep(query.clone(), std::sync::Arc::clone(&answer), now);
        Ok(answer)
    }
}

/// The origin (`http://host:port`) of an address.
pub fn origin_of(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

/// Reads an answer (protocol 5.8). A bad member makes the whole answer bad.
pub fn parse(body: &[u8]) -> Result<Answer, String> {
    #[derive(serde::Deserialize)]
    struct RawItem {
        id: String,
        label: String,
        #[serde(default)]
        has_children: bool,
    }
    #[derive(serde::Deserialize)]
    struct Raw {
        items: Vec<RawItem>,
        #[serde(default)]
        next: Option<String>,
    }
    let raw: Raw = serde_json::from_slice(body).map_err(|e| format!("bad options: {e}"))?;
    if raw.items.len() > PAGE_ITEMS {
        return Err(format!("more than {PAGE_ITEMS} options"));
    }
    let mut items = Vec::with_capacity(raw.items.len());
    for item in raw.items {
        if item.id.is_empty() || item.id.len() > MAX_ID_BYTES {
            return Err("an option has a bad id".to_string());
        }
        if item.label.chars().count() > MAX_LABEL_CHARS {
            return Err("an option has a label that is too long".to_string());
        }
        items.push(Item {
            id: item.id,
            label: item.label,
            has_children: item.has_children,
        });
    }
    Ok(Answer {
        items,
        next: raw.next.filter(|n| !n.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn query(parent: &str) -> Query {
        Query {
            service: "db".into(),
            subject: "alice".into(),
            parent: parent.into(),
            kind: "database".into(),
            q: String::new(),
            after: String::new(),
        }
    }

    #[test]
    fn the_query_string_encodes_each_value_as_protocol_5_7_says() {
        let mut q = query("host:prod/database:a%2Fb");
        q.q = "test 1".into();
        q.after = "c+d".into();
        assert_eq!(
            q.query_string(),
            "parent=host%3Aprod%2Fdatabase%3Aa%252Fb&type=database&q=test%201&limit=50&after=c%2Bd"
        );
        assert_eq!(query("").query_string(), "type=database&limit=50");
    }

    #[test]
    fn an_answer_is_read_within_the_numbers_of_the_protocol() {
        let good = br#"{"items":[{"id":"a","label":"A","has_children":true},{"id":"b","label":"B"}],"next":"p2"}"#;
        let answer = parse(good).unwrap();
        assert_eq!(answer.items.len(), 2);
        assert!(answer.items[0].has_children && !answer.items[1].has_children);
        assert_eq!(answer.next.as_deref(), Some("p2"));
        assert_eq!(parse(br#"{"items":[],"next":null}"#).unwrap().next, None);
        let long = format!(
            r#"{{"items":[{{"id":"{}","label":"x"}}]}}"#,
            "i".repeat(257)
        );
        assert!(parse(long.as_bytes()).is_err());
        let label = format!(r#"{{"items":[{{"id":"a","label":"{}"}}]}}"#, "l".repeat(61));
        assert!(parse(label.as_bytes()).is_err());
        let many = format!(
            r#"{{"items":[{}]}}"#,
            (0..51)
                .map(|i| format!(r#"{{"id":"i{i}","label":"l"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(parse(many.as_bytes()).is_err());
        assert!(parse(b"not json").is_err());
        assert!(parse(br#"{"items":[{"id":"","label":"x"}]}"#).is_err());
    }

    #[test]
    fn the_cache_keeps_an_answer_for_a_minute_and_drops_the_oldest_first() {
        let options = Options::new(Duration::from_secs(1)).unwrap();
        let answer = Arc::new(Answer {
            items: vec![Item {
                id: "a".into(),
                label: "A".into(),
                has_children: false,
            }],
            next: None,
        });
        let now = Instant::now();
        options.keep(query("one"), Arc::clone(&answer), now);
        assert!(options.kept(&query("one"), now).is_some());
        assert!(options.kept(&query("two"), now).is_none(), "another parent");
        assert!(options
            .kept(&query("one"), now + Duration::from_secs(61))
            .is_none());
        for n in 0..MAX_ENTRIES + 10 {
            options.keep(query(&format!("n{n}")), Arc::clone(&answer), now);
        }
        assert_eq!(options.len(), MAX_ENTRIES);
        assert!(
            options.kept(&query("one"), now).is_none(),
            "the oldest left"
        );
        assert!(options
            .kept(&query(&format!("n{}", MAX_ENTRIES + 9)), now)
            .is_some());
    }

    #[test]
    fn the_origin_of_an_address_has_no_path() {
        assert_eq!(
            origin_of("http://notes:8080/.well-known/procurauth/actions").as_deref(),
            Some("http://notes:8080")
        );
        assert_eq!(
            origin_of("https://db.example/x").as_deref(),
            Some("https://db.example")
        );
        assert_eq!(origin_of("/relative"), None);
    }
}
