use super::{cases::BlipCaseBundle, Blip};
use crate::ui::panel;
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use console::style;
use std::collections::HashMap;

// ── Grid constants (matches Python) ───────────────────────────────────────────
const ROWS: usize = 23;
const COLS: usize = 71;
const CR: usize = ROWS / 2; // center row  = 11
const CC: usize = COLS / 2; // center col  = 35
const MR: f64 = (CR - 1) as f64; // max radius rows = 10
const MC: f64 = MR * 2.0; // max radius cols = 20 (2× aspect ratio)

// Ring fraction from center (matches Python R_FRAC)
fn ring_frac(ring: &str) -> f64 {
    match ring {
        "adopt" => 0.20,
        "trial" => 0.45,
        "assess" => 0.70,
        "hold" => 0.92,
        _ => 0.70,
    }
}

// Quadrant center angle, clockwise from top (matches Python Q_CENTER)
fn q_center_deg(q: &str) -> f64 {
    match q {
        "q1" => 45.0,
        "q2" => 315.0,
        "q3" => 225.0,
        "q4" => 135.0,
        _ => 45.0,
    }
}

#[allow(dead_code)]
fn q_color(q: &str) -> &'static str {
    match q {
        "q1" => "cyan",
        "q2" => "green",
        "q3" => "yellow",
        "q4" => "magenta",
        _ => "white",
    }
}

#[allow(dead_code)]
fn ring_color(ring: &str) -> &'static str {
    match ring {
        "adopt" => "green",
        "trial" => "cyan",
        "assess" => "yellow",
        "hold" => "red",
        _ => "white",
    }
}

// ── Coordinate conversion (matches Python _p2g) ───────────────────────────────
// angle_deg: clockwise from top (0=top, 90=right, 180=bottom, 270=left)
fn p2g(r_frac: f64, angle_deg: f64) -> Option<(usize, usize)> {
    let rad = angle_deg.to_radians();
    let row = CR as f64 - r_frac * MR * rad.cos();
    let col = CC as f64 + r_frac * MC * rad.sin();
    let r = row.round() as isize;
    let c = col.round() as isize;
    if r >= 0 && r < ROWS as isize && c >= 0 && c < COLS as isize {
        Some((r as usize, c as usize))
    } else {
        None
    }
}

type Grid = Vec<Vec<char>>;
type ColorGrid = Vec<Vec<String>>;

fn set(grid: &mut Grid, colors: &mut ColorGrid, r: usize, c: usize, ch: char, color: &str) {
    grid[r][c] = ch;
    colors[r][c] = color.to_string();
}

