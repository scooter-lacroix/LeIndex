//! Input validation utilities.

/// Validate an email address format.
pub fn validate_email(email: &str) -> bool {
    let parts: Vec<&str> = email.split('@').collect();
    parts.len() == 2 && !parts[0].is_empty() && parts[1].contains('.')
}

/// Validate a username.
pub fn validate_username(username: &str) -> Result<(), String> {
    if username.is_empty() {
        return Err("username cannot be empty".into());
    }
    if username.len() < 3 {
        return Err("username must be at least 3 characters".into());
    }
    if username.len() > 64 {
        return Err("username must be at most 64 characters".into());
    }
    if !username.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err("username can only contain alphanumeric characters and underscores".into());
    }
    Ok(())
}

/// Validate a project name.
pub fn validate_project_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("project name cannot be empty".into());
    }
    if name.len() > 128 {
        return Err("project name must be at most 128 characters".into());
    }
    Ok(())
}

/// Validate a document title.
pub fn validate_document_title(title: &str) -> Result<(), String> {
    if title.is_empty() {
        return Err("title cannot be empty".into());
    }
    if title.len() > 256 {
        return Err("title must be at most 256 characters".into());
    }
    Ok(())
}
