//! The limits of a grant: a maximum duration, a number of requests in a time window.
//!
//! A service declares its limits in its document (see [`Declaration`]). A grant can carry a value
//! for some of them (see [`Limits`]). The settings pages read the declaration to show the text of
//! each limit. The PDP gives the ceilings to the server, which applies them, and it counts the
//! budgets.
//!
//! There are two kinds of limit (protocol 6.1, 7.1):
//! - A **ceiling** is a maximum that applies to each use, for example the duration of a query. Its
//!   value is a number from the `min` to the `max` of its declaration.
//! - A **budget** is an amount of units in one or more time windows, for example 300 requests in
//!   a minute. Its value is a list of one to four items, one for each window. The PDP counts each
//!   window on its own counter.
//!
//! Two values of one ceiling combine to the lower one. A budget is never compared with another
//! budget: each subject keeps its own value and its own counters (protocol 6.7, 8.1). A limit
//! that a grant does not give is a limit that the portal does not set: the caps of the service
//! still apply.
//!
//! This module has no input or output. The checks and the arithmetic are here, so that tests can
//! cover them all.

// The grants, the document and the PDP use this module step by step. Until they all do, some
// items have no caller outside the tests.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{json, Map, Value as Json};

use crate::grants::ACCESS;

/// The most limits that one grant gives.
pub const MAX_LIMITS: usize = 8;
/// The most limits that one document declares.
pub const MAX_DECLARATIONS: usize = 16;
/// The largest number that a limit accepts: `min`, `max`, the value of a ceiling and the amount of a budget item.
/// It is 2^53 - 1, the largest integer that is safe in JSON (protocol 6.1): a browser, a script and a JSON library
/// that reads numbers as doubles all keep it exact.
pub const MAX_NUMBER: u64 = 9_007_199_254_740_991;
const MAX_NAME_CHARS: usize = 32;
const MAX_LABEL_CHARS: usize = 60;
const MAX_DESCRIPTION_CHARS: usize = 300;
const MAX_UNIT_CHARS: usize = 20;
/// The longest text form of the limits of one grant.
pub const MAX_JSON_BYTES: usize = 1024;
/// The prefix of the name of a ceiling (protocol 7.1). No budget name starts with it (protocol 6.1).
const CEILING_PREFIX: &str = "max_";

/// The time window of a budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Window {
    Second,
    Minute,
    Hour,
    Day,
}

impl Window {
    pub const ALL: [Window; 4] = [Window::Second, Window::Minute, Window::Hour, Window::Day];

    pub fn name(self) -> &'static str {
        match self {
            Self::Second => "second",
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }

    pub fn seconds(self) -> u64 {
        match self {
            Self::Second => 1,
            Self::Minute => 60,
            Self::Hour => 3600,
            Self::Day => 86_400,
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|window| window.name() == text)
            .ok_or_else(|| format!("{text:?} is not a window: use second, minute, hour or day"))
    }
}

/// An amount in one window: 300 each minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Item {
    pub amount: u64,
    pub window: Window,
}

/// The value of a budget: one item for each window, from the shortest window to the longest.
/// There are one to four items, and two items never have the same window (protocol 6.1).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Budget(Vec<Item>);

impl Budget {
    /// Makes a budget. The items can come in any order. An amount is a whole number from 1 to
    /// [`MAX_NUMBER`].
    pub fn new(mut items: Vec<Item>) -> Result<Self, String> {
        if items.is_empty() || items.len() > Window::ALL.len() {
            return Err("a budget has one to four items, one for each window".into());
        }
        if let Some(item) = items.iter().find(|i| !(1..=MAX_NUMBER).contains(&i.amount)) {
            return Err(format!(
                "the amount {} of the window {} must be a whole number from 1 to {MAX_NUMBER}",
                item.amount,
                item.window.name()
            ));
        }
        items.sort_by_key(|i| i.window);
        if let Some(pair) = items.windows(2).find(|p| p[0].window == p[1].window) {
            return Err(format!(
                "the window {} is in the budget two times",
                pair[0].window.name()
            ));
        }
        Ok(Self(items))
    }