// ── Build radar (matches Python build_radar logic) ────────────────────────────
#[allow(dead_code)]
pub fn build_radar(blips: &mut [Blip], _q_names: &HashMap<String, String>) {
    let mut grid: Grid = vec![vec![' '; COLS]; ROWS];
    let mut colors: ColorGrid = vec![vec!["none".to_string(); COLS]; ROWS];

    // 1. Ring circles (fractions 0.25, 0.5, 0.75, 1.0 — the circle outlines)
    for ring_frac in [0.25f64, 0.50, 0.75, 1.00] {
        for deg in 0..360 {
            if let Some((r, c)) = p2g(ring_frac, deg as f64) {
                if grid[r][c] == ' ' {
                    set(&mut grid, &mut colors, r, c, '·', "grey30");
                }
            }
        }
    }

    // 2. Quadrant dividers at 0°/90°/180°/270°
    for deg in [0u32, 90, 180, 270] {
        for step in 1..=100 {
            let frac = step as f64 / 100.0;
            if let Some((r, c)) = p2g(frac, deg as f64) {
                if grid[r][c] == ' ' || grid[r][c] == '·' {
                    let ch = if deg == 0 || deg == 180 { '│' } else { '─' };
                    set(&mut grid, &mut colors, r, c, ch, "grey42");
                }
            }
        }
    }

    // 3. Center dot
    set(&mut grid, &mut colors, CR, CC, '+', "grey42");

    // 4. Ring labels on right side (90°): A/T/S/H
    for (name, frac) in [
        ("adopt", 0.20),
        ("trial", 0.45),
        ("assess", 0.70),
        ("hold", 0.92),
    ] {
        if let Some((r, c)) = p2g(frac, 90.0) {
            let label = match name {
                "adopt" => 'A',
                "trial" => 'T',
                "assess" => 'S',
                _ => 'H',
            };
            let nc = (c + 1).min(COLS - 1);
            set(&mut grid, &mut colors, r, nc, label, "grey54");
        }
    }

    // 5. Place blips — group by (quadrant, ring) sector
    let mut sectors: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, blip) in blips.iter().enumerate() {
        let key = (blip.quadrant.clone(), blip.ring.clone());
        sectors.entry(key).or_default().push(i);
    }

    let order = ["q1", "q2", "q3", "q4"];
    let rings = ["adopt", "trial", "assess", "hold"];
    let mut num = 1usize;

    for q in order {
        for ring in rings {
            let key = (q.to_string(), ring.to_string());
            let Some(indices) = sectors.get(&key) else {
                continue;
            };
            let n = indices.len();
            let ca = q_center_deg(q);
            let frac = ring_frac(ring);
            let spread = 38.0f64.min(10.0f64.max(n as f64 * 11.0));

            let angles: Vec<f64> = if n == 1 {
                vec![ca]
            } else {
                (0..n)
                    .map(|i| ca - spread / 2.0 + spread * i as f64 / (n - 1) as f64)
                    .collect()
            };

            for (idx, &blip_idx) in indices.iter().enumerate() {
                blips[blip_idx].number = num;
                let is_oss = blips[blip_idx].is_open_source;
                let color = if is_oss {
                    "bold bright_green"
                } else {
                    "bold bright_red"
                };
                let ns = num.to_string();

                if let Some((r, c)) = p2g(frac, angles[idx]) {
                    // collision avoidance: try 9 offsets
                    let offsets: &[(isize, isize)] = &[
                        (0, 0),
                        (0, 1),
                        (0, -1),
                        (-1, 0),
                        (1, 0),
                        (0, 2),
                        (0, -2),
                        (-1, 1),
                        (1, 1),
                    ];
                    let mut placed = false;
                    'outer: for &(dr, dc) in offsets {
                        let nr = r as isize + dr;
                        let nc = c as isize + dc;
                        if nr < 0 || nr >= ROWS as isize {
                            continue;
                        }
                        let nc = nc as usize;
                        let nr = nr as usize;
                        let fits = ns.chars().enumerate().all(|(k, _)| {
                            nc + k < COLS && (grid[nr][nc + k] == ' ' || grid[nr][nc + k] == '·')
                        });
                        if fits {
                            for (k, ch) in ns.chars().enumerate() {
                                set(&mut grid, &mut colors, nr, nc + k, ch, color);
                            }
                            placed = true;
                            break 'outer;
                        }
                    }
                    if !placed {
                        for (k, ch) in ns.chars().enumerate() {
                            if c + k < COLS {
                                set(&mut grid, &mut colors, r, c + k, ch, color);
                            }
                        }
                    }
                }
                num += 1;
            }
        }
    }

    // Store built grid for rendering (passed through render_radar args)
    // We embed the grid into a thread-local or just return it
    // Actually, let's store in a module-level state via returning from caller.
    // The caller will use a separate render function that takes grid.
    // Since Rust doesn't have Python's mutable return of grid easily, we use a wrapper.
    // We'll store grid+colors in RadarGrid returned from build_radar_grid().
    drop(grid);
    drop(colors);
}

pub struct RadarGrid {
    pub grid: Grid,
    pub colors: ColorGrid,
}

