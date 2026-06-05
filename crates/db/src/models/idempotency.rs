use sqlx::Error as SqlxError;

pub fn normalize_idempotency_key(key: Option<String>) -> Option<String> {
    key.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub fn is_unique_violation(err: &SqlxError) -> bool {
    matches!(err, SqlxError::Database(db_err) if db_err.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::normalize_idempotency_key;

    #[test]
    fn normalize_idempotency_key_trims_and_drops_blank_values() {
        assert_eq!(
            normalize_idempotency_key(Some(" key ".to_string())),
            Some("key".to_string())
        );
        assert_eq!(normalize_idempotency_key(Some(" \t\n ".to_string())), None);
        assert_eq!(normalize_idempotency_key(None), None);
    }
}
