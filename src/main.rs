mod cache;
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
        fetch_hackernews_multi, fetch_medium_rss, fetch_tech_news,
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

// ── Cost display helpers ────────────────────────────────────────────────────────

fn fmt_tokens(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::new();
    let len = bytes.len();
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

fn print_usage(llm: &LLMClient) {
    let (p, c, cost) = llm.usage();
    if p + c == 0 {
        return;
    }
    let is_estimate = llm.model.contains("gpt-5") || llm.model.contains("o1");
    let cost_str = if is_estimate {
        format!("≈ ${:.4} (估算)", cost)
    } else {
        format!("≈ ${:.4}", cost)
    };
    println!(
        "  {} {}  {} + {} tokens  {}",
        style("用量").dim(),
        style(&llm.model).dim(),
        style(fmt_tokens(p)).dim(),
        style(fmt_tokens(c)).dim(),
        style(cost_str).yellow(),
    );
}

// ── Cache helpers ──────────────────────────────────────────────────────────────

/// Display cached items and print a TTL hint. Returns true so callers can early-return.
fn show_cached(items: &[cache::DisplayItem], ttl_secs: u64) -> bool {
    let mins = ttl_secs / 60;
    println!(
        "  {} 快取命中（還有 {} 分鐘到期）",
        style("✓").green(),
        mins,
    );
    separator();
    for item in items {
        panel(&item.title, &item.content, &item.color);
        print_url(&item.url);
        separator();
    }
    true
}

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

    let cache_key = ["news", kw, &cfg.model, &max.to_string()];
    if let Some((cached, ttl)) = cache::get(&cache_key) {
        show_cached(&cached, ttl);
        return Ok(());
    }

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

    let (hn, rss, medium) = tokio::join!(
        fetch_hackernews_multi(&en_kw, max),
        fetch_all_rss(&en_kw, &zh_kw, max),
        fetch_medium_rss(&en_kw, max),
    );

    // HN: light relevance filter — at least ONE expanded keyword (en_kw) must appear
    // in title+description combined.  We use expanded terms so that results fetched
    // via "ray serve" / "ray operator" are not discarded when the raw query is "kuberay".
    // HN link posts (empty story_text) are intentionally kept — show title + URL.
    let en_kw_lower: Vec<String> = en_kw.iter().map(|k| k.to_lowercase()).collect();
    let hn_filtered = hn.into_iter().filter(|item| !item.url.contains("github.com")).filter(
        |item| {
            if en_kw_lower.is_empty() {
                return true;
            }
            let combined = format!(
                "{} {}",
                item.title.to_lowercase(),
                item.description.to_lowercase()
            );
            en_kw_lower.iter().any(|k| combined.contains(k.as_str()))
        },
    );

    let mut items: Vec<_> = hn_filtered
        .chain(rss.into_iter().filter(|item| !item.description.trim().is_empty()))
        .chain(medium.into_iter().filter(|item| !item.description.trim().is_empty()))
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

    let mut to_cache: Vec<cache::DisplayItem> = Vec::with_capacity(items.len());
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
            "Medium" => "green",
            _ => "cyan",
        };
        let title = format!("[{}] {}", i + 1, item.title);
        panel(&title, &content, color);
        print_url(&item.url);
        separator();
        to_cache.push(cache::DisplayItem {
            title,
            content,
            url: item.url.clone(),
            color: color.to_string(),
        });
    }
    cache::put(&cache_key, &to_cache);

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
    let mode_key = if is_emerging { "emerging" } else { "hot" };

    let cache_key = [
        "github",
        kw,
        mode_key,
        &cfg.model,
        &cfg.max_results.to_string(),
    ];
    if let Some((cached, ttl)) = cache::get(&cache_key) {
        show_cached(&cached, ttl);
        return Ok(());
    }

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

    let mut to_cache: Vec<cache::DisplayItem> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let spinner = Spinner::new(&format!("摘要第 {}/{} 個專案...", i + 1, items.len()));
        let summary = summarize_one(item, kw, llm).await;
        spinner.finish("");

        let date_str = item
            .published
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());

        let content = format!("來源: GitHub | {}: {}\n\n{}", label_date, date_str, summary);
        let title = format!("[{}] {}", i + 1, item.title);
        panel(&title, &content, "green");
        print_url(&item.url);
        separator();
        to_cache.push(cache::DisplayItem {
            title,
            content,
            url: item.url.clone(),
            color: "green".to_string(),
        });
    }
    cache::put(&cache_key, &to_cache);

    Ok(())
}

