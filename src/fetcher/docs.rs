use regex::Regex;
use std::sync::OnceLock;

// ── Compiled regexes (initialised once) ────────────────────────────────────────

fn re_blocks() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?si)<(?:script|style|noscript|nav|header|footer|aside)[^>]*>.*?</(?:script|style|noscript|nav|header|footer|aside)>").unwrap()
    })
}

fn re_block_tags() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)</?(?:p|div|section|article|main|h[1-6]|li|td|th|br)[^>]*>").unwrap()
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
    R.get_or_init(|| Regex::new(r#"(?i)href=["']([^"']+)["']"#).unwrap())
}

// ── URL helpers ──────────────────────────────────────────────────────────────────

/// Extract the host (and port, if present) from an absolute or protocol-relative URL.
/// Returns `None` for relative paths or unknown schemes.
///
/// Examples:
///   "https://docs.k8s.io/docs/" → Some("docs.k8s.io")
///   "//twitter.com/foo"         → Some("twitter.com")
///   "/relative/path"            → None
fn href_host(href: &str) -> Option<&str> {
    let after_scheme = href
        .strip_prefix("https://")
        .or_else(|| href.strip_prefix("http://"))
        .or_else(|| href.strip_prefix("//"))?;
    // Host ends at the first '/', '?', or '#'
    Some(
        after_scheme
            .split(&['/', '?', '#'] as &[char])
            .next()
            .unwrap_or(after_scheme),
    )
}

/// Extract just the host from the page URL (used as the canonical reference host).
fn page_host(url: &str) -> &str {
    href_host(url).unwrap_or(url)
}

/// Return `true` when `href` points to a page on the same host as `host`.
///
/// Rules:
/// - Absolute or protocol-relative URLs (`http://`, `https://`, `//`):
///   accept only when the extracted host matches `host` *exactly*.
///   This prevents lookalike hosts (e.g. `docs.example.com.evil`) and
///   protocol-relative cross-domain links (`//twitter.com/...`).
/// - Relative paths (anything else that is not a known external scheme):
///   always accepted.
/// - Known non-http schemes (`mailto:`, `javascript:`, `data:`, `tel:`):
///   always rejected.
fn is_internal(href: &str, host: &str) -> bool {
    // Reject non-navigable schemes first
    if href.starts_with("mailto:")
        || href.starts_with("javascript:")
        || href.starts_with("data:")
        || href.starts_with("tel:")
    {
        return false;
    }

    // Absolute or protocol-relative: require exact host match
    if href.starts_with("http://") || href.starts_with("https://") || href.starts_with("//") {
        return href_host(href).map(|h| h == host).unwrap_or(false);
    }

    // Everything else is a relative path — always internal
    true
}

// ── HTML helpers ─────────────────────────────────────────────────────────────────

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

fn extract_nav_links(html: &str, page_url: &str) -> Vec<String> {
    let host = page_host(page_url);
    let asset_exts = [
        ".png", ".jpg", ".jpeg", ".svg", ".gif", ".webp", ".css", ".js",
        ".ico", ".woff", ".woff2", ".ttf", ".eot", ".pdf", ".zip",
    ];

    let mut links: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for cap in re_href().captures_iter(html) {
        let href = cap[1].trim();

        if href.is_empty() || href.starts_with('#') {
            continue;
        }

        // Skip asset files by extension
        let lower = href.to_lowercase();
        if asset_exts.iter().any(|ext| lower.ends_with(ext)) {
            continue;
        }

        if !is_internal(href, host) {
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
    let s = re_blocks().replace_all(html, "\n");
    let s = re_block_tags().replace_all(&s, "\n");
    let s = re_tags().replace_all(&s, "");
    let s = decode_entities(&s);
    let s = re_ws().replace_all(&s, " ");
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
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
        .build()
        .ok()?;

    let mut resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    // Stream the response body, stopping once 300 KB have been read.
    // This avoids downloading a large page (or a binary file at a wrong URL)
    // entirely into memory before we can truncate it.
    const MAX_BYTES: usize = 300_000;
    let mut raw: Vec<u8> = Vec::with_capacity(65_536);

    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let space = MAX_BYTES.saturating_sub(raw.len());
                if space == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..chunk.len().min(space)]);
                if raw.len() >= MAX_BYTES {
                    break;
                }
            }
            _ => break, // end of body or network error — process what we have
        }
    }

    let html = String::from_utf8_lossy(&raw).into_owned();

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

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── href_host ─────────────────────────────────────────────────────────────

    #[test]
    fn href_host_https() {
        assert_eq!(href_host("https://docs.k8s.io/docs/"), Some("docs.k8s.io"));
    }

    #[test]
    fn href_host_protocol_relative() {
        assert_eq!(href_host("//twitter.com/foo"), Some("twitter.com"));
    }

    #[test]
    fn href_host_relative_is_none() {
        assert_eq!(href_host("/relative/path"), None);
    }

    #[test]
    fn href_host_with_port() {
        assert_eq!(href_host("https://localhost:8080/"), Some("localhost:8080"));
    }

    // ── is_internal ───────────────────────────────────────────────────────────

    #[test]
    fn internal_relative_path() {
        assert!(is_internal("/docs/getting-started", "kubernetes.io"));
        assert!(is_internal("./api", "kubernetes.io"));
        assert!(is_internal("../concepts/", "kubernetes.io"));
    }

    #[test]
    fn internal_absolute_same_host() {
        assert!(is_internal("https://kubernetes.io/docs/", "kubernetes.io"));
    }

    #[test]
    fn external_absolute_different_host() {
        assert!(!is_internal("https://github.com/kubernetes", "kubernetes.io"));
    }

    #[test]
    fn external_lookalike_host_rejected() {
        // A host that merely starts with the page host must not be accepted.
        assert!(!is_internal(
            "https://kubernetes.io.evil.com/",
            "kubernetes.io"
        ));
    }

    #[test]
    fn external_protocol_relative_rejected() {
        assert!(!is_internal("//twitter.com/k8s", "kubernetes.io"));
    }

    #[test]
    fn non_navigable_schemes_rejected() {
        assert!(!is_internal("mailto:a@b.com", "example.com"));
        assert!(!is_internal("javascript:void(0)", "example.com"));
        assert!(!is_internal("data:text/plain,hi", "example.com"));
    }
}
