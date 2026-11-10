//! The rules of the web page of a question (lot L14): the rows under the options, the sheet of the blocks, and the
//! reading of the answer form. This module writes no HTML. The Android app has the same rules in `FlagsMatrix.kt` and
//! `AnswerSheet.kt`, so the page and the phone show the same rows and refuse the same answers.
//!
//! The server stays the judge: `Items::answer` runs `questions::check_answer` for each answer, and this module only
//! lets the page show a mistake with the text that the owner typed.
//!
//! # The names of the fields of the form
//!
//! A block has the number `n`, counted from 0. A question that is not a form has one block.
//!
//! | Field | Meaning |
//! |---|---|
//! | `b{n}` | The option of a radio block: `o:<id>`, or `x` for "Other answer". |
//! | `b{n}o_<id>` | One tick of a checkbox block. |
//! | `b{n}x` | The tick "Other answer" of a checkbox block. |
//! | `b{n}t` | The text: the free answer, or the answer of a text block. |

use std::collections::HashMap;

use inbox_core::proto::{FormAnswer, FormQuestionType, InputOption, InputType, Interaction};

use crate::inbox::page_items::OwnerAnswer;

/// The longest free text, in characters. The server takes the same number (SPEC 3.7.63).
pub const MAX_TEXT_CHARS: usize = crate::inbox::page_items::MAX_FREE_TEXT_CHARS;

pub const DECLINE_WORD: &str = "Decline to answer";
pub const AUTOMATIC_HEADER: &str = "Automatic system (no conversation)";
pub const SECRET_WARNING: &str = "Do not enter a password or a secret.";
pub const OTHER_WORD: &str = "Other answer";
pub const OTHER_NOTE: &str = "Your text is the answer. It ends the question.";
pub const ASK_WORD: &str = "Ask the agent";
pub const ASK_NOTE: &str = "This is not an answer. The question stays open and the agent replies.";
pub const NO_DISCUSSION: &str = "No free text. No discussion.";
pub const LOCK_LINE: &str =
    "Send is off: this agent is new. Name it or allow it first. You can decline now.";
pub const NO_ANSWER_NOTE: &str =
    "This message takes no answer. You can close it with Decline to answer.";

/// The row that does not exist for a combination that the protocol forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    TextWithCustom,
    NoneWithFlag,
    OneShotWithFlag,
    OneShotInput,
    FormCustomWithoutChoice,
}

impl Invalid {
    pub fn line(self) -> &'static str {
        match self {
            Invalid::TextWithCustom => "A text question cannot also take another answer.",
            Invalid::NoneWithFlag => "A message with no answer cannot take an answer row.",
            Invalid::OneShotWithFlag => "An automatic system cannot take a text or a discussion.",
            Invalid::OneShotInput => "An automatic system can only ask a choice or a yes or no.",
            Invalid::FormCustomWithoutChoice => "A form with no choice cannot take another answer.",
        }
    }
}

/// The rows under the options (DIAL 1.4.2 and 4.2). One pure function decides them.
///
/// | `allow_custom` | `allow_ask_back` | Rows |
/// |---|---|---|
/// | false | false | none: the question is closed |
/// | true | false | "Other answer" with a field |
/// | false | true | "Ask the agent" |
/// | true | true | both rows |
///
/// A combination that the protocol forbids gives [`Rows::invalid`] and no row: the page never shows an escape that the
/// server refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rows {
    pub other_answer: bool,
    pub ask_agent: bool,
    pub invalid: Option<Invalid>,
}

impl Rows {
    const CLOSED: Rows = Rows {
        other_answer: false,
        ask_agent: false,
        invalid: None,
    };

    fn bad(why: Invalid) -> Rows {
        Rows {
            other_answer: false,
            ask_agent: false,
            invalid: Some(why),
        }
    }

    /// The question has no escape row: the owner can only choose, write the answer, or decline.
    pub fn closed(&self) -> bool {
        !self.other_answer && !self.ask_agent
    }
}