// ── Run: arXiv Paper Summary ───────────────────────────────────────────────────

async fn run_paper_summary(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    let cache_key = ["arxiv", kw, &cfg.model, &cfg.max_results.to_string()];
    if let Some((cached, ttl)) = cache::get(&cache_key) {
        show_cached(&cached, ttl);
        return Ok(());
    }

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

    let mut to_cache: Vec<cache::DisplayItem> = Vec::with_capacity(papers.len());
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
        let title = format!("[{}] {}", i + 1, paper.title);
        panel(&title, &content, "magenta");
        print_url(&paper.url);
        separator();
        to_cache.push(cache::DisplayItem {
            title,
            content,
            url: paper.url.clone(),
            color: "magenta".to_string(),
        });
    }
    cache::put(&cache_key, &to_cache);

    Ok(())
}

// ── Run: Podcast Summary ───────────────────────────────────────────────────────

async fn run_podcast_summary(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    let cache_key = ["podcast", kw, &cfg.model, &cfg.max_results.to_string()];
    if let Some((cached, ttl)) = cache::get(&cache_key) {
        show_cached(&cached, ttl);
        return Ok(());
    }

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

    let mut to_cache: Vec<cache::DisplayItem> = Vec::with_capacity(total);
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
        let title = format!("[{}] {}", i + 1, ep.title);
        panel(&title, &content, "blue");
        print_url(&ep.url);
        separator();
        to_cache.push(cache::DisplayItem {
            title,
            content,
            url: ep.url.clone(),
            color: "blue".to_string(),
        });
    }
    cache::put(&cache_key, &to_cache);

    Ok(())
}

// ── Run: Knowledge Graph ───────────────────────────────────────────────────────

/// Fetch GitHub repos for `node`, summarize, then let the user optionally
/// drill into one repo as a new KG keyword.
/// Returns `Some(repo_name)` to push onto nav_stack, `None` to go back.
async fn kg_github_search(node: &str, cfg: &Config, llm: &LLMClient) -> Option<String> {
    let max = cfg.max_results.max(5);
    let spinner = Spinner::new(&format!("搜尋 GitHub Repos：{}", node));
    let items = fetch_github(node, max).await;
    spinner.finish(&format!("找到 {} 個專案", items.len()));

    if items.is_empty() {
        panel("GitHub Repos", "找不到相關開源專案。", "yellow");
        return None;
    }

    let mut repo_titles: Vec<String> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let spinner = Spinner::new(&format!("摘要第 {}/{} 個專案...", i + 1, items.len()));
        let summary = summarize_one(item, node, llm).await;
        spinner.finish("");

        let date_str = item
            .published
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string());
        let content = format!("最後推送: {}\n\n{}", date_str, summary);
        let title = format!("[{}] {}", i + 1, item.title);
        panel(&title, &content, "green");
        print_url(&item.url);
        separator();
        repo_titles.push(item.title.clone());
    }

    // Let user pick a repo to drill into as KG, or go back
    let mut choices = vec!["← 返回".to_string()];
    choices.extend(repo_titles.iter().map(|t| format!("▲ {} — 深入知識圖譜", t)));

    let sel = Select::new("選擇要深入的專案:", choices)
        .prompt()
        .unwrap_or_else(|_| "← 返回".to_string());

    if sel.starts_with("←") {
        return None;
    }

    // "▲ owner/repo — 深入知識圖譜" → extract repo name
    sel.strip_prefix("▲ ")
        .and_then(|s| s.split(" — ").next())
        .map(|s| s.to_string())
}

