use anyhow::Result;
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestMessage, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs,
    },
    Client,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

pub struct LLMClient {
    client: Client<OpenAIConfig>,
    pub model: String,
    prompt_tokens: Arc<AtomicU64>,
    completion_tokens: Arc<AtomicU64>,
}

impl LLMClient {
    pub fn new(model: &str) -> Result<Self> {
        let client = Client::new();
        Ok(Self {
            client,
            model: model.to_string(),
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
    /// Retries up to 3 times on transient network / API errors (1 s → 2 s backoff).
    pub async fn invoke_with_limit(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let mut last_err = anyhow::anyhow!("no attempts");

        for attempt in 0..3u32 {
            if attempt > 0 {
                let wait = 1u64 << (attempt - 1); // 1 s, 2 s
                eprintln!("  [llm] 第 {}/{} 次重試，等待 {}s...", attempt + 1, 3, wait);
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            }

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

            let response = match self.client.chat().create(request).await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.into();
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
                    eprintln!("  [llm] finish_reason: {r}");
                }
            }

            match choice.message.content {
                Some(text) if !text.trim().is_empty() => return Ok(text),
                _ => {
                    eprintln!("  [llm] content 為空，finish_reason={:?}", reason);
                    last_err =
                        anyhow::anyhow!("LLM 回傳空內容（finish_reason={:?}）", reason);
                    continue;
                }
            }
        }

        Err(last_err)
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
