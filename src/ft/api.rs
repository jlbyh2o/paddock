//! HTTP client for a running FreeToken server.
//!
//! Only the control-plane routes matter here: `/health`, `/v1/stats`,
//! `/v1/cache/status`, `/v1/cache/rebuild`, `/v1/requests`, `/v1/models` and the raw
//! `/generate` smoke test. Every call has a short timeout — the UI polls on a timer and
//! must never be held up by a server that is busy loading 200 GiB of weights.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use super::types::*;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
}

/// True when the request never reached a server at all — nothing is listening on the
/// port, or the host refused the connection.
///
/// Worth separating from every other failure because the two mean opposite things about
/// whether anything is wrong: paddock polls an endpoint it does not require anyone to be
/// serving, so a refused connection is the expected state whenever the engine is stopped,
/// while a 500 or a decode failure is a fault whatever the engine is doing.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_connect)
}

impl Client {
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_millis(1500))
            .user_agent(concat!("paddock/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;
        Ok(Self { http, base: base_url.into().trim_end_matches('/').to_string() })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp =
            self.http.get(self.url(path)).send().await.with_context(|| format!("GET {path}"))?;
        let status = resp.status();
        let body = resp.bytes().await.with_context(|| format!("reading {path}"))?;
        if !status.is_success() {
            anyhow::bail!("{path} returned {status}: {}", snippet(&body));
        }
        serde_json::from_slice(&body)
            .with_context(|| format!("decoding {path}: {}", snippet(&body)))
    }

    async fn post_json<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        timeout: Option<Duration>,
    ) -> Result<T> {
        let mut req = self.http.post(self.url(path)).json(body);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await.with_context(|| format!("POST {path}"))?;
        let status = resp.status();
        let bytes = resp.bytes().await.with_context(|| format!("reading {path}"))?;
        if !status.is_success() {
            anyhow::bail!("{path} returned {status}: {}", snippet(&bytes));
        }
        serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding {path}: {}", snippet(&bytes)))
    }

    pub async fn health(&self) -> Result<Health> {
        self.get_json("/health").await
    }

    pub async fn stats(&self) -> Result<Stats> {
        self.get_json("/v1/stats").await
    }

    pub async fn cache_status(&self) -> Result<CacheStatus> {
        self.get_json("/v1/cache/status").await
    }

    pub async fn requests(&self, since: u64, limit: u32) -> Result<RequestPage> {
        self.get_json(&format!("/v1/requests?since={since}&limit={}", limit.clamp(1, 512))).await
    }

    /// Resize the live cache pools. The server only accepts this while the scheduler is
    /// idle, and the call blocks until the rebuild finishes, so it gets its own generous
    /// timeout rather than the polling one.
    pub async fn cache_rebuild(&self, req: &CacheRebuild) -> Result<serde_json::Value> {
        let timeout = Duration::from_secs_f64(req.timeout + 15.0);
        self.post_json("/v1/cache/rebuild", req, Some(timeout)).await
    }

    /// Raw completion, bypassing the chat template. Used by the Dashboard smoke test to
    /// prove the engine actually generates. Returns the streamed text.
    /// One chat completion, through the OpenAI-compatible route.
    ///
    /// `/generate` is the raw-completion smoke test and takes no chat template; every
    /// model paddock serves is instruction-tuned, so anything that wants an answer rather
    /// than a continuation has to go through the template `/v1/chat/completions` applies.
    ///
    /// The timeout is the caller's because this is the one request whose length is the
    /// model's decision: a long summary of a long diff is the request working, not hanging.
    pub async fn chat(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
        reasoning_effort: Option<&str>,
        timeout: Duration,
    ) -> Result<String> {
        #[derive(Serialize)]
        struct Message<'a> {
            role: &'a str,
            content: &'a str,
        }
        #[derive(Serialize)]
        struct Body<'a> {
            model: &'a str,
            messages: Vec<Message<'a>>,
            max_tokens: u32,
            temperature: f32,
            stream: bool,
            /// Graded by the checkpoint's own template; `/v1/models` advertises the
            /// vocabulary. Omitted rather than guessed when the caller has nothing to say,
            /// and harmless where a template does not read it — Jinja never sees an
            /// undeclared variable, which is why FreeToken broadcasts these rather than
            /// routing them per family.
            #[serde(skip_serializing_if = "Option::is_none")]
            reasoning_effort: Option<&'a str>,
        }
        let body = Body {
            model,
            messages: vec![
                Message { role: "system", content: system },
                Message { role: "user", content: user },
            ],
            max_tokens,
            // Low, not zero: this is a summary of a diff, where invention is the failure
            // mode and the wording is not the point.
            temperature: 0.2,
            stream: false,
            reasoning_effort,
        };
        let resp = self
            .http
            .post(self.url("/v1/chat/completions"))
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .context("POST /v1/chat/completions")?;
        let status = resp.status();
        let bytes = resp.bytes().await.context("reading /v1/chat/completions")?;
        if !status.is_success() {
            anyhow::bail!("/v1/chat/completions returned {status}: {}", snippet(&bytes));
        }
        let v: serde_json::Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding /v1/chat/completions: {}", snippet(&bytes)))?;
        let message = &v["choices"][0]["message"];
        let text = message["content"].as_str().unwrap_or_default().trim();
        if !text.is_empty() {
            return Ok(text.to_string());
        }
        // A reasoning model that spent its whole budget thinking answers with an empty
        // content and a full reasoning_content. Saying so beats returning nothing.
        if let Some(thought) =
            message["reasoning_content"].as_str().filter(|r| !r.trim().is_empty())
        {
            anyhow::bail!(
                "the model spent all {max_tokens} tokens reasoning ({} of them) without \
                 writing an answer",
                thought.split_whitespace().count(),
            );
        }
        anyhow::bail!("the model returned an empty answer");
    }

    pub async fn generate(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        #[derive(Serialize)]
        struct Body<'a> {
            prompt: &'a str,
            max_tokens: u32,
            ignore_eos: bool,
        }
        let resp = self
            .http
            .post(self.url("/generate"))
            .timeout(Duration::from_secs(120))
            .json(&Body { prompt, max_tokens, ignore_eos: false })
            .send()
            .await
            .context("POST /generate")?;
        let status = resp.status();
        let text = resp.text().await.context("reading /generate")?;
        if !status.is_success() {
            anyhow::bail!(
                "/generate returned {status}: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        Ok(collect_sse_text(&text))
    }
}

