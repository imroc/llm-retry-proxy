use serde_json::Value;
use tracing_subscriber::EnvFilter;

/// Initialize tracing subscriber with the given log level.
///
/// Supports `RUST_LOG` env var override. If `RUST_LOG_FORMAT=json`, use JSON output.
pub fn init_tracing(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let format = std::env::var("RUST_LOG_FORMAT").unwrap_or_default();
    if format == "json" {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }
}

/// Best-effort extraction of the `model` field from a request body.
///
/// Works with OpenAI chat completions format (`{"model": "glm-latest", ...}`)
/// and Anthropic Messages format (`{"model": "claude-3", ...}`).
/// Returns `None` on any parse error.
pub fn extract_model(body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    value.get("model")?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_model_openai() {
        let body = br#"{"model": "glm-latest", "messages": []}"#;
        assert_eq!(extract_model(body), Some("glm-latest".into()));
    }

    #[test]
    fn test_extract_model_anthropic() {
        let body = br#"{"model": "claude-3", "max_tokens": 1024}"#;
        assert_eq!(extract_model(body), Some("claude-3".into()));
    }

    #[test]
    fn test_extract_model_empty() {
        assert_eq!(extract_model(b""), None);
    }

    #[test]
    fn test_extract_model_no_model_field() {
        let body = br#"{"foo": "bar"}"#;
        assert_eq!(extract_model(body), None);
    }

    #[test]
    fn test_extract_model_invalid_json() {
        assert_eq!(extract_model(b"not json"), None);
    }
}
