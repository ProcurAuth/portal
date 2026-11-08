//! The answer to a form: the check of the answers against the questions that the item stores.
//!
//! The parser of the frame checks the shape of `answers` only: the number of entries, the form of the ids, a
//! `choice` or a `text` in each entry. The ids are known when the item is known. The server calls
//! [`validate_form_answer`] with the form that it holds for the item, before it takes the answer. A phone can call it
//! too, to show the mistake before it sends.

use crate::proto::{FormAnswer, FormQuestion, FormQuestionType, MAX_USER_TEXT_CHARS};

/// Why a set of answers does not fit a form. Each variant names the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormAnswerError {
    /// The form has no question, or more than the protocol allows.
    NoForm,
    /// A question has no answer.
    Missing(String),
    /// An answer names a question that the form does not have.
    UnknownQuestion(String),
    /// Two answers name the same question.
    Duplicate(String),
    /// An answer has neither `choice` nor `text`, or an empty one.
    Empty(String),
    /// A choice question: the number of ticks is outside `min..=max`, or a tick repeats.
    ChoiceCount(String),
    /// A tick names an option that the question does not have.
    UnknownOption { question: String, option: String },
    /// A text question: the answer has `choice`.
    ChoiceOnText(String),
    /// A choice question: the answer is a text, and the item does not allow a free answer (`allow_custom`).
    CustomNotAllowed(String),
    /// A choice question: the answer has ticks and a text. The free answer is exclusive of the ticks.
    CustomWithChoices(String),
    /// A text is longer than the limit of a text of the user, or it holds a NUL.
    TextInvalid(String),
}

impl std::fmt::Display for FormAnswerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormAnswerError::NoForm => write!(f, "the item has no form"),
            FormAnswerError::Missing(q) => write!(f, "question {q}: no answer"),
            FormAnswerError::UnknownQuestion(q) => write!(f, "question {q}: not in the form"),
            FormAnswerError::Duplicate(q) => write!(f, "question {q}: answered more than once"),
            FormAnswerError::Empty(q) => write!(f, "question {q}: the answer is empty"),
            FormAnswerError::ChoiceCount(q) => write!(f, "question {q}: wrong number of ticks"),
            FormAnswerError::UnknownOption { question, option } => {
                write!(f, "question {question}: unknown option {option}")
            }
            FormAnswerError::ChoiceOnText(q) => {
                write!(f, "question {q}: a text question takes a text")
            }
            FormAnswerError::CustomNotAllowed(q) => {
                write!(f, "question {q}: a free answer is not allowed")
            }
            FormAnswerError::CustomWithChoices(q) => {
                write!(f, "question {q}: a free answer excludes the ticks")
            }
            FormAnswerError::TextInvalid(q) => write!(f, "question {q}: the text is not valid"),
        }
    }
}

impl std::error::Error for FormAnswerError {}

