use chrono::{DateTime, Duration, Utc};
use quick_xml::events::Event;
use quick_xml::Reader;
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct PodcastEpisode {
    pub podcast_name: String,
    pub title: String,
    pub url: String,
    pub published: Option<DateTime<Utc>>,
    pub description: String,
    pub duration: String,
}

// ── iTunes Search ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ItunesResponse {
    results: Vec<ItunesResult>,
}

#[derive(Deserialize)]
struct ItunesResult {
    #[serde(rename = "collectionName", default)]
    collection_name: String,
    #[serde(rename = "feedUrl", default)]
    feed_url: String,
}

async fn search_itunes(kw: &str, max_pods: usize) -> Vec<(String, String)> {
    let url = format!(
        "https://itunes.apple.com/search?term={}&media=podcast&limit={}",
        kw.replace(' ', "+"),
        max_pods
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let Ok(resp) = client.get(&url).send().await else {
        return vec![];
    };
    let Ok(data) = resp.json::<ItunesResponse>().await else {
        return vec![];
    };

    data.results
        .into_iter()
        .filter(|r| !r.feed_url.is_empty())
        .map(|r| (r.collection_name, r.feed_url))
        .collect()
}

// ── RSS Feed Parser ────────────────────────────────────────────────────────────

async fn fetch_rss_episodes(
    podcast_name: &str,
    feed_url: &str,
    max_eps: usize,
    cutoff: Option<DateTime<Utc>>,
) -> Vec<PodcastEpisode> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("news_lab/0.1")
        .build()
        .unwrap_or_default();

    let Ok(resp) = client.get(feed_url).send().await else {
        return vec![];
    };
    let Ok(body) = resp.text().await else {
        return vec![];
    };

    parse_rss(&body, podcast_name, max_eps, cutoff)
}

fn parse_rss(
    xml: &str,
    podcast_name: &str,
    max_eps: usize,
    cutoff: Option<DateTime<Utc>>,
) -> Vec<PodcastEpisode> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut episodes = Vec::new();
    let mut in_item = false;
    let mut current_tag = String::new();

    let mut title = String::new();
    let mut url = String::new();
    let mut published: Option<DateTime<Utc>> = None;
    let mut description = String::new();
    let mut duration = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                let tag = std::str::from_utf8(name.as_ref()).unwrap_or("").to_string();
                current_tag = tag.clone();

                if tag == "item" {
                    in_item = true;
                    title.clear();
                    url.clear();
                    published = None;
                    description.clear();
                    duration.clear();
                }

                if tag == "enclosure" && in_item {
                    for attr in e.attributes().flatten() {
                        let key = std::str::from_utf8(attr.key.local_name().as_ref())
                            .unwrap_or("")
                            .to_string();
                        let val = attr.unescape_value().unwrap_or_default().to_string();
                        if key == "url" && url.is_empty() {
                            url = val;
                        }
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if !in_item {
                    continue;
                }
                let text = std::str::from_utf8(&e).unwrap_or("").to_string();
                update_episode_field(
                    &current_tag,
                    &text,
                    &mut title,
                    &mut url,
                    &mut published,
                    &mut description,
                    &mut duration,
                );
            }
            Ok(Event::Text(e)) => {
                if !in_item {
                    continue;
                }
                let text = e.unescape().unwrap_or_default().to_string();
                let text = text.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                update_episode_field(
                    &current_tag,
                    &text,
                    &mut title,
                    &mut url,
                    &mut published,
                    &mut description,
                    &mut duration,
                );
            }
            Ok(Event::End(e)) => {
                let name = e.local_name();
                let tag = std::str::from_utf8(name.as_ref()).unwrap_or("").to_string();
                if tag == "item" && in_item {
                    let passes_cutoff = cutoff
                        .map(|c| published.is_none_or(|p| p >= c))
                        .unwrap_or(true);

                    if passes_cutoff && !title.is_empty() {
                        let desc = strip_html(&description);
                        let desc: String = desc.chars().take(500).collect();
                        episodes.push(PodcastEpisode {
                            podcast_name: podcast_name.to_string(),
                            title: title.clone(),
                            url: url.clone(),
                            published,
                            description: desc,
                            duration: duration.clone(),
                        });
                        if episodes.len() >= max_eps {
                            break;
                        }
                    }
                    in_item = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    episodes
}

fn update_episode_field(
    tag: &str,
    text: &str,
    title: &mut String,
    url: &mut String,
    published: &mut Option<DateTime<Utc>>,
    description: &mut String,
    duration: &mut String,
) {
    match tag {
        "title" if title.is_empty() => *title = text.to_string(),
        "link" if url.is_empty() => *url = text.to_string(),
        "pubDate" if published.is_none() => {
            if let Ok(dt) = DateTime::parse_from_rfc2822(text) {
                *published = Some(dt.with_timezone(&Utc));
            }
        }
        "description" | "summary" if description.is_empty() => {
            *description = text.to_string();
        }
        "duration" if duration.is_empty() => *duration = text.to_string(),
        _ => {}
    }
}

fn strip_html(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let stripped = re.replace_all(s, "");
    html_escape::decode_html_entities(&stripped).to_string()
}

// ── Public API ─────────────────────────────────────────────────────────────────

pub async fn fetch_podcast_content(
    kw: &str,
    max_pods: usize,
    max_eps: usize,
    days: i64,
) -> Vec<PodcastEpisode> {
    let cutoff = if days > 0 {
        Some(Utc::now() - Duration::days(days))
    } else {
        None
    };

    let pods = search_itunes(kw, max_pods).await;
    if pods.is_empty() {
        return vec![];
    }

    // Concurrent RSS fetching
    let futures: Vec<_> = pods
        .into_iter()
        .map(|(name, feed_url)| async move {
            fetch_rss_episodes(&name, &feed_url, max_eps, cutoff).await
        })
        .collect();

    let results: Vec<Vec<PodcastEpisode>> = futures::future::join_all(futures).await;
    let mut all_episodes: Vec<PodcastEpisode> = results.into_iter().flatten().collect();

    // Sort by date descending
    all_episodes.sort_by(|a, b| b.published.cmp(&a.published));
    all_episodes
}