    pub fn items(&self) -> &[Item] {
        &self.0
    }

    /// The amount of one window, when the budget has an item for it.
    pub fn amount(&self, window: Window) -> Option<u64> {
        self.0.iter().find(|i| i.window == window).map(|i| i.amount)
    }
}

/// The kind of a limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ceiling,
    Budget,
}

/// One limit that a service declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    /// The text that the settings pages show.
    pub label: String,
    pub description: String,
    /// The unit of the number, for example `second` or `request`.
    pub unit: String,
    pub kind: Kind,
    /// The range of a ceiling. A budget has no range.
    pub range: Option<(u64, u64)>,
}

/// A limit that a grant can hold, as a form shows it: the text of its
/// declaration and its kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub name: String,
    pub label: String,
    pub description: String,
    pub unit: String,
    /// `true` for a budget: the form has one field for each window.
    pub budget: bool,
    /// The range of a ceiling.
    pub range: Option<(u64, u64)>,
}

/// The value of one limit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    Ceiling(u64),
    Budget(Budget),
}

impl Value {
    /// The lower of two values of one ceiling. A budget is never compared with another budget:
    /// each subject keeps its own value (protocol 8.1). In every other case `self` stays.
    pub fn lower(self, other: &Self) -> Self {
        match (&self, other) {
            (Self::Ceiling(a), Self::Ceiling(b)) => Self::Ceiling((*a).min(*b)),
            _ => self,
        }
    }

    fn to_json(&self) -> Json {
        match self {
            Self::Ceiling(n) => json!(n),
            Self::Budget(budget) => Json::Array(
                budget
                    .items()
                    .iter()
                    .map(|i| json!({ "amount": i.amount, "window": i.window.name() }))
                    .collect(),
            ),
        }
    }
}

/// The values that one grant gives, by the name of the limit.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Limits(BTreeMap<String, Value>);

impl Limits {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }

    /// Sets one value. The name must be a valid limit name.
    pub fn set(&mut self, name: &str, value: Value) -> Result<(), String> {
        check_name(name)?;
        if !self.0.contains_key(name) && self.0.len() >= MAX_LIMITS {
            return Err(format!("a grant gives at most {MAX_LIMITS} limits"));
        }
        self.0.insert(name.to_string(), value);
        Ok(())
    }

    /// The combination of the ceilings of two sets: each ceiling that both give has the lower
    /// value. A ceiling that only one gives stays as it is. The result holds no budget, because a
    /// budget is not combined (protocol 8.1).
    pub fn lower(&self, other: &Self) -> Self {
        let mut out: BTreeMap<String, Value> = self
            .0
            .iter()
            .filter(|(_, v)| matches!(v, Value::Ceiling(_)))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        for (name, value) in &other.0 {
            if !matches!(value, Value::Ceiling(_)) {
                continue;
            }
            match out.get_mut(name) {
                Some(mine) => *mine = mine.clone().lower(value),
                None => {
                    out.insert(name.clone(), value.clone());
                }
            }
        }
        Self(out)
    }

    /// Reads the text form. The form is a JSON object. A ceiling is a number. A budget is an
    /// array of objects with `amount` and `window` (protocol 6.1). The empty text and `{}` are
    /// the empty set. Any other shape stops the read.
    pub fn from_json(text: &str) -> Result<Self, String> {
        if text.len() > MAX_JSON_BYTES {
            return Err(format!("the limits have more than {MAX_JSON_BYTES} bytes"));
        }
        if text.trim().is_empty() {
            return Ok(Self::new());
        }
        let parsed: Json = serde_json::from_str(text)
            .map_err(|e| format!("the limits are not valid JSON: {e}"))?;
        let Json::Object(map) = parsed else {
            return Err("the limits must be a JSON object".into());
        };
        let mut limits = Self::new();
        for (name, value) in map {
            let value = match value {
                Json::Number(n) => Value::Ceiling(number(&n, &name)?),
                Json::Array(items) => Value::Budget(budget(&items, &name)?),
                _ => {
                    return Err(format!(
                        "the limit {name:?} must be a number or an array of items with amount and window"
                    ))
                }
            };
            limits.set(&name, value)?;
        }
        Ok(limits)
    }

    /// The text form. The names are in the order of the alphabet. The text of equal sets is equal.
    pub fn to_json(&self) -> String {
        let map: Map<String, Json> = self
            .0
            .iter()
            .map(|(name, value)| (name.clone(), value.to_json()))
            .collect();
        Json::Object(map).to_string()
    }

    /// Checks the values against the declarations (protocol 6.1). `action` is the
    /// action of the grant, and `listed` names the limits that this action lists. On the action
    /// `access`, a value is good only for a budget that the document declares. On another
    /// action, a value is good only for a ceiling that the action lists.
    pub fn check(
        &self,
        declarations: &[Declaration],
        action: &str,
        listed: &[String],
    ) -> Result<(), String> {
        for (name, value) in &self.0 {
            let declaration = declarations
                .iter()
                .find(|d| d.name == *name)
                .ok_or_else(|| format!("the service does not declare the limit {name:?}"))?;
            match (action == ACCESS, declaration.kind) {
                (true, Kind::Budget) => {}
                (true, Kind::Ceiling) => {
                    return Err(format!(
                        "the action {ACCESS:?} takes budgets only: {name:?} is a ceiling"
                    ))
                }
                (false, Kind::Budget) => {
                    return Err(format!(
                    "a budget is a value of the action {ACCESS:?} only: {name:?} is on {action:?}"
                ))
                }
                (false, Kind::Ceiling) => {
                    if !listed.iter().any(|l| l == name) {
                        return Err(format!(
                            "the action {action:?} does not list the limit {name:?}"
                        ));
                    }
                }
            }
            declaration.check(value)?;
        }
        Ok(())
    }
}