/// Body for `POST /v1/cache/rebuild`. Omitted pools keep their current size.
#[derive(Debug, Clone, Serialize)]
pub struct CacheRebuild {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moe_cache_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_pages: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_mamba_slots: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_swa_pages: Option<u64>,
    /// The server only supports `if_idle` today.
    pub mode: &'static str,
    pub timeout: f64,
}

impl Default for CacheRebuild {
    fn default() -> Self {
        Self {
            moe_cache_size: None,
            num_pages: None,
            num_mamba_slots: None,
            num_swa_pages: None,
            mode: "if_idle",
            timeout: 300.0,
        }
    }
}

impl CacheRebuild {
    pub fn is_empty(&self) -> bool {
        self.moe_cache_size.is_none()
            && self.num_pages.is_none()
            && self.num_mamba_slots.is_none()
            && self.num_swa_pages.is_none()
    }
}

/// Pull the generated text out of a `text/event-stream` body. FreeToken streams
/// `data: {"text": ...}` frames; the last frame carries the full text.
fn collect_sse_text(body: &str) -> String {
    let mut last = String::new();
    for line in body.lines() {
        let Some(payload) = line.strip_prefix("data:") else { continue };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
            if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
                last = t.to_string();
            }
        }
    }
    last
}

fn snippet(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_text_takes_the_final_cumulative_frame() {
        let body = "data: {\"text\": \"He\"}\n\ndata: {\"text\": \"Hello\"}\n\ndata: [DONE]\n\n";
        assert_eq!(collect_sse_text(body), "Hello");
    }

    #[test]
    fn rebuild_omits_untouched_pools() {
        let req = CacheRebuild { moe_cache_size: Some(256), ..Default::default() };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("moe_cache_size"));
        assert!(!json.contains("num_pages"));
    }
}
