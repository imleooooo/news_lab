use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct ReleaseItem {
    pub tag_name: String,
    pub name: String,
    pub body: String,
    pub url: String,
    pub published: Option<DateTime<Utc>>,
    pub is_major: bool,
}

#[derive(Deserialize)]
struct GHRelease {
    tag_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

/// Parse the first three numeric components from a version tag like "v1.2.3" or "1.2".
fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    // Strip leading non-digit characters (e.g. 'v', 'V', 'release-')
    let s = tag.trim_start_matches(|c: char| !c.is_ascii_digit());
    // Ignore pre-release suffix after '-'
    let s = s.split('-').next().unwrap_or(s);
    let parts: Vec<u64> = s.split('.').filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [a, b, c, ..] => Some((*a, *b, *c)),
        [a, b] => Some((*a, *b, 0)),
        [a] => Some((*a, 0, 0)),
        _ => None,
    }
}

/// A release is considered "major" when minor == 0 && patch == 0 (e.g. v2.0.0, v3.0).
fn is_major_release(tag: &str) -> bool {
    matches!(parse_version(tag), Some((_, 0, 0)))
}

pub struct RepoReleases {
    /// Latest 5 non-major (minor / patch) releases, newest first.
    pub minor_releases: Vec<ReleaseItem>,
    /// Latest major release (vX.0.0), if any exists.
    pub major_release: Option<ReleaseItem>,
}

/// Normalise a user-supplied repo string to "owner/repo".
/// Accepts "owner/repo" or full GitHub URLs.
pub fn normalise_repo(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');
    // Strip scheme and host if present
    let s = if let Some(path) = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
    {
        path
    } else {
        s
    };
    // Take at most two path components (owner/repo)
    let parts: Vec<&str> = s.splitn(3, '/').collect();
    match parts.as_slice() {
        [owner, repo, ..] => format!("{}/{}", owner, repo),
        _ => s.to_string(),
    }
}

pub async fn fetch_repo_releases(repo: &str) -> RepoReleases {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("news-lab/1.0")
        .build()
        .unwrap_or_default();

    let url = format!(
        "https://api.github.com/repos/{}/releases?per_page=50",
        repo
    );

    let mut req = client.get(&url);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.trim().is_empty() {
            req = req.header("Authorization", format!("Bearer {}", token.trim()));
        }
    }

    let raw_releases: Vec<GHRelease> = match req.send().await {
        Ok(resp) => resp.json().await.unwrap_or_default(),
        Err(_) => vec![],
    };

    let items: Vec<ReleaseItem> = raw_releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .map(|r| {
            let published = r.published_at.as_deref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            });
            let major = is_major_release(&r.tag_name);
            let display_name = if r.name.trim().is_empty() {
                r.tag_name.clone()
            } else {
                r.name.clone()
            };
            ReleaseItem {
                tag_name: r.tag_name,
                name: display_name,
                body: r.body.chars().take(2000).collect(),
                url: r.html_url,
                published,
                is_major: major,
            }
        })
        .collect();

    let minor_releases: Vec<ReleaseItem> = items
        .iter()
        .filter(|r| !r.is_major)
        .take(5)
        .cloned()
        .collect();

    let major_release = items.into_iter().find(|r| r.is_major);

    RepoReleases {
        minor_releases,
        major_release,
    }
}
