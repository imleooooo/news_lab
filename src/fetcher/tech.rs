use super::NewsItem;
use crate::llm::LLMClient;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use quick_xml::events::Event;
use quick_xml::Reader;
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

// ── Hacker News ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct HNResponse {
    hits: Vec<HNHit>,
}

#[derive(Deserialize)]
struct HNHit {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default, rename = "objectID")]
    object_id: String,
    #[serde(default, rename = "story_text")]
    story_text: String,
    #[serde(default, rename = "created_at")]
    created_at: String,
}

pub async fn fetch_hackernews(kw: &str, max: usize) -> Vec<NewsItem> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    // Tier 1: date-first, then relevance fallback
    let urls = [
        format!(
            "https://hn.algolia.com/api/v1/search_by_date?query={}&tags=story&hitsPerPage={}",
            urlencoding(kw),
            max
        ),
        format!(
            "https://hn.algolia.com/api/v1/search?query={}&tags=story&hitsPerPage={}",
            urlencoding(kw),
            max
        ),
        format!(
            "https://hn.algolia.com/api/v1/search?query={}&hitsPerPage={}",
            urlencoding(kw),
            max
        ),
    ];

    for url in &urls {
        let Some(data) = retry_get_json::<HNResponse>(&client, url).await else {
            continue;
        };
        let items: Vec<NewsItem> = data
            .hits
            .into_iter()
            .filter(|h| !h.title.is_empty())
            .map(|h| {
                let published = DateTime::parse_from_rfc3339(&h.created_at)
                    .ok()
                    .map(|d| d.with_timezone(&Utc));
                NewsItem {
                    title: h.title.clone(),
                    url: if h.url.is_empty() {
                        format!("https://news.ycombinator.com/item?id={}", h.object_id)
                    } else {
                        h.url.clone()
                    },
                    source: "Hacker News".to_string(),
                    published,
                    description: h.story_text.chars().take(300).collect(),
                }
            })
            .collect();
        if !items.is_empty() {
            return items;
        }
    }
    vec![]
}

// ── Keyword expansion ──────────────────────────────────────────────────────────

const NEWS_EXPAND_PROMPT: &str = r#"你是技術新聞搜尋專家。請根據以下關鍵字，同時生成英文與繁體中文兩組搜尋詞組，用於搜尋技術新聞與案例分享。

英文詞組（4–6 個）：
- 目標工具的子元件、開發公司或社群、常見使用場景術語
- ❌ 不要包含競品；不要包含過於通用的詞彙

繁體中文詞組（3–5 個）：
- 適合搜尋台灣技術媒體（iThome、科技新報）的說法
- 可直接使用英文原名（如 KubeRay、Ray Framework），或業界慣用中文術語
- ❌ 不要逐字翻譯英文

✅ 例子："kuberay" →
  en: ["ray cluster", "ray serve", "anyscale ray", "ray operator"]
  zh: ["KubeRay", "Ray 框架", "Ray 分散式"]

關鍵字：{keyword}

只回傳 JSON，不加任何說明：
{"en": ["ray cluster", "ray serve", "anyscale ray"], "zh": ["KubeRay", "Ray 框架", "Ray 分散式"]}"#;

#[derive(serde::Deserialize)]
struct ExpandResult {
    #[serde(default)]
    en: Vec<String>,
    #[serde(default)]
    zh: Vec<String>,
}

fn strip_fence(s: &str) -> &str {
    let s = s.trim();
    let s = if s.starts_with("```") {
        s.split_once('\n').map(|x| x.1).unwrap_or(s)
    } else {
        s
    };
    if s.ends_with("```") {
        s.rsplit_once("```").map(|x| x.0).unwrap_or(s).trim_end()
    } else {
        s
    }
}

