pub mod terminal;

use crate::fetcher::NewsItem;
use crate::llm::LLMClient;
use anyhow::Result;
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Blip struct (mirrors Python version) ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blip {
    pub name: String,
    /// "q1" | "q2" | "q3" | "q4"
    pub quadrant: String,
    /// "adopt" | "trial" | "assess" | "hold"
    pub ring: String,
    #[serde(default)]
    pub is_open_source: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub upstream: Vec<String>,
    #[serde(default)]
    pub downstream: Vec<String>,
    #[serde(default)]
    pub pros: Vec<String>,
    #[serde(default)]
    pub cons: Vec<String>,
    #[serde(default)]
    pub rationale: String,
    // filled by check_oss_activity
    #[serde(default)]
    pub github_repo: String,
    #[serde(default)]
    pub github_days: Option<i64>,
    // assigned during build_radar
    #[serde(skip)]
    pub number: usize,
}

// ── Prompt (matches Python version) ───────────────────────────────────────────

const RADAR_PROMPT: &str = r#"你是一位技術生態系統分析師。今天日期：{today}。
根據以下關於「{keyword}」的最新新聞，分析該領域完整的開源/閉源專案生態。

最新新聞（{n_news} 篇）：
{news_list}

任務：
1. 從新聞中找出所有提到的開源/閉源專案、模型、工具、框架、技術方法
2. 加入你知道的「{keyword}」領域其他重要專案（確保生態圖完整）
3. 根據「{keyword}」領域特性，為 4 個象限命名（例如 AI 領域可用「模型、框架、工具、技術」）
4. 為每個項目判斷成熟度環形（以 {today} 為基準，評估當下的業界地位）：
   - adopt  → 目前生產環境主流，業界已廣泛採用的主力選擇
   - trial  → 有成功案例，值得在新專案中採用，但尚未全面普及
   - assess → 值得關注與探索，仍在快速發展或剛進入市場
   - hold   → 已被新一代取代、技術過時、或有重大疑慮應暫緩採用

   ⚠️ 環形判斷原則（重要）：
   - 環形代表「業界採用成熟度」，與開源/閉源無關
   - OpenAI（GPT-5.x）、Anthropic（Claude）、Google（Gemini）的旗艦 API 均有數百萬用戶、完整企業 SLA、
     穩定文件，應列為 adopt，不因閉源而降級
   - 「閉源」應體現在 cons 欄位（隱私、成本、廠商鎖定），而非用來壓低環形評級
   - 每個產品只列最新版本，不列舊版

只回傳 JSON，格式如下（不要加任何其他文字）：
{
  "quadrant_names": {
    "q1": "模型 & 演算法",
    "q2": "框架 & 函式庫",
    "q3": "工具 & 平台",
    "q4": "技術 & 方法"
  },
  "blips": [
    {
      "name": "ToolX",
      "quadrant": "q2",
      "ring": "adopt",
      "is_open_source": true,
      "description": "一句話說明用途。\n• 核心功能亮點\n• 技術特色\n• 適用場景",
      "license": "Apache 2.0",
      "upstream": ["上游 A", "上游 B"],
      "downstream": ["下游 A", "下游 B"],
      "pros": ["推薦理由 1", "推薦理由 2"],
      "cons": ["風險或限制 1", "風險或限制 2"],
      "rationale": "說明為什麼放這個環形與象限。"
    }
  ]
}

命名規則（⚠️ 嚴格遵守）：
- 同一產品系列只能出現一次，版本號以最新版為準
- name 用英文或常見縮寫，≤20 字元
- 4 個環形都要有項目，blips 數量：15–40 個
- 開源/閉源要準確（GitHub 有 repo 的為開源，API-only 的為閉源）
- quadrant 欄位只能填 q1 / q2 / q3 / q4
- ring 欄位只能填 adopt / trial / assess / hold（全小寫）"#;

// ── JSON extraction ────────────────────────────────────────────────────────────

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

// ── Deduplication (mirrors Python _deduplicate) ────────────────────────────────

fn key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

fn tokens(name: &str) -> std::collections::HashSet<String> {
    let stopwords = ["&", "and", "the", "by", "for"];
    name.split([' ', '.', '-', '_'])
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty() && !stopwords.contains(&t.as_str()))
        .collect()
}

fn ring_rank(ring: &str) -> usize {
    match ring {
        "adopt" => 0,
        "trial" => 1,
        "assess" => 2,
        "hold" => 3,
        _ => 2,
    }
}

