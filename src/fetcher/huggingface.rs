use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct HFModel {
    pub model_id: String,
    pub downloads: u64,
    pub likes: u64,
    pub pipeline_tag: String,
    pub tags: Vec<String>,
    pub last_modified: Option<DateTime<Utc>>,
    pub url: String,
}

#[derive(Deserialize)]
struct HFApiModel {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(rename = "pipeline_tag")]
    pipeline_tag: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(rename = "lastModified")]
    last_modified: Option<String>,
}

pub enum HFSort {
    Trending,
    Downloads,
    Likes,
}

impl HFSort {
    pub fn as_param(&self) -> &str {
        match self {
            HFSort::Trending => "trendingScore",
            HFSort::Downloads => "downloads",
            HFSort::Likes => "likes",
        }
    }
}

// Tags that add no useful information for display/summarization
const BOILERPLATE_TAGS: &[&str] = &[
    "transformers",
    "pytorch",
    "safetensors",
    "gguf",
    "ggml",
    "endpoints_compatible",
    "has_space",
    "text-generation-inference",
    "autotrain_compatible",
    "diffusers",
    "jax",
    "tf",
];

pub async fn fetch_hf_models(sort: HFSort, limit: usize) -> Vec<HFModel> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("news_lab/0.1")
        .build()
        .unwrap_or_default();

    let url = format!(
        "https://huggingface.co/api/models?sort={}&direction=-1&limit={}",
        sort.as_param(),
        limit
    );

    let Ok(resp) = client.get(&url).send().await else {
        return vec![];
    };
    let Ok(raw) = resp.json::<Vec<HFApiModel>>().await else {
        return vec![];
    };

    raw.into_iter()
        .map(|m| {
            let pipeline = m.pipeline_tag.unwrap_or_else(|| "unknown".to_string());

            let tags: Vec<String> = m
                .tags
                .iter()
                .filter(|t| {
                    !t.starts_with("license:")
                        && !t.starts_with("arxiv:")
                        && !t.starts_with("region:")
                        && !BOILERPLATE_TAGS.contains(&t.as_str())
                        && t.len() < 40
                })
                .take(6)
                .cloned()
                .collect();

            let last_modified = m
                .last_modified
                .as_deref()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok());

            HFModel {
                url: format!("https://huggingface.co/{}", m.id),
                model_id: m.id,
                downloads: m.downloads,
                likes: m.likes,
                pipeline_tag: pipeline,
                tags,
                last_modified,
            }
        })
        .collect()
}

pub fn fmt_num(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
