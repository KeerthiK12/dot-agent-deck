use std::time::Duration;

use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {body}")]
    ApiError { status: u16, body: String },
    #[error("Missing API key: set {env_var} environment variable")]
    MissingApiKey { env_var: String },
    #[error("Unexpected response format")]
    UnexpectedFormat,
    #[error("Unsupported provider: {0}")]
    UnsupportedProvider(String),
}

pub struct LlmRequest {
    pub system_prompt: String,
    pub user_message: String,
    pub provider: String,
    pub model: String,
}

/// Env vars consulted for the Anthropic key, in order. The prefixed name comes
/// first so a deck-specific key can be exported without shadowing
/// `ANTHROPIC_API_KEY`, which Claude Code claims for its own auth — an exported
/// `ANTHROPIC_API_KEY` overrides a Claude.ai subscription even when logged in.
/// The bare name stays as a fallback so existing setups keep working.
const ANTHROPIC_KEY_VARS: &[&str] = &["DOT_AGENT_DECK_ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY"];

const OPENAI_KEY_VARS: &[&str] = &["DOT_AGENT_DECK_OPENAI_API_KEY", "OPENAI_API_KEY"];

/// First non-empty value among `vars`. An empty value counts as unset, matching
/// how a shell export blanked out in a settings file behaves — sending an empty
/// key would otherwise produce a confusing 401 instead of "missing key".
fn env_api_key(vars: &[&str]) -> Result<String, LlmError> {
    vars.iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.trim().is_empty())
        .ok_or_else(|| LlmError::MissingApiKey {
            env_var: vars.join(" or "),
        })
}

pub async fn call_llm(request: &LlmRequest) -> Result<String, LlmError> {
    match request.provider.as_str() {
        "anthropic" => call_anthropic(request).await,
        "openai" => call_openai(request).await,
        "ollama" => call_ollama(request).await,
        other => Err(LlmError::UnsupportedProvider(other.to_string())),
    }
}

async fn call_anthropic(request: &LlmRequest) -> Result<String, LlmError> {
    let api_key = env_api_key(ANTHROPIC_KEY_VARS)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let body = json!({
        "model": request.model,
        "max_tokens": 1024,
        "system": [{"type": "text", "text": request.system_prompt}],
        "messages": [{"role": "user", "content": request.user_message}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        return Err(LlmError::ApiError { status, body });
    }

    let json: serde_json::Value = resp.json().await?;
    json["content"][0]["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or(LlmError::UnexpectedFormat)
}

async fn call_openai(request: &LlmRequest) -> Result<String, LlmError> {
    let api_key = env_api_key(OPENAI_KEY_VARS)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let body = json!({
        "model": request.model,
        "max_tokens": 1024,
        "messages": [
            {"role": "system", "content": request.system_prompt},
            {"role": "user", "content": request.user_message}
        ]
    });

    let resp = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        return Err(LlmError::ApiError { status, body });
    }

    let json: serde_json::Value = resp.json().await?;
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or(LlmError::UnexpectedFormat)
}

async fn call_ollama(request: &LlmRequest) -> Result<String, LlmError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;

    let prompt = format!("{}\n\n{}", request.system_prompt, request.user_message);
    let body = json!({
        "model": request.model,
        "prompt": prompt,
        "stream": false
    });

    let resp = client
        .post("http://localhost:11434/api/generate")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        return Err(LlmError::ApiError { status, body });
    }

    let json: serde_json::Value = resp.json().await?;
    json["response"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or(LlmError::UnexpectedFormat)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize env-var-mutating tests to avoid races.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const PREFIXED: &str = "DOT_AGENT_DECK_ANTHROPIC_API_KEY";
    const BARE: &str = "ANTHROPIC_API_KEY";

    /// Restores every var it touched on drop, so a failed assertion cannot leak
    /// a key into the rest of the suite.
    struct EnvGuard(Vec<(&'static str, Option<String>)>);

    impl EnvGuard {
        fn set(vars: &[(&'static str, Option<&str>)]) -> Self {
            let saved = vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in vars {
                restore(key, *value);
            }
            Self(saved)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                restore(key, value.as_deref());
            }
        }
    }

    fn restore(key: &str, value: Option<&str>) {
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn prefixed_anthropic_key_wins_over_bare() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::set(&[(PREFIXED, Some("deck-key")), (BARE, Some("claude-key"))]);

        assert_eq!(env_api_key(ANTHROPIC_KEY_VARS).unwrap(), "deck-key");
    }

    #[test]
    fn bare_anthropic_key_still_works_as_fallback() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::set(&[(PREFIXED, None), (BARE, Some("claude-key"))]);

        assert_eq!(env_api_key(ANTHROPIC_KEY_VARS).unwrap(), "claude-key");
    }

    /// A blanked-out export (the trick used to stop a coding agent from picking
    /// the key up) must not shadow the fallback.
    #[test]
    fn blank_prefixed_key_falls_through_to_bare() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::set(&[(PREFIXED, Some("   ")), (BARE, Some("claude-key"))]);

        assert_eq!(env_api_key(ANTHROPIC_KEY_VARS).unwrap(), "claude-key");
    }

    #[test]
    fn missing_anthropic_key_names_both_vars() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::set(&[(PREFIXED, None), (BARE, Some(""))]);

        let err = env_api_key(ANTHROPIC_KEY_VARS).unwrap_err();
        assert!(
            matches!(&err, LlmError::MissingApiKey { env_var } if env_var == "DOT_AGENT_DECK_ANTHROPIC_API_KEY or ANTHROPIC_API_KEY"),
            "unexpected error: {err}"
        );
    }
}
