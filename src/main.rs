mod config;
mod fetcher;
mod knowledge;
mod llm;
mod radar;
mod summarizer;
mod ui;

use anyhow::Result;
use config::{configure, Config};
use console::style;
use fetcher::{
    arxiv::fetch_domain_papers,
    cncf::fetch_cncf_projects,
    huggingface::{fetch_hf_models, fmt_num, HFSort},
    podcast::fetch_podcast_content,
    tech::{
        expand_news_keywords, fetch_all_rss, fetch_github, fetch_github_emerging,
        fetch_hackernews_multi, fetch_tech_news,
    },
};
use inquire::{validator::Validation, Select, Text};
use llm::LLMClient;
use radar::{check_oss_activity, extract_blips, review_and_augment, terminal as radar_terminal};
use summarizer::{
    analyze_competition, summarize_arxiv, summarize_cncf_project, summarize_hf_model,
    summarize_one, summarize_podcast, CompetitorRow,
};
use ui::{panel, print_url, separator, Spinner};

// ── Banner ─────────────────────────────────────────────────────────────────────

fn print_banner() {
    let banner = r#"
  ██╗   ██╗███████╗██╗    ██╗███████╗      ██╗      █████╗ ██████╗
  ████╗  ██║██╔════╝██║    ██║██╔════╝      ██║     ██╔══██╗██╔══██╗
  ██╔██╗ ██║█████╗  ██║ █╗ ██║███████╗      ██║     ███████║██████╔╝
  ██║╚██╗██║██╔══╝  ██║███╗██║╚════██║      ██║     ██╔══██║██╔══██╗
  ██║ ╚████║███████╗╚███╔███╔╝███████║      ███████╗██║  ██║██████╔╝
  ╚═╝  ╚═══╝╚══════╝ ╚══╝╚══╝ ╚══════╝      ╚══════╝╚═╝  ╚═╝╚═════╝"#;
    println!("{}", style(banner).cyan().bold());
    println!();
    println!("  {}", style("科技新聞摘要 + 技術雷達 CLI").bold().white());
    println!();
}

// ── Run: News Summary (Hacker News + InfoQ + iThome) ──────────────────────────

async fn run_news_summary(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    let max = cfg.max_results;

    // Step 1: expand keyword into English + Chinese term lists via LLM
    let expand_spinner = Spinner::new("正在展開搜尋關鍵字...");
    let (en_kw, zh_kw) = expand_news_keywords(kw, llm).await;
    expand_spinner.finish(&format!(
        "EN：{}  ／  ZH：{}",
        en_kw.join("、"),
        zh_kw.join("、")
    ));

    // Step 2: fetch from all sources in parallel using language-appropriate keywords
    let spinner = Spinner::new(&format!("正在抓取新聞：{}", kw));

    let (hn, rss) = tokio::join!(
        fetch_hackernews_multi(&en_kw, max),
        fetch_all_rss(&en_kw, &zh_kw, max),
    );

    let mut items: Vec<_> = hn
        .into_iter()
        .filter(|item| !item.url.contains("github.com"))
        .chain(rss)
        .filter(|item| !item.description.trim().is_empty())
        .collect();

    // Deduplicate by URL across all sources
    let mut seen_urls = std::collections::HashSet::new();
    items.retain(|item| seen_urls.insert(item.url.clone()));

    items.sort_by(|a, b| {
        b.published
            .unwrap_or_default()
            .cmp(&a.published.unwrap_or_default())
    });
    items.truncate(max);

    spinner.finish(&format!("抓取完成，共 {} 筆", items.len()));

    if items.is_empty() {
        panel("新聞摘要", "找不到相關新聞，請嘗試其他關鍵字。", "yellow");
        return Ok(());
    }

    for (i, item) in items.iter().enumerate() {
        let spinner = Spinner::new(&format!("摘要第 {}/{} 篇...", i + 1, items.len()));
        let summary = summarize_one(item, kw, llm).await;
        spinner.finish("");

        let pub_str = item
            .published
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());

        let content = format!("來源: {} | 日期: {}\n\n{}", item.source, pub_str, summary);
        let color = match item.source.as_str() {
            "InfoQ" => "blue",
            "iThome" => "magenta",
            _ => "cyan", // Hacker News
        };
        panel(&format!("[{}] {}", i + 1, item.title), &content, color);
        print_url(&item.url);
        separator();
    }

    Ok(())
}