/// Expand a search keyword into English and Traditional Chinese term lists via LLM.
/// Returns `(en_keywords, zh_keywords)`, each with the original keyword prepended.
pub async fn expand_news_keywords(kw: &str, llm: &LLMClient) -> (Vec<String>, Vec<String>) {
    let prompt = NEWS_EXPAND_PROMPT.replace("{keyword}", kw);
    let Ok(response) = llm.invoke(&prompt).await else {
        return (vec![kw.to_string()], vec![kw.to_string()]);
    };
    let s = strip_fence(&response);
    let s = s.find('{').map(|i| &s[i..]).unwrap_or(s);

    let mut en = vec![kw.to_string()];
    let mut zh = vec![kw.to_string()];
    if let Ok(result) = serde_json::from_str::<ExpandResult>(s) {
        en.extend(result.en);
        zh.extend(result.zh);
    }
    (en, zh)
}

/// Search HN with multiple queries in parallel; deduplicate by URL.
/// Uses combined title+description matching for all queries.
pub async fn fetch_hackernews_multi(queries: &[String], max: usize) -> Vec<NewsItem> {
    let per_query = ((max / queries.len().max(1)) + 2).min(max);
    let futures: Vec<_> = queries
        .iter()
        .map(|q| fetch_hackernews(q, per_query))
        .collect();
    let results = join_all(futures).await;

    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<NewsItem> = results
        .into_iter()
        .flatten()
        .filter(|item| seen.insert(item.url.clone()))
        .collect();

    all.sort_by(|a, b| b.published.cmp(&a.published));
    all.truncate(max);
    all
}

// ── GitHub ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct GitHubResponse {
    items: Vec<GitHubItem>,
}

#[derive(Deserialize)]
struct GitHubItem {
    full_name: String,
    html_url: String,
    description: Option<String>,
    pushed_at: Option<String>,
    created_at: Option<String>,
    stargazers_count: Option<u64>,
    language: Option<String>,
}

pub async fn fetch_github(kw: &str, max: usize) -> Vec<NewsItem> {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("news_lab/0.1")
        .build()
        .unwrap_or_default();

    // Try progressively wider date windows until we get results
    for days in [30u64, 90, 365] {
        let since = (Utc::now() - chrono::Duration::days(days as i64))
            .format("%Y-%m-%d")
            .to_string();
        let query = format!("{} pushed:>{}", kw, since);
        let url = format!(
            "https://api.github.com/search/repositories?q={}&sort=stars&order=desc&per_page={}",
            urlencoding(&query),
            max
        );

        let mut req = client.get(&url);
        if let Some(ref tok) = token {
            req = req.header("Authorization", format!("token {}", tok));
        }

        let Ok(resp) = req.send().await else { continue };
        if resp.status() == 403 {
            break;
        } // rate limited
        let Ok(data) = resp.json::<GitHubResponse>().await else {
            continue;
        };
        if data.items.is_empty() {
            continue;
        }

        let items = data
            .items
            .into_iter()
            .map(|item| {
                let published = item
                    .pushed_at
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc));

                let stars = item.stargazers_count.unwrap_or(0);
                let lang = item.language.as_deref().unwrap_or("unknown");
                let desc = item.description.as_deref().unwrap_or("No description");
                let description = format!("⭐ {:>6} | {} | {}", stars, lang, desc);

                NewsItem {
                    title: item.full_name.clone(),
                    url: item.html_url.clone(),
                    source: "GitHub".to_string(),
                    published,
                    description,
                }
            })
            .collect();

        return items;
    }
    vec![]
}