pub fn build_radar_grid(blips: &mut [Blip], _q_names: &HashMap<String, String>) -> RadarGrid {
    let mut grid: Grid = vec![vec![' '; COLS]; ROWS];
    let mut colors: ColorGrid = vec![vec!["none".to_string(); COLS]; ROWS];

    // 1. Ring circles
    for frac in [0.25f64, 0.50, 0.75, 1.00] {
        for deg in 0..360 {
            if let Some((r, c)) = p2g(frac, deg as f64) {
                if grid[r][c] == ' ' {
                    set(&mut grid, &mut colors, r, c, '·', "grey30");
                }
            }
        }
    }

    // 2. Quadrant dividers
    for deg in [0u32, 90, 180, 270] {
        for step in 1..=100 {
            if let Some((r, c)) = p2g(step as f64 / 100.0, deg as f64) {
                if grid[r][c] == ' ' || grid[r][c] == '·' {
                    let ch = if deg == 0 || deg == 180 { '│' } else { '─' };
                    set(&mut grid, &mut colors, r, c, ch, "grey42");
                }
            }
        }
    }

    // 3. Center
    set(&mut grid, &mut colors, CR, CC, '+', "grey42");

    // 4. Ring labels
    for (name, frac) in [
        ("adopt", 0.20f64),
        ("trial", 0.45),
        ("assess", 0.70),
        ("hold", 0.92),
    ] {
        if let Some((r, c)) = p2g(frac, 90.0) {
            let ch = match name {
                "adopt" => 'A',
                "trial" => 'T',
                "assess" => 'S',
                _ => 'H',
            };
            set(
                &mut grid,
                &mut colors,
                r,
                (c + 1).min(COLS - 1),
                ch,
                "grey54",
            );
        }
    }

    // 5. Blip placement
    let mut sectors: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, blip) in blips.iter().enumerate() {
        sectors
            .entry((blip.quadrant.clone(), blip.ring.clone()))
            .or_default()
            .push(i);
    }

    let mut num = 1usize;
    for q in ["q1", "q2", "q3", "q4"] {
        for ring in ["adopt", "trial", "assess", "hold"] {
            let Some(indices) = sectors.get(&(q.to_string(), ring.to_string())) else {
                continue;
            };
            let n = indices.len();
            let ca = q_center_deg(q);
            let frac = ring_frac(ring);
            let spread = 38.0f64.min(10.0f64.max(n as f64 * 11.0));

            let angles: Vec<f64> = if n == 1 {
                vec![ca]
            } else {
                (0..n)
                    .map(|i| ca - spread / 2.0 + spread * i as f64 / (n - 1) as f64)
                    .collect()
            };

            for (idx, &bi) in indices.iter().enumerate() {
                blips[bi].number = num;
                let is_oss = blips[bi].is_open_source;
                let color = if is_oss { "bright_green" } else { "bright_red" };
                let ns = num.to_string();

                if let Some((r, c)) = p2g(frac, angles[idx]) {
                    let offsets: &[(isize, isize)] = &[
                        (0, 0),
                        (0, 1),
                        (0, -1),
                        (-1, 0),
                        (1, 0),
                        (0, 2),
                        (0, -2),
                        (-1, 1),
                        (1, 1),
                    ];
                    let mut placed = false;
                    'outer: for &(dr, dc) in offsets {
                        let nr = r as isize + dr;
                        let nc = c as isize + dc;
                        if nr < 0 || nr >= ROWS as isize || nc < 0 {
                            continue;
                        }
                        let (nr, nc) = (nr as usize, nc as usize);
                        let fits = ns.chars().enumerate().all(|(k, _)| {
                            nc + k < COLS && (grid[nr][nc + k] == ' ' || grid[nr][nc + k] == '·')
                        });
                        if fits {
                            for (k, ch) in ns.chars().enumerate() {
                                set(&mut grid, &mut colors, nr, nc + k, ch, color);
                            }
                            placed = true;
                            break 'outer;
                        }
                    }
                    if !placed {
                        for (k, ch) in ns.chars().enumerate() {
                            if c + k < COLS {
                                set(&mut grid, &mut colors, r, c + k, ch, color);
                            }
                        }
                    }
                }
                num += 1;
            }
        }
    }

    RadarGrid { grid, colors }
}