fn number(n: &serde_json::Number, name: &str) -> Result<u64, String> {
    n.as_u64()
        .filter(|v| *v <= MAX_NUMBER)
        .ok_or_else(|| format!("the limit {name:?} must be a whole number from 0 to {MAX_NUMBER}"))
}

fn budget(items: &[Json], name: &str) -> Result<Budget, String> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Json::Object(members) = item else {
            return Err(format!(
                "each item of the budget {name:?} must be an object with amount and window"
            ));
        };
        if members.len() != 2 || !members.contains_key("amount") || !members.contains_key("window")
        {
            return Err(format!(
                "each item of the budget {name:?} must have exactly amount and window"
            ));
        }
        let amount = match &members["amount"] {
            Json::Number(n) => number(n, name)?,
            _ => {
                return Err(format!(
                    "the amount of the budget {name:?} must be a number"
                ))
            }
        };
        let window = match &members["window"] {
            Json::String(text) => Window::parse(text)?,
            _ => return Err(format!("the window of the budget {name:?} must be a text")),
        };
        out.push(Item { amount, window });
    }
    Budget::new(out).map_err(|e| format!("the budget {name:?}: {e}"))
}

/// A limit name: a to z first, then a to z, 0 to 9 and underscore. At most 32 characters
/// (protocol 5.2, 6.1).
pub fn check_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    if first_ok
        && name.len() <= MAX_NAME_CHARS
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        Ok(())
    } else {
        Err(format!(
            "{name:?} is not a limit name: a to z first, then a to z, 0 to 9 and _, at most {MAX_NAME_CHARS} characters"
        ))
    }
}

impl Declaration {
    /// Checks a value against this declaration. A budget value has its own checks when it is
    /// made (see [`Budget::new`]), and a budget has no range.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        let name = &self.name;
        match (self.range, value) {
            (Some((min, max)), Value::Ceiling(n)) => {
                if (min..=max).contains(n) {
                    Ok(())
                } else {
                    Err(format!(
                        "the limit {name:?} must be from {min} to {max} {}",
                        self.unit
                    ))
                }
            }
            (None, Value::Budget(_)) => Ok(()),
            (Some(_), Value::Budget(_)) => {
                Err(format!("the limit {name:?} is a ceiling: give a number"))
            }
            (None, Value::Ceiling(_)) => Err(format!(
                "the limit {name:?} is a budget: give a list of amounts and windows"
            )),
        }
    }
}

