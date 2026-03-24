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

/// Examine a set of candidates and decide whether to restrict to non-slash tags.
///
/// Returns `true` when both slash and non-slash tags are present — that signals a
/// monorepo where slash-prefixed tags belong to sub-components (e.g. apache/airflow's
/// `helm-chart/1.20.0`). In that case the caller should ignore slash tags.
///
/// Returns `false` when all tags use the same convention (all slash or all non-slash),
/// meaning no filtering is needed.
fn prefer_non_slash(candidates: &[GHRelease]) -> bool {
    let has_non_slash = candidates.iter().any(|r| !r.tag_name.contains('/'));
    let has_slash = candidates.iter().any(|r| r.tag_name.contains('/'));
    has_non_slash && has_slash
}

/// Given the current buffer and the resolved `skip_slash` strategy, return true
/// if we already have enough items to satisfy both output goals.
fn buffer_is_sufficient(buffer: &[GHRelease], skip_slash: bool) -> bool {
    let mut minor_count = 0usize;
    let mut has_major = false;
    for r in buffer {
        if skip_slash && r.tag_name.contains('/') {
            continue;
        }
        if is_major_release(&r.tag_name) {
            has_major = true;
        } else {
            minor_count += 1;
        }
        if minor_count >= 5 && has_major {
            return true;
        }
    }
    false
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

    // Phase 1: collect candidates across pages.
    //
    // Strategy (skip_slash) is determined from the *full* buffer, not page 1
    // alone, so we keep paginating as long as the strategy is still undecided
    // (only-slash seen so far) or we haven't yet collected enough items of the
    // right type. An early-exit fires once the strategy is confirmed AND the
    // buffer already holds the required items, capping at 10 pages (500 releases).
    let mut buffer: Vec<GHRelease> = Vec::new();

    for page in 1u32..=10 {
        let raw = fetch_page(&client, repo, page, &auth).await;
        let is_last = raw.len() < 50;

        buffer.extend(raw.into_iter().filter(|r| !r.draft && !r.prerelease));

        // Recompute strategy from the current buffer (grows with every page).
        // This means a non-slash tag appearing on page 2 correctly flips the
        // strategy before we commit any results.
        let skip_slash = prefer_non_slash(&buffer);

        // If the strategy is now confirmed (we've seen both types, or we've
        // exhausted all slash-only pages) and we have enough items, stop early.
        let strategy_confirmed = skip_slash || !buffer.iter().any(|r| r.tag_name.contains('/'));
        if strategy_confirmed && buffer_is_sufficient(&buffer, skip_slash) {
            break;
        }

        if is_last {
            break;
        }
    }

    // Phase 2: determine final strategy from the complete buffer and extract results.
    let skip_slash = prefer_non_slash(&buffer);

    let mut minor_releases: Vec<ReleaseItem> = Vec::new();
    let mut major_release: Option<ReleaseItem> = None;

    for r in buffer {
        if skip_slash && r.tag_name.contains('/') {
            continue;
        }
        let item = to_release_item(r);
        if item.is_major {
            if major_release.is_none() {
                major_release = Some(item);
            }
        } else if minor_releases.len() < 5 {
            minor_releases.push(item);
        }
    }

    RepoReleases {
        minor_releases,
        major_release,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_release(tag: &str) -> GHRelease {
        GHRelease {
            tag_name: tag.to_string(),
            name: String::new(),
            body: String::new(),
            html_url: String::new(),
            published_at: None,
            prerelease: false,
            draft: false,
        }
    }

    // ── prefer_non_slash ──────────────────────────────────────────────────────

    #[test]
    fn mixed_tags_prefer_non_slash() {
        let releases = vec![
            make_release("helm-chart/1.20.0"),
            make_release("airflow-ctl/0.1.3"),
            make_release("2.10.4"),
        ];
        assert!(prefer_non_slash(&releases));
    }

    #[test]
    fn all_slash_tags_do_not_filter() {
        let releases = vec![
            make_release("release/1.2.3"),
            make_release("release/1.2.2"),
        ];
        assert!(!prefer_non_slash(&releases));
    }

    #[test]
    fn all_non_slash_tags_do_not_filter() {
        let releases = vec![make_release("v1.2.3"), make_release("v1.2.2")];
        assert!(!prefer_non_slash(&releases));
    }

    // ── buffer_is_sufficient ──────────────────────────────────────────────────

    #[test]
    fn buffer_sufficient_non_slash_only() {
        let releases: Vec<GHRelease> = (0..5)
            .map(|i| make_release(&format!("v1.0.{}", i + 1)))
            .chain(std::iter::once(make_release("v2.0.0")))
            .collect();
        // No slash tags → skip_slash = false
        assert!(buffer_is_sufficient(&releases, false));
    }

    #[test]
    fn buffer_insufficient_missing_major() {
        let releases: Vec<GHRelease> = (0..5)
            .map(|i| make_release(&format!("v1.0.{}", i + 1)))
            .collect();
        assert!(!buffer_is_sufficient(&releases, false));
    }

    #[test]
    fn buffer_sufficient_with_slash_filter() {
        // Mix: 5 slash sub-component + 5 non-slash minor + 1 non-slash major
        let mut releases: Vec<GHRelease> = (0..5)
            .map(|i| make_release(&format!("sub/1.0.{}", i)))
            .collect();
        releases.extend((1..=5).map(|i| make_release(&format!("v1.0.{}", i))));
        releases.push(make_release("v2.0.0"));
        // skip_slash = true → should count only non-slash items
        assert!(buffer_is_sufficient(&releases, true));
    }

    #[test]
    fn buffer_insufficient_when_slash_filtered_out() {
        // Only slash-tagged releases; with skip_slash=true the buffer looks empty.
        let releases: Vec<GHRelease> = (0..10)
            .map(|i| make_release(&format!("sub/1.0.{}", i)))
            .collect();
        assert!(!buffer_is_sufficient(&releases, true));
    }

    // ── strategy confirmed late (page 2 scenario) ────────────────────────────

    #[test]
    fn strategy_flips_when_non_slash_appears() {
        // Simulate: page 1 = all slash, page 2 adds a non-slash tag.
        let mut buffer: Vec<GHRelease> = (0..5)
            .map(|i| make_release(&format!("helm-chart/1.{}.0", i)))
            .collect();
        // After page 1: only slash → strategy not yet "prefer non-slash"
        assert!(!prefer_non_slash(&buffer));

        // Page 2 arrives with a non-slash release
        buffer.push(make_release("2.10.4"));
        // Now strategy should flip
        assert!(prefer_non_slash(&buffer));
    }

    // ── normalise_repo ────────────────────────────────────────────────────────

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

    // ── major release detection ───────────────────────────────────────────────

    #[test]
    fn major_release_detection() {
        assert!(is_major_release("v2.0.0"));
        assert!(is_major_release("v3.0"));
        assert!(!is_major_release("v1.2.0"));
        assert!(!is_major_release("v1.0.1"));
        assert!(!is_major_release("v1.2.3"));
    }
}