// ── Run: GitHub Open Source Summary ───────────────────────────────────────────

async fn run_github_summary(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    let mode = Select::new(
        "選擇模式:",
        vec![
            "近期熱門  — 近期有活動，按星數排序",
            "新興專案  — 近期創建，已累積初始熱度",
        ],
    )
    .prompt()
    .unwrap_or("近期熱門  — 近期有活動，按星數排序");

    let is_emerging = mode.contains("新興");

    let (label_fetch, label_date) = if is_emerging {
        ("正在搜尋新興 GitHub 專案", "建立日期")
    } else {
        ("正在搜尋近期熱門 GitHub 專案", "最後推送")
    };

    let spinner = Spinner::new(&format!("{}：{}", label_fetch, kw));
    let items = if is_emerging {
        fetch_github_emerging(kw, cfg.max_results).await
    } else {
        fetch_github(kw, cfg.max_results).await
    };
    spinner.finish(&format!("找到 {} 個專案", items.len()));

    if items.is_empty() {
        panel(
            "開源專案摘要",
            "找不到相關專案，請嘗試其他關鍵字。",
            "yellow",
        );
        return Ok(());
    }

    for (i, item) in items.iter().enumerate() {
        let spinner = Spinner::new(&format!("摘要第 {}/{} 個專案...", i + 1, items.len()));
        let summary = summarize_one(item, kw, llm).await;
        spinner.finish("");

        let date_str = item
            .published
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());

        let content = format!("來源: GitHub | {}: {}\n\n{}", label_date, date_str, summary);
        panel(&format!("[{}] {}", i + 1, item.title), &content, "green");
        print_url(&item.url);
        separator();
    }

    Ok(())
}

// ── Run: arXiv Paper Summary ───────────────────────────────────────────────────

async fn run_paper_summary(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    let spinner = Spinner::new(&format!("LLM 擴展搜尋關鍵字：{}", kw));
    let (papers, terms) = fetch_domain_papers(kw, cfg.max_results, 30, llm).await;
    spinner.finish(&format!(
        "搜尋術語：{}　找到 {} 篇論文",
        terms.join(" / "),
        papers.len()
    ));

    if papers.is_empty() {
        panel("arXiv 論文", "找不到相關論文，請嘗試其他關鍵字。", "yellow");
        return Ok(());
    }

    for (i, paper) in papers.iter().enumerate() {
        let spinner = Spinner::new(&format!("摘要第 {}/{} 篇論文...", i + 1, papers.len()));
        let summary = summarize_arxiv(paper, kw, llm).await;
        spinner.finish("");

        let pub_str = paper
            .published
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());
        let authors = paper.authors.join(", ");

        let content = format!(
            "arXiv ID: {} | 日期: {}\n作者: {}\n\n{}",
            paper.arxiv_id, pub_str, authors, summary
        );
        panel(&format!("[{}] {}", i + 1, paper.title), &content, "magenta");
        print_url(&paper.url);
        separator();
    }

    Ok(())
}

// ── Run: Podcast Summary ───────────────────────────────────────────────────────

