use anyhow::{anyhow, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use std::error::Error;
use std::fmt;

pub const SEARXNG_DISABLED: &str = "SEARXNG_DISABLED";

#[derive(Debug)]
struct SearxngDisabledError;

impl fmt::Display for SearxngDisabledError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SEARXNG_DISABLED)
    }
}

impl Error for SearxngDisabledError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub content: String,
    pub published: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Debug, Deserialize)]
struct SearxngResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
}

pub fn searxng_base_url() -> Option<String> {
    searxng_base_url_from_env_value(std::env::var("SEARXNG_URL").ok().as_deref())
}

pub fn searxng_base_url_from_env_value(value: Option<&str>) -> Option<String> {
    value
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

pub fn searxng_search_url(base_url: &str, query: &str) -> String {
    format!(
        "{}/search?q={}&format=json&language=auto&safesearch=0",
        base_url.trim().trim_end_matches('/'),
        urlencoding(query)
    )
}

pub fn is_searxng_disabled_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<SearxngDisabledError>().is_some() || err.to_string() == SEARXNG_DISABLED
}

pub async fn search_searxng(query: &str, client: &reqwest::Client) -> Result<Vec<SearchResult>> {
    let Some(base_url) = searxng_base_url() else {
        return Err(anyhow!(SearxngDisabledError));
    };
    search_searxng_with_base_url(query, client, &base_url).await
}

pub async fn search_searxng_with_base_url(
    query: &str,
    client: &reqwest::Client,
    base_url: &str,
) -> Result<Vec<SearchResult>> {
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    if base_url.is_empty() {
        return Err(anyhow!(SearxngDisabledError));
    }

    let url = searxng_search_url(&base_url, query);
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("SearXNG 搜尋失敗（{}）: {}", query, e))?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "SearXNG 搜尋失敗（{}）: HTTP {}",
            query,
            resp.status()
        ));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| anyhow!("SearXNG 回應讀取失敗（{}）: {}", query, e))?;
    parse_searxng_results(&body)
}

pub fn parse_searxng_results(body: &str) -> Result<Vec<SearchResult>> {
    let parsed: SearxngResponse = serde_json::from_str(body)?;
    Ok(parsed
        .results
        .into_iter()
        .filter_map(|item| {
            let url = item.url.trim();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return None;
            }
            let title = item.title.trim();
            if title.is_empty() {
                return None;
            }
            Some(SearchResult {
                title: title.to_string(),
                url: url.to_string(),
                content: item.content.trim().to_string(),
                published: item.published_date.as_deref().and_then(parse_searxng_date),
            })
        })
        .collect())
}

fn parse_searxng_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
        .or_else(|| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
        })
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            _ => {
                let mut buf = [0u8; 4];
                let len = c.encode_utf8(&mut buf).len();
                buf[..len]
                    .iter()
                    .map(|b| format!("%{:02X}", b))
                    .collect::<String>()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        is_searxng_disabled_error, parse_searxng_results, search_searxng_with_base_url,
        searxng_base_url_from_env_value, searxng_search_url,
    };

    #[test]
    fn searxng_base_url_from_env_value_disables_empty_or_unset() {
        assert_eq!(searxng_base_url_from_env_value(None), None);
        assert_eq!(searxng_base_url_from_env_value(Some("   ")), None);
        assert_eq!(
            searxng_base_url_from_env_value(Some("http://127.0.0.1:8888/")),
            Some("http://127.0.0.1:8888".to_string())
        );
    }

    #[test]
    fn searxng_search_url_trims_base_and_encodes_query() {
        assert_eq!(
            searxng_search_url("http://127.0.0.1:8888/", "LLM inference"),
            "http://127.0.0.1:8888/search?q=LLM+inference&format=json&language=auto&safesearch=0"
        );
    }

    #[tokio::test]
    async fn search_with_empty_base_url_returns_disabled_error() {
        let client = reqwest::Client::new();
        let err = search_searxng_with_base_url("test", &client, "")
            .await
            .unwrap_err();

        assert!(is_searxng_disabled_error(&err));
    }

    #[test]
    fn parse_searxng_results_reads_core_fields() {
        let body = r#"{
          "results": [
            {
              "title": "Example News",
              "url": "https://example.com/news",
              "content": "A useful summary",
              "publishedDate": "2026-05-01"
            }
          ]
        }"#;

        let results = parse_searxng_results(body).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example News");
        assert_eq!(results[0].url, "https://example.com/news");
        assert_eq!(results[0].content, "A useful summary");
        assert!(results[0].published.is_some());
    }

    #[test]
    fn parse_searxng_results_accepts_null_published_date() {
        let body = r#"{
          "results": [
            {
              "title": "Undated Result",
              "url": "https://example.com/undated",
              "content": "No known date",
              "publishedDate": null
            }
          ]
        }"#;

        let results = parse_searxng_results(body).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Undated Result");
        assert!(results[0].published.is_none());
    }

    #[test]
    fn parse_searxng_results_accepts_empty_results() {
        let results = parse_searxng_results(r#"{"results":[]}"#).unwrap();
        assert!(results.is_empty());
    }
}
