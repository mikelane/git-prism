use base64::{engine::general_purpose::STANDARD, Engine as _};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const CURSOR_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("invalid cursor: {0}")]
    InvalidEncoding(String),

    #[error("unsupported cursor version {got}, expected {expected}")]
    UnsupportedVersion { got: u32, expected: u32 },

    #[error("cursor is stale: repository has changed since the cursor was created")]
    StaleRepository,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PaginationCursor {
    pub v: u32,
    pub offset: usize,
    pub base_sha: String,
    pub head_sha: String,
}

pub fn encode_cursor(cursor: &PaginationCursor) -> String {
    let json = serde_json::to_string(cursor).expect("cursor serializes");
    STANDARD.encode(json.as_bytes())
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PaginationInfo {
    pub total_files: usize,
    pub page_start: usize,
    pub page_size: usize,
    pub next_cursor: Option<String>,
}

pub fn clamp_page_size(size: usize) -> usize {
    size.clamp(1, 500)
}

pub fn validate_cursor(
    cursor: &PaginationCursor,
    current_base_sha: &str,
    current_head_sha: &str,
) -> Result<(), CursorError> {
    if cursor.v != CURSOR_VERSION {
        return Err(CursorError::UnsupportedVersion {
            got: cursor.v,
            expected: CURSOR_VERSION,
        });
    }
    if cursor.base_sha != current_base_sha || cursor.head_sha != current_head_sha {
        return Err(CursorError::StaleRepository);
    }
    Ok(())
}

pub fn decode_cursor(s: &str) -> Result<PaginationCursor, CursorError> {
    let bytes = STANDARD
        .decode(s)
        .map_err(|e| CursorError::InvalidEncoding(e.to_string()))?;
    let json = String::from_utf8(bytes).map_err(|e| CursorError::InvalidEncoding(e.to_string()))?;
    serde_json::from_str(&json).map_err(|e| CursorError::InvalidEncoding(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cursor(offset: usize, base_sha: &str, head_sha: &str) -> PaginationCursor {
        PaginationCursor {
            v: CURSOR_VERSION,
            offset,
            base_sha: base_sha.into(),
            head_sha: head_sha.into(),
        }
    }

    #[test]
    fn decode_rejects_invalid_base64() {
        let result = decode_cursor("not-valid-base64!!!");
        assert!(matches!(result, Err(CursorError::InvalidEncoding(_))));
    }

    #[test]
    fn decode_rejects_valid_base64_but_invalid_json() {
        let encoded = STANDARD.encode(b"not json at all");
        let result = decode_cursor(&encoded);
        assert!(matches!(result, Err(CursorError::InvalidEncoding(_))));
    }

    #[test]
    fn decode_rejects_empty_string() {
        let result = decode_cursor("");
        assert!(result.is_err());
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let cursor = make_cursor(100, "abc123", "def456");
        let encoded = encode_cursor(&cursor);
        let decoded = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded.v, CURSOR_VERSION);
        assert_eq!(decoded.offset, 100);
        assert_eq!(decoded.base_sha, "abc123");
        assert_eq!(decoded.head_sha, "def456");
    }

    #[test]
    fn encode_then_decode_round_trips_different_values() {
        let cursor = make_cursor(
            0,
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
            "f6e5d4c3b2a1f6e5d4c3b2a1f6e5d4c3b2a1f6e5",
        );
        let encoded = encode_cursor(&cursor);
        let decoded = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded.offset, 0);
        assert_eq!(decoded.base_sha, cursor.base_sha);
        assert_eq!(decoded.head_sha, cursor.head_sha);
    }

    #[test]
    fn validate_cursor_succeeds_when_shas_match() {
        let cursor = make_cursor(50, "abc", "def");
        assert!(validate_cursor(&cursor, "abc", "def").is_ok());
    }

    #[test]
    fn validate_cursor_fails_when_base_sha_changed() {
        let cursor = make_cursor(50, "abc", "def");
        assert!(matches!(
            validate_cursor(&cursor, "DIFFERENT", "def"),
            Err(CursorError::StaleRepository)
        ));
    }

    #[test]
    fn validate_cursor_fails_when_head_sha_changed() {
        let cursor = make_cursor(50, "abc", "def");
        assert!(matches!(
            validate_cursor(&cursor, "abc", "DIFFERENT"),
            Err(CursorError::StaleRepository)
        ));
    }

    #[test]
    fn validate_cursor_rejects_wrong_version() {
        let cursor = PaginationCursor {
            v: 99,
            offset: 0,
            base_sha: "abc".into(),
            head_sha: "def".into(),
        };
        assert!(matches!(
            validate_cursor(&cursor, "abc", "def"),
            Err(CursorError::UnsupportedVersion {
                got: 99,
                expected: 1
            })
        ));
    }

    #[test]
    fn pagination_info_serializes_with_cursor() {
        let info = PaginationInfo {
            total_files: 300,
            page_start: 0,
            page_size: 100,
            next_cursor: Some("abc".into()),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["total_files"], 300);
        assert_eq!(json["page_start"], 0);
        assert_eq!(json["page_size"], 100);
        assert_eq!(json["next_cursor"], "abc");
    }

    #[test]
    fn pagination_info_serializes_null_cursor_on_last_page() {
        let info = PaginationInfo {
            total_files: 50,
            page_start: 0,
            page_size: 100,
            next_cursor: None,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json["next_cursor"].is_null());
    }

    #[test]
    fn clamp_page_size_caps_at_500() {
        assert_eq!(clamp_page_size(999), 500);
    }

    #[test]
    fn clamp_page_size_floors_at_1() {
        assert_eq!(clamp_page_size(0), 1);
    }

    #[test]
    fn clamp_page_size_passes_through_valid_values() {
        assert_eq!(clamp_page_size(50), 50);
        assert_eq!(clamp_page_size(100), 100);
        assert_eq!(clamp_page_size(500), 500);
        assert_eq!(clamp_page_size(1), 1);
    }
}