// ── Render radar (matches Python render_radar) ────────────────────────────────
pub fn render_radar(
    rg: &RadarGrid,
    q_names: &HashMap<String, String>,
    kw: &str,
    title: &str,
    show_source_legend: bool,
) {
    let q1 = q_names.get("q1").map(|s| s.as_str()).unwrap_or("Q1");
    let q2 = q_names.get("q2").map(|s| s.as_str()).unwrap_or("Q2");
    let q3 = q_names.get("q3").map(|s| s.as_str()).unwrap_or("Q3");
    let q4 = q_names.get("q4").map(|s| s.as_str()).unwrap_or("Q4");

    println!();
    println!("  {}", style(format!("「{}」{}", kw, title)).bold().cyan());
    println!();

    // top quadrant labels
    println!(
        "  {} │ {}",
        style(format!("{:<35}", q2)).green().dim(),
        style(format!("{:<35}", q1)).cyan().dim()
    );

    // grid rows
    for (row_chars, row_colors) in rg.grid.iter().zip(rg.colors.iter()) {
        print!("  ");
        for (ch, color) in row_chars.iter().zip(row_colors.iter()) {
            let s = ch.to_string();
            match color.as_str() {
                "bright_green" => print!("{}", style(s).green().bold()),
                "bright_red" => print!("{}", style(s).red().bold()),
                "grey30" => print!("{}", style(s).dim()),
                "grey42" => print!("{}", style(s).dim()),
                "grey54" => print!("{}", style(s).dim()),
                _ => print!("{}", s),
            }
        }
        println!();
    }

    // bottom quadrant labels
    println!(
        "  {} │ {}",
        style(format!("{:<35}", q3)).yellow().dim(),
        style(format!("{:<35}", q4)).magenta().dim()
    );
    println!();

    // Ring legend
    print!("  ");
    if show_source_legend {
        print!("{} ", style("▲=開源").green());
        print!("{} ", style("●=閉源").red());
        print!("  ");
    }
    print!("{}", style("A").green());
    print!("=Adopt ");
    print!("{}", style("T").cyan());
    print!("=Trial ");
    print!("{}", style("S").yellow());
    print!("=Assess ");
    print!("{}", style("H").red());
    print!("=Hold");
    println!();
}

// ── Render legend table ───────────────────────────────────────────────────────
pub fn render_legend(
    blips: &[Blip],
    q_names: &HashMap<String, String>,
    item_label: &str,
    show_source_icon: bool,
) {
    println!();
    println!(
        "{}",
        style(format!("  {}項目清單", item_label)).bold().white()
    );

    let mut current_q: Option<String> = None;
    let ordered: Vec<&Blip> = {
        let mut v: Vec<&Blip> = blips.iter().collect();
        v.sort_by_key(|b| {
            (
                ["q1", "q2", "q3", "q4"]
                    .iter()
                    .position(|&q| q == b.quadrant)
                    .unwrap_or(0),
                ["adopt", "trial", "assess", "hold"]
                    .iter()
                    .position(|&r| r == b.ring)
                    .unwrap_or(0),
                b.number,
            )
        });
        v
    };

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_width(78);
    let icon_header = if show_source_icon { "" } else { "類型" };
    table.set_header(vec!["#", icon_header, item_label, "成熟度", "象限"]);

    for blip in &ordered {
        let q = &blip.quadrant;
        if current_q.as_deref() != Some(q) {
            current_q = Some(q.clone());
            let label = q_names.get(q).cloned().unwrap_or_else(|| q.clone());
            let colored_label = match q.as_str() {
                "q1" => format!("── {} ──", label),
                "q2" => format!("── {} ──", label),
                "q3" => format!("── {} ──", label),
                _ => format!("── {} ──", label),
            };
            table.add_row(vec![
                "".to_string(),
                "".to_string(),
                colored_label,
                "".to_string(),
                "".to_string(),
            ]);
        }

        let icon = if show_source_icon {
            if blip.is_open_source {
                "▲"
            } else {
                "●"
            }
        } else {
            "◆"
        };
        let ring_label = blip.ring.to_uppercase();
        let q_label = q_names.get(q).cloned().unwrap_or_else(|| q.clone());

        table.add_row(vec![
            blip.number.to_string(),
            icon.to_string(),
            blip.name.clone(),
            ring_label,
            q_label,
        ]);
    }

    println!("{}", table);
}