/// The declaration of one limit as the document writes it. Any other member stops the read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawDeclaration {
    name: String,
    label: String,
    description: String,
    unit: String,
    kind: String,
    #[serde(default)]
    min: Option<u64>,
    #[serde(default)]
    max: Option<u64>,
}

/// Checks the list of declarations of a document. A bad declaration stops the read of the whole
/// document.
pub fn check_declarations(raw: Vec<RawDeclaration>) -> Result<Vec<Declaration>, String> {
    if raw.len() > MAX_DECLARATIONS {
        return Err(format!("more than {MAX_DECLARATIONS} limits"));
    }
    let mut out: Vec<Declaration> = Vec::with_capacity(raw.len());
    for item in raw {
        check_name(&item.name)?;
        if out.iter().any(|d| d.name == item.name) {
            return Err(format!(
                "the limit {:?} is in the list two times",
                item.name
            ));
        }
        let what = format!("the limit {:?}", item.name);
        check_text(
            &item.label,
            MAX_LABEL_CHARS,
            &format!("the label of {what}"),
        )?;
        check_text(
            &item.description,
            MAX_DESCRIPTION_CHARS,
            &format!("the description of {what}"),
        )?;
        check_text(&item.unit, MAX_UNIT_CHARS, &format!("the unit of {what}"))?;
        let (kind, range) = match item.kind.as_str() {
            "ceiling" => {
                let (Some(min), Some(max)) = (item.min, item.max) else {
                    return Err(format!("{what} is a ceiling: give its min and its max"));
                };
                if min > max {
                    return Err(format!("{what} needs min <= max, not {min} and {max}"));
                }
                if max > MAX_NUMBER {
                    return Err(format!(
                        "{what} has the max {max}: a limit is a whole number from 0 to {MAX_NUMBER}"
                    ));
                }
                if !item.name.starts_with(CEILING_PREFIX) || item.name.len() == CEILING_PREFIX.len()
                {
                    return Err(format!(
                        "{what} is a ceiling: its name is {CEILING_PREFIX:?} and a noun"
                    ));
                }
                (Kind::Ceiling, Some((min, max)))
            }
            "budget" => {
                if item.min.is_some() || item.max.is_some() {
                    return Err(format!("{what} is a budget: it has no min and no max"));
                }
                if item.name.starts_with(CEILING_PREFIX) {
                    return Err(format!(
                        "{what} is a budget: its name does not start with {CEILING_PREFIX:?}"
                    ));
                }
                (Kind::Budget, None)
            }
            other => {
                return Err(format!(
                    "{what} has the kind {other:?}: use ceiling or budget"
                ))
            }
        };
        out.push(Declaration {
            name: item.name,
            label: item.label,
            description: item.description,
            unit: item.unit,
            kind,
            range,
        });
    }
    Ok(out)
}

