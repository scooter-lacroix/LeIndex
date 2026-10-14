//! Privacy scrub for remote System-One requests.
//!
//! Remote providers receive evidence with absolute paths replaced by opaque
//! aliases (`pathref-N`). The same real path always gets the same alias within
//! one call, so repeated paths stay repeated and same-basename projects stay
//! distinct. The alias map lives only in this process and is never logged.
//!
//! Pick answers come back as aliases. [`PathScrub::unscrub_response`] restores a
//! choice only when it exactly equals an alias this call issued for an option;
//! any other choice passes through and the decoder rejects it as before.
//!
//! Known limit: an unquoted path with spaces is only partly covered (the span
//! ends at the first whitespace). Quote such paths, or send them as a whole
//! field value, which is aliased in full.

use super::types::{Answer, Question, Request, Response};
use serde_json::Value;
use std::collections::HashMap;

/// Alias map for one `ask` call. Built once so retries send the same mapping.
pub(super) struct PathScrub {
    prefix: String,
    /// Real path span -> alias issued for it.
    aliases: HashMap<String, String>,
    /// Issued alias of a Pick option value -> real value (only values that changed).
    choices: HashMap<String, String>,
}

impl PathScrub {
    /// The alias prefix is extended until no string in `request` contains it, so
    /// an alias can never equal pre-existing input.
    pub(super) fn new(request: &Request) -> Self {
        let mut texts = Vec::new();
        collect_strings(&request.state, &mut texts);
        for (name, question) in &request.questions {
            texts.push(name.as_str());
            match question {
                Question::YesNo { instructions } => texts.push(instructions),
                Question::Pick {
                    instructions,
                    options,
                } => {
                    texts.push(instructions);
                    for (value, description) in options {
                        texts.push(value);
                        texts.push(description);
                    }
                }
            }
        }
        let mut prefix = String::from("pathref");
        while texts.iter().any(|text| text.contains(prefix.as_str())) {
            prefix.push('x');
        }
        Self {
            prefix,
            aliases: HashMap::new(),
            choices: HashMap::new(),
        }
    }

    /// Copy of `request` with every path span aliased. Question names come from
    /// code and hold no paths, so they pass through unchanged.
    pub(super) fn scrub_request(&mut self, request: &Request) -> Request {
        let state = self.scrub_value(&request.state);
        let questions = request
            .questions
            .iter()
            .map(|(name, question)| (name.clone(), self.scrub_question(question)))
            .collect();
        Request { state, questions }
    }

    /// Restore Pick choices, and their probability keys, that exactly equal an
    /// alias issued for an option. Everything else is returned untouched.
    pub(super) fn unscrub_response(&self, mut response: Response) -> Response {
        for answer in response.answers.values_mut() {
            let Answer::Pick {
                choice,
                probabilities,
                ..
            } = answer
            else {
                continue;
            };
            if let Some(real) = self.choices.get(choice.as_str()) {
                choice.clone_from(real);
            }
            for (value, _) in probabilities.iter_mut() {
                if let Some(real) = self.choices.get(value.as_str()) {
                    value.clone_from(real);
                }
            }
        }
        response
    }

    fn scrub_question(&mut self, question: &Question) -> Question {
        match question {
            Question::YesNo { instructions } => Question::YesNo {
                instructions: self.scrub_field(instructions),
            },
            Question::Pick {
                instructions,
                options,
            } => Question::Pick {
                instructions: self.scrub_field(instructions),
                options: options
                    .iter()
                    .map(|(value, description)| {
                        let sanitized = self.scrub_field(value);
                        if sanitized != *value {
                            self.choices.insert(sanitized.clone(), value.clone());
                        }
                        (sanitized, self.scrub_field(description))
                    })
                    .collect(),
            },
        }
    }