async fn run_podcast_summary(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    let max_pods = (cfg.max_results / 2).max(3);
    let max_eps = 3;

    let spinner = Spinner::new(&format!("正在搜尋 Podcast：{}", kw));
    let episodes = fetch_podcast_content(kw, max_pods, max_eps, 90).await;
    spinner.finish(&format!("找到 {} 集", episodes.len()));

    if episodes.is_empty() {
        panel(
            "Podcast 摘要",
            "找不到相關播客，請嘗試其他關鍵字。",
            "yellow",
        );
        return Ok(());
    }

    let ep_to_show = episodes.iter().take(cfg.max_results);
    let total = episodes.len().min(cfg.max_results);

    for (i, ep) in ep_to_show.enumerate() {
        let spinner = Spinner::new(&format!("摘要第 {}/{} 集...", i + 1, total));
        let summary = summarize_podcast(ep, kw, llm).await;
        spinner.finish("");

        let pub_str = ep
            .published
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());

        let content = format!(
            "播客: {} | 日期: {} | 時長: {}\n\n{}",
            ep.podcast_name, pub_str, ep.duration, summary
        );
        panel(&format!("[{}] {}", i + 1, ep.title), &content, "blue");
        print_url(&ep.url);
        separator();
    }

    Ok(())
}

// ── Run: Knowledge Graph ───────────────────────────────────────────────────────

async fn run_knowledge_graph(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    let fetch_n = cfg.max_results.max(10);
    let spinner = Spinner::new(&format!("正在抓取技術資料：{}", kw));
    let items = fetch_tech_news(kw, fetch_n).await;
    spinner.finish(&format!("取得 {} 筆資料", items.len()));

    if items.is_empty() {
        panel("知識圖譜", "找不到足夠的技術資料。", "yellow");
        return Ok(());
    }

    let spinner = Spinner::new("LLM 建構知識圖譜...");
    let kg = knowledge::extract_knowledge_graph(&items, kw, llm).await?;
    spinner.finish(&format!(
        "識別出 {} 個分類、{} 個關係",
        kg.clusters.len(),
        kg.relations.len()
    ));

    if kg.clusters.is_empty() {
        panel("知識圖譜", "無法從資料中建構知識圖譜。", "yellow");
        return Ok(());
    }

    knowledge::terminal::render_knowledge_graph(&kg);
    Ok(())
}

// ── Competitive analysis: render competitor table ──────────────────────────────

fn render_competitor_table(rows: &[CompetitorRow], target_name: &str) {
    use comfy_table::{presets::UTF8_FULL, Attribute, Cell, CellAlignment, ContentArrangement, Table};

    if rows.is_empty() {
        return;
    }

    println!();
    println!("  {} 競品對比表", style("1.").bold().cyan());
    println!();

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_width(92);
    table.set_header(vec![
        Cell::new("產品").add_attribute(Attribute::Bold),
        Cell::new("類型").add_attribute(Attribute::Bold),
        Cell::new("核心定位").add_attribute(Attribute::Bold),
        Cell::new("主要優勢").add_attribute(Attribute::Bold),
        Cell::new("主要劣勢").add_attribute(Attribute::Bold),
    ]);

    for row in rows {
        let name_cell = if row.name == target_name {
            Cell::new(format!("▶ {}", row.name))
                .add_attribute(Attribute::Bold)
                .set_alignment(CellAlignment::Left)
        } else {
            Cell::new(&row.name)
        };
        table.add_row(vec![
            name_cell,
            Cell::new(&row.type_),
            Cell::new(&row.positioning),
            Cell::new(&row.pros),
            Cell::new(&row.cons),
        ]);
    }

    println!("{}", table);
    println!();
}

// ── Competitive analysis: render sections 2–5 with per-section colours ─────────

fn sc_bold(s: &str, color: &str) -> String {
    match color {
        "green"  => style(s).green().bold().to_string(),
        "red"    => style(s).red().bold().to_string(),
        "cyan"   => style(s).cyan().bold().to_string(),
        "yellow" => style(s).yellow().bold().to_string(),
        _        => style(s).bold().to_string(),
    }
}

fn sc_dim(s: &str, color: &str) -> String {
    match color {
        "green"  => style(s).green().dim().to_string(),
        "red"    => style(s).red().dim().to_string(),
        "cyan"   => style(s).cyan().dim().to_string(),
        "yellow" => style(s).yellow().dim().to_string(),
        _        => style(s).dim().to_string(),
    }
}

