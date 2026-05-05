use anyhow::{Context, Result};
use inquire::{validator::Validation, Password, Select, Text};

#[derive(Clone)]
pub enum LLMProvider {
    OpenAI,
    Custom { base_url: String },
}

#[derive(Clone)]
pub struct LLMSettings {
    pub provider: LLMProvider,
    pub api_key: String,
    pub model: String,
}

impl LLMSettings {
    pub fn cache_key(&self) -> String {
        match &self.provider {
            LLMProvider::OpenAI => format!("openai:{}", self.model),
            LLMProvider::Custom { base_url } => format!("custom:{}:{}", base_url, self.model),
        }
    }

    pub fn with_model(&self, model: &str) -> anyhow::Result<Self> {
        if model.trim().is_empty() {
            anyhow::bail!("Model name 不可為空");
        }
        let mut settings = self.clone();
        settings.model = model.to_string();
        Ok(settings)
    }
}

pub struct Config {
    pub llm: LLMSettings,
    pub max_results: usize,
}

pub fn configure() -> Result<Config> {
    println!();
    let provider_label = Select::new("選擇 LLM Provider:", vec!["OpenAI", "自定義 API"])
        .with_starting_cursor(0)
        .prompt()
        .context("provider selection failed")?;

    let llm = if provider_label == "自定義 API" {
        configure_custom_llm()?
    } else {
        configure_openai_llm()?
    };

    let max_str = Text::new("每次最多抓取幾筆資料:")
        .with_default("10")
        .with_validator(|input: &str| match input.trim().parse::<usize>() {
            Ok(n) if n >= 1 => Ok(Validation::Valid),
            Ok(_) => Ok(Validation::Invalid("請輸入至少 1 以上的整數".into())),
            Err(_) => Ok(Validation::Invalid("請輸入正整數（例如：10）".into())),
        })
        .prompt()
        .context("max_results input failed")?;

    let max_results: usize = max_str.trim().parse::<usize>().unwrap();

    Ok(Config { llm, max_results })
}

fn configure_openai_llm() -> Result<LLMSettings> {
    let model_choices = vec![
        "基礎模型  (gpt-5-mini-2025-08-07)",
        "進階模型  (gpt-5.4-2026-03-05)",
    ];

    let model_label = Select::new("選擇 LLM 模型:", model_choices)
        .with_starting_cursor(0)
        .prompt()
        .context("model selection failed")?;

    let model = if model_label.contains("進階") {
        "gpt-5.4-2026-03-05".to_string()
    } else {
        "gpt-5-mini-2025-08-07".to_string()
    };

    let key = std::env::var("OPENAI_API_KEY")
        .context("OPENAI_API_KEY not set. Please add it to .env or environment.")?;
    if key.trim().is_empty() {
        anyhow::bail!("OPENAI_API_KEY is empty");
    }

    Ok(LLMSettings {
        provider: LLMProvider::OpenAI,
        api_key: key.trim().to_string(),
        model,
    })
}

fn configure_custom_llm() -> Result<LLMSettings> {
    let base_url = Text::new("自定義 API URL (base URL，例如 https://example.com/v1):")
        .with_validator(|input: &str| {
            let trimmed = input.trim();
            match reqwest::Url::parse(trimmed) {
                Ok(url) if url.scheme() == "http" || url.scheme() == "https" => {
                    Ok(Validation::Valid)
                }
                Ok(_) => Ok(Validation::Invalid("URL 必須使用 http 或 https".into())),
                Err(_) => Ok(Validation::Invalid(
                    "請輸入有效 URL，例如 https://example.com/v1".into(),
                )),
            }
        })
        .prompt()
        .context("custom API URL input failed")?;

    let api_key = Password::new("自定義 API Key:")
        .without_confirmation()
        .prompt()
        .context("custom API key input failed")?;
    if api_key.trim().is_empty() {
        anyhow::bail!("自定義 API Key 不可為空");
    }

    let model = Text::new("自定義 Model Name:")
        .with_validator(|input: &str| {
            if input.trim().is_empty() {
                Ok(Validation::Invalid("Model name 不可為空".into()))
            } else {
                Ok(Validation::Valid)
            }
        })
        .prompt()
        .context("custom model name input failed")?;

    Ok(LLMSettings {
        provider: LLMProvider::Custom {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
        },
        api_key: api_key.trim().to_string(),
        model: model.trim().to_string(),
    })
}