    fn scrub_value(&mut self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.scrub_field(text)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.scrub_value(item)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, item)| (self.scrub_field(key), self.scrub_value(item)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Replace each path span in `text` with its alias; surrounding prose is kept.
    fn scrub_text(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (start, end) in path_spans(text) {
            out.push_str(&text[last..start]);
            let alias = self.alias(&text[start..end]);
            out.push_str(&alias);
            last = end;
        }
        out.push_str(&text[last..]);
        out
    }

    /// A value that is itself a path is aliased whole, so spaces inside it cannot
    /// split it. Any other text gets its path spans replaced in place.
    fn scrub_field(&mut self, text: &str) -> String {
        if is_path_field(text) {
            self.alias(text)
        } else {
            self.scrub_text(text)
        }
    }

    fn alias(&mut self, path: &str) -> String {
        if let Some(alias) = self.aliases.get(path) {
            return alias.clone();
        }
        let alias = format!("{}-{}", self.prefix, self.aliases.len() + 1);
        self.aliases.insert(path.to_string(), alias.clone());
        alias
    }
}

fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        Value::Object(map) => {
            for (key, item) in map {
                out.push(key);
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

/// Byte ranges of absolute, home-relative (`~/`), UNC, and Windows drive paths.
fn path_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let preceded_by = text[..cursor].chars().next_back();
        let span = if starts_token(preceded_by) {
            span_at(rest)
        } else {
            None
        };
        match span {
            Some((start, end)) => {
                spans.push((cursor + start, cursor + end));
                cursor += end;
            }
            None => cursor += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    spans
}

/// A path starts only where a token begins, so URLs, `word/word` separators,
/// and path tails are left alone.
fn starts_token(preceded_by: Option<char>) -> bool {
    !preceded_by.is_some_and(|c| {
        c.is_alphanumeric() || matches!(c, ':' | '/' | '\\' | '.' | '_' | '-' | '~')
    })
}

/// Span of the path starting at `rest`, as byte offsets into `rest`. A quoted
/// path runs to its closing quote, so spaces inside the quotes are covered.
fn span_at(rest: &str) -> Option<(usize, usize)> {
    if let Some(quote) = rest.chars().next().filter(|&c| matches!(c, '"' | '\'')) {
        let inner = &rest[1..];
        let close = inner.find(quote)?;
        let content = &inner[..close];
        return (path_head(content).is_some() && !content.contains('\n')).then_some((1, 1 + close));
    }
    path_len(rest).map(|len| (0, len))
}

/// A whole value that starts with a path head is one path, spaces included.
fn is_path_field(text: &str) -> bool {
    path_head(text).is_some() && !text.contains('\n')
}

/// Length of the path head at `rest`: `~/`, a drive prefix (`C:\`, `C:/`), a UNC
/// prefix (`\\server`, `//server`), or a bare root `/`.
fn path_head(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    if rest.starts_with("~/") {
        Some(2)
    } else if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        Some(3)
    } else if bytes.len() >= 3
        && bytes[0] == bytes[1]
        && matches!(bytes[0], b'\\' | b'/')
        && bytes[2] != bytes[0]
    {
        Some(2)
    } else if bytes.first() == Some(&b'/') {
        Some(1)
    } else {
        None
    }
}

/// Length of the unquoted path span starting at `rest`. The span ends at the
/// first whitespace, so `/home/Alice Smith/x` covers only `/home/Alice`.
/// Quoted paths and whole-field values are the supported ways to cover spaces.
fn path_len(rest: &str) -> Option<usize> {
    let head = path_head(rest)?;
    let bytes = rest.as_bytes();
    let body = rest[head..]
        .find(|c: char| !is_path_char(c))
        .map_or(rest.len(), |i| head + i);
    let mut end = body;
    while end > head && matches!(bytes[end - 1], b'.' | b':') {
        end -= 1;
    }
    rest[head..end]
        .chars()
        .any(|c| !matches!(c, '/' | '\\'))
        .then_some(end)
}

fn is_path_char(c: char) -> bool {
    !(c.is_whitespace()
        || matches!(
            c,
            '"' | '\''
                | '`'
                | '<'
                | '>'
                | '|'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | ','
                | ';'
                | '!'
                | '?'
                | '*'
        ))
}

#[cfg(test)]
#[path = "scrub_test.rs"]
mod scrub_test;