fn render_analysis_sections(text: &str) {
    const CONFIGS: &[(u32, &str, &str)] = &[
        (2, "green",  "核心競爭優勢"),
        (3, "red",    "主要劣勢與風險"),
        (4, "cyan",   "選型建議"),
        (5, "yellow", "總結"),
    ];

    // ── Parse sections by "N." at line start ───────────────────────────
    let mut sections: Vec<(u32, Vec<&str>)> = Vec::new();
    let mut cur_num: u32 = 0;
    let mut cur_lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        let is_hdr = matches!(
            (t.chars().next(), t.chars().nth(1)),
            (Some(c), Some('.')) if c.is_ascii_digit()
        );
        if is_hdr {
            if cur_num > 0 {
                sections.push((cur_num, cur_lines.clone()));
            }
            cur_num = t.chars().next().and_then(|c| c.to_digit(10)).unwrap_or(0);
            cur_lines = Vec::new();
        } else {
            cur_lines.push(line);
        }
    }
    if cur_num > 0 {
        sections.push((cur_num, cur_lines));
    }

    // ── Render each section ────────────────────────────────────────────
    for (num, lines) in &sections {
        let (color, title) = CONFIGS
            .iter()
            .find(|(n, _, _)| n == num)
            .map(|(_, c, t)| (*c, *t))
            .unwrap_or(("white", ""));

        // Section header: bold coloured title + dim divider
        println!();
        println!("  {}", sc_bold(title, color));
        println!("  {}", sc_dim(&"─".repeat(72), color));
        println!();

        let mut pending_sub: Option<bool> = None; // orphaned lone "-"
        let mut first_bullet = true;

        for raw in lines {
            let clean = raw.replace("**", "");
            let trimmed = clean.trim();
            let indent = clean.len() - clean.trim_start().len();
            let is_sub = indent >= 2;

            // Lone "-" on its own line → remember for next content line
            if trimmed == "-" {
                pending_sub = Some(is_sub);
                continue;
            }
            if trimmed.is_empty() {
                continue;
            }

            // Determine bullet type and content
            let (bullet, content): (Option<&str>, String) =
                if let Some(rest) = trimmed.strip_prefix("- ") {
                    (Some(if is_sub { "  ›" } else { "•" }), rest.to_string())
                } else if let Some(sub) = pending_sub.take() {
                    // Join orphaned "-" with this line
                    (Some(if sub { "  ›" } else { "•" }), trimmed.to_string())
                } else {
                    (None, trimmed.to_string())
                };

            match bullet {
                Some(b) => {
                    // Blank line between consecutive bullets for breathing room
                    if !first_bullet {
                        println!();
                    }
                    first_bullet = false;

                    // Split on Chinese full-width colon：
                    // → key phrase in bold colour, description in normal
                    if let Some((key, desc)) = content.split_once('：') {
                        println!("  {}", sc_bold(&format!("{} {}：", b, key.trim()), color));
                        let pad = if b.trim_start() == "›" { "      " } else { "    " };
                        println!("  {}{}", pad, desc.trim());
                    } else {
                        // No colon — bullet marker in colour, rest normal
                        println!("  {} {}", sc_bold(b, color), content);
                    }
                }
                None => {
                    // Plain text (notes, sub-headings, etc.) — slightly dimmed
                    println!("    {}", style(trimmed).dim());
                }
            }
        }
    }
    println!();
}

// ── Run: Terminal Radar ────────────────────────────────────────────────────────