/// Newly created repos that already have some star traction.
/// Tries progressively wider creation windows and lower star thresholds.
pub async fn fetch_github_emerging(kw: &str, max: usize) -> Vec<NewsItem> {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("news_lab/0.1")
        .build()
        .unwrap_or_default();

    // (created within N days, minimum stars)
    for (days, min_stars) in [(90u64, 10u64), (180, 5), (365, 2)] {
        let since = (Utc::now() - chrono::Duration::days(days as i64))
            .format("%Y-%m-%d")
            .to_string();
        let query = format!("{} created:>{} stars:>{}", kw, since, min_stars);
        let url = format!(
            "https://api.github.com/search/repositories?q={}&sort=stars&order=desc&per_page={}",
            urlencoding(&query),
            max
        );

        let mut req = client.get(&url);
        if let Some(ref tok) = token {
            req = req.header("Authorization", format!("token {}", tok));
        }

        let Ok(resp) = req.send().await else { continue };
        if resp.status() == 403 {
            break;
        }
        let Ok(data) = resp.json::<GitHubResponse>().await else {
            continue;
        };
        if data.items.is_empty() {
            continue;
        }

        let items = data
            .items
            .into_iter()
            .map(|item| {
                // Show creation date (more meaningful for emerging projects)
                let published = item
                    .created_at
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc));

                let stars = item.stargazers_count.unwrap_or(0);
                let lang = item.language.as_deref().unwrap_or("unknown");
                let desc = item.description.as_deref().unwrap_or("No description");
                let description = format!("⭐ {:>6} | {} | {}", stars, lang, desc);

                NewsItem {
                    title: item.full_name.clone(),
                    url: item.html_url.clone(),
                    source: "GitHub".to_string(),
                    published,
                    description,
                }
            })
            .collect();

        return items;
    }
    vec![]
}

// ── Combined ───────────────────────────────────────────────────────────────────

pub async fn fetch_tech_news(kw: &str, max: usize) -> Vec<NewsItem> {
    let hn_max = (max as f64 * 0.6).ceil() as usize;
    let gh_max = (max as f64 * 0.4).ceil() as usize;

    let (hn, gh) = tokio::join!(fetch_hackernews(kw, hn_max), fetch_github(kw, gh_max));

    let mut combined: Vec<NewsItem> = hn.into_iter().chain(gh).collect();
    combined.sort_by(|a, b| b.published.cmp(&a.published));
    combined.truncate(max);
    combined
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            _ => {
                // Encode each UTF-8 byte separately (e.g. '機' → %E6%A9%9F)
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

// ── Retry helpers ───────────────────────────────────────────────────────────────

/// GET + deserialize JSON with up to 3 attempts (1 s / 2 s exponential backoff).
/// Returns None on persistent failure or 4xx response.
async fn retry_get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Option<T> {
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1))).await;
        }
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        if resp.status().is_client_error() {
            return None; // 4xx → don't retry
        }
        if resp.status().is_server_error() {
            continue; // 5xx → retry
        }
        if let Ok(data) = resp.json::<T>().await {
            return Some(data);
        }
    }
    None
}

/// GET text (RSS / Atom XML) with up to 3 attempts (1 s / 2 s exponential backoff).
/// Returns None on persistent failure or 4xx response.
async fn retry_get_text(client: &reqwest::Client, url: &str) -> Option<String> {
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1))).await;
        }
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        if resp.status().is_client_error() {
            return None;
        }
        if resp.status().is_server_error() {
            continue;
        }
        if let Ok(text) = resp.text().await {
            return Some(text);
        }
    }
    None
}

// ── RSS Feed Parser ─────────────────────────────────────────────────────────────

fn strip_html(s: &str) -> String {
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    static WS_RE: OnceLock<Regex> = OnceLock::new();
    let tag_re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let ws_re = WS_RE.get_or_init(|| Regex::new(r"\s+").unwrap());
    let stripped = tag_re.replace_all(s, " ");
    ws_re.replace_all(stripped.trim(), " ").into_owned()
}

