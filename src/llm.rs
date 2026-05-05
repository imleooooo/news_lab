use crate::config::{LLMProvider, LLMSettings};
use anyhow::Result;
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestMessage, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs,
    },
    Client,
};
use log::warn;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::time::Instant;

const MAX_BACKOFF_SHIFT: u32 = 6; // 64s
const MAX_BACKOFF_SECS: u64 = 64;
const DEFAULT_PROGRESS_INTERVAL_SECS: u64 = 10;

static DEBUG_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_debug_mode(enabled: bool) {
    DEBUG_MODE.store(enabled, Ordering::Relaxed);
}

pub fn debug_mode_enabled() -> bool {
    DEBUG_MODE.load(Ordering::Relaxed)
}

fn debug_progress(msg: &str) {
    if debug_mode_enabled() {
        println!("  [debug] {msg}");
    }
}

pub struct LLMClient {
    client: Client<OpenAIConfig>,
    pub model: String,
    provider: LLMProvider,
    prompt_tokens: Arc<AtomicU64>,
    completion_tokens: Arc<AtomicU64>,
}

impl LLMClient {
    pub fn new(settings: &LLMSettings) -> Result<Self> {
        let mut openai_config = OpenAIConfig::new().with_api_key(settings.api_key.clone());
        if let LLMProvider::Custom { base_url } = &settings.provider {
            openai_config = openai_config.with_api_base(base_url.clone());
        }
        let client = Client::with_config(openai_config);
        Ok(Self {
            client,
            model: settings.model.clone(),
            provider: settings.provider.clone(),
            prompt_tokens: Arc::new(AtomicU64::new(0)),
            completion_tokens: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn reset_usage(&self) {
        self.prompt_tokens.store(0, Ordering::Relaxed);
        self.completion_tokens.store(0, Ordering::Relaxed);
    }

    /// Returns (prompt_tokens, completion_tokens, estimated_cost_usd).
    pub fn usage(&self) -> (u64, u64, f64) {
        let p = self.prompt_tokens.load(Ordering::Relaxed);
        let c = self.completion_tokens.load(Ordering::Relaxed);
        (p, c, model_cost(&self.model, p, c))
    }

    /// `max_tokens`: max_completion_tokens to request.
    /// Use a larger value (e.g. 16384) for complex outputs like radar JSON.
    /// Retries on transient network / API errors with configurable timeout.
    pub async fn invoke_with_limit(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let timeout_secs = env_u64("LLM_TIMEOUT_SECS", 3600);
        let max_retries = env_u32("LLM_MAX_RETRIES", 3).max(1);
        let progress_interval_secs =
            env_u64("LLM_PROGRESS_INTERVAL_SECS", DEFAULT_PROGRESS_INTERVAL_SECS);
        let mut last_err = anyhow::anyhow!("no attempts");

        for attempt in 0..max_retries {
            if attempt > 0 {
                let shift = (attempt - 1).min(MAX_BACKOFF_SHIFT);
                let wait = (1u64 << shift).min(MAX_BACKOFF_SECS); // 1..64s (capped)
                warn!(
                    "[llm] 第 {}/{} 次重試，等待 {}s...",
                    attempt + 1,
                    max_retries,
                    wait
                );
                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
            debug_progress(&format!(
                "LLM attempt {}/{} 開始 (provider={}, model={}, prompt_chars={}, max_tokens={}, timeout={}s)",
                attempt + 1,
                max_retries,
                self.provider_label(),
                self.model,
                prompt.chars().count(),
                max_tokens,
                timeout_secs
            ));

            // Rebuild per iteration: both user_msg and request are consumed by the call.
            let user_msg: ChatCompletionRequestMessage =
                match ChatCompletionRequestUserMessageArgs::default()
                    .content(prompt)
                    .build()
                {
                    Ok(m) => m.into(),
                    Err(e) => return Err(e.into()), // deterministic build failure
                };

            let request = match CreateChatCompletionRequestArgs::default()
                .model(&self.model)
                .messages(vec![user_msg])
                .max_completion_tokens(max_tokens)
                .build()
            {
                Ok(r) => r,
                Err(e) => return Err(e.into()),
            };

            let chat = self.client.chat();
            let response_fut = chat.create(request);
            tokio::pin!(response_fut);
            let call_started = Instant::now();
            let response = loop {
                let elapsed = call_started.elapsed().as_secs();
                if elapsed >= timeout_secs {
                    break Err(anyhow::anyhow!(
                        "LLM 請求逾時（{}s，provider={}，model={}，attempt={}/{})",
                        timeout_secs,
                        self.provider_label(),
                        self.model,
                        attempt + 1,
                        max_retries
                    ));
                }
                let remaining = timeout_secs - elapsed;
                let step = remaining.min(progress_interval_secs).max(1);
                match tokio::time::timeout(Duration::from_secs(step), &mut response_fut).await {
                    Ok(Ok(resp)) => break Ok(resp),
                    Ok(Err(e)) => break Err(e.into()),
                    Err(_) => {
                        debug_progress(&format!(
                            "LLM 等待中... 已等待 {}s (attempt {}/{})",
                            call_started.elapsed().as_secs(),
                            attempt + 1,
                            max_retries
                        ));
                    }
                }
            };
            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    // Custom OpenAI-compatible providers may reject this field.
                    if self.is_custom() && should_fallback_to_max_tokens(&e.to_string()) {
                        debug_progress("偵測到 max_completion_tokens 不相容，改用 legacy max_tokens 重試");
                        match self.invoke_with_legacy_max_tokens(prompt, max_tokens, timeout_secs).await {
                            Ok(r) => return Ok(r),
                            Err(fallback_err) => {
                                last_err = fallback_err;
                                continue;
                            }
                        }
                    }
                    last_err = e;
                    continue; // network / API error → retry
                }
            };

            // Accumulate token usage before consuming the response.
            let usage = response.usage;
            let choices = response.choices;

            if let Some(ref u) = usage {
                self.prompt_tokens
                    .fetch_add(u.prompt_tokens as u64, Ordering::Relaxed);
                self.completion_tokens
                    .fetch_add(u.completion_tokens as u64, Ordering::Relaxed);
            }

            let choice = match choices.into_iter().next() {
                Some(c) => c,
                None => {
                    last_err = anyhow::anyhow!("API 回傳 0 個 choices");
                    continue;
                }
            };

            let reason = choice.finish_reason.as_ref().map(|r| format!("{:?}", r));
            if let Some(ref r) = reason {
                if r != "\"Stop\"" && r != "Stop" {
                    warn!("[llm] finish_reason: {r}");
                }
            }

            match choice.message.content {
                Some(text) if !text.trim().is_empty() => {
                    debug_progress(&format!(
                        "LLM attempt {}/{} 成功 (elapsed={}s, response_chars={})",
                        attempt + 1,
                        max_retries,
                        call_started.elapsed().as_secs(),
                        text.chars().count()
                    ));
                    return Ok(text);
                }
                _ => {
                    warn!("[llm] content 為空，finish_reason={:?}", reason);
                    last_err = anyhow::anyhow!("LLM 回傳空內容（finish_reason={:?}）", reason);
                    continue;
                }
            }
        }

        Err(last_err)
    }

    async fn invoke_with_legacy_max_tokens(
        &self,
        prompt: &str,
        max_tokens: u32,
        timeout_secs: u64,
    ) -> Result<String> {
        let progress_interval_secs =
            env_u64("LLM_PROGRESS_INTERVAL_SECS", DEFAULT_PROGRESS_INTERVAL_SECS);
        let user_msg: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()?
            .into();
        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(vec![user_msg])
            .max_tokens(max_tokens)
            .build()?;
        let chat = self.client.chat();
        let response_fut = chat.create(request);
        tokio::pin!(response_fut);
        let call_started = Instant::now();
        let response = loop {
            let elapsed = call_started.elapsed().as_secs();
            if elapsed >= timeout_secs {
                anyhow::bail!(
                    "LLM 請求逾時（legacy max_tokens，{}s，provider={}，model={}）",
                    timeout_secs,
                    self.provider_label(),
                    self.model
                );
            }
            let remaining = timeout_secs - elapsed;
            let step = remaining.min(progress_interval_secs).max(1);
            match tokio::time::timeout(Duration::from_secs(step), &mut response_fut).await {
                Ok(Ok(resp)) => break resp,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => debug_progress(&format!(
                    "LLM (legacy max_tokens) 等待中... 已等待 {}s",
                    call_started.elapsed().as_secs()
                )),
            }
        };

        let usage = response.usage;
        let choices = response.choices;
        if let Some(ref u) = usage {
            self.prompt_tokens
                .fetch_add(u.prompt_tokens as u64, Ordering::Relaxed);
            self.completion_tokens
                .fetch_add(u.completion_tokens as u64, Ordering::Relaxed);
        }
        let Some(choice) = choices.into_iter().next() else {
            anyhow::bail!("API 回傳 0 個 choices");
        };
        match choice.message.content {
            Some(text) if !text.trim().is_empty() => Ok(text),
            _ => anyhow::bail!("LLM 回傳空內容（legacy max_tokens）"),
        }
    }

    fn is_custom(&self) -> bool {
        matches!(self.provider, LLMProvider::Custom { .. })
    }

    fn provider_label(&self) -> &'static str {
        match self.provider {
            LLMProvider::OpenAI => "openai",
            LLMProvider::Custom { .. } => "custom",
        }
    }

    /// Default limit for summaries (4096 tokens is enough for short outputs).
    pub async fn invoke(&self, prompt: &str) -> Result<String> {
        self.invoke_with_limit(prompt, 4096).await
    }
}

/// Estimate cost in USD based on approximate per-model pricing (per 1M tokens).
pub fn model_cost(model: &str, prompt: u64, completion: u64) -> f64 {
    let (price_in, price_out): (f64, f64) = if model.contains("gpt-4o-mini") {
        (0.15, 0.60)
    } else if model.contains("gpt-4o") {
        (2.50, 10.0)
    } else if model.contains("gpt-4-turbo") {
        (10.0, 30.0)
    } else if model.contains("o1-mini") {
        (3.0, 12.0)
    } else if model.contains("o1") || model.contains("gpt-5") {
        (15.0, 60.0) // rough estimate for advanced models
    } else {
        (2.50, 10.0) // default to gpt-4o pricing
    };
    (prompt as f64 * price_in + completion as f64 * price_out) / 1_000_000.0
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn should_fallback_to_max_tokens(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    (e.contains("max_completion_tokens") || e.contains("max completion tokens"))
        && (e.contains("unknown") || e.contains("invalid") || e.contains("unsupported"))
}

#[cfg(test)]
mod tests {
    use super::{env_u32, env_u64, should_fallback_to_max_tokens};

    #[test]
    fn fallback_detection_works() {
        assert!(should_fallback_to_max_tokens(
            "unknown field `max_completion_tokens` in request"
        ));
        assert!(should_fallback_to_max_tokens(
            "invalid parameter: max completion tokens unsupported"
        ));
        assert!(!should_fallback_to_max_tokens("rate limit exceeded"));
    }

    #[test]
    fn env_parsing_defaults_on_invalid_values() {
        unsafe { std::env::set_var("LLM_TIMEOUT_SECS", "45"); }
        unsafe { std::env::set_var("LLM_MAX_RETRIES", "2"); }
        assert_eq!(env_u64("LLM_TIMEOUT_SECS", 60), 45);
        assert_eq!(env_u32("LLM_MAX_RETRIES", 3), 2);

        unsafe { std::env::set_var("LLM_TIMEOUT_SECS", "0"); }
        unsafe { std::env::set_var("LLM_MAX_RETRIES", "bad"); }
        assert_eq!(env_u64("LLM_TIMEOUT_SECS", 60), 60);
        assert_eq!(env_u32("LLM_MAX_RETRIES", 3), 3);
    }
}
