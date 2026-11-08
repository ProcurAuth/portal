//! Rendering of the text of an agent: a safe subset of Markdown and the color of code.
//!
//! The server renders them to HTML and the phone to spans, from the same tree and the same tokens. See
//! [`markdown::parse`] and [`highlight::tokens`].

pub mod highlight;
pub mod markdown;