async fn run_terminal_radar(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    // Fetch news (at least 12 items for better radar coverage)
    let fetch_n = cfg.max_results.max(12);
    let spinner = Spinner::new(&format!("正在抓取技術資料：{}", kw));
    let items = fetch_tech_news(kw, fetch_n).await;
    spinner.finish(&format!("取得 {} 筆資料", items.len()));

    if items.is_empty() {
        panel("技術雷達", "找不到足夠的技術資料。", "yellow");
        return Ok(());
    }

    let spinner = Spinner::new("LLM 分析技術生態...");
    let (q_names, mut blips) = extract_blips(&items, kw, llm).await?;
    spinner.finish(&format!("識別出 {} 個技術項目", blips.len()));

    if blips.is_empty() {
        panel("技術雷達", "無法從資料中提取技術項目。", "yellow");
        return Ok(());
    }

    // Review loop: advanced model audits and augments, up to 2 rounds
    let review_llm = LLMClient::new("gpt-5.4-2026-03-05")?;
    for round in 1..=2u8 {
        let spinner = Spinner::new(&format!("進階模型審核雷達圖（第 {}/2 輪）...", round));
        let satisfied = review_and_augment(&mut blips, &q_names, kw, &review_llm).await;
        if satisfied {
            spinner.finish(&format!(
                "第 {} 輪審核通過，共 {} 個項目",
                round,
                blips.len()
            ));
            break;
        }
        spinner.finish(&format!(
            "第 {} 輪補充完成，現有 {} 個項目",
            round,
            blips.len()
        ));
    }

    // GitHub activity check for open-source blips
    let spinner = Spinner::new("檢查開源專案 GitHub 活躍度...");
    check_oss_activity(&mut blips).await;
    spinner.finish("");

    // Build grid and assign blip numbers
    let rg = radar_terminal::build_radar_grid(&mut blips, &q_names);

    // Render
    radar_terminal::render_radar(&rg, &q_names, kw);
    radar_terminal::render_legend(&blips, &q_names);

    // Interactive blip browser
    loop {
        let mut choices: Vec<String> = vec!["← 返回主選單".to_string()];
        let mut sorted_blips: Vec<&radar::Blip> = blips.iter().collect();
        sorted_blips.sort_by_key(|b| b.number);
        for b in &sorted_blips {
            let icon = if b.is_open_source { "▲" } else { "●" };
            choices.push(format!("#{} {} {} ({})", b.number, icon, b.name, b.ring));
        }

        let sel = Select::new("查看技術項目詳情:", choices)
            .prompt()
            .unwrap_or_else(|_| "← 返回主選單".to_string());

        if sel.starts_with("←") {
            break;
        }

        // parse number from "#N "
        if let Some(num_str) = sel.strip_prefix('#') {
            if let Some(n) = num_str
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<usize>().ok())
            {
                if let Some(b) = blips.iter().find(|b| b.number == n) {
                    radar_terminal::show_blip_detail(b, &q_names);
                    separator();

                    // Sub-menu: competitive analysis or back
                    let action = Select::new(
                        "選擇動作:",
                        vec![
                            format!("⚔  對「{}」進行競品分析", b.name),
                            "← 後退".to_string(),
                        ],
                    )
                    .prompt()
                    .unwrap_or_else(|_| "← 後退".to_string());

                    if action.contains("競品分析") {
                        let spinner = Spinner::new(&format!("LLM 分析「{}」的競品...", b.name));
                        let analysis = analyze_competition(b, &blips, &q_names, kw, llm).await;
                        spinner.finish("");
                        // Section 1: comfy-table competitor comparison
                        render_competitor_table(&analysis.competitors, &b.name);
                        // Sections 2–5: per-section coloured panels
                        render_analysis_sections(&analysis.text);
                        separator();
                    }
                }
            }
        }
    }

    Ok(())
}

// ── Run: HuggingFace Model Summary ────────────────────────────────────────────