fn parse_rss_items(xml: &str, source: &str, keywords: &[String], max: usize) -> Vec<NewsItem> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut items: Vec<NewsItem> = Vec::new();
    let mut buf = Vec::new();

    let mut in_item = false;
    let mut cur_tag = String::new();
    let mut f_title = String::new();
    let mut f_link = String::new();
    let mut f_desc = String::new();
    let mut f_date = String::new();

    // Per-phrase token lists for conjunction matching.
    // An article passes if any full keyword phrase appears as a substring, OR
    // the first ≤2 meaningful tokens of any phrase both appear individually.
    // Using only 2 tokens per phrase avoids over-constraining long phrases
    // (e.g. "Kubernetes GPU device plugin" → check "kubernetes" + "gpu" only).
    let kw_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
    let phrase_tokens: Vec<Vec<String>> = kw_lower
        .iter()
        .map(|k| {
            k.split_whitespace()
                .filter(|w| w.len() > 1 && w.starts_with(|c: char| c.is_alphanumeric()))
                .take(2)
                .map(|w| w.to_string())
                .collect::<Vec<String>>()
        })
        .filter(|v| !v.is_empty())
        .collect();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let tag = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_lowercase();
                if tag == "item" || tag == "entry" {
                    in_item = true;
                    f_title.clear();
                    f_link.clear();
                    f_desc.clear();
                    f_date.clear();
                } else if in_item {
                    cur_tag = tag.clone();
                    // Atom: <link href="..."> with attribute (not text)
                    if tag == "link" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"href" {
                                f_link = String::from_utf8_lossy(&attr.value).into_owned();
                            }
                        }
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                if !in_item {
                    buf.clear();
                    continue;
                }
                let tag = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_lowercase();
                if tag == "link" {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"href" {
                            f_link = String::from_utf8_lossy(&attr.value).into_owned();
                        }
                    }
                }
            }
            Ok(Event::Text(ref e)) => {
                if !in_item {
                    buf.clear();
                    continue;
                }
                let text = e.unescape().unwrap_or_default().into_owned();
                match cur_tag.as_str() {
                    "title" => f_title = text,
                    "link" if f_link.is_empty() => f_link = text,
                    "description" | "summary" | "content" | "content:encoded" => f_desc = text,
                    "pubdate" | "published" | "updated" | "dc:date" => f_date = text,
                    _ => {}
                }
            }
            Ok(Event::CData(ref e)) => {
                if !in_item {
                    buf.clear();
                    continue;
                }
                let text = String::from_utf8_lossy(e).into_owned();
                match cur_tag.as_str() {
                    "title" => f_title = text,
                    "link" if f_link.is_empty() => f_link = text,
                    "description" | "summary" | "content" | "content:encoded" => f_desc = text,
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let tag = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_lowercase();
                if (tag == "item" || tag == "entry") && in_item {
                    in_item = false;
                    cur_tag.clear();

                    if f_title.is_empty() || f_link.is_empty() {
                        buf.clear();
                        continue;
                    }

                    // Keyword filter: skipped when keywords is empty (e.g. Medium tag feeds
                    // where the URL already scopes relevance). Otherwise, full keyword phrase
                    // appears OR the first ≤2 tokens of any phrase all appear.
                    if !kw_lower.is_empty() {
                        let title_lower = f_title.to_lowercase();
                        let desc_lower = f_desc.to_lowercase();
                        let combined = format!("{} {}", title_lower, desc_lower);
                        let matches = kw_lower.iter().any(|k| combined.contains(k.as_str()))
                            || phrase_tokens.iter().any(|tokens| {
                                tokens.iter().all(|t| combined.contains(t.as_str()))
                            });
                        if !matches {
                            buf.clear();
                            continue;
                        }
                    }

                    // Parse date: try RFC 2822 (RSS) then RFC 3339 (Atom)
                    let published = DateTime::parse_from_rfc2822(f_date.trim())
                        .ok()
                        .or_else(|| {
                            f_date
                                .trim()
                                .parse::<DateTime<Utc>>()
                                .ok()
                                .map(|d| d.fixed_offset())
                        })
                        .map(|d| d.with_timezone(&Utc));

                    let desc_clean = strip_html(&f_desc).chars().take(300).collect();

                    items.push(NewsItem {
                        title: f_title.clone(),
                        url: f_link.clone(),
                        source: source.to_string(),
                        published,
                        description: desc_clean,
                    });

                    if items.len() >= max {
                        break;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    items
}

async fn fetch_rss_feed(url: &str, source: &str, keywords: &[String], max: usize) -> Vec<NewsItem> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .user_agent("news_lab/0.1")
        .build()
        .unwrap_or_default();

    let Some(body) = retry_get_text(&client, url).await else {
        return vec![];
    };
    parse_rss_items(&body, source, keywords, max)
}

// ── All RSS sources ────────────────────────────────────────────────────────────
// To add a new source: (url, display_name, is_chinese_language)

const RSS_SOURCES: &[(&str, &str, bool)] = &[
    // ── English ────────────────────────────────────────────────────────────────
    ("https://feed.infoq.com/", "InfoQ", false),
    (
        "https://www.theregister.com/headlines.atom",
        "The Register",
        false,
    ),
    (
        "https://feeds.arstechnica.com/arstechnica/technology-lab",
        "Ars Technica",
        false,
    ),
    ("https://techcrunch.com/feed/", "TechCrunch", false),
    ("https://thenewstack.io/feed/", "The New Stack", false),
    ("https://lobste.rs/rss", "Lobsters", false),
    ("https://dev.to/feed", "dev.to", false),
    // ── Traditional Chinese ────────────────────────────────────────────────────
    ("https://www.ithome.com.tw/rss", "iThome", true),
    ("https://technews.tw/feed/", "科技新報", true),
    ("https://www.inside.com.tw/feed", "INSIDE", true),
];

/// Fetch all RSS sources in parallel using language-appropriate keywords.
/// Each source has an independent 8-second timeout; a slow/failed source does not
/// affect the others.
/// Requests are staggered by 200 ms each to avoid bursting all sources simultaneously.
pub async fn fetch_all_rss(en_kw: &[String], zh_kw: &[String], max: usize) -> Vec<NewsItem> {
    let per_source = ((max / RSS_SOURCES.len()) + 2).min(max);
    let futures: Vec<_> = RSS_SOURCES
        .iter()
        .enumerate()
        .map(|(i, (url, source, is_zh))| {
            let kw: &[String] = if *is_zh { zh_kw } else { en_kw };
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(i as u64 * 200)).await;
                tokio::time::timeout(
                    std::time::Duration::from_secs(8),
                    fetch_rss_feed(url, source, kw, per_source),
                )
                .await
                .unwrap_or_default()
            }
        })
        .collect();

    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<NewsItem> = join_all(futures)
        .await
        .into_iter()
        .flatten()
        .filter(|item| seen.insert(item.url.clone()))
        .collect();

    all.sort_by(|a, b| b.published.cmp(&a.published));
    all
}