async fn run_knowledge_graph(kw: &str, cfg: &Config, llm: &LLMClient) -> Result<()> {
    let fetch_n = cfg.max_results.max(10);

    // nav: (display_name, search_query, cached_kg)
    // display_name = shown in breadcrumb / KG title / menus
    // search_query = passed to fetch_tech_news; for drill-downs it includes
    //   the parent display_name as context so "Brokers & Bookies" searches
    //   as "Apache Pulsar Brokers & Bookies" instead of the bare term.
    // Cached KGs survive back-navigation (no re-fetch on ← 返回上一層).
    let mut nav: Vec<(String, String, Option<knowledge::KnowledgeGraph>)> =
        vec![(kw.to_string(), kw.to_string(), None)];

    // ── Outer loop: one iteration per KG level ────────────────────────────────
    'nav: loop {
        let (current_display, current_search) = {
            let e = nav.last().unwrap();
            (e.0.clone(), e.1.clone())
        };

        // Breadcrumb
        if nav.len() > 1 {
            let crumb: Vec<&str> = nav.iter().map(|(d, _, _)| d.as_str()).collect();
            println!(
                "\n  {} {}",
                style("路徑:").dim(),
                style(crumb.join(" › ")).cyan()
            );
            println!();
        }

        // Fetch + build KG only when not yet cached for this level
        if nav.last().unwrap().2.is_none() {
            let spinner = Spinner::new(&format!("正在抓取技術資料：{}", current_display));
            let items = fetch_tech_news(&current_search, fetch_n).await;
            spinner.finish(&format!("取得 {} 筆資料", items.len()));

            if items.is_empty() {
                panel("知識圖譜", "找不到足夠的技術資料。", "yellow");
                nav.pop();
                if nav.is_empty() {
                    return Ok(());
                }
                continue 'nav; // back to cached parent
            }

            let spinner = Spinner::new("LLM 建構知識圖譜...");
            let kg = knowledge::extract_knowledge_graph(&items, &current_display, llm).await?;
            spinner.finish(&format!(
                "識別出 {} 個分類、{} 個關係",
                kg.clusters.len(),
                kg.relations.len()
            ));

            if kg.clusters.is_empty() {
                panel("知識圖譜", "無法從資料中建構知識圖譜。", "yellow");
                nav.pop();
                if nav.is_empty() {
                    return Ok(());
                }
                continue 'nav;
            }

            nav.last_mut().unwrap().2 = Some(kg);
        }

        // Clone KG out of nav so 'menu loop can borrow nav mutably later
        let kg = nav.last().unwrap().2.as_ref().unwrap().clone();
        knowledge::terminal::render_knowledge_graph(&kg);

        // ── Inner loop: node selection (no re-fetch on GitHub return) ─────────
        'menu: loop {
            // Build choices + a parallel index with the *exact* choice string.
            // Exact-string lookup (P2 #1 fix): avoids prefix-match ambiguity
            // (e.g. "Ray" wrongly matching "Ray Serve").
            let mut node_index: Vec<(String, String, String)> = Vec::new(); // (line, cluster, node)
            let back_label = if nav.len() > 1 {
                "← 返回上一層"
            } else {
                "← 返回主選單"
            };
            let mut choices: Vec<String> = vec![back_label.to_string()];

            for cluster in &kg.clusters {
                let icon = if cluster.name == "GitHub Repos" { "▲" } else { "◆" };
                for node in &cluster.nodes {
                    let line = if node.description.is_empty() {
                        format!("{} [{}]  {}", icon, cluster.name, node.name)
                    } else {
                        format!(
                            "{} [{}]  {}  — {}",
                            icon, cluster.name, node.name, node.description
                        )
                    };
                    choices.push(line.clone());
                    node_index.push((line, cluster.name.clone(), node.name.clone()));
                }
            }

            let sel = Select::new(
                &format!("「{}」知識圖譜 — 選擇節點:", current_display),
                choices,
            )
            .prompt()
            .unwrap_or_else(|_| back_label.to_string());

            if sel == back_label {
                nav.pop();
                if !nav.is_empty() {
                    separator();
                    continue 'nav; // re-render parent from cache (no fetch)
                }
                break 'nav;
            }

            // Exact match: find by the stored choice line (P2 #1)
            let Some((_, cluster_name, node_name)) =
                node_index.iter().find(|(line, _, _)| line == &sel)
            else {
                continue 'menu;
            };
            let (cluster_name, node_name) = (cluster_name.clone(), node_name.clone());

            separator();

            let is_github = cluster_name == "GitHub Repos";
            let start_cursor: usize = if is_github { 1 } else { 0 };

            let action = Select::new(
                &format!("「{}」— 選擇動作:", node_name),
                vec![
                    "知識圖譜 — 深入搜尋".to_string(),
                    "GitHub Repos — 搜尋開源專案".to_string(),
                    "← 返回".to_string(),
                ],
            )
            .with_starting_cursor(start_cursor)
            .prompt()
            .unwrap_or_else(|_| "← 返回".to_string());

            if action.starts_with("←") {
                continue 'menu;
            }

            separator();

            if action.contains("知識圖譜") {
                // search_query = "{parent_display} {node}" so context is preserved
                let search_q = format!("{} {}", current_display, node_name);
                nav.push((node_name.clone(), search_q, None));
                continue 'nav;
            }

            if action.contains("GitHub Repos") {
                if let Some(repo) = kg_github_search(&node_name, cfg, llm).await {
                    let search_q = format!("{} {}", current_display, repo);
                    nav.push((repo.clone(), search_q, None));
                    continue 'nav;
                }
                continue 'menu;
            }
        }
    }

    Ok(())
}