async fn run_hf_summary(llm: &LLMClient) -> Result<()> {
    let sort_label = Select::new("排序方式:", vec!["熱門趨勢", "最多下載", "最多收藏"])
        .prompt()
        .unwrap_or("熱門趨勢");

    let sort = if sort_label.contains("下載") {
        HFSort::Downloads
    } else if sort_label.contains("收藏") {
        HFSort::Likes
    } else {
        HFSort::Trending
    };

    let spinner = Spinner::new("正在從 HuggingFace 抓取熱門模型...");
    let models = fetch_hf_models(sort, 20).await;
    spinner.finish(&format!("取得 {} 個模型", models.len()));

    if models.is_empty() {
        panel(
            "HuggingFace 模型整理",
            "無法取得模型資料，請稍後再試。",
            "yellow",
        );
        return Ok(());
    }

    for (i, model) in models.iter().enumerate() {
        let spinner = Spinner::new(&format!("摘要第 {}/{} 個模型...", i + 1, models.len()));
        let summary = summarize_hf_model(model, llm).await;
        spinner.finish("");

        let date_str = model
            .last_modified
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());

        let content = format!(
            "任務: {} | 更新: {}\n⬇ {} 下載  ❤ {} 收藏\n\n{}",
            model.pipeline_tag,
            date_str,
            fmt_num(model.downloads),
            fmt_num(model.likes),
            summary
        );
        panel(&format!("[{}] {}", i + 1, model.model_id), &content, "cyan");
        print_url(&model.url);
        separator();
    }

    Ok(())
}

// ── Run: CNCF Project Summary ──────────────────────────────────────────────────

async fn run_cncf_summary(cfg: &Config, llm: &LLMClient) -> Result<()> {
    let max = cfg.max_results.max(10);
    let spinner = Spinner::new("正在從 CNCF TOC 抓取最近專案...");
    let projects = fetch_cncf_projects(max).await;
    spinner.finish(&format!("取得 {} 個 CNCF 專案", projects.len()));

    if projects.is_empty() {
        panel(
            "CNCF 專案整理",
            "無法取得 CNCF 專案資料，建議設定 GITHUB_TOKEN 環境變數以避免 API 限速。",
            "yellow",
        );
        return Ok(());
    }

    for (i, project) in projects.iter().enumerate() {
        let spinner = Spinner::new(&format!("摘要第 {}/{} 個專案...", i + 1, projects.len()));
        let summary = summarize_cncf_project(project, llm).await;
        spinner.finish("");

        let maturity_icon = match project.maturity.as_str() {
            "graduated" => "🎓",
            "incubating" => "🌱",
            _ => "🔬",
        };
        let accepted_str = project
            .accepted_at
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());
        let updated_str = project
            .last_updated
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());

        let content = format!(
            "{} {} | ⭐ {} stars | 語言: {}\n加入: {} | 最後更新: {}\n\n{}",
            maturity_icon,
            project.maturity,
            project.stars,
            project.language.as_deref().unwrap_or("未知"),
            accepted_str,
            updated_str,
            summary
        );

        let color = match project.maturity.as_str() {
            "graduated" => "cyan",
            "incubating" => "green",
            _ => "yellow",
        };
        panel(&format!("[{}] {}", i + 1, project.name), &content, color);
        print_url(&project.url);
        separator();
    }

    Ok(())
}

// ── Run: Extras sub-menu ───────────────────────────────────────────────────────

async fn run_extras(cfg: &Config, llm: &LLMClient) -> Result<()> {
    loop {
        let sel = Select::new(
            "其他功能:",
            vec![
                "HuggingFace 模型整理  — 前 20 名熱門模型摘要",
                "CNCF 專案整理        — 最近值得關注的 CNCF 專案",
                "← 返回主選單",
            ],
        )
        .prompt()
        .unwrap_or("← 返回主選單");

        if sel.starts_with("←") {
            break;
        }

        separator();

        let result = if sel.contains("HuggingFace") {
            run_hf_summary(llm).await
        } else {
            run_cncf_summary(cfg, llm).await
        };

        if let Err(e) = result {
            eprintln!("  {} 錯誤: {}", style("✗").red(), e);
        }

        separator();
    }
    Ok(())
}