/// The rows for the type of the input, the two flags and the interaction of the key. `form_has_choice` is for a form: at
/// least one of its questions is a choice.
pub fn rows(
    kind: InputType,
    allow_custom: bool,
    allow_ask_back: bool,
    interaction: Interaction,
    form_has_choice: bool,
) -> Rows {
    if interaction == Interaction::OneShot {
        return if allow_custom || allow_ask_back {
            Rows::bad(Invalid::OneShotWithFlag)
        } else if !matches!(kind, InputType::Choice | InputType::Confirm) {
            Rows::bad(Invalid::OneShotInput)
        } else {
            Rows::CLOSED
        };
    }
    let both = |custom: bool, ask: bool| Rows {
        other_answer: custom,
        ask_agent: ask,
        invalid: None,
    };
    match kind {
        InputType::None if allow_custom || allow_ask_back => Rows::bad(Invalid::NoneWithFlag),
        InputType::None => Rows::CLOSED,
        InputType::Text if allow_custom => Rows::bad(Invalid::TextWithCustom),
        InputType::Text => both(false, allow_ask_back),
        InputType::Form if allow_custom && !form_has_choice => {
            Rows::bad(Invalid::FormCustomWithoutChoice)
        }
        InputType::Form | InputType::Choice | InputType::Confirm => {
            both(allow_custom, allow_ask_back)
        }
    }
}

/// How a block takes its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// One option of a list.
    Radio,
    /// Several options of a list, `min` to `max`.
    Checkbox,
    /// Yes or no: a radio with two fixed options.
    Confirm,
    /// A free text.
    Text,
}

/// One question that the owner answers. All text in a block comes from the agent.
#[derive(Debug, Clone)]
pub struct Block {
    /// The id of the question of a form. `None` for a single question.
    pub id: Option<String>,
    pub header: Option<String>,
    pub text: Option<String>,
    pub kind: BlockKind,
    pub options: Vec<InputOption>,
    pub min: u8,
    pub max: u8,
    /// The row "Other answer" shows in this block.
    pub other: bool,
    pub placeholder: Option<String>,
}

/// The blocks of a question, in the order of the page.
#[derive(Debug, Clone)]
pub struct Sheet {
    pub blocks: Vec<Block>,
    pub is_form: bool,
}

fn confirm_options() -> Vec<InputOption> {
    vec![
        InputOption {
            id: "yes".into(),
            label: "Yes".into(),
            description: None,
        },
        InputOption {
            id: "no".into(),
            label: "No".into(),
            description: None,
        },
    ]
}

impl Sheet {
    /// The sheet of a question, or `None` when the page takes no answer: a message with no answer, or a combination that
    /// the protocol forbids (only Decline works then).
    pub fn of(input: &inbox_core::proto::Input, rows: &Rows) -> Option<Sheet> {
        if rows.invalid.is_some() {
            return None;
        }
        let other = rows.other_answer;
        let single = |kind: BlockKind, options: Vec<InputOption>| Sheet {
            blocks: vec![Block {
                id: None,
                header: None,
                text: None,
                kind,
                options,
                min: input.min,
                max: input.max,
                other,
                placeholder: input.placeholder.clone(),
            }],
            is_form: false,
        };
        match input.kind {
            InputType::None => None,
            InputType::Choice => Some(single(
                if input.multiple {
                    BlockKind::Checkbox
                } else {
                    BlockKind::Radio
                },
                input.options.clone(),
            )),
            InputType::Confirm => Some(single(BlockKind::Confirm, confirm_options())),
            InputType::Text => Some(single(BlockKind::Text, Vec::new())),
            InputType::Form => {
                let blocks = input
                    .questions
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|q| match q.kind {
                        FormQuestionType::Choice => Block {
                            id: Some(q.id.clone()),
                            header: Some(q.header.clone()),
                            text: Some(q.text.clone()),
                            kind: if q.multiple {
                                BlockKind::Checkbox
                            } else {
                                BlockKind::Radio
                            },
                            options: q.options.clone(),
                            min: q.min,
                            max: q.max,
                            other,
                            placeholder: None,
                        },
                        FormQuestionType::Text => Block {
                            id: Some(q.id.clone()),
                            header: Some(q.header.clone()),
                            text: Some(q.text.clone()),
                            kind: BlockKind::Text,
                            options: Vec::new(),
                            min: 0,
                            max: 0,
                            other: false,
                            placeholder: q.placeholder.clone(),
                        },
                    })
                    .collect();
                Some(Sheet {
                    blocks,
                    is_form: true,
                })
            }
        }
    }
}

