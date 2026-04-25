//! `Config` impl for the async-openai client that supports keyless mode.
//!
//! `async-openai`'s built-in [`OpenAIConfig::headers`] unconditionally inserts
//! an `Authorization: Bearer <key>` header on every request — even when the
//! key is empty. Local/keyless servers (Ollama, LM Studio, llama.cpp, vLLM)
//! accept any token in practice, but some strict OpenAI-compatible servers
//! and corporate proxies reject empty bearers. To honor "no API key configured
//! ⇒ no Authorization header" cleanly, this module wraps `OpenAIConfig` and
//! strips the header when the provider was configured keyless.

use async_openai::config::{Config, OpenAIConfig};
use reqwest::header::{AUTHORIZATION, HeaderMap};
use secrecy::SecretString;

/// `Config` for the LLM client. Delegates to [`OpenAIConfig`] for everything
/// except `Authorization`, which is suppressed in keyless mode.
#[derive(Clone, Debug)]
pub struct LlmEndpointConfig {
    inner: OpenAIConfig,
    send_authorization: bool,
}

impl LlmEndpointConfig {
    /// Build a config that sends `Authorization: Bearer <api_key>`.
    pub fn with_key(base_url: &str, api_key: &str) -> Self {
        Self {
            inner: OpenAIConfig::new()
                .with_api_base(base_url)
                .with_api_key(api_key),
            send_authorization: true,
        }
    }

    /// Build a config that omits the `Authorization` header entirely. Use for
    /// local/keyless providers like Ollama where requiring a dummy env var
    /// just to satisfy validation is bad UX (steve-jhhw).
    ///
    /// Note: `OpenAIConfig::default()` pulls `OPENAI_API_KEY` from the env to
    /// seed its api_key field. We neutralize that by calling `with_api_key("")`
    /// — keyless mode must not leak an unrelated provider's key through the
    /// `api_key()` accessor.
    pub fn keyless(base_url: &str) -> Self {
        Self {
            inner: OpenAIConfig::new().with_api_base(base_url).with_api_key(""),
            send_authorization: false,
        }
    }
}

impl Config for LlmEndpointConfig {
    fn headers(&self) -> HeaderMap {
        let mut headers = self.inner.headers();
        if !self.send_authorization {
            headers.remove(AUTHORIZATION);
        }
        headers
    }

    fn url(&self, path: &str) -> String {
        self.inner.url(path)
    }

    fn api_base(&self) -> &str {
        self.inner.api_base()
    }

    fn api_key(&self) -> &SecretString {
        self.inner.api_key()
    }

    fn query(&self) -> Vec<(&str, &str)> {
        self.inner.query()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn keyless_config_omits_authorization_header() {
        // The whole point of steve-jhhw: keyless providers must never see an
        // Authorization header on the wire.
        let cfg = LlmEndpointConfig::keyless("http://localhost:11434/v1");
        let headers = cfg.headers();
        assert!(
            !headers.contains_key(AUTHORIZATION),
            "keyless mode must NOT send Authorization header, got: {headers:?}",
        );
    }

    #[test]
    fn with_key_config_includes_authorization_header() {
        let cfg = LlmEndpointConfig::with_key("https://api.openai.com/v1", "sk-test-key");
        let headers = cfg.headers();
        let auth = headers
            .get(AUTHORIZATION)
            .expect("with_key mode must send Authorization header");
        assert_eq!(auth.to_str().unwrap(), "Bearer sk-test-key");
    }

    #[test]
    fn api_base_propagates_from_inner() {
        let cfg = LlmEndpointConfig::keyless("http://localhost:8080/v1");
        assert_eq!(cfg.api_base(), "http://localhost:8080/v1");
    }

    #[test]
    fn api_key_returns_underlying_secret() {
        // Even in keyless mode, async-openai's Config trait requires returning
        // *some* SecretString — `OpenAIConfig`'s default is empty when no key
        // was supplied. We propagate that through so the trait stays honest.
        let cfg = LlmEndpointConfig::keyless("http://localhost/v1");
        assert!(cfg.api_key().expose_secret().is_empty());

        let cfg = LlmEndpointConfig::with_key("http://localhost/v1", "secret-value");
        assert_eq!(cfg.api_key().expose_secret(), "secret-value");
    }
}