fn deduplicate(blips: Vec<Blip>) -> Vec<Blip> {
    let mut kept: Vec<Blip> = Vec::new();

    for candidate in blips {
        let ck = key(&candidate.name);
        let ct = tokens(&candidate.name);
        let mut merged = false;

        for existing in kept.iter_mut() {
            let ek = key(&existing.name);
            let et = tokens(&existing.name);

            let is_dup = ck.starts_with(&ek)
                || ek.starts_with(&ck)
                || (!ct.is_empty() && !et.is_empty() && (ct.is_subset(&et) || et.is_subset(&ct)));

            if is_dup {
                let c_rank = ring_rank(&candidate.ring);
                let e_rank = ring_rank(&existing.ring);
                if ck.len() > ek.len() || (ck.len() == ek.len() && c_rank < e_rank) {
                    *existing = candidate.clone();
                }
                merged = true;
                break;
            }
        }
        if !merged {
            kept.push(candidate);
        }
    }
    kept
}

// ── GitHub OSS activity check ──────────────────────────────────────────────────

pub async fn check_oss_activity(blips: &mut [Blip]) {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("news_lab/0.1")
        .build()
        .unwrap_or_default();

    for blip in blips.iter_mut() {
        if !blip.is_open_source || blip.name.is_empty() {
            continue;
        }
        // +in:name ensures the repo's name contains the search term (avoids unrelated repos
        // that merely mention the technology in their README/description).
        // We fetch 5 candidates and pick the most recently pushed one among those whose
        // repo name (the part after "/") fuzzy-matches the blip name.
        let url = format!(
            "https://api.github.com/search/repositories?q={}+in:name&sort=stars&order=desc&per_page=5",
            urlencoding(&blip.name)
        );
        let mut req = client.get(&url);
        if let Some(ref tok) = token {
            req = req.header("Authorization", format!("token {}", tok));
        }
        let Ok(resp) = req.send().await else { continue };
        if resp.status() == 403 {
            break;
        } // rate limited
        let Ok(data) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        let Some(items) = data["items"].as_array() else {
            continue;
        };
        if items.is_empty() {
            continue;
        }

        let name_key: String = blip
            .name
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();

        // Score: (name_mismatch 0/1, days_since_push) — lower is better
        let best = items.iter().min_by_key(|repo| {
            let repo_name: String = repo["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            let name_match: usize =
                if repo_name.contains(&name_key) || name_key.contains(&repo_name) {
                    0
                } else {
                    1
                };
            let days_since = repo["pushed_at"]
                .as_str()
                .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
                .map(|d| (Utc::now() - d).num_days() as usize)
                .unwrap_or(usize::MAX);
            (name_match, days_since)
        });
        let Some(repo) = best else { continue };

        let pushed_at = repo["pushed_at"].as_str().unwrap_or("");
        let Ok(last_push) = pushed_at.parse::<chrono::DateTime<Utc>>() else {
            continue;
        };
        let days = (Utc::now() - last_push).num_days();

        blip.github_repo = repo["full_name"].as_str().unwrap_or("").to_string();
        blip.github_days = Some(days);

        let old = blip.ring.clone();
        if days > 365 {
            blip.ring = downgrade_ring(&old, 2);
            blip.rationale = format!(
                "{}\n⚠️ GitHub 最後更新 {} 天前，活躍度極低，從 {} 下調兩級。",
                blip.rationale,
                days,
                old.to_uppercase()
            );
        } else if days > 180 {
            blip.ring = downgrade_ring(&old, 1);
            blip.rationale = format!(
                "{}\n⚠️ GitHub 最後更新 {} 天前，活躍度偏低，從 {} 下調一級。",
                blip.rationale,
                days,
                old.to_uppercase()
            );
        }
    }
}

fn downgrade_ring(ring: &str, steps: usize) -> String {
    let order = ["adopt", "trial", "assess", "hold"];
    let idx = order.iter().position(|&r| r == ring).unwrap_or(2);
    order[(idx + steps).min(3)].to_string()
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

// ── Main extraction ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RadarResponse {
    #[serde(default)]
    quadrant_names: HashMap<String, String>,
    #[serde(default)]
    blips: Vec<Blip>,
}

pub fn default_quadrant_names() -> HashMap<String, String> {
    [
        ("q1", "模型 & 演算法"),
        ("q2", "框架 & 函式庫"),
        ("q3", "工具 & 平台"),
        ("q4", "技術 & 方法"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

pub async fn extract_blips(
    items: &[NewsItem],
    kw: &str,
    llm: &LLMClient,
) -> Result<(HashMap<String, String>, Vec<Blip>)> {
    let news_list: String = items
        .iter()
        .take(10)
        .map(|item| format!("- [{}] {}", item.source, item.title))
        .collect::<Vec<_>>()
        .join("\n");

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let prompt = RADAR_PROMPT
        .replace("{today}", &today)
        .replace("{keyword}", kw)
        .replace("{n_news}", &items.len().min(10).to_string())
        .replace("{news_list}", &news_list);

    // Radar JSON can be large (15-40 blips); use higher token limit
    let response = llm.invoke_with_limit(&prompt, 16384).await?;
    let json_str = extract_json(&response);

    let parsed: RadarResponse = match serde_json::from_str(json_str) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  [radar] JSON 解析失敗: {e}");
            eprintln!("  [radar] LLM 原始回應:\n{response}");
            return Ok((default_quadrant_names(), vec![]));
        }
    };

    let q_names = if parsed.quadrant_names.is_empty() {
        default_quadrant_names()
    } else {
        parsed.quadrant_names
    };

    // Normalize ring/quadrant to lowercase
    let mut blips: Vec<Blip> = parsed
        .blips
        .into_iter()
        .map(|mut b| {
            b.ring = b.ring.to_lowercase().trim().to_string();
            b.quadrant = b.quadrant.to_lowercase().trim().to_string();
            // Validate values
            if !["adopt", "trial", "assess", "hold"].contains(&b.ring.as_str()) {
                b.ring = "assess".to_string();
            }
            if !["q1", "q2", "q3", "q4"].contains(&b.quadrant.as_str()) {
                b.quadrant = "q1".to_string();
            }
            b
        })
        .collect();

    blips = deduplicate(blips);

    Ok((q_names, blips))
}

// ── Review prompt ───────────────────────────────────────────────────────────────

const REVIEW_PROMPT: &str = r#"你是一位技術生態系統審核專家。今天日期：{today}。
關鍵字：「{keyword}」

目前雷達已有 {n_blips} 個項目：
{blips_summary}

象限定義：
{q_names}

任務：審核這份雷達圖是否完整涵蓋「{keyword}」領域的技術生態。請考量：
1. 各象限是否有明顯缺漏的重要開源或閉源專案？
2. 開源與閉源的代表性是否平衡？
3. 是否有近期重要的新興技術未被納入？

如果雷達圖已足夠完整，只回傳：
{"satisfied": true}

如果有重要缺漏，回傳（只列出缺漏項目，不重複現有項目）：
{
  "satisfied": false,
  "reason": "簡短說明缺漏原因",
  "blips": [
    {
      "name": "ToolX",
      "quadrant": "q2",
      "ring": "adopt",
      "is_open_source": true,
      "description": "一句話說明用途。",
      "license": "Apache 2.0",
      "upstream": [],
      "downstream": [],
      "pros": ["優點"],
      "cons": ["缺點"],
      "rationale": "為什麼需要加入。"
    }
  ]
}

規則：
- 只新增真正缺漏的重要項目（3–10 個）
- quadrant 只填 q1/q2/q3/q4，ring 只填 adopt/trial/assess/hold（全小寫）
- name ≤20 字元，只回傳 JSON，不加任何其他文字"#;

// ── Review response ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ReviewResponse {
    #[serde(default)]
    satisfied: bool,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    blips: Vec<Blip>,
}

/// Asks the review LLM to evaluate and optionally augment the blip list.
/// Returns `true` if the LLM is satisfied (no additions needed).
pub async fn review_and_augment(
    blips: &mut Vec<Blip>,
    q_names: &HashMap<String, String>,
    kw: &str,
    review_llm: &LLMClient,
) -> bool {
    let today = Utc::now().format("%Y-%m-%d").to_string();

    let blips_summary: String = blips
        .iter()
        .map(|b| {
            let oss = if b.is_open_source { "開源" } else { "閉源" };
            format!(
                "  - {} | {} | {} | {}",
                b.name,
                q_names
                    .get(&b.quadrant)
                    .map(|s| s.as_str())
                    .unwrap_or(&b.quadrant),
                b.ring,
                oss
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let q_names_str: String = q_names
        .iter()
        .map(|(k, v)| format!("  {k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = REVIEW_PROMPT
        .replace("{today}", &today)
        .replace("{keyword}", kw)
        .replace("{n_blips}", &blips.len().to_string())
        .replace("{blips_summary}", &blips_summary)
        .replace("{q_names}", &q_names_str);

    let response = match review_llm.invoke_with_limit(&prompt, 8192).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  [review] LLM 呼叫失敗: {e}");
            return true; // treat as satisfied on error
        }
    };

    let json_str = extract_json(&response);
    let parsed: ReviewResponse = match serde_json::from_str(json_str) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  [review] JSON 解析失敗: {e}");
            return true;
        }
    };

    if parsed.satisfied || parsed.blips.is_empty() {
        return true;
    }

    eprintln!("  [review] 補充原因: {}", parsed.reason);

    // Normalize and merge new blips
    let new_blips: Vec<Blip> = parsed
        .blips
        .into_iter()
        .map(|mut b| {
            b.ring = b.ring.to_lowercase().trim().to_string();
            b.quadrant = b.quadrant.to_lowercase().trim().to_string();
            if !["adopt", "trial", "assess", "hold"].contains(&b.ring.as_str()) {
                b.ring = "assess".to_string();
            }
            if !["q1", "q2", "q3", "q4"].contains(&b.quadrant.as_str()) {
                b.quadrant = "q1".to_string();
            }
            b
        })
        .collect();

    blips.extend(new_blips);
    *blips = deduplicate(std::mem::take(blips));

    false
}
