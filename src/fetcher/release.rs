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

fn to_release_item(r: GHRelease) -> ReleaseItem {
    let published = r.published_at.as_deref().and_then(|s| {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    });
    let major = is_major_release(&r.tag_name);
    let display_name = if r.name.trim().is_empty() {
        r.tag_name.clone()
    } else {
        r.name
    };
    ReleaseItem {
        tag_name: r.tag_name,
        name: display_name,
        body: r.body.chars().take(2000).collect(),
        url: r.html_url,
        published,
        is_major: major,
    }
}

pub struct RepoReleases {
    /// Latest 5 non-major (minor / patch) releases, newest first.
    pub minor_releases: Vec<ReleaseItem>,
    /// Latest major release (vX.0.0), if any exists.
    pub major_release: Option<ReleaseItem>,
}

/// Normalise a user-supplied repo string to "owner/repo".
/// Handles:
///   - owner/repo
///   - https://github.com/owner/repo
///   - https://github.com/owner/repo.git
///   - https://github.com/owner/repo?tab=readme-ov-file
///   - https://github.com/owner/repo#readme
pub fn normalise_repo(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');

    // Strip scheme and host if present
    let s = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);

    // Take at most two path components (owner/repo), ignore sub-paths like /tree/main
    let parts: Vec<&str> = s.splitn(3, '/').collect();
    let (owner, repo_raw) = match parts.as_slice() {
        [owner, repo, ..] => (*owner, *repo),
        _ => return s.to_string(),
    };

    // Strip query string and fragment from the repo component, then .git suffix
    let repo = repo_raw
        .split('?')
        .next()
        .unwrap_or(repo_raw)
        .split('#')
        .next()
        .unwrap_or(repo_raw)
        .trim_end_matches(".git");

    format!("{}/{}", owner, repo)
}

/// Fetch one page of releases; returns an empty vec on any error or when the
/// page is beyond the last one.
async fn fetch_page(
    client: &reqwest::Client,
    repo: &str,
    page: u32,
    auth: &Option<String>,
) -> Vec<GHRelease> {
    let url = format!(
        "https://api.github.com/repos/{}/releases?per_page=50&page={}",
        repo, page
    );
    let mut req = client.get(&url);
    if let Some(token) = auth {
        req = req.header("Authorization", format!("Bearer {}", token));
    }
    match req.send().await {
        Ok(resp) => resp.json::<Vec<GHRelease>>().await.unwrap_or_default(),
        Err(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_bare() {
        assert_eq!(normalise_repo("owner/repo"), "owner/repo");
    }

    #[test]
    fn normalise_https_url() {
        assert_eq!(
            normalise_repo("https://github.com/owner/repo"),
            "owner/repo"
        );
    }

    #[test]
    fn normalise_git_suffix() {
        assert_eq!(
            normalise_repo("https://github.com/owner/repo.git"),
            "owner/repo"
        );
    }

    #[test]
    fn normalise_query_fragment() {
        assert_eq!(
            normalise_repo("https://github.com/owner/repo?tab=readme-ov-file"),
            "owner/repo"
        );
        assert_eq!(
            normalise_repo("https://github.com/owner/repo#readme"),
            "owner/repo"
        );
    }

    #[test]
    fn normalise_trailing_slash() {
        assert_eq!(
            normalise_repo("https://github.com/owner/repo/"),
            "owner/repo"
        );
    }

    #[test]
    fn normalise_sub_path_ignored() {
        assert_eq!(
            normalise_repo("https://github.com/owner/repo/tree/main"),
            "owner/repo"
        );
    }

    #[test]
    fn major_release_detection() {
        assert!(is_major_release("v2.0.0"));
        assert!(is_major_release("v3.0"));
        assert!(!is_major_release("v1.2.0"));
        assert!(!is_major_release("v1.0.1"));
        assert!(!is_major_release("v1.2.3"));
    }
}

pub async fn fetch_repo_releases(repo: &str) -> RepoReleases {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("news-lab/1.0")
        .build()
        .unwrap_or_default();

    let auth = std::env::var("GITHUB_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    let mut minor_releases: Vec<ReleaseItem> = Vec::new();
    let mut major_release: Option<ReleaseItem> = None;

    // Page through releases (newest first). Collect the first 5 non-major releases
    // from whichever page they appear on, and keep paginating until a major release
    // is found or we exhaust all pages (cap at 10 pages = 500 releases).
    'pages: for page in 1u32..=10 {
        let raw = fetch_page(&client, repo, page, &auth).await;
        let is_last = raw.len() < 50;

        for r in raw {
            if r.draft || r.prerelease {
                continue;
            }
            let item = to_release_item(r);
            if item.is_major {
                if major_release.is_none() {
                    major_release = Some(item);
                }
                // Both goals satisfied — no need to fetch further pages.
                if minor_releases.len() >= 5 {
                    break 'pages;
                }
            } else if minor_releases.len() < 5 {
                minor_releases.push(item);
            }
        }

        if is_last {
            break;
        }

        // If we already have both 5 minor releases and a major release, stop early.
        if minor_releases.len() >= 5 && major_release.is_some() {
            break;
        }
    }

    RepoReleases {
        minor_releases,
        major_release,
    }
}
