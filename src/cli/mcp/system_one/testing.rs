//! Test doubles shared by the gate's unit tests.

use super::types::{Answer, Asker, Request, Response, SystemOneError};

/// An [`Asker`] backed by a closure.
pub(crate) struct FnAsker<F>(pub F);

impl<F> Asker for FnAsker<F>
where
    F: Fn(&Request) -> Result<Response, SystemOneError> + Send + Sync,
{
    async fn ask(&self, request: &Request) -> Result<Response, SystemOneError> {
        (self.0)(request)
    }
}

/// A response answering yes/no questions with the given probabilities.
pub(crate) fn yes_answers(pairs: &[(&str, f32)]) -> Response {
    let mut response = Response::default();
    for (name, p_yes) in pairs {
        response
            .answers
            .insert((*name).to_string(), Answer::YesNo { p_yes: *p_yes });
    }
    response
}

/// Alias used by the pre-gate tests.
pub(crate) fn answers_of(pairs: &[(&str, f32)]) -> Response {
    yes_answers(pairs)
}
