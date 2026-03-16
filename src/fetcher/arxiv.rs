use crate::llm::LLMClient;
use chrono::{DateTime, Duration, Utc};
use quick_xml::events::Event;
use quick_xml::Reader;
use regex::Regex;

#[derive(Debug, Clone)]
pub struct ArxivPaper {
    pub arxiv_id: String,
    pub title: String,
    pub url: String,
    pub published: Option<DateTime<Utc>>,
    pub authors: Vec<String>,
    pub abstract_text: String,
    pub categories: Vec<String>,
}

fn parse_arxiv_atom(xml: &str) -> Vec<ArxivPaper> {
    let version_re = Regex::new(r"v\d+$").expect("valid regex");
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut papers: Vec<ArxivPaper> = Vec::new();
    let mut current: Option<ArxivPaper> = None;
    let mut in_entry = false;
    let mut current_tag = String::new();
    let mut in_author = false;
    let mut author_name_buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                let tag = std::str::from_utf8(name.as_ref()).unwrap_or("").to_string();
                current_tag = tag.clone();

                match tag.as_str() {
                    "entry" => {
                        in_entry = true;
                        current = Some(ArxivPaper {
                            arxiv_id: String::new(),
                            title: String::new(),
                            url: String::new(),
                            published: None,
                            authors: Vec::new(),
                            abstract_text: String::new(),
                            categories: Vec::new(),
                        });
                    }
                    "author" if in_entry => {
                        in_author = true;
                        author_name_buf.clear();
                    }
                    "link" if in_entry => {
                        if let Some(paper) = current.as_mut() {
                            // Check rel and href attributes
                            let mut is_alternate = false;
                            let mut href = String::new();
                            for attr in e.attributes().flatten() {
                                let key = std::str::from_utf8(attr.key.local_name().as_ref())
                                    .unwrap_or("")
                                    .to_string();
                                let val = attr.unescape_value().unwrap_or_default().to_string();
                                if key == "rel" && val == "alternate" {
                                    is_alternate = true;
                                }
                                if key == "href" {
                                    href = val;
                                }
                            }
                            if is_alternate && !href.is_empty() {
                                paper.url = href;
                            }
                        }
                    }
                    "category" if in_entry => {
                        if let Some(paper) = current.as_mut() {
                            for attr in e.attributes().flatten() {
                                let key = std::str::from_utf8(attr.key.local_name().as_ref())
                                    .unwrap_or("")
                                    .to_string();
                                let val = attr.unescape_value().unwrap_or_default().to_string();
                                if key == "term" {
                                    paper.categories.push(val);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                let text = text.trim().to_string();
                if text.is_empty() || !in_entry {
                    continue;
                }
                if let Some(paper) = current.as_mut() {
                    match current_tag.as_str() {
                        "id" => {
                            // Strip version suffix: arxiv.org/abs/XXXX.XXXXX v1 → XXXX.XXXXX
                            let id = text
                                .trim_start_matches("http://arxiv.org/abs/")
                                .trim_start_matches("https://arxiv.org/abs/");
                            let id = version_re.replace(id, "").to_string();
                            paper.arxiv_id = id.trim().to_string();
                            if paper.url.is_empty() {
                                paper.url = format!("https://arxiv.org/abs/{}", paper.arxiv_id);
                            }
                        }
                        "title" => {
                            let collapsed = collapse_whitespace(&text);
                            paper.title = collapsed;
                        }
                        "summary" => {
                            let collapsed = collapse_whitespace(&text);
                            paper.abstract_text = collapsed.chars().take(500).collect();
                        }
                        "published" => {
                            if let Ok(dt) = text.parse::<DateTime<Utc>>() {
                                paper.published = Some(dt);
                            }
                        }
                        "name" if in_author => {
                            author_name_buf = text;
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.local_name();
                let tag = std::str::from_utf8(name.as_ref()).unwrap_or("").to_string();
                match tag.as_str() {
                    "entry" => {
                        if let Some(mut paper) = current.take() {
                            // Trim authors to max 3 + "et al."
                            if paper.authors.len() > 3 {
                                paper.authors.truncate(3);
                                paper.authors.push("et al.".to_string());
                            }
                            papers.push(paper);
                        }
                        in_entry = false;
                    }
                    "author" if in_entry => {
                        if let Some(paper) = current.as_mut() {
                            if !author_name_buf.is_empty() {
                                paper.authors.push(author_name_buf.clone());
                            }
                        }
                        in_author = false;
                        author_name_buf.clear();
                    }
                    _ => {}
                }
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    papers
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── Domain query expansion ──────────────────────────────────────────────────────

const EXPAND_PROMPT: &str = r#"你是一位熟悉軟體工程、資料工程、AI/ML 與雲端技術的研究員。
使用者正在搜尋與「軟體/技術領域」相關的 arXiv 學術論文。

請根據以下關鍵字，判斷其在**軟體與資訊技術**脈絡下的含義，列出 5–8 個適合在 arXiv 上搜尋的英文技術術語。

⚠️ 重要規則：
- 每個術語最多 3 個英文單字，越短越好（arXiv 搜尋引擎對長短語支援不佳）
- 關鍵字可能是工具名稱（如 Airflow = Apache Airflow 工作流程排程器，而非流體力學）
- 請優先考慮軟體/CS 領域的解讀
- 術語應為該領域的核心技術概念，而非重複關鍵字本身
- 不要包含括號、縮寫或特殊符號

關鍵字：{keyword}

只回傳 JSON 字串陣列，不加任何其他文字，範例：
["workflow scheduling", "DAG execution", "task orchestration", "pipeline automation"]"#;

async fn expand_query(kw: &str, llm: &LLMClient) -> Vec<String> {
    let prompt = EXPAND_PROMPT.replace("{keyword}", kw);
    let Ok(response) = llm.invoke(&prompt).await else {
        return vec![kw.to_string()];
    };
    let s = response.trim();
    let s = if s.starts_with("```") {
        s.split_once('\n').map(|x| x.1).unwrap_or(s)
    } else {
        s
    };
    let s = if s.ends_with("```") {
        s.rsplit_once("```").map(|x| x.0).unwrap_or(s).trim_end()
    } else {
        s
    };
    // find the JSON array
    let s = if let Some(start) = s.find('[') {
        &s[start..]
    } else {
        s
    };
    match serde_json::from_str::<Vec<String>>(s) {
        Ok(terms) if !terms.is_empty() => terms,
        _ => vec![kw.to_string()],
    }
}

/// URL-encode a term for use inside arXiv field query (ti:/abs:).
/// Spaces → `+`, special chars → percent-encoded.
fn arxiv_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' => c.to_string(),
            ' ' => "+".to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

/// Returns true if all meaningful words (len > 2) in `term` appear anywhere in `text`.
fn term_matches(term: &str, text: &str) -> bool {
    let words: Vec<&str> = term.split_whitespace().filter(|w| w.len() > 2).collect();
    !words.is_empty() && words.iter().all(|w| text.contains(w))
}

/// Build an arXiv AND-anchor clause from the original keyword's meaningful words.
/// e.g. "medical ai" → "+AND+(ti:medical+OR+abs:medical)"
/// Returns empty string if no meaningful anchor words (all too short).
fn anchor_clause(kw: &str) -> String {
    let anchors: Vec<String> = kw
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .map(|w| {
            let enc = arxiv_encode(w);
            format!("ti:{enc}+OR+abs:{enc}")
        })
        .collect();
    if anchors.is_empty() {
        String::new()
    } else {
        format!("+AND+({})", anchors.join("+OR+"))
    }
}

/// Domain-aware arXiv search: expands keyword into related technical terms via LLM,
/// each term is searched WITH the original keyword as anchor context,
/// merges and deduplicates, falls back to wider date windows.
pub async fn fetch_domain_papers(
    kw: &str,
    max: usize,
    _days: i64,
    llm: &LLMClient,
) -> (Vec<ArxivPaper>, Vec<String>) {
    let terms = expand_query(kw, llm).await;
    let terms_lower: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    let anchor = anchor_clause(kw);

    // Try progressively wider date windows: 90 days → 365 days → no limit
    for days in [90i64, 365, 0] {
        // Each term query is constrained by the original keyword anchor
        let per_term = (max * 2).max(10);
        let queries: Vec<String> = terms
            .iter()
            .map(|term| {
                let enc = arxiv_encode(term);
                format!("(ti:{enc}+OR+abs:{enc}){anchor}")
            })
            .collect();
        let futures: Vec<_> = queries
            .iter()
            .map(|q| search_arxiv_query(q, per_term, days))
            .collect();

        let results = futures::future::join_all(futures).await;

        // Deduplicate by arxiv_id
        let mut seen = std::collections::HashSet::new();
        let mut all: Vec<ArxivPaper> = results
            .into_iter()
            .flatten()
            .filter(|p| seen.insert(p.arxiv_id.clone()))
            .collect();

        // Post-filter: all words of at least one term must appear in title+abstract
        all.retain(|p| {
            let text = format!(
                "{} {}",
                p.title.to_lowercase(),
                p.abstract_text.to_lowercase()
            );
            terms_lower.iter().any(|t| term_matches(t, &text))
        });

        if !all.is_empty() {
            all.sort_by(|a, b| {
                b.published
                    .unwrap_or_default()
                    .cmp(&a.published.unwrap_or_default())
            });
            all.truncate(max);
            return (all, terms);
        }
    }

    (vec![], terms)
}

/// Search arXiv with a pre-built query string (supports complex OR expressions).
async fn search_arxiv_query(query: &str, max: usize, days: i64) -> Vec<ArxivPaper> {
    let url = format!(
        "https://export.arxiv.org/api/query?search_query={}&start=0&max_results={}&sortBy=submittedDate&sortOrder=descending",
        query,
        max * 3
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let Ok(resp) = client.get(&url).send().await else {
        return vec![];
    };
    let Ok(body) = resp.text().await else {
        return vec![];
    };

    let cutoff = if days > 0 {
        Some(Utc::now() - Duration::days(days))
    } else {
        None
    };

    let mut papers = parse_arxiv_atom(&body);
    if let Some(cutoff) = cutoff {
        papers.retain(|p| p.published.map(|d| d >= cutoff).unwrap_or(true));
    }
    papers.truncate(max);
    papers
}
