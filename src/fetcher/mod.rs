pub mod arxiv;
pub mod cncf;
pub mod docs;
pub mod huggingface;
pub mod podcast;
pub mod release;
pub mod tech;

use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct NewsItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub published: Option<DateTime<Utc>>,
    pub description: String,
}
