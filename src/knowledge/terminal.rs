use super::{KGCluster, KGRelation, KnowledgeGraph};
use console::style;
use unicode_width::UnicodeWidthStr;

const WIDTH: usize = 76;

// ── Helpers ────────────────────────────────────────────────────────────────────

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn center_line(s: &str, inner: usize) -> String {
    let w = display_width(s);
    let pad_l = inner.saturating_sub(w) / 2;
    let pad_r = inner.saturating_sub(w + pad_l);
    format!("│{}{}{}│", " ".repeat(pad_l), s, " ".repeat(pad_r))
}

// ── Cluster block ──────────────────────────────────────────────────────────────

fn render_cluster(cluster: &KGCluster) {
    if cluster.nodes.is_empty() {
        return;
    }

    let is_github = cluster.name == "GitHub Repos";

    if is_github {
        println!(
            "  {} {}",
            style("▲").green().bold(),
            style(&cluster.name).bold().white()
        );
    } else {
        println!(
            "  {} {}",
            style("◆").cyan().bold(),
            style(&cluster.name).bold().white()
        );
    }

    let last = cluster.nodes.len() - 1;
    for (i, node) in cluster.nodes.iter().enumerate() {
        let branch = if i == last { "└─" } else { "├─" };
        let name_display_w = display_width(&node.name);
        let name_styled = if is_github {
            style(&node.name).green().to_string()
        } else {
            style(&node.name).yellow().to_string()
        };

        if node.description.is_empty() {
            println!("    {} {}", branch, name_styled);
        } else {
            let col_w = 22usize;
            let pad = col_w.saturating_sub(name_display_w);
            println!(
                "    {} {}{}  {}",
                branch,
                name_styled,
                " ".repeat(pad),
                style(&node.description).dim()
            );
        }
    }
    println!();
}

// ── Relations block ────────────────────────────────────────────────────────────

fn render_relations(relations: &[KGRelation]) {
    if relations.is_empty() {
        return;
    }

    println!(
        "  {} {}",
        style("─").dim(),
        style("關鍵關係").bold().white()
    );
    println!("  {}", style("─".repeat(WIDTH - 4)).dim());

    // Find longest "from" for column alignment
    let max_from = relations
        .iter()
        .map(|r| display_width(&r.from))
        .max()
        .unwrap_or(0);

    for rel in relations {
        let pad = max_from.saturating_sub(display_width(&rel.from));
        println!(
            "    {}{}  {}{}{}  {}",
            style(&rel.from).yellow(),
            " ".repeat(pad),
            style("──[").dim(),
            style(&rel.label).cyan(),
            style("]──→").dim(),
            style(&rel.to).yellow(),
        );
    }
    println!();
}

// ── Main render ────────────────────────────────────────────────────────────────

pub fn render_knowledge_graph(kg: &KnowledgeGraph) {
    let inner = WIDTH - 2;

    // ── Title box ──
    let title = format!("知識圖譜：{}", kg.center);
    println!("╭{}╮", "─".repeat(inner));
    println!(
        "{}",
        center_line(&style(&title).bold().white().to_string(), inner)
    );
    println!("╰{}╯", "─".repeat(inner));
    println!();

    // ── Center node ──
    let center_text = format!("  {}  ", kg.center);
    let cw = display_width(&center_text);
    let indent = (WIDTH.saturating_sub(cw + 2)) / 2;
    println!("{}┌{}┐", " ".repeat(indent), "─".repeat(cw));
    println!(
        "{}│{}│",
        " ".repeat(indent),
        style(&center_text).bold().cyan()
    );
    println!("{}└{}┘", " ".repeat(indent), "─".repeat(cw));

    // stem line down from center box
    let stem_col = indent + cw / 2 + 1;
    println!("{}│", " ".repeat(stem_col));
    println!();

    // ── Clusters ──
    for cluster in &kg.clusters {
        render_cluster(cluster);
    }

    // ── Relations ──
    render_relations(&kg.relations);
}
