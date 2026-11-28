//! LEINDEX-arm unit tests: wire-faithful response text extraction.

use super::*;

#[test]
fn test_text_of_response_extracts_content_text() {
    let response = serde_json::json!({
        "content": [{ "type": "text", "text": "symbol found at line 12" }],
        "isError": false
    });
    assert_eq!(response_text(&response), "symbol found at line 12");
}

#[test]
fn test_text_of_response_error_form_matches_transport() {
    // The transport renders failures as text content with isError: true;
    // the arm must surface exactly that text (never unwrap to empty).
    let response = serde_json::json!({
        "content": [{ "type": "text", "text": "Error: no such tool: leindex_run" }],
        "isError": true
    });
    let text = response_text(&response);
    assert!(text.starts_with("Error:"), "got: {text}");
}

#[test]
fn test_unsupported_marker_is_the_loud_string() {
    assert_eq!(UNSUPPORTED_IN_THIS_BUILD, "UNSUPPORTED in this build");
}