// ── Competitive analysis: render competitor table ──────────────────────────────

fn render_competitor_table(rows: &[CompetitorRow], target_name: &str) {
    use comfy_table::{
        presets::UTF8_FULL, Attribute, Cell, CellAlignment, ContentArrangement, Table,
    };

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
        "green" => style(s).green().bold().to_string(),
        "red" => style(s).red().bold().to_string(),
        "cyan" => style(s).cyan().bold().to_string(),
        "yellow" => style(s).yellow().bold().to_string(),
        _ => style(s).bold().to_string(),
    }
}

fn sc_dim(s: &str, color: &str) -> String {
    match color {
        "green" => style(s).green().dim().to_string(),
        "red" => style(s).red().dim().to_string(),
        "cyan" => style(s).cyan().dim().to_string(),
        "yellow" => style(s).yellow().dim().to_string(),
        _ => style(s).dim().to_string(),
    }
}

fn render_analysis_sections(text: &str) {
    const CONFIGS: &[(u32, &str, &str)] = &[
        (2, "green", "核心競爭優勢"),
        (3, "red", "主要劣勢與風險"),
        (4, "cyan", "選型建議"),
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
                        let pad = if b.trim_start() == "›" {
                            "      "
                        } else {
                            "    "
                        };
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
    // Use REVIEW_MODEL env var if set, otherwise default to gpt-5.4-2026-03-05.
    // If the model is unavailable, review_and_augment() treats the failure as satisfied
    // and skips the round gracefully.
    let review_model =
        std::env::var("REVIEW_MODEL").unwrap_or_else(|_| "gpt-5.4-2026-03-05".to_string());
    let review_llm = LLMClient::new(&review_model)?;
    for round in 1..=2u8 {
        let spinner = Spinner::new(&format!(
            "進階模型審核雷達圖（第 {}/2 輪，{}）...",
            round, review_model
        ));
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

    print_usage(&review_llm);

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
    // Load .env first so RUST_LOG set inside it is visible to env_logger
    if dotenvy::dotenv().is_err() {
        let parent_env = std::path::Path::new("..").join(".env");
        dotenvy::from_path(parent_env).ok();
    }

    // Logging: default warn; override with RUST_LOG=debug / RUST_LOG=info
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format(|buf, record| {
            use std::io::Write;
            writeln!(buf, "  [{}] {}", record.level(), record.args())
        })
        .init();

    let api_key_ok = std::env::var("OPENAI_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if !api_key_ok {
        eprintln!(
            "\n  {} 請設定 OPENAI_API_KEY 環境變數，或在 {} 加入 OPENAI_API_KEY=sk-...",
            style("✗").red().bold(),
            style(".env").cyan(),
        );
        std::process::exit(1);
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
                "清空快取".to_string(),
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

            llm.reset_usage();
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
                f if f.contains("清空快取") => {
                    let n = cache::clear_all();
                    println!(
                        "  {} 已清除 {} 筆快取",
                        style("✓").green(),
                        style(n).cyan()
                    );
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

            print_usage(&llm);
            separator();
        }
    }

    Ok(())
}