/// Checks the answers of a form against its questions.
///
/// - Every question has exactly one answer, and every answer names a question.
/// - A choice question takes ticks (`min..=max`, known option ids, no repeat) or, with `allow_custom`, one free text
///   that is exclusive of the ticks.
/// - A text question takes a text of 1 to 8000 characters.
///
/// `allow_custom` is the flag of the item, not of a question: it adds a free answer to each choice question.
pub fn validate_form_answer(
    questions: &[FormQuestion],
    allow_custom: bool,
    answers: &[FormAnswer],
) -> Result<(), FormAnswerError> {
    if questions.is_empty() || questions.len() > crate::proto::MAX_FORM_QUESTIONS {
        return Err(FormAnswerError::NoForm);
    }
    let mut seen = std::collections::HashSet::new();
    for a in answers {
        if !questions.iter().any(|q| q.id == a.question_id) {
            return Err(FormAnswerError::UnknownQuestion(a.question_id.clone()));
        }
        if !seen.insert(a.question_id.as_str()) {
            return Err(FormAnswerError::Duplicate(a.question_id.clone()));
        }
    }
    for q in questions {
        let Some(a) = answers.iter().find(|a| a.question_id == q.id) else {
            return Err(FormAnswerError::Missing(q.id.clone()));
        };
        let id = || q.id.clone();
        if let Some(t) = &a.text {
            if t.is_empty() {
                return Err(FormAnswerError::Empty(id()));
            }
            if t.chars().count() > MAX_USER_TEXT_CHARS || t.contains('\0') {
                return Err(FormAnswerError::TextInvalid(id()));
            }
        }
        match q.kind {
            FormQuestionType::Text => {
                if a.choice.is_some() {
                    return Err(FormAnswerError::ChoiceOnText(id()));
                }
                if a.text.is_none() {
                    return Err(FormAnswerError::Empty(id()));
                }
            }
            FormQuestionType::Choice => match (&a.choice, &a.text) {
                (None, None) => return Err(FormAnswerError::Empty(id())),
                (Some(c), Some(_)) if !c.is_empty() => {
                    return Err(FormAnswerError::CustomWithChoices(id()));
                }
                (_, Some(_)) => {
                    if !allow_custom {
                        return Err(FormAnswerError::CustomNotAllowed(id()));
                    }
                }
                (Some(c), None) => {
                    let (min, max) = (q.min as usize, q.max as usize);
                    let mut ticks = std::collections::HashSet::new();
                    if c.len() < min || c.len() > max || !c.iter().all(|o| ticks.insert(o.as_str()))
                    {
                        return Err(FormAnswerError::ChoiceCount(id()));
                    }
                    if let Some(unknown) = c.iter().find(|o| !q.options.iter().any(|x| &x.id == *o))
                    {
                        return Err(FormAnswerError::UnknownOption {
                            question: id(),
                            option: unknown.clone(),
                        });
                    }
                }
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::InputOption;

    fn opts(ids: &[&str]) -> Vec<InputOption> {
        ids.iter()
            .map(|i| InputOption { id: (*i).into(), label: i.to_uppercase(), description: None })
            .collect()
    }

    fn choice(id: &str, multiple: bool, min: u8, max: u8) -> FormQuestion {
        FormQuestion {
            id: id.into(),
            header: id.into(),
            text: format!("Question {id}?"),
            kind: FormQuestionType::Choice,
            multiple,
            min,
            max,
            options: opts(&["a", "b", "c"]),
            placeholder: None,
        }
    }

    fn text(id: &str) -> FormQuestion {
        FormQuestion {
            id: id.into(),
            header: id.into(),
            text: "Say it.".into(),
            kind: FormQuestionType::Text,
            multiple: false,
            min: 0,
            max: 0,
            options: vec![],
            placeholder: None,
        }
    }

    fn ticks(id: &str, ids: &[&str]) -> FormAnswer {
        FormAnswer {
            question_id: id.into(),
            choice: Some(ids.iter().map(|s| s.to_string()).collect()),
            text: None,
        }
    }

    fn said(id: &str, t: &str) -> FormAnswer {
        FormAnswer { question_id: id.into(), choice: None, text: Some(t.into()) }
    }

    fn form() -> Vec<FormQuestion> {
        vec![choice("one", false, 1, 1), choice("many", true, 1, 2), text("why")]
    }

    #[test]
    fn a_complete_answer_passes() {
        let answers = [ticks("one", &["a"]), ticks("many", &["b", "c"]), said("why", "because")];
        assert_eq!(validate_form_answer(&form(), false, &answers), Ok(()));
        // The order of the answers does not count.
        let answers = [said("why", "x"), ticks("many", &["a"]), ticks("one", &["c"])];
        assert_eq!(validate_form_answer(&form(), false, &answers), Ok(()));
    }

    #[test]
    fn table_of_refusals() {
        use FormAnswerError as E;
        let good = || vec![ticks("one", &["a"]), ticks("many", &["b"]), said("why", "x")];
        let replace = |i: usize, a: FormAnswer| {
            let mut v = good();
            v[i] = a;
            v
        };
        let cases: Vec<(&str, Vec<FormAnswer>, bool, E)> = vec![
            ("missing answer", good()[..2].to_vec(), false, E::Missing("why".into())),
            ("no answer at all", vec![], false, E::Missing("one".into())),
            (
                "unknown question",
                [good(), vec![said("zz", "x")]].concat(),
                false,
                E::UnknownQuestion("zz".into()),
            ),
            (
                "duplicate",
                [good(), vec![ticks("one", &["b"])]].concat(),
                false,
                E::Duplicate("one".into()),
            ),
            (
                "too few ticks",
                replace(
                    1,
                    FormAnswer { question_id: "many".into(), choice: Some(vec![]), text: None },
                ),
                false,
                E::ChoiceCount("many".into()),
            ),
            (
                "too many ticks (single)",
                replace(0, ticks("one", &["a", "b"])),
                false,
                E::ChoiceCount("one".into()),
            ),
            (
                "too many ticks (multiple)",
                replace(1, ticks("many", &["a", "b", "c"])),
                false,
                E::ChoiceCount("many".into()),
            ),
            (
                "a tick repeats",
                replace(1, ticks("many", &["a", "a"])),
                false,
                E::ChoiceCount("many".into()),
            ),
            (
                "unknown option",
                replace(0, ticks("one", &["zz"])),
                false,
                E::UnknownOption { question: "one".into(), option: "zz".into() },
            ),
            (
                "ticks on a text question",
                replace(2, ticks("why", &["a"])),
                false,
                E::ChoiceOnText("why".into()),
            ),
            (
                "text on a choice question, custom off",
                replace(0, said("one", "other")),
                false,
                E::CustomNotAllowed("one".into()),
            ),
            (
                "empty answer",
                replace(2, FormAnswer { question_id: "why".into(), choice: None, text: None }),
                false,
                E::Empty("why".into()),
            ),
            ("empty text", replace(2, said("why", "")), false, E::Empty("why".into())),
            (
                "text too long",
                replace(2, said("why", &"x".repeat(8001))),
                false,
                E::TextInvalid("why".into()),
            ),
            ("text with NUL", replace(2, said("why", "a\0b")), false, E::TextInvalid("why".into())),
            (
                "custom text with ticks",
                replace(
                    0,
                    FormAnswer {
                        question_id: "one".into(),
                        choice: Some(vec!["a".into()]),
                        text: Some("other".into()),
                    },
                ),
                true,
                E::CustomWithChoices("one".into()),
            ),
        ];
        for (name, answers, allow_custom, want) in cases {
            assert_eq!(validate_form_answer(&form(), allow_custom, &answers), Err(want), "{name}");
        }
    }

    #[test]
    fn custom_text_needs_allow_custom_and_is_exclusive() {
        let answers = vec![said("one", "my own"), ticks("many", &["a"]), said("why", "x")];
        assert_eq!(validate_form_answer(&form(), true, &answers), Ok(()));
        assert_eq!(
            validate_form_answer(&form(), false, &answers),
            Err(FormAnswerError::CustomNotAllowed("one".into()))
        );
        // An empty list of ticks with a text is a free answer too.
        let with_empty = vec![
            FormAnswer {
                question_id: "one".into(),
                choice: Some(vec![]),
                text: Some("own".into()),
            },
            ticks("many", &["a"]),
            said("why", "x"),
        ];
        assert_eq!(validate_form_answer(&form(), true, &with_empty), Ok(()));
        // The limits of the ticks do not apply to a free answer.
        let many_custom =
            vec![ticks("one", &["a"]), said("many", "none of them"), said("why", "x")];
        assert_eq!(validate_form_answer(&form(), true, &many_custom), Ok(()));
    }

    #[test]
    fn an_item_without_a_form_is_refused() {
        assert_eq!(validate_form_answer(&[], false, &[]), Err(FormAnswerError::NoForm));
        let five: Vec<FormQuestion> = (0..5).map(|i| text(&format!("q{i}"))).collect();
        assert_eq!(validate_form_answer(&five, false, &[]), Err(FormAnswerError::NoForm));
    }

    #[test]
    fn every_error_names_its_question() {
        let e = FormAnswerError::UnknownOption { question: "q".into(), option: "o".into() };
        assert!(e.to_string().contains('q') && e.to_string().contains('o'));
    }
}