// ── Medium tag RSS ─────────────────────────────────────────────────────────────

/// Convert a keyword phrase into a Medium tag slug (lowercase, spaces → hyphens,
/// non-alphanumeric/hyphen characters removed).
fn to_medium_slug(kw: &str) -> String {
    kw.to_lowercase()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Fetch Medium tag RSS feeds for the first N expanded keywords in parallel.
/// Medium tag URLs are dynamic (keyword-dependent), so they cannot be in RSS_SOURCES.
pub async fn fetch_medium_rss(en_kw: &[String], max: usize) -> Vec<NewsItem> {
    // Take at most 4 keywords to avoid hammering Medium with too many requests.
    let slugs: Vec<String> = en_kw
        .iter()
        .take(4)
        .map(|k| to_medium_slug(k))
        .filter(|s| !s.is_empty())
        .collect();

    if slugs.is_empty() {
        return vec![];
    }

    let per_slug = ((max / slugs.len()) + 2).min(max);
    let futures: Vec<_> = slugs
        .iter()
        .enumerate()
        .map(|(i, slug)| {
            let url = format!("https://medium.com/feed/tag/{}", slug);
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(i as u64 * 300)).await;
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    // Pass &[] so parse_rss_items skips keyword filtering —
                    // the tag URL already scopes relevance.
                    fetch_rss_feed(&url, "Medium", &[], per_slug),
                )
                .await
                .unwrap_or_default()
            }
        })
        .collect();

    let mut seen = std::collections::HashSet::new();
    let mut all: Vec<NewsItem> = join_all(futures)
        .await
        .into_iter()
        .flatten()
        .filter(|item| seen.insert(item.url.clone()))
        .collect();

    all.sort_by(|a, b| b.published.cmp(&a.published));
    all
}