// ── Main ───────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Try current dir, then parent dir (news-app/.env)
    if dotenvy::dotenv().is_err() {
        let parent_env = std::path::Path::new("..").join(".env");
        dotenvy::from_path(parent_env).ok();
    }

    print_banner();

    let mut cfg = configure()?;
    let llm = LLMClient::new(&cfg.model)?;

    println!(
        "\n  {} 模型: {} | 最大結果: {}",
        style("✓").green(),
        style(&cfg.model).cyan(),
        style(cfg.max_results).cyan()
    );
    separator();

    let mut keyword = String::new();

    // ── outer loop: keyword selection ──────────────────────────────────────
    'kw: loop {
        let kw_prompt = if keyword.is_empty() {
            "輸入搜尋關鍵字 (英文):".to_string()
        } else {
            format!("輸入搜尋關鍵字 (英文，Enter 保留 \"{}\"):", keyword)
        };

        let new_kw = Text::new(&kw_prompt)
            .prompt()
            .unwrap_or_else(|_| "quit".to_string());

        if new_kw.trim().to_lowercase() == "quit" || new_kw.trim().to_lowercase() == "q" {
            println!("\n  再見！");
            break 'kw;
        }

        if !new_kw.trim().is_empty() {
            keyword = new_kw.trim().to_string();
        }

        if keyword.is_empty() {
            println!("  請輸入關鍵字。");
            continue 'kw;
        }

        // ── inner loop: feature selection (stays on same keyword) ──────────
        'feat: loop {
            let feature_choices = vec![
                "新聞摘要".to_string(),
                "開源專案摘要".to_string(),
                "arXiv 論文摘要".to_string(),
                "Podcast 摘要".to_string(),
                "知識圖譜".to_string(),
                "技術生態雷達和競品分析 (請使用如 AI on K8s 去提問)".to_string(),
                "其他功能 ▶".to_string(),
                format!("調整筆數 (目前: {})", cfg.max_results),
                "更換關鍵字".to_string(),
                "離開".to_string(),
            ];

            let feature = Select::new(
                &format!("關鍵字: \"{}\" — 選擇功能:", keyword),
                feature_choices,
            )
            .prompt()
            .unwrap_or_else(|_| "離開".to_string());

            separator();

            let result = match feature.as_str() {
                f if f.contains("新聞摘要") => run_news_summary(&keyword, &cfg, &llm).await,
                f if f.contains("開源專案") => run_github_summary(&keyword, &cfg, &llm).await,
                f if f.contains("arXiv") => run_paper_summary(&keyword, &cfg, &llm).await,
                f if f.contains("Podcast") => run_podcast_summary(&keyword, &cfg, &llm).await,
                f if f.contains("知識圖譜") => run_knowledge_graph(&keyword, &cfg, &llm).await,
                f if f.contains("雷達") => run_terminal_radar(&keyword, &cfg, &llm).await,
                f if f.contains("其他功能") => run_extras(&cfg, &llm).await,
                f if f.contains("調整筆數") => {
                    let cur = cfg.max_results.to_string();
                    let input = Text::new("每次最多抓取幾筆資料:")
                        .with_default(&cur)
                        .with_validator(|s: &str| match s.trim().parse::<usize>() {
                            Ok(n) if n >= 1 => Ok(Validation::Valid),
                            Ok(_) => Ok(Validation::Invalid("請輸入至少 1 以上的整數".into())),
                            Err(_) => Ok(Validation::Invalid("請輸入正整數（例如：10）".into())),
                        })
                        .prompt()
                        .unwrap_or(cur);
                    if let Ok(n) = input.trim().parse::<usize>() {
                        cfg.max_results = n;
                        println!(
                            "  {} 已更新為每次抓取 {} 筆",
                            style("✓").green(),
                            style(n).cyan()
                        );
                    }
                    separator();
                    continue 'feat;
                }
                f if f.contains("更換關鍵字") => {
                    break 'feat; // back to keyword prompt
                }
                _ => {
                    println!("\n  再見！");
                    break 'kw;
                }
            };

            if let Err(e) = result {
                eprintln!("  {} 錯誤: {}", style("✗").red(), e);
            }

            separator();
        }
    }

    Ok(())
}
