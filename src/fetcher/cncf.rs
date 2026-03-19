use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct CNCFProject {
    pub name: String,
    pub full_name: String, // owner/repo on GitHub
    pub url: String,       // GitHub URL
    pub description: String,
    pub stars: u64,
    pub language: Option<String>,
    pub last_updated: Option<DateTime<Utc>>,
    pub maturity: String, // "graduated" | "incubating" | "sandbox"
    pub accepted_at: Option<DateTime<Utc>>,
}

// ── GitHub API types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TocIssue {
    title: String,
    body: Option<String>,
    closed_at: Option<String>,
}

#[derive(Deserialize)]
struct GhRepo {
    full_name: String,
    html_url: String,
    description: Option<String>,
    #[serde(default)]
    stargazers_count: u64,
    language: Option<String>,
    pushed_at: Option<String>,
}

#[derive(Deserialize)]
struct SearchResult {
    items: Vec<GhRepo>,
}

// ── Title / body parsing ───────────────────────────────────────────────────────

/// Strip common CNCF TOC issue title boilerplate to get just the project name.
fn parse_project_name(title: &str) -> Option<String> {
    // Only process titles that look like application submissions
    let tl = title.to_lowercase();
    let is_application = tl.contains("application") || tl.contains("joining cncf");
    if !is_application {
        return None;
    }

    let mut name = title.to_string();

    // Strip leading brackets: "[Graduation] ", "[Incubation] ", "[Sandbox] "
    if let Some(rest) = name
        .strip_prefix("[Graduation] ")
        .or_else(|| name.strip_prefix("[Incubation] "))
        .or_else(|| name.strip_prefix("[Sandbox] "))
    {
        name = rest.to_string();
    }

    // Strip trailing " Graduation Application" / " Incubation Application" etc.
    for suffix in &[
        " Graduation Application",
        " Incubation Application",
        " Sandbox Application",
        " CNCF Sandbox Application",
        " graduation application",
        " incubation application",
    ] {
        if let Some(s) = name.strip_suffix(suffix) {
            name = s.to_string();
            break;
        }
    }

    // Handle "Project Moving Levels Checklist: X joining CNCF at Incubation level"
    if let Some(rest) = name.strip_prefix("Project Moving Levels Checklist: ") {
        name = rest.to_string();
    }
    for marker in [" joining CNCF", " joins CNCF", " to join CNCF"] {
        if let Some(idx) = name.find(marker) {
            name = name[..idx].to_string();
            break;
        }
    }

    let name = name.trim().to_string();
    if name.len() < 2 || name.to_lowercase().contains("checklist") {
        return None;
    }
    Some(name)
}

/// Extract the first plausible project GitHub repo from an issue body.
/// Prefers lines that start with "Project Repo(s):" or similar.
fn extract_github_repo(body: &str, project_name: &str) -> Option<String> {
    let re = Regex::new(r"https://github\.com/([\w\-]+/[\w\-\.]+)").ok()?;

    let skip_repos: &[&str] = &[
        "toc",
        "landscape",
        ".github",
        "artwork",
        "foundation",
        "cncf",
    ];
    let name_key: String = project_name
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();

    // First pass: look for the "Project Repo" line
    for line in body.lines() {
        let ll = line.to_lowercase();
        if ll.contains("project repo") || ll.contains("github.com") {
            if let Some(cap) = re.captures(line) {
                let full = cap[1].to_string();
                let parts: Vec<&str> = full.splitn(2, '/').collect();
                if parts.len() == 2 {
                    let (owner, repo) = (parts[0], parts[1]);
                    if owner == "cncf" || skip_repos.contains(&repo) {
                        continue;
                    }
                    return Some(full);
                }
            }
        }
    }

    // Second pass: pick the first non-CNCF GitHub link whose name matches
    let mut fallback: Option<String> = None;
    for cap in re.captures_iter(body) {
        let full = cap[1].to_string();
        let parts: Vec<&str> = full.splitn(2, '/').collect();
        if parts.len() != 2 {
            continue;
        }
        let (owner, repo) = (parts[0], parts[1]);
        if owner == "cncf" || skip_repos.contains(&repo) {
            continue;
        }
        let repo_key: String = repo
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        let owner_key: String = owner
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();

        if repo_key.contains(&name_key)
            || name_key.contains(&repo_key)
            || owner_key.contains(&name_key)
            || name_key.contains(&owner_key)
        {
            return Some(full);
        }
        if fallback.is_none() {
            fallback = Some(full);
        }
    }
    fallback
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

// ── Keyword search ─────────────────────────────────────────────────────────────

/// Search GitHub repos that have the `cncf` topic and match `kw`.
pub async fn fetch_cncf_by_keyword(kw: &str, max: usize) -> Vec<CNCFProject> {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("news_lab/0.1")
        .build()
        .unwrap_or_default();

    let query = format!("{} topic:cncf", urlencoding(kw));
    let url = format!(
        "https://api.github.com/search/repositories?q={}&sort=stars&order=desc&per_page={}",
        query,
        max.min(30)
    );
    let mut req = client.get(&url);
    if let Some(ref tok) = token {
        req = req.header("Authorization", format!("token {}", tok));
    }
    let Ok(resp) = req.send().await else {
        return vec![];
    };
    let Ok(result) = resp.json::<SearchResult>().await else {
        return vec![];
    };

    result
        .items
        .into_iter()
        .take(max)
        .map(|r| {
            let last_updated = r
                .pushed_at
                .as_deref()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok());
            CNCFProject {
                name: r
                    .full_name
                    .split('/')
                    .nth(1)
                    .unwrap_or(&r.full_name)
                    .to_string(),
                full_name: r.full_name.clone(),
                url: r.html_url,
                description: r.description.unwrap_or_default(),
                stars: r.stargazers_count,
                language: r.language,
                last_updated,
                maturity: String::new(), // not available from search API
                accepted_at: None,
            }
        })
        .collect()
}