// ── Blip detail panel ─────────────────────────────────────────────────────────
pub fn show_blip_detail(
    blip: &Blip,
    q_names: &HashMap<String, String>,
    case_bundle: Option<&BlipCaseBundle>,
    case_error: Option<String>,
    show_source_icon: bool,
) {
    let q_label = q_names
        .get(&blip.quadrant)
        .cloned()
        .unwrap_or_else(|| blip.quadrant.clone());
    let ring_upper = blip.ring.to_uppercase();
    let type_label = if show_source_icon && blip.is_open_source {
        "▲ 開源".to_string()
    } else if show_source_icon {
        "● 閉源".to_string()
    } else {
        "◆ 方法".to_string()
    };

    let mut content = String::new();

    // Header info
    content.push_str(&format!("{type_label}   {ring_upper}   {q_label}"));
    if !blip.license.is_empty() {
        content.push_str(&format!("   {}", blip.license));
    }
    content.push_str("\n\n");

    // Description
    if !blip.description.is_empty() {
        content.push_str(&blip.description);
        content.push('\n');
    }

    // Upstream / Downstream
    if !blip.upstream.is_empty() {
        content.push_str(&format!("\n⬆ 上游依賴  {}", blip.upstream.join("  ")));
    }
    if !blip.downstream.is_empty() {
        content.push_str(&format!("\n⬇ 下游生態  {}", blip.downstream.join("  ")));
    }

    // GitHub activity
    if let Some(days) = blip.github_days {
        let icon = if days > 365 {
            "🔴"
        } else if days > 180 {
            "🟡"
        } else {
            "🟢"
        };
        content.push_str(&format!(
            "\n\nGitHub 活躍度  {} 最後更新 {} 天前",
            icon, days
        ));
        if !blip.github_repo.is_empty() {
            content.push_str(&format!("  ({})", blip.github_repo));
        }
    }

    // Enterprise cases
    content.push_str("\n\n🏢 企業案例\n");
    if let Some(bundle) = case_bundle {
        if bundle.cases.is_empty() {
            content.push_str("  • 未找到符合官方標準的公開案例\n");
        } else {
            for case in &bundle.cases {
                content.push_str(&format!("  • {}：{}\n", case.company, case.usage_summary));
                let mut meta = vec![case.publisher.clone()];
                if !case.published_at.is_empty() {
                    meta.push(case.published_at.clone());
                }
                if !case.evidence_type.is_empty() {
                    meta.push(case.evidence_type.clone());
                }
                content.push_str(&format!(
                    "    來源：{} — {}\n",
                    case.title,
                    meta.join(" | ")
                ));
                content.push_str(&format!("    URL：{}\n", case.url));
            }
        }
    } else if let Some(err) = case_error {
        content.push_str(&format!("  • 查找失敗：{}\n", err));
    } else {
        content.push_str("  • 尚未載入案例資料\n");
    }

    // Pros
    if !blip.pros.is_empty() {
        content.push_str("\n\n推薦理由\n");
        for p in &blip.pros {
            content.push_str(&format!("  • {}\n", p));
        }
    }

    // Cons
    if !blip.cons.is_empty() {
        content.push_str("\n⚠️  不推薦理由\n");
        for c in &blip.cons {
            content.push_str(&format!("  • {}\n", c));
        }
    }

    // Rationale
    if !blip.rationale.is_empty() {
        content.push_str(&format!("\n📌 分類依據\n{}", blip.rationale));
    }

    let color = match blip.ring.as_str() {
        "adopt" => "green",
        "trial" => "cyan",
        "assess" => "yellow",
        "hold" => "magenta",
        _ => "white",
    };
    panel(&format!("#{} {}", blip.number, blip.name), &content, color);
}
