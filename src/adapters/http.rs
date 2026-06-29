//! HTTP client seam.
//!
//! The real implementation (`RealHttp`) is the only place in the crate that
//! touches `ureq` types. Network/HTTP/parse failures will be mapped to the
//! core source error by the `http-adapter`/`version-resolve` tasks; this
//! scaffold establishes the trait seam.

/// GET JSON / text from an upstream source. The orchestration uses this to
/// resolve the latest stable PowerShell release; tests inject a fake.
pub trait HttpClient {
    /// GET the URL and return the raw response body as text.
    fn get_text(&self, url: &str) -> anyhow::Result<String>;
}

/// Real blocking HTTP client over `ureq`. Sets a `User-Agent` (GitHub rejects
/// requests without one). Confined to this adapter — nothing else in the crate
/// touches `ureq` types.
pub struct RealHttp {
    agent: ureq::Agent,
    user_agent: String,
}

impl RealHttp {
    /// Construct a real client with the given `User-Agent`.
    pub fn new(user_agent: impl Into<String>) -> Self {
        Self {
            agent: ureq::Agent::new_with_defaults(),
            user_agent: user_agent.into(),
        }
    }
}

impl HttpClient for RealHttp {
    fn get_text(&self, url: &str) -> anyhow::Result<String> {
        let body = self
            .agent
            .get(url)
            .header("User-Agent", &self.user_agent)
            .call()?
            .body_mut()
            .read_to_string()?;
        Ok(body)
    }
}
