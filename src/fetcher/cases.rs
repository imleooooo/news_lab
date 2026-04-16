use crate::cache;
use crate::fetcher::docs::fetch_doc_page;
use crate::llm::LLMClient;
use crate::radar::Blip;
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;

const CASE_CACHE_PREFIX: &str = "enterprise_cases";
const SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/?q=";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SourcePolicy {
    OfficialOnly,
}

impl SourcePolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::OfficialOnly => "official_only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseEvidence {
    pub company: String,
    pub title: String,
    pub url: String,
    pub publisher: String,
    #[serde(default)]
    pub published_at: String,
    pub usage_summary: String,
    #[serde(default)]
    pub evidence_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlipCaseBundle {
    pub product_name: String,
    pub cases: Vec<CaseEvidence>,
    pub fetched_at: String,
    pub source_policy: String,
}

#[derive(Debug, Clone)]
struct SearchResult {
    title: String,
    url: String,
}

struct PageCollection {
    pages: String,
    fetched_pages: usize,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoResponse {
    #[serde(default)]
    homepage: String,
}

#[derive(Debug, Deserialize)]
struct CaseExtraction {
    #[serde(default)]
    cases: Vec<CaseEvidence>,
}

const CASE_EXTRACTION_PROMPT: &str = r#"你是企業案例抽取助手。請只根據以下「官方來源」頁面內容，提取明確表示某公司/組織正在使用「{name}」的案例。

重要規則：
1. 只能根據提供的頁面內容判斷，不可自行猜測
2. 必須有明確公司/組織名稱，且內容要能看出它在使用此產品
3. 若只是產品介紹、合作夥伴列表、媒體報導、或提到但未說明使用，不要列入
4. `usage_summary` 用繁體中文，限 1 句，說明該公司如何使用這個產品
5. `publisher` 填發布案例的官方網站名稱
6. `evidence_type` 填 customer story / case study / blog / docs 其中之一
7. 最多回傳 {max_cases} 筆，不重複公司

只回傳 JSON：
{
  "cases": [
    {
      "company": "Company",
      "title": "案例頁標題",
      "url": "https://example.com/case",
      "publisher": "Example",
      "published_at": "2025-03-12",
      "usage_summary": "一句話說明企業怎麼使用。",
      "evidence_type": "case study"
    }
  ]
}

產品：{name}
官方來源頁面：
{pages}"#;

pub async fn fetch_enterprise_cases(
    blip: &Blip,
    llm: &LLMClient,
    max_cases: usize,
    policy: SourcePolicy,
) -> Result<BlipCaseBundle> {
    let cache_key = [
        CASE_CACHE_PREFIX,
        blip.name.as_str(),
        blip.github_repo.as_str(),
        policy.as_str(),
        &max_cases.to_string(),
    ];
    if let Some((items, _ttl)) = cache::get(&cache_key) {
        if let Some(item) = items.first() {
            let bundle = serde_json::from_str::<BlipCaseBundle>(&item.content)?;
            return Ok(bundle);
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("news_lab/0.1")
        .build()?;

    let hosts = discover_official_hosts(blip, &client).await?;
    if hosts.is_empty() {
        let empty = empty_bundle(blip, policy);
        put_bundle_cache(&cache_key, &empty);
        return Ok(empty);
    }

    let page_collection = collect_case_pages(blip, &hosts, &client).await?;
    if page_collection.pages.is_empty() {
        let empty = empty_bundle(blip, policy);
        if page_collection.fetched_pages > 0 {
            put_bundle_cache(&cache_key, &empty);
        }
        return Ok(empty);
    }

    let prompt = CASE_EXTRACTION_PROMPT
        .replace("{name}", &blip.name)
        .replace("{max_cases}", &max_cases.to_string())
        .replace("{pages}", &page_collection.pages);
    let response = llm.invoke_with_limit(&prompt, 2048).await?;
    let parsed: CaseExtraction =
        serde_json::from_str(&sanitize_json_strings(extract_json(&response)))
            .map_err(|e| anyhow!("企業案例 JSON 解析失敗: {e}"))?;

    let mut seen_companies = HashSet::new();
    let cases: Vec<CaseEvidence> = parsed
        .cases
        .into_iter()
        .filter(|c| !c.company.trim().is_empty() && !c.url.trim().is_empty())
        .filter(|c| {
            let key = normalize_text(&c.company);
            !key.is_empty() && seen_companies.insert(key)
        })
        .take(max_cases)
        .collect();

    let bundle = BlipCaseBundle {
        product_name: blip.name.clone(),
        cases,
        fetched_at: Utc::now().format("%Y-%m-%d").to_string(),
        source_policy: policy.as_str().to_string(),
    };
    put_bundle_cache(&cache_key, &bundle);
    Ok(bundle)
}

fn empty_bundle(blip: &Blip, policy: SourcePolicy) -> BlipCaseBundle {
    BlipCaseBundle {
        product_name: blip.name.clone(),
        cases: vec![],
        fetched_at: Utc::now().format("%Y-%m-%d").to_string(),
        source_policy: policy.as_str().to_string(),
    }
}

fn put_bundle_cache(parts: &[&str], bundle: &BlipCaseBundle) {
    if let Ok(content) = serde_json::to_string(bundle) {
        let item = cache::DisplayItem {
            title: bundle.product_name.clone(),
            content,
            url: String::new(),
            color: "cyan".to_string(),
        };
        cache::put(parts, &[item]);
    }
}

async fn discover_official_hosts(blip: &Blip, client: &reqwest::Client) -> Result<Vec<String>> {
    let mut hosts = Vec::new();
    let mut seen = HashSet::new();

    if !blip.github_repo.is_empty() {
        if let Some(host) = github_homepage_host(&blip.github_repo, client).await {
            if seen.insert(host.clone()) {
                hosts.push(host);
            }
        }
    }

    let query = format!("{} official website", blip.name);
    let search_results = match search_duckduckgo(&query, client).await {
        Ok(results) => results,
        Err(_) if !hosts.is_empty() => return Ok(hosts),
        Err(err) => return Err(err),
    };
    for result in search_results.into_iter().take(6) {
        if let Some(host) = url_host(&result.url) {
            if !is_excluded_host(&host)
                && title_matches_official_host(&result.title, &blip.name, &host)
                && seen.insert(host.clone())
            {
                hosts.push(host);
            }
        }
    }

    Ok(hosts)
}

async fn github_homepage_host(repo: &str, client: &reqwest::Client) -> Option<String> {
    let url = format!("https://api.github.com/repos/{repo}");
    let mut req = client.get(url);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.header("Authorization", format!("token {token}"));
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let repo: GitHubRepoResponse = resp.json().await.ok()?;
    url_host(&repo.homepage)
}

async fn collect_case_pages(
    blip: &Blip,
    hosts: &[String],
    client: &reqwest::Client,
) -> Result<PageCollection> {
    let mut sections = Vec::new();
    let mut seen_urls = HashSet::new();
    let mut fetched_pages = 0usize;
    let mut total_candidate_urls = 0usize;

    for host in hosts.iter().take(3) {
        let mut host_candidate_urls = 0usize;
        let homepage_url = format!("https://{host}");
        if let Some(homepage) = fetch_doc_page(&homepage_url).await {
            fetched_pages += 1;
            if looks_like_case_page(&homepage.url, &homepage.title, &homepage.text, &blip.name) {
                sections.push(format_case_section(&homepage, host));
            }
            for url in candidate_case_urls_from_nav_links(&homepage_url, host, &homepage.nav_links)
            {
                if seen_urls.insert(url) {
                    total_candidate_urls += 1;
                    host_candidate_urls += 1;
                }
            }
        }

        for query in case_search_queries(host, &blip.name) {
            for result in search_duckduckgo(&query, client).await?.into_iter().take(4) {
                if !host_matches(&result.url, host) || !seen_urls.insert(result.url.clone()) {
                    continue;
                }
                total_candidate_urls += 1;
                host_candidate_urls += 1;
            }
            if host_candidate_urls >= 12 {
                break;
            }
        }

        let candidate_list: Vec<String> = seen_urls
            .iter()
            .filter(|url| host_matches(url, host) && url.as_str() != homepage_url)
            .cloned()
            .collect();

        for url in candidate_list {
            let Some(page) = fetch_doc_page(&url).await else {
                continue;
            };
            fetched_pages += 1;
            if !looks_like_case_page(&page.url, &page.title, &page.text, &blip.name) {
                continue;
            }
            sections.push(format_case_section(&page, host));
            if sections.len() >= 6 {
                break;
            }
        }
        if sections.len() >= 6 {
            break;
        }
    }

    if total_candidate_urls > 0 && fetched_pages == 0 {
        return Err(anyhow!("無法抓取企業案例候選頁面"));
    }

    Ok(PageCollection {
        pages: sections.join("\n\n---\n\n"),
        fetched_pages,
    })
}

fn case_search_queries(host: &str, product_name: &str) -> Vec<String> {
    let name = product_name.trim();
    [
        format!(
            "site:{host} \"{name}\" (customer story OR customer stories OR case study OR case studies)"
        ),
        format!("site:{host} \"{name}\" (customers OR success story OR success stories)"),
        format!("site:{host} \"{name}\" (users OR use case OR use cases OR testimonial)"),
    ]
    .into_iter()
    .collect()
}

fn candidate_case_urls_from_nav_links(
    base_url: &str,
    host: &str,
    nav_links: &[String],
) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();

    for href in nav_links {
        if !looks_like_case_link(href) {
            continue;
        }
        let Some(url) = resolve_internal_url(base_url, href) else {
            continue;
        };
        if host_matches(&url, host) && seen.insert(url.clone()) {
            urls.push(url);
        }
    }

    urls
}

fn looks_like_case_link(href: &str) -> bool {
    let href = href.to_lowercase();
    [
        "customer",
        "customers",
        "case-study",
        "case-studies",
        "case_study",
        "case_studies",
        "success-story",
        "success-stories",
        "stories",
        "story",
        "use-case",
        "use-cases",
        "users",
        "testimonials",
    ]
    .iter()
    .any(|kw| href.contains(kw))
}

fn resolve_internal_url(base_url: &str, href: &str) -> Option<String> {
    if href.starts_with("https://") || href.starts_with("http://") {
        return Some(href.to_string());
    }
    if href.starts_with("//") {
        return Some(format!("https:{href}"));
    }

    let base = base_url.trim_end_matches('/');
    if href.starts_with('/') {
        return Some(format!("{base}{href}"));
    }

    if href.starts_with('#') || href.is_empty() {
        return None;
    }

    Some(format!("{base}/{href}"))
}

fn search_error(query: &str, reason: &str) -> anyhow::Error {
    anyhow!("DuckDuckGo 搜尋失敗（{}）: {}", query, reason)
}

async fn search_duckduckgo(query: &str, client: &reqwest::Client) -> Result<Vec<SearchResult>> {
    let url = format!("{}{}", SEARCH_ENDPOINT, urlencoding(query));
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|_| search_error(query, "request error"))?;
    if !resp.status().is_success() {
        return Err(search_error(query, &format!("HTTP {}", resp.status())));
    }
    let body = resp
        .text()
        .await
        .map_err(|_| search_error(query, "response body error"))?;
    Ok(parse_duckduckgo_results(&body))
}

fn parse_duckduckgo_results(html: &str) -> Vec<SearchResult> {
    static RESULT_RE: OnceLock<Regex> = OnceLock::new();
    let re = RESULT_RE.get_or_init(|| {
        Regex::new(r#"(?is)<a[^>]+class="[^"]*result__a[^"]*"[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#)
            .unwrap()
    });

    let mut results = Vec::new();
    for cap in re.captures_iter(html) {
        let href = decode_ddg_redirect(&decode_html_entities(&cap[1]));
        let title = strip_tags(&decode_html_entities(&cap[2]));
        if href.starts_with("http://") || href.starts_with("https://") {
            results.push(SearchResult { title, url: href });
        }
        if results.len() >= 10 {
            break;
        }
    }
    results
}

fn decode_ddg_redirect(href: &str) -> String {
    let needle = "uddg=";
    if let Some(idx) = href.find(needle) {
        return percent_decode(&href[idx + needle.len()..]);
    }
    href.to_string()
}

fn looks_like_case_page(url: &str, title: &str, text: &str, product_name: &str) -> bool {
    let combined = format!(
        "{} {} {}",
        url.to_lowercase(),
        title.to_lowercase(),
        text.to_lowercase()
    );
    let has_product = combined.contains(&product_name.to_lowercase());
    let has_case_signal = [
        "customer",
        "customers",
        "customer story",
        "customer stories",
        "case study",
        "case studies",
        "success story",
        "success stories",
        "testimonial",
        "use case",
        "use cases",
        "uses ",
        "used by",
    ]
    .iter()
    .any(|kw| combined.contains(kw));
    has_product && has_case_signal
}

fn format_case_section(page: &crate::fetcher::docs::DocPage, publisher: &str) -> String {
    let published = extract_date(&page.text).unwrap_or_default();
    let snippet: String = page.text.chars().take(1_500).collect();
    format!(
        "URL: {}\nTITLE: {}\nPUBLISHER: {}\nPUBLISHED_AT: {}\nTEXT:\n{}",
        page.url, page.title, publisher, published, snippet
    )
}

fn extract_date(text: &str) -> Option<String> {
    static DATE_RE: OnceLock<Regex> = OnceLock::new();
    let re = DATE_RE.get_or_init(|| Regex::new(r"\b(20\d{2}-\d{2}-\d{2})\b").unwrap());
    if let Some(cap) = re.captures(text) {
        return Some(cap[1].to_string());
    }

    static SLASH_DATE_RE: OnceLock<Regex> = OnceLock::new();
    let re = SLASH_DATE_RE.get_or_init(|| Regex::new(r"\b(20\d{2}/\d{2}/\d{2})\b").unwrap());
    re.captures(text)
        .and_then(|cap| NaiveDate::parse_from_str(&cap[1], "%Y/%m/%d").ok())
        .map(|d| d.format("%Y-%m-%d").to_string())
}

fn title_mentions_product(title: &str, name: &str) -> bool {
    let title = normalize_text(title);
    let name = normalize_text(name);
    !name.is_empty() && title.contains(&name)
}

fn title_matches_official_host(title: &str, product_name: &str, host: &str) -> bool {
    if title_mentions_product(title, product_name) {
        return true;
    }

    let host_label = host_brand_label(host);
    if host_label.is_empty() {
        return false;
    }

    let title = normalize_text(title);
    !title.is_empty() && title.contains(&host_label)
}

fn host_brand_label(host: &str) -> String {
    let host = host
        .split(':')
        .next()
        .unwrap_or(host)
        .trim_start_matches("www.");
    let labels: Vec<&str> = host.split('.').collect();

    if labels.len() >= 2 {
        let suffix_len = public_suffix_len(&labels);
        if labels.len() > suffix_len {
            let registrable = labels[labels.len() - suffix_len - 1];
            let normalized = normalize_text(registrable);
            if !normalized.is_empty() {
                return normalized;
            }
        }
    }

    for label in labels {
        if !is_generic_host_label(label) {
            return normalize_text(label);
        }
    }

    String::new()
}

fn public_suffix_len(labels: &[&str]) -> usize {
    let last = labels.last().copied().unwrap_or_default();
    let second_last = labels
        .get(labels.len().saturating_sub(2))
        .copied()
        .unwrap_or_default();

    if last.len() == 2
        && matches!(
            second_last,
            "ac" | "co" | "com" | "edu" | "gov" | "mil" | "net" | "org"
        )
        && labels.len() >= 3
    {
        2
    } else {
        1
    }
}

fn is_generic_host_label(label: &str) -> bool {
    matches!(
        label,
        "www" | "docs" | "doc" | "blog" | "help" | "support" | "home"
    )
}

fn is_excluded_host(host: &str) -> bool {
    let host = host.to_lowercase();
    [
        "duckduckgo.com",
        "google.com",
        "bing.com",
        "github.com",
        "news.ycombinator.com",
        "reddit.com",
        "medium.com",
        "linkedin.com",
        "wikipedia.org",
    ]
    .iter()
    .any(|bad| host == *bad || host.ends_with(&format!(".{bad}")))
}

fn host_matches(url: &str, host: &str) -> bool {
    url_host(url)
        .map(|u| u == host || u.ends_with(&format!(".{host}")))
        .unwrap_or(false)
}

fn url_host(url: &str) -> Option<String> {
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = url
        .split(&['/', '?', '#'] as &[char])
        .next()
        .unwrap_or(url)
        .trim()
        .to_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn normalize_text(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn strip_tags(s: &str) -> String {
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    re.replace_all(s, " ").trim().to_string()
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

fn strip_code_fences(s: &str) -> &str {
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

fn extract_json(response: &str) -> &str {
    let stripped = strip_code_fences(response);
    if stripped.trim_start().starts_with('{') {
        return stripped;
    }
    if let Some(m) = Regex::new(r"(?s)\{.+\}")
        .ok()
        .and_then(|re| re.find(response))
    {
        return m.as_str();
    }
    response
}

fn sanitize_json_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    let mut in_string = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if !in_string {
            out.push(c);
            if c == '"' {
                in_string = true;
            }
        } else {
            match c {
                '\\' => {
                    out.push('\\');
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => {
                    out.push('"');
                    in_string = false;
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(c),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duckduckgo_redirect_url() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fcase-study";
        assert_eq!(
            decode_ddg_redirect(href),
            "https://example.com/case-study".to_string()
        );
    }

    #[test]
    fn host_match_allows_subdomain() {
        assert!(host_matches(
            "https://customers.example.com/story",
            "example.com"
        ));
    }

    #[test]
    fn title_match_uses_normalized_text() {
        assert!(title_mentions_product(
            "Redis Enterprise Official Site",
            "Redis"
        ));
    }

    #[test]
    fn official_host_match_accepts_vendor_title() {
        assert!(title_matches_official_host(
            "OpenAI",
            "ChatGPT Enterprise",
            "openai.com"
        ));
    }

    #[test]
    fn host_brand_label_uses_registrable_domain_for_cc_tlds() {
        assert_eq!(host_brand_label("vendor.co.uk"), "vendor");
        assert_eq!(host_brand_label("docs.vendor.com.au"), "vendor");
    }

    #[test]
    fn host_brand_label_skips_generic_subdomains() {
        assert_eq!(host_brand_label("support.docs.openai.com"), "openai");
    }

    #[test]
    fn slash_dates_are_normalized() {
        assert_eq!(
            extract_date("Published on 2025/03/12 by vendor"),
            Some("2025-03-12".to_string())
        );
    }

    #[test]
    fn case_search_queries_cover_common_case_terms() {
        let queries = case_search_queries("example.com", "Redis");

        assert_eq!(queries.len(), 3);
        assert!(queries.iter().any(|q| q.contains("customer stories")));
        assert!(queries.iter().any(|q| q.contains("case studies")));
        assert!(queries.iter().any(|q| q.contains("use cases")));
    }

    #[test]
    fn resolve_internal_url_supports_relative_and_root_links() {
        assert_eq!(
            resolve_internal_url("https://example.com", "/customers/acme"),
            Some("https://example.com/customers/acme".to_string())
        );
        assert_eq!(
            resolve_internal_url("https://example.com", "case-studies/acme"),
            Some("https://example.com/case-studies/acme".to_string())
        );
    }

    #[test]
    fn candidate_case_urls_from_nav_links_keeps_case_like_paths() {
        let urls = candidate_case_urls_from_nav_links(
            "https://example.com",
            "example.com",
            &[
                "/pricing".to_string(),
                "/customers".to_string(),
                "https://example.com/case-studies/acme".to_string(),
                "https://other.com/customers".to_string(),
            ],
        );

        assert_eq!(
            urls,
            vec![
                "https://example.com/customers".to_string(),
                "https://example.com/case-studies/acme".to_string(),
            ]
        );
    }

    #[test]
    fn case_page_detection_accepts_common_plural_signals() {
        assert!(looks_like_case_page(
            "https://example.com/customers",
            "Customer Stories",
            "Redis customer stories from enterprise teams",
            "Redis"
        ));
    }
}
