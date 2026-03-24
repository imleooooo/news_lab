use regex::Regex;
use std::sync::OnceLock;

// ── Compiled regexes (initialised once) ────────────────────────────────────────

fn re_blocks() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Remove entire content blocks whose text is never useful for a summary.
    // No backreference needed: the closing tag just has to be one of the same set.
    R.get_or_init(|| {
        Regex::new(r"(?si)<(?:script|style|noscript|nav|header|footer|aside)[^>]*>.*?</(?:script|style|noscript|nav|header|footer|aside)>").unwrap()
    })
}

fn re_tags() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"<[^>]+>").unwrap())
}

fn re_ws() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[ \t]{2,}").unwrap())
}

fn re_blank_lines() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\n{3,}").unwrap())
}

fn re_title() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?si)<title[^>]*>(.*?)</title>").unwrap())
}

fn re_href() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Match href="..." or href='...'
    R.get_or_init(|| Regex::new(r#"(?i)href=["']([^"']+)["']"#).unwrap())
}

// ── Helpers ─────────────────────────────────────────────────────────────────────

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
}

fn extract_title(html: &str) -> String {
    re_title()
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| {
            let raw = re_tags().replace_all(m.as_str(), "");
            decode_entities(raw.trim())
        })
        .unwrap_or_default()
}

/// Return the `https://host` origin prefix for deduplication / relative-URL detection.
fn url_origin(url: &str) -> &str {
    // Find the end of the host component: after "https://" find the next "/"
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host_end = rest.find('/').map(|i| i + url.len() - rest.len()).unwrap_or(url.len());
    &url[..host_end]
}

fn extract_nav_links(html: &str, page_url: &str) -> Vec<String> {
    let origin = url_origin(page_url);
    let asset_exts = [".png", ".jpg", ".jpeg", ".svg", ".gif", ".webp", ".css", ".js", ".ico", ".woff", ".ttf"];

    let mut links: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for cap in re_href().captures_iter(html) {
        let href = cap[1].trim();

        // Skip empty, anchors-only, mailto, javascript
        if href.is_empty() || href.starts_with('#') || href.starts_with("mailto:")
            || href.starts_with("javascript:")
        {
            continue;
        }

        // Skip asset files
        let lower = href.to_lowercase();
        if asset_exts.iter().any(|ext| lower.ends_with(ext)) {
            continue;
        }

        // Accept relative paths, or absolute URLs on the same origin
        let is_relative = !href.starts_with("http");
        let is_same_origin = href.starts_with(origin);
        if !is_relative && !is_same_origin {
            continue;
        }

        if seen.insert(href.to_string()) {
            links.push(href.to_string());
        }
        if links.len() >= 30 {
            break;
        }
    }
    links
}

fn clean_text(html: &str) -> String {
    // 1. Remove block-level noise elements
    let s = re_blocks().replace_all(html, "\n");
    // 2. Replace block tags with newlines to preserve paragraph breaks
    let s = Regex::new(r"(?i)</?(?:p|div|section|article|main|h[1-6]|li|td|th|br)[^>]*>")
        .unwrap()
        .replace_all(&s, "\n");
    // 3. Strip remaining tags
    let s = re_tags().replace_all(&s, "");
    // 4. Decode HTML entities
    let s = decode_entities(&s);
    // 5. Collapse horizontal whitespace, preserve line breaks
    let s = re_ws().replace_all(&s, " ");
    // 6. Collapse excessive blank lines
    let s = re_blank_lines().replace_all(&s, "\n\n");
    s.trim().to_string()
}

// ── Public API ───────────────────────────────────────────────────────────────────

pub struct DocPage {
    pub url: String,
    pub title: String,
    /// Cleaned text content, truncated to ≤ 6 000 chars.
    pub text: String,
    /// Up to 30 internal navigation links discovered on the page.
    pub nav_links: Vec<String>,
}

pub async fn fetch_doc_page(url: &str) -> Option<DocPage> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        // Use a browser UA so doc sites don't return empty shells
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
        .build()
        .ok()?;

    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    // Cap raw HTML at 300 KB before any processing to bound memory use.
    let bytes = resp.bytes().await.ok()?;
    let html = String::from_utf8_lossy(bytes.get(..300_000.min(bytes.len()))?).into_owned();

    let title = extract_title(&html);
    let nav_links = extract_nav_links(&html, url);
    let text: String = clean_text(&html).chars().take(6_000).collect();

    Some(DocPage {
        url: url.to_string(),
        title,
        text,
        nav_links,
    })
}
