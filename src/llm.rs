use anyhow::{bail, Result};
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestMessage, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs,
    },
    Client,
};

pub struct LLMClient {
    client: Client<OpenAIConfig>,
    pub model: String,
}

impl LLMClient {
    pub fn new(model: &str) -> Result<Self> {
        let client = Client::new();
        Ok(Self {
            client,
            model: model.to_string(),
        })
    }

    /// `max_tokens`: max_completion_tokens to request.
    /// Use a larger value (e.g. 16384) for complex outputs like radar JSON.
    pub async fn invoke_with_limit(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let user_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestUserMessageArgs::default()
                .content(prompt)
                .build()?
                .into();

        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(vec![user_msg])
            .max_completion_tokens(max_tokens)
            .build()?;

        let response = self.client.chat().create(request).await?;

        let choice = match response.choices.into_iter().next() {
            Some(c) => c,
            None => bail!("API 回傳 0 個 choices"),
        };

        let reason = choice.finish_reason.as_ref().map(|r| format!("{:?}", r));
        if let Some(ref r) = reason {
            if r != "\"Stop\"" && r != "Stop" {
                eprintln!("  [llm] finish_reason: {r}");
            }
        }

        match choice.message.content {
            Some(text) if !text.trim().is_empty() => Ok(text),
            _ => {
                eprintln!("  [llm] content 為空，finish_reason={:?}", reason);
                bail!("LLM 回傳空內容（finish_reason={:?}）", reason)
            }
        }
    }

    /// Default limit for summaries (4096 tokens is enough for short outputs).
    pub async fn invoke(&self, prompt: &str) -> Result<String> {
        self.invoke_with_limit(prompt, 4096).await
    }
}
