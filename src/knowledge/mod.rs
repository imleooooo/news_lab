pub mod terminal;

use crate::fetcher::NewsItem;
use crate::llm::LLMClient;
use anyhow::Result;
use chrono::Utc;
use log::{debug, warn};
use regex::Regex;
use serde::{Deserialize, Serialize};

// ── Structs ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KGNode {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KGCluster {
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<KGNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KGRelation {
    pub from: String,
    pub to: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeGraph {
    pub center: String,
    #[serde(default)]
    pub clusters: Vec<KGCluster>,
    #[serde(default)]
    pub relations: Vec<KGRelation>,
}

// ── Prompt ─────────────────────────────────────────────────────────────────────

const KG_PROMPT: &str = r#"你是一位技術知識整理專家。今天日期：{today}。
根據以下關於「{keyword}」的最新新聞，建立一個結構化知識圖譜。

新聞（{n_news} 篇）：
{news_list}

只回傳 JSON，不要加任何其他文字：
{
  "center": "關鍵字",
  "clusters": [
    {
      "name": "核心概念",
      "nodes": [
        {"name": "概念名稱", "description": "一句話說明"}
      ]
    },
    {
      "name": "相關專案 & 模型",
      "nodes": []
    },
    {
      "name": "框架 & 工具",
      "nodes": []
    },
    {
      "name": "應用領域",
      "nodes": []
    }
  ],
  "relations": [
    {"from": "節點A", "to": "節點B", "label": "關係動詞"}
  ]
}

規則：
- clusters：4–6 個分類，每個分類 3–8 個節點，涵蓋整個知識生態
- relations：5–10 個，描述重要的依賴、使用、繼承、實作等關係
- name：英文或中英混合，≤20 字元
- description：繁體中文，≤30 字元
- label：繁體中文動詞短語，≤8 字元（例：「基於」「實作」「應用於」「整合」）"#;

// ── JSON extraction ────────────────────────────────────────────────────────────

/// Escape literal newlines / carriage returns inside JSON string values.
/// LLMs occasionally emit real line-breaks in strings instead of `\n` / `\r`,
/// which causes serde_json to fail with "expected `,` or `]`".
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

fn extract_json(response: &str) -> &str {
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
    if s.trim_start().starts_with('{') {
        return s;
    }
    if let Some(m) = Regex::new(r"(?s)\{.+\}")
        .ok()
        .and_then(|re| re.find(response))
    {
        return m.as_str();
    }
    response
}

// ── Main extraction ────────────────────────────────────────────────────────────

pub async fn extract_knowledge_graph(
    items: &[NewsItem],
    kw: &str,
    llm: &LLMClient,
) -> Result<KnowledgeGraph> {
    let news_list: String = items
        .iter()
        .take(10)
        .map(|item| format!("- [{}] {}", item.source, item.title))
        .collect::<Vec<_>>()
        .join("\n");

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let prompt = KG_PROMPT
        .replace("{today}", &today)
        .replace("{keyword}", kw)
        .replace("{n_news}", &items.len().min(10).to_string())
        .replace("{news_list}", &news_list);

    let response = llm.invoke_with_limit(&prompt, 8192).await?;
    let json_str = sanitize_json_strings(extract_json(&response));

    match serde_json::from_str::<KnowledgeGraph>(&json_str) {
        Ok(mut kg) => {
            if kg.center.is_empty() {
                kg.center = kw.to_string();
            }
            Ok(kg)
        }
        Err(e) => {
            warn!("[kg] JSON 解析失敗: {e}");
            debug!("[kg] LLM 原始回應:\n{response}");
            Ok(KnowledgeGraph {
                center: kw.to_string(),
                clusters: vec![],
                relations: vec![],
            })
        }
    }
}