fn check_text(text: &str, max: usize, what: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if text.chars().count() > max {
        return Err(format!("{what} has more than {max} characters"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!("{what} has a control character"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> Vec<RawDeclaration> {
        serde_json::from_str(json).unwrap()
    }

    fn requests_declaration() -> Declaration {
        check_declarations(raw(
            r#"[{"name":"requests","label":"Requests","description":"The requests that a subject sends.",
                 "unit":"request","kind":"budget"}]"#,
        ))
        .unwrap()
        .remove(0)
    }

    fn duration_declaration() -> Declaration {
        check_declarations(raw(
            r#"[{"name":"max_duration","label":"Maximum duration","description":"The longest run of one use.",
                 "unit":"seconds","kind":"ceiling","min":1,"max":3600}]"#,
        ))
        .unwrap()
        .remove(0)
    }

    /// The convention of the units: a ceiling has a plural noun and a budget has a singular noun. The unit is a text
    /// for a person: nothing compares it, so a service can publish either form.
    #[test]
    fn plural_ceiling_units_and_singular_budget_units_are_accepted_and_read_well() {
        let declarations = check_declarations(raw(
            r#"[{"name":"requests","label":"Requests","description":"d","unit":"request","kind":"budget"},
                {"name":"max_rows","label":"Rows","description":"d","unit":"rows","kind":"ceiling","min":1,"max":100},
                {"name":"max_duration","label":"Duration","description":"d","unit":"seconds","kind":"ceiling","min":1,"max":60}]"#,
        ))
        .unwrap();
        let names: Vec<String> = vec!["max_rows".into(), "max_duration".into()];
        let too_big = Limits::from_json(r#"{"max_rows":101}"#).unwrap();
        let error = too_big
            .check(&declarations, "query.run.read", &names)
            .unwrap_err();
        assert!(error.contains("from 1 to 100 rows"), "{error}");
        let good = Limits::from_json(r#"{"max_rows":100,"max_duration":60}"#).unwrap();
        assert!(good.check(&declarations, "query.run.read", &names).is_ok());
        let budget = Limits::from_json(r#"{"requests":[{"amount":5,"window":"minute"}]}"#).unwrap();
        assert!(budget.check(&declarations, ACCESS, &[]).is_ok());
    }

    fn per(amount: u64, window: Window) -> Item {
        Item { amount, window }
    }

    #[test]
    fn a_budget_keeps_one_item_for_each_window_from_the_shortest() {
        let budget = Budget::new(vec![
            per(300, Window::Minute),
            per(10, Window::Second),
            per(5000, Window::Day),
        ])
        .unwrap();
        assert_eq!(
            budget.items(),
            [
                per(10, Window::Second),
                per(300, Window::Minute),
                per(5000, Window::Day)
            ]
        );
        assert_eq!(budget.amount(Window::Minute), Some(300));
        assert_eq!(budget.amount(Window::Hour), None);
        let four: Vec<Item> = Window::ALL.iter().map(|w| per(1, *w)).collect();
        assert!(Budget::new(four).is_ok());
        for (bad, why) in [
            (vec![], "no item"),
            (
                vec![per(1, Window::Minute), per(2, Window::Minute)],
                "a window two times",
            ),
            (vec![per(0, Window::Minute)], "an amount of 0"),
            (
                vec![per(MAX_NUMBER + 1, Window::Minute)],
                "an amount above the maximum",
            ),
        ] {
            assert!(Budget::new(bad).is_err(), "{why}");
        }
    }

    #[test]
    fn the_text_form_reads_and_writes_the_same_values() {
        let text = r#"{"max_duration":30,"requests":[{"amount":10,"window":"second"},{"amount":300,"window":"minute"}]}"#;
        let limits = Limits::from_json(text).unwrap();
        assert_eq!(limits.get("max_duration"), Some(&Value::Ceiling(30)));
        let Some(Value::Budget(budget)) = limits.get("requests") else {
            panic!("the budget is missing");
        };
        assert_eq!(budget.amount(Window::Second), Some(10));
        assert_eq!(budget.amount(Window::Minute), Some(300));
        assert_eq!(limits.to_json(), text);
        assert_eq!(Limits::from_json("").unwrap(), Limits::new());
        assert_eq!(Limits::from_json("{}").unwrap(), Limits::new());
        assert_eq!(Limits::new().to_json(), "{}");
        // The items of a budget can come in any order. The text form puts them in order.
        let reversed =
            r#"{"requests":[{"amount":300,"window":"minute"},{"amount":10,"window":"second"}]}"#;
        assert_eq!(
            Limits::from_json(reversed).unwrap().to_json(),
            r#"{"requests":[{"amount":10,"window":"second"},{"amount":300,"window":"minute"}]}"#
        );
    }

    #[test]
    fn a_bad_text_form_stops_the_read() {
        for bad in [
            "[]",
            "3",
            r#"{"Rate":1}"#,
            r#"{"1a":1}"#,
            r#"{"a":-1}"#,
            r#"{"a":1.5}"#,
            r#"{"a":9007199254740992}"#,
            r#"{"a":"x"}"#,
            r#"{"a":true}"#,
            r#"{"a":null}"#,
            // The old form of a budget: one object, with the member `per`.
            r#"{"a":{"amount":1,"window":"minute"}}"#,
            r#"{"a":[]}"#,
            r#"{"a":[1]}"#,
            r#"{"a":[{"amount":1}]}"#,
            r#"{"a":[{"amount":1,"per":"minute"}]}"#,
            r#"{"a":[{"amount":1,"window":"week"}]}"#,
            r#"{"a":[{"amount":1,"window":"minute","burst":2}]}"#,
            r#"{"a":[{"amount":"1","window":"minute"}]}"#,
            r#"{"a":[{"amount":1,"window":2}]}"#,
            r#"{"a":[{"amount":0,"window":"minute"}]}"#,
            r#"{"a":[{"amount":1,"window":"minute"},{"amount":2,"window":"minute"}]}"#,
            r#"{"a":[{"amount":1,"window":"second"},{"amount":1,"window":"minute"},{"amount":1,"window":"hour"},{"amount":1,"window":"day"},{"amount":1,"window":"day"}]}"#,
            "{",
        ] {
            assert!(Limits::from_json(bad).is_err(), "{bad}");
        }
        let nine: Vec<String> = (0..9).map(|i| format!("\"a{i}\":1")).collect();
        assert!(Limits::from_json(&format!("{{{}}}", nine.join(","))).is_err());
        let long = format!("{{\"a\":1,\"b\":\"{}\"}}", "x".repeat(MAX_JSON_BYTES));
        assert!(Limits::from_json(&long).is_err());
    }

    #[test]
    fn two_sets_combine_their_ceilings_and_drop_the_budgets() {
        let a = Limits::from_json(
            r#"{"max_duration":30,"requests":[{"amount":300,"window":"minute"}]}"#,
        )
        .unwrap();
        let b = Limits::from_json(r#"{"max_duration":10,"max_rows":5}"#).unwrap();
        let c = a.lower(&b);
        assert_eq!(c.get("max_duration"), Some(&Value::Ceiling(10)));
        assert_eq!(
            c.get("max_rows"),
            Some(&Value::Ceiling(5)),
            "a ceiling that only one side gives stays"
        );
        assert!(c.get("requests").is_none(), "a budget is never combined");
        assert_eq!(a.lower(&b), b.lower(&a), "the order does not matter");
        let ceilings = Limits::from_json(r#"{"max_duration":30}"#).unwrap();
        assert_eq!(ceilings.lower(&Limits::new()), ceilings);
        assert_eq!(Limits::new().lower(&ceilings), ceilings);
        assert!(a.lower(&a).get("requests").is_none());
    }

    #[test]
    fn the_check_uses_the_declaration_the_action_and_its_list() {
        let declarations = vec![requests_declaration(), duration_declaration()];
        let listed = vec!["max_duration".to_string()];
        // A budget is on `access`. A ceiling is on an action that lists it.
        let budget =
            Limits::from_json(r#"{"requests":[{"amount":300,"window":"minute"}]}"#).unwrap();
        let ceiling = Limits::from_json(r#"{"max_duration":30}"#).unwrap();
        assert!(budget.check(&declarations, ACCESS, &[]).is_ok());
        assert!(ceiling
            .check(&declarations, "query.run.read", &listed)
            .is_ok());
        assert!(Limits::new().check(&declarations, ACCESS, &[]).is_ok());
        for (limits, action, listed, why) in [
            (
                &budget,
                "query.run.read",
                &listed[..],
                "a budget on another action",
            ),
            (&ceiling, ACCESS, &listed[..], "a ceiling on access"),
            (
                &ceiling,
                "query.run.read",
                &[][..],
                "a ceiling that the action does not list",
            ),
        ] {
            assert!(
                limits.check(&declarations, action, listed).is_err(),
                "{why}"
            );
        }
        for (bad, action, why) in [
            (r#"{"max_duration":0}"#, "a.b", "below the minimum"),
            (r#"{"max_duration":3601}"#, "a.b", "above the maximum"),
            (
                r#"{"max_duration":[{"amount":1,"window":"minute"}]}"#,
                "a.b",
                "a ceiling needs a number",
            ),
            (
                r#"{"requests":30}"#,
                ACCESS,
                "a budget needs a list of items",
            ),
            (r#"{"unknown":1}"#, "a.b", "not declared"),
            (r#"{"unknown":1}"#, ACCESS, "not declared on access"),
        ] {
            let limits = Limits::from_json(bad).unwrap();
            assert!(
                limits.check(&declarations, action, &listed).is_err(),
                "{why}: {bad}"
            );
        }
    }

    #[test]
    fn a_ceiling_accepts_the_minimum_zero() {
        let declarations = check_declarations(raw(
            r#"[{"name":"max_key_lifetime","label":"Longest key lifetime","description":"The longest lifetime of a key, in seconds.",
                 "unit":"seconds","kind":"ceiling","min":0,"max":31536000}]"#,
        ))
        .unwrap();
        let listed = vec!["max_key_lifetime".to_string()];
        let zero = Limits::from_json(r#"{"max_key_lifetime":0}"#).unwrap();
        assert!(zero.check(&declarations, "keys.own.use", &listed).is_ok());
    }

    #[test]
    fn the_declarations_follow_the_closed_schema() {
        let ok = check_declarations(raw(
            r#"[{"name":"requests","label":"Requests","description":"d","unit":"request","kind":"budget"},
                {"name":"max_duration","label":"Duration","description":"d","unit":"seconds","kind":"ceiling","min":1,"max":60}]"#,
        ))
        .unwrap();
        assert_eq!(ok[0].kind, Kind::Budget);
        assert_eq!(ok[0].range, None);
        assert_eq!(ok[1].range, Some((1, 60)));
        let head = r#""label":"x","description":"d","unit":"u""#;
        for bad in [
            format!(r#"[{{"name":"requests",{head},"kind":"other"}}]"#),
            format!(r#"[{{"name":"Requests",{head},"kind":"budget"}}]"#),
            r#"[{"name":"max_a","label":"","description":"d","unit":"u","kind":"ceiling","min":1,"max":2}]"#.to_string(),
            // A declaration without description or unit is bad.
            r#"[{"name":"max_a","label":"x","unit":"u","kind":"ceiling","min":1,"max":2}]"#.to_string(),
            r#"[{"name":"max_a","label":"x","description":"d","kind":"ceiling","min":1,"max":2}]"#.to_string(),
            r#"[{"name":"max_a","label":"x","description":"","unit":"u","kind":"ceiling","min":1,"max":2}]"#.to_string(),
            // A ceiling needs min and max, and min is not above max.
            format!(r#"[{{"name":"max_a",{head},"kind":"ceiling"}}]"#),
            format!(r#"[{{"name":"max_a",{head},"kind":"ceiling","min":1}}]"#),
            format!(r#"[{{"name":"max_a",{head},"kind":"ceiling","min":3,"max":2}}]"#),
            // A budget has neither.
            format!(r#"[{{"name":"requests",{head},"kind":"budget","min":1}}]"#),
            format!(r#"[{{"name":"requests",{head},"kind":"budget","max":2}}]"#),
            // The name rules: a ceiling is `max_` and a noun, and a budget never starts with `max_`.
            format!(r#"[{{"name":"duration",{head},"kind":"ceiling","min":1,"max":2}}]"#),
            format!(r#"[{{"name":"max_",{head},"kind":"ceiling","min":1,"max":2}}]"#),
            format!(r#"[{{"name":"max_requests",{head},"kind":"budget"}}]"#),
            // The members of the old form.
            format!(r#"[{{"name":"requests",{head},"kind":"budget","windows":["minute"]}}]"#),
            format!(r#"[{{"name":"requests",{head},"kind":"budget","scope":"action"}}]"#),
            format!(r#"[{{"name":"requests",{head},"kind":"budget","default_window":"minute"}}]"#),
            format!(r#"[{{"name":"max_a",{head},"kind":"ceiling","min":1,"max":2}},{{"name":"max_a",{head},"kind":"ceiling","min":1,"max":2}}]"#),
            r#"[{"name":"max_a","label":"x\u0000","description":"d","unit":"u","kind":"ceiling","min":1,"max":2}]"#.to_string(),
            format!(r#"[{{"name":"max_a","label":"x","description":"d","unit":"{}","kind":"ceiling","min":1,"max":2}}]"#, "u".repeat(21)),
        ] {
            let parsed: Result<Vec<RawDeclaration>, _> = serde_json::from_str(&bad);
            let outcome = parsed
                .map_err(|e| e.to_string())
                .and_then(check_declarations);
            assert!(outcome.is_err(), "{bad}");
        }
        // An unknown member stops the read.
        let unknown: Result<Vec<RawDeclaration>, _> = serde_json::from_str(
            r#"[{"name":"max_a","label":"x","description":"d","unit":"u","kind":"ceiling","min":1,"max":2,"color":"red"}]"#,
        );
        assert!(unknown.is_err());
        let seventeen: Vec<String> = (0..17)
            .map(|i| {
                format!(r#"{{"name":"max_a{i}","label":"x","description":"d","unit":"u","kind":"ceiling","min":1,"max":2}}"#)
            })
            .collect();
        let too_many = format!("[{}]", seventeen.join(","));
        assert!(check_declarations(raw(&too_many)).is_err());
    }

    /// The largest number of a limit is 2^53 - 1 (protocol 6.1). The text form keeps it exact in both directions, as a
    /// browser and a JSON library that reads doubles need.
    #[test]
    fn the_largest_number_is_2_pow_53_minus_1_and_stays_exact_in_the_text_form() {
        assert_eq!(MAX_NUMBER, 9_007_199_254_740_991);
        assert_eq!(MAX_NUMBER, (1u64 << 53) - 1);
        let text = r#"{"max_duration":9007199254740991,"requests":[{"amount":9007199254740991,"window":"day"}]}"#;
        let limits = Limits::from_json(text).unwrap();
        assert_eq!(
            limits.get("max_duration"),
            Some(&Value::Ceiling(MAX_NUMBER))
        );
        assert_eq!(
            limits.to_json(),
            text,
            "the number is written as it was read"
        );
        assert_eq!(Limits::from_json(&limits.to_json()).unwrap(), limits);
        // A number above the bound is refused, in a ceiling and in a budget.
        for bad in [
            r#"{"max_duration":9007199254740992}"#,
            r#"{"requests":[{"amount":9007199254740992,"window":"day"}]}"#,
            r#"{"max_duration":18446744073709551615}"#,
        ] {
            assert!(Limits::from_json(bad).is_err(), "{bad}");
        }
        // A ceiling that Dash declares above 1e9 works: a value of 5 GB (the old bound was 1e9).
        let wide = check_declarations(raw(
            r#"[{"name":"max_read_size","label":"Read size","description":"d","unit":"bytes","kind":"ceiling","min":1,"max":1000000000000000}]"#,
        ))
        .unwrap();
        let big = Limits::from_json(r#"{"max_read_size":5000000000}"#).unwrap();
        assert!(big
            .check(&wide, "query.run.read", &["max_read_size".to_string()])
            .is_ok());
        // The declaration itself is bounded too.
        let too_wide = format!(
            r#"[{{"name":"max_a","label":"x","description":"d","unit":"u","kind":"ceiling","min":0,"max":{}}}]"#,
            MAX_NUMBER + 1
        );
        assert!(check_declarations(raw(&too_wide)).is_err());
        let at_bound = format!(
            r#"[{{"name":"max_a","label":"x","description":"d","unit":"u","kind":"ceiling","min":0,"max":{MAX_NUMBER}}}]"#
        );
        assert!(check_declarations(raw(&at_bound)).is_ok());
    }

    #[test]
    fn the_lowest_value_wins_for_two_ceilings_only() {
        for a in [1u64, 5, 30] {
            for b in [1u64, 5, 30] {
                assert_eq!(
                    Value::Ceiling(a).lower(&Value::Ceiling(b)),
                    Value::Ceiling(a.min(b))
                );
            }
        }
        // A budget is never compared: the first value stays.
        let budget = Value::Budget(Budget::new(vec![per(1, Window::Minute)]).unwrap());
        assert_eq!(Value::Ceiling(3).lower(&budget), Value::Ceiling(3));
        assert_eq!(budget.clone().lower(&Value::Ceiling(3)), budget);
    }
}