// ── Main fetch ─────────────────────────────────────────────────────────────────

pub async fn fetch_cncf_projects(max: usize) -> Vec<CNCFProject> {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("news_lab/0.1")
        .build()
        .unwrap_or_default();

    // (label, maturity string)
    let level_labels = [
        ("level%2Fgraduation", "graduated"),
        ("level%2Fincubation", "incubating"),
        ("level%2Fsandbox", "sandbox"),
    ];

    // Collect (name, maturity, accepted_at, repo_hint) candidates
    type Candidate = (String, String, Option<DateTime<Utc>>, Option<String>);
    let mut candidates: Vec<Candidate> = Vec::new();

    for (label, maturity) in &level_labels {
        let url = format!(
            "https://api.github.com/repos/cncf/toc/issues?state=closed&labels={}&sort=updated&direction=desc&per_page=50",
            label
        );
        let mut req = client.get(&url);
        if let Some(ref tok) = token {
            req = req.header("Authorization", format!("token {}", tok));
        }
        let Ok(resp) = req.send().await else { continue };
        let Ok(issues) = resp.json::<Vec<TocIssue>>().await else {
            continue;
        };

        for issue in issues {
            let Some(name) = parse_project_name(&issue.title) else {
                continue;
            };
            let accepted_at = issue
                .closed_at
                .as_deref()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok());
            let repo_hint = issue
                .body
                .as_deref()
                .and_then(|b| extract_github_repo(b, &name));
            candidates.push((name, maturity.to_string(), accepted_at, repo_hint));
        }
    }

    // Sort by accepted_at descending (most recent first), deduplicate by name
    candidates.sort_by(|a, b| b.2.cmp(&a.2));
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|(name, _, _, _)| {
            let key: String = name
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            !key.is_empty() && seen.insert(key)
        })
        .take(max * 3)
        .collect();

    // Fetch GitHub stats for each candidate (sequential, with short-circuit on rate limit)
    let mut projects: Vec<CNCFProject> = Vec::new();

    for (name, maturity, accepted_at, repo_hint) in candidates {
        if projects.len() >= max {
            break;
        }

        let repo: Option<GhRepo> = if let Some(ref full_name) = repo_hint {
            let api_url = format!("https://api.github.com/repos/{}", full_name);
            let mut req = client.get(&api_url);
            if let Some(ref tok) = token {
                req = req.header("Authorization", format!("token {}", tok));
            }
            match req.send().await {
                Ok(resp) if resp.status() == 403 => break, // rate limited
                Ok(resp) if resp.status().is_success() => resp.json::<GhRepo>().await.ok(),
                _ => None,
            }
        } else {
            // Search GitHub by project name
            let url = format!(
                "https://api.github.com/search/repositories?q={}+in:name&sort=stars&order=desc&per_page=3",
                urlencoding(&name)
            );
            let mut req = client.get(&url);
            if let Some(ref tok) = token {
                req = req.header("Authorization", format!("token {}", tok));
            }
            match req.send().await {
                Ok(resp) if resp.status() == 403 => break,
                Ok(resp) => resp
                    .json::<SearchResult>()
                    .await
                    .ok()
                    .and_then(|s| s.items.into_iter().next()),
                Err(_) => None,
            }
        };

        if let Some(r) = repo {
            let last_updated = r
                .pushed_at
                .as_deref()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok());
            projects.push(CNCFProject {
                name: name.clone(),
                full_name: r.full_name,
                url: r.html_url,
                description: r.description.unwrap_or_default(),
                stars: r.stargazers_count,
                language: r.language,
                last_updated,
                maturity,
                accepted_at,
            });
        }
    }

    projects
}