/// What the owner entered in one block, and the mistake of the block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockDraft {
    /// The ticked option ids. A radio has at most one.
    pub ticked: Vec<String>,
    /// "Other answer" is selected.
    pub other_on: bool,
    /// The text of "Other answer", as typed.
    pub other_text: String,
    /// The text of a text block, as typed.
    pub text: String,
    /// The rule that the entry breaks. A sentence for the owner.
    pub error: Option<String>,
}

/// The entries of the form, kept when the server refuses the answer, so the owner does not type again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub blocks: Vec<BlockDraft>,
    /// The text of the box "Ask the agent".
    pub ask_text: String,
    /// A mistake of the box "Ask the agent".
    pub ask_error: Option<String>,
}

impl Draft {
    /// Reads the fields of the form. A field that this reads is removed from `f`. The caller refuses a form with a field
    /// left over.
    pub fn read(sheet: &Sheet, f: &mut HashMap<String, String>) -> Draft {
        let mut blocks = Vec::new();
        for (n, b) in sheet.blocks.iter().enumerate() {
            let mut d = BlockDraft::default();
            match b.kind {
                BlockKind::Radio | BlockKind::Confirm => {
                    match f.remove(&format!("b{n}")).as_deref() {
                        Some("x") => d.other_on = true,
                        Some(v) => {
                            if let Some(id) = v
                                .strip_prefix("o:")
                                .filter(|id| b.options.iter().any(|o| o.id == *id))
                            {
                                d.ticked.push(id.to_string());
                            }
                        }
                        None => {}
                    }
                }
                BlockKind::Checkbox => {
                    for o in &b.options {
                        if f.remove(&format!("b{n}o_{}", o.id)).is_some() {
                            d.ticked.push(o.id.clone());
                        }
                    }
                    d.other_on = f.remove(&format!("b{n}x")).is_some();
                }
                BlockKind::Text => {}
            }
            let text = f.remove(&format!("b{n}t")).unwrap_or_default();
            if b.kind == BlockKind::Text {
                d.text = text;
            } else {
                d.other_text = text;
            }
            blocks.push(d);
        }
        Draft {
            blocks,
            ask_text: String::new(),
            ask_error: None,
        }
    }

    /// The empty draft of a sheet.
    pub fn empty(sheet: &Sheet) -> Draft {
        Draft {
            blocks: sheet.blocks.iter().map(|_| BlockDraft::default()).collect(),
            ask_text: String::new(),
            ask_error: None,
        }
    }

    /// Checks each block with the rules of the server, and sets the mistake of each block that breaks one. The result is
    /// the answer when every block is right. The text is never changed.
    pub fn answer(&mut self, sheet: &Sheet) -> Option<OwnerAnswer> {
        let mut parts: Vec<(Option<Vec<String>>, Option<String>)> = Vec::new();
        let mut ok = true;
        for (b, d) in sheet.blocks.iter().zip(self.blocks.iter_mut()) {
            d.error = None;
            // A text in the field "Other answer" with nothing else chosen is the free answer: the field says so.
            if b.other
                && b.kind != BlockKind::Text
                && d.ticked.is_empty()
                && !d.other_text.trim().is_empty()
            {
                d.other_on = true;
            }
            match check_block(b, d) {
                Ok(part) => parts.push(part),
                Err(why) => {
                    d.error = Some(why);
                    ok = false;
                }
            }
        }
        if !ok {
            return None;
        }
        if !sheet.is_form {
            let (choice, text) = parts.pop()?;
            return Some(OwnerAnswer {
                choice,
                text,
                intent: None,
                answers: None,
            });
        }
        let answers = sheet
            .blocks
            .iter()
            .zip(parts)
            .map(|(b, (choice, text))| FormAnswer {
                question_id: b.id.clone().unwrap_or_default(),
                choice,
                text,
            })
            .collect();
        Some(OwnerAnswer {
            choice: None,
            text: None,
            intent: None,
            answers: Some(answers),
        })
    }
}

