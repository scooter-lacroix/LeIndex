use crate::{FormatMode, PhaseSelection};

use super::model::OrchestrationRequest;

/// Parse a light command string into an orchestration request.
///
/// Supported tokens:
/// - `all`
/// - `phase=<1..5>`
/// - `mode=<ultra|balanced|verbose>`
/// - `path=<path>`
pub fn parse_request(command: &str) -> Result<OrchestrationRequest, String> {
    let mut request = OrchestrationRequest::default();

    for token in command.split_whitespace() {
        if token.eq_ignore_ascii_case("all") {
            request.selection = PhaseSelection::All;
            continue;
        }

        if let Some(value) = token.strip_prefix("phase=") {
            let parsed = value
                .parse::<u8>()
                .map_err(|_| format!("invalid phase token: {token}"))?;
            request.selection = PhaseSelection::from_number(parsed)
                .ok_or_else(|| format!("phase must be in range 1..5, got {parsed}"))?;
            continue;
        }

        if let Some(value) = token.strip_prefix("mode=") {
            request.mode = Some(
                FormatMode::parse(value).ok_or_else(|| format!("invalid mode token: {value}"))?,
            );
            continue;
        }

        if let Some(value) = token.strip_prefix("path=") {
            request.path = Some(value.into());
        }
    }

    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_mode_and_path() {
        let request = parse_request("all mode=verbose path=.").expect("valid request");
        assert_eq!(request.selection, PhaseSelection::All);
        assert_eq!(request.mode, Some(FormatMode::Verbose));
        assert!(request.path.is_some());
    }

    #[test]
    fn rejects_invalid_phase() {
        let err = parse_request("phase=9").expect_err("must fail");
        assert!(err.contains("range"));
    }

    #[test]
    fn rejects_invalid_mode() {
        let err = parse_request("mode=fast").expect_err("must fail");
        assert!(err.contains("invalid mode"));
    }

    #[test]
    fn rejects_non_numeric_phase_token() {
        let err = parse_request("phase=abc").expect_err("must fail");
        assert!(err.contains("invalid phase token"));
    }
}
