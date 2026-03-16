use anyhow::{Context, Result};
use inquire::{validator::Validation, Select, Text};

pub struct Config {
    pub model: String,
    pub max_results: usize,
}

pub fn configure() -> Result<Config> {
    println!();
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

    // Validate API key
    let key = std::env::var("OPENAI_API_KEY")
        .context("OPENAI_API_KEY not set. Please add it to .env or environment.")?;
    if key.trim().is_empty() {
        anyhow::bail!("OPENAI_API_KEY is empty");
    }

    Ok(Config { model, max_results })
}