/// A text that is an answer: 1 to 2000 characters, no NUL, and not only spaces. The text is not changed.
fn text_rule(text: &str) -> Result<(), String> {
    let n = text.chars().count();
    if text.trim().is_empty() {
        return Err("Type your answer. An empty text is not an answer.".into());
    }
    if n > MAX_TEXT_CHARS {
        return Err(format!(
            "The text has {n} characters. The limit is {MAX_TEXT_CHARS}."
        ));
    }
    if text.contains('\0') {
        return Err("The text has a character that is not allowed.".into());
    }
    Ok(())
}

type Part = (Option<Vec<String>>, Option<String>);

fn check_block(b: &Block, d: &BlockDraft) -> Result<Part, String> {
    if b.kind == BlockKind::Text {
        text_rule(&d.text)?;
        return Ok((None, Some(d.text.clone())));
    }
    if d.other_on {
        if !b.other {
            return Err("This question takes no answer of your own.".into());
        }
        if !d.ticked.is_empty() {
            return Err("Your own answer replaces the choices. Clear the choices or select Other answer only.".into());
        }
        text_rule(&d.other_text)?;
        return Ok((None, Some(d.other_text.clone())));
    }
    if !d.other_text.is_empty() {
        return Err("Select Other answer to send this text, or clear the text.".into());
    }
    let n = d.ticked.len();
    let (min, max) = (b.min as usize, b.max as usize);
    match b.kind {
        BlockKind::Radio | BlockKind::Confirm if n != 1 => Err("Choose one.".into()),
        BlockKind::Checkbox if n < min || n > max => Err(if min == max {
            format!("Choose {min}.")
        } else if n < min {
            format!("Choose at least {min}.")
        } else {
            format!("Choose at most {max}.")
        }),
        _ => {
            // The ticks go in the order of the options.
            let ordered: Vec<String> = b
                .options
                .iter()
                .filter(|o| d.ticked.contains(&o.id))
                .map(|o| o.id.clone())
                .collect();
            Ok((Some(ordered), None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inbox_core::proto::{FormQuestion, Input};

    const TYPES: [InputType; 5] = [
        InputType::Choice,
        InputType::Confirm,
        InputType::Text,
        InputType::None,
        InputType::Form,
    ];

    fn choice_input(multiple: bool, min: u8, max: u8) -> Input {
        Input {
            kind: InputType::Choice,
            multiple,
            min,
            max,
            options: ["a", "b", "c"]
                .iter()
                .map(|id| InputOption {
                    id: (*id).into(),
                    label: id.to_uppercase(),
                    description: None,
                })
                .collect(),
            placeholder: None,
            questions: None,
        }
    }

    fn sheet_of(input: &Input, custom: bool) -> Sheet {
        let r = rows(input.kind, custom, false, Interaction::Conversational, true);
        Sheet::of(input, &r).expect("a sheet")
    }

    fn form(f: &mut HashMap<String, String>, pairs: &[(&str, &str)]) {
        for (k, v) in pairs {
            f.insert((*k).into(), (*v).into());
        }
    }

    #[test]
    fn the_four_rows_of_the_matrix_for_a_choice_and_a_confirm() {
        for kind in [InputType::Choice, InputType::Confirm] {
            let c = Interaction::Conversational;
            assert!(rows(kind, false, false, c, true).closed());
            let only_custom = rows(kind, true, false, c, true);
            assert!(only_custom.other_answer && !only_custom.ask_agent);
            let only_ask = rows(kind, false, true, c, true);
            assert!(!only_ask.other_answer && only_ask.ask_agent);
            let both = rows(kind, true, true, c, true);
            assert!(both.other_answer && both.ask_agent && both.invalid.is_none());
        }
    }

    /// 5 types x 4 flag pairs x 2 interactions = 40 combinations, and 17 of them are valid (as `FlagsMatrixRowsTest`).
    #[test]
    fn seventeen_of_the_forty_combinations_are_valid_and_the_others_show_no_row() {
        let mut valid = 0;
        for kind in TYPES {
            for custom in [false, true] {
                for ask in [false, true] {
                    for who in [Interaction::Conversational, Interaction::OneShot] {
                        let r = rows(kind, custom, ask, who, true);
                        if r.invalid.is_some() {
                            assert!(r.closed(), "{kind:?} {custom} {ask} {who:?}");
                        } else {
                            valid += 1;
                            assert!(!r.other_answer || custom);
                            assert!(!r.ask_agent || (ask && who == Interaction::Conversational));
                        }
                    }
                }
            }
        }
        assert_eq!(valid, 17);
    }

    #[test]
    fn a_text_question_has_no_other_row_and_a_form_needs_a_choice_for_it() {
        let c = Interaction::Conversational;
        assert_eq!(
            rows(InputType::Text, true, false, c, true).invalid,
            Some(Invalid::TextWithCustom)
        );
        assert!(rows(InputType::Text, false, true, c, true).ask_agent);
        assert_eq!(
            rows(InputType::Form, true, false, c, false).invalid,
            Some(Invalid::FormCustomWithoutChoice)
        );
        assert!(rows(InputType::Form, false, true, c, false).ask_agent);
        assert_eq!(
            rows(InputType::None, true, false, c, true).invalid,
            Some(Invalid::NoneWithFlag)
        );
    }

    #[test]
    fn a_radio_takes_one_option_and_keeps_the_text_that_was_typed() {
        let input = choice_input(false, 1, 1);
        let sheet = sheet_of(&input, true);
        let mut f = HashMap::new();
        form(&mut f, &[("b0", "o:b")]);
        let mut d = Draft::read(&sheet, &mut f);
        assert!(f.is_empty());
        let a = d.answer(&sheet).expect("an answer");
        assert_eq!(a.choice.as_deref(), Some(&["b".to_string()][..]));
        assert!(a.text.is_none());

        let mut f = HashMap::new();
        form(&mut f, &[("b0", "x"), ("b0t", "  not before 18:00 ")]);
        let mut d = Draft::read(&sheet, &mut f);
        let a = d.answer(&sheet).expect("a free answer");
        assert!(a.choice.is_none());
        assert_eq!(
            a.text.as_deref(),
            Some("  not before 18:00 "),
            "not changed"
        );

        let mut f = HashMap::new();
        form(&mut f, &[("b0", "x"), ("b0t", "   ")]);
        let mut d = Draft::read(&sheet, &mut f);
        assert!(d.answer(&sheet).is_none());
        assert!(d.blocks[0].error.as_deref().unwrap().contains("empty text"));
        assert!(d.blocks[0].other_on);
    }

    #[test]
    fn a_text_over_the_limit_is_refused_and_stays_in_the_draft() {
        let input = choice_input(false, 1, 1);
        let sheet = sheet_of(&input, true);
        let long = "é".repeat(MAX_TEXT_CHARS + 1);
        let mut f = HashMap::new();
        form(&mut f, &[("b0", "x"), ("b0t", &long)]);
        let mut d = Draft::read(&sheet, &mut f);
        assert!(d.answer(&sheet).is_none());
        assert!(d.blocks[0].error.as_deref().unwrap().contains("2001"));
        assert_eq!(d.blocks[0].other_text, long);
        let exact = "é".repeat(MAX_TEXT_CHARS);
        let mut f = HashMap::new();
        form(&mut f, &[("b0", "x"), ("b0t", &exact)]);
        assert!(Draft::read(&sheet, &mut f).answer(&sheet).is_some());
    }

    #[test]
    fn other_and_the_ticks_exclude_each_other_and_a_text_with_a_tick_is_not_sent() {
        let input = choice_input(true, 1, 2);
        let sheet = sheet_of(&input, true);
        let mut f = HashMap::new();
        form(&mut f, &[("b0o_a", "1"), ("b0x", "1"), ("b0t", "mine")]);
        let mut d = Draft::read(&sheet, &mut f);
        assert!(d.answer(&sheet).is_none());
        assert!(d.blocks[0].error.as_deref().unwrap().contains("replaces"));
        // A text with a tick is two answers: the owner decides, the page does not guess.
        let mut f = HashMap::new();
        form(&mut f, &[("b0o_a", "1"), ("b0t", "mine")]);
        let mut d = Draft::read(&sheet, &mut f);
        assert!(d.answer(&sheet).is_none());
        assert!(d.blocks[0]
            .error
            .as_deref()
            .unwrap()
            .contains("Select Other"));
        // A text with nothing else chosen is the free answer, and the page selects the row.
        let mut f = HashMap::new();
        form(&mut f, &[("b0t", "mine")]);
        let mut d = Draft::read(&sheet, &mut f);
        let a = d.answer(&sheet).expect("the free answer");
        assert_eq!(a.text.as_deref(), Some("mine"));
        assert!(d.blocks[0].other_on);
    }

    #[test]
    fn a_checkbox_follows_min_and_max_and_the_ticks_keep_the_order_of_the_options() {
        let input = choice_input(true, 2, 2);
        let sheet = sheet_of(&input, false);
        let mut f = HashMap::new();
        form(&mut f, &[("b0o_c", "1"), ("b0o_a", "1")]);
        let a = Draft::read(&sheet, &mut f).answer(&sheet).unwrap();
        assert_eq!(a.choice.unwrap(), vec!["a".to_string(), "c".to_string()]);
        let mut f = HashMap::new();
        form(&mut f, &[("b0o_c", "1")]);
        let mut d = Draft::read(&sheet, &mut f);
        assert!(d.answer(&sheet).is_none());
        assert_eq!(d.blocks[0].error.as_deref(), Some("Choose 2."));
    }

    #[test]
    fn a_confirm_gives_yes_or_no_and_a_text_question_gives_the_text() {
        let confirm = Input {
            kind: InputType::Confirm,
            multiple: false,
            min: 0,
            max: 0,
            options: vec![],
            placeholder: None,
            questions: None,
        };
        let sheet = sheet_of(&confirm, false);
        let mut f = HashMap::new();
        form(&mut f, &[("b0", "o:no")]);
        let a = Draft::read(&sheet, &mut f).answer(&sheet).unwrap();
        assert_eq!(a.choice.unwrap(), vec!["no".to_string()]);
        let text = Input {
            kind: InputType::Text,
            ..confirm
        };
        let sheet = sheet_of(&text, false);
        let mut f = HashMap::new();
        form(&mut f, &[("b0t", "every night")]);
        let a = Draft::read(&sheet, &mut f).answer(&sheet).unwrap();
        assert_eq!(a.text.as_deref(), Some("every night"));
        assert!(a.choice.is_none());
    }

    #[test]
    fn a_form_needs_one_answer_for_each_block() {
        let q = |id: &str, kind: FormQuestionType, multiple: bool, min: u8, max: u8| FormQuestion {
            id: id.into(),
            header: id.to_uppercase(),
            text: "text".into(),
            kind,
            multiple,
            min,
            max,
            options: if kind == FormQuestionType::Choice {
                choice_input(false, 1, 1).options
            } else {
                vec![]
            },
            placeholder: None,
        };
        let input = Input {
            kind: InputType::Form,
            multiple: false,
            min: 0,
            max: 0,
            options: vec![],
            placeholder: None,
            questions: Some(vec![
                q("one", FormQuestionType::Choice, false, 1, 1),
                q("two", FormQuestionType::Text, false, 0, 0),
            ]),
        };
        let sheet = sheet_of(&input, true);
        assert!(sheet.is_form && sheet.blocks.len() == 2);
        assert!(sheet.blocks[0].other && !sheet.blocks[1].other);
        let mut f = HashMap::new();
        form(&mut f, &[("b0", "o:a")]);
        let mut d = Draft::read(&sheet, &mut f);
        assert!(d.answer(&sheet).is_none());
        assert!(d.blocks[0].error.is_none() && d.blocks[1].error.is_some());
        let mut f = HashMap::new();
        form(&mut f, &[("b0", "o:a"), ("b1t", "later")]);
        let a = Draft::read(&sheet, &mut f).answer(&sheet).unwrap();
        let list = a.answers.unwrap();
        assert_eq!(list[0].question_id, "one");
        assert_eq!(list[1].text.as_deref(), Some("later"));
        assert!(a.choice.is_none() && a.text.is_none());
    }

    #[test]
    fn a_message_with_no_answer_has_no_sheet() {
        let none = Input {
            kind: InputType::None,
            multiple: false,
            min: 0,
            max: 0,
            options: vec![],
            placeholder: None,
            questions: None,
        };
        let r = rows(
            InputType::None,
            false,
            false,
            Interaction::Conversational,
            true,
        );
        assert!(Sheet::of(&none, &r).is_none());
    }
}
