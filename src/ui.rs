use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const WIDTH: usize = 76;
const INNER: usize = WIDTH - 2; // 74

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn apply_color(s: &str, color: &str) -> String {
    match color {
        "cyan" => style(s).cyan().to_string(),
        "green" => style(s).green().to_string(),
        "yellow" => style(s).yellow().to_string(),
        "blue" => style(s).blue().to_string(),
        "magenta" => style(s).magenta().to_string(),
        _ => style(s).white().to_string(),
    }
}

pub fn panel(title: &str, content: &str, color: &str) {
    // ╭──────╮
    println!(
        "{}",
        apply_color(&format!("╭{}╮", "─".repeat(INNER)), color)
    );

    // │ Title  padding │  — wrap if title is too long
    let avail = WIDTH - 4;
    let title_lines = wrap_display(title, avail);
    for (i, tline) in title_lines.iter().enumerate() {
        let dw = display_width(tline);
        let pad = avail.saturating_sub(dw);
        let content = if i == 0 {
            format!("│ {}{} │", style(tline.as_str()).bold(), " ".repeat(pad))
        } else {
            format!(
                "│   {}{} │",
                style(tline.as_str()).bold(),
                " ".repeat(pad.saturating_sub(2))
            )
        };
        println!("{}", apply_color(&content, color));
    }

    // ├──────┤
    println!(
        "{}",
        apply_color(&format!("├{}┤", "─".repeat(INNER)), color)
    );

    // Content lines — available = WIDTH - 4 columns
    let avail = WIDTH - 4; // 72
    for line in content.lines() {
        for chunk in wrap_display(line, avail) {
            let chunk_dw = display_width(&chunk);
            let pad = avail.saturating_sub(chunk_dw);
            println!(
                "{}",
                apply_color(&format!("│ {}{} │", chunk, " ".repeat(pad)), color)
            );
        }
    }

    // ╰──────╯
    println!(
        "{}",
        apply_color(&format!("╰{}╯", "─".repeat(INNER)), color)
    );
}

/// Word-wrap with character-level fallback for words wider than max_w
/// (handles CJK text that has no whitespace between words).
fn wrap_display(s: &str, max_w: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut col = 0usize;

    let push_line = |lines: &mut Vec<String>, current: &mut String, col: &mut usize| {
        lines.push(std::mem::take(current));
        *col = 0;
    };

    for word in s.split_whitespace() {
        let ww = display_width(word);

        if ww > max_w {
            // Word itself is too wide — flush current and break char-by-char
            if col > 0 {
                push_line(&mut lines, &mut current, &mut col);
            }
            for ch in word.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
                if col + cw > max_w && col > 0 {
                    push_line(&mut lines, &mut current, &mut col);
                }
                current.push(ch);
                col += cw;
            }
        } else if col == 0 {
            current.push_str(word);
            col = ww;
        } else if col + 1 + ww <= max_w {
            current.push(' ');
            current.push_str(word);
            col += 1 + ww;
        } else {
            push_line(&mut lines, &mut current, &mut col);
            current.push_str(word);
            col = ww;
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Print a URL on its own line, outside any panel box.
/// Uses OSC 8 hyperlink so macOS Terminal renders it as a clickable link.
pub fn print_url(url: &str) {
    // OSC 8 format: ESC]8;;URL ESC\ LINK_TEXT ESC]8;; ESC\
    let clickable = format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\");
    println!("  {} {}", style("🔗").dim(), clickable);
}

pub fn separator() {
    println!("{}", style("─".repeat(WIDTH)).dim());
}

pub struct Spinner {
    bar: ProgressBar,
}

impl Spinner {
    pub fn new(msg: &str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::default_spinner()
                .tick_strings(&["⠋", "⠙", "⠸", "⠴", "⠦", "⠇"])
                .template("{spinner:.cyan} {msg}")
                .unwrap(),
        );
        bar.set_message(msg.to_string());
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }

    /// Erase spinner and optionally print a status line.
    /// Pass an empty string to suppress output.
    pub fn finish(&self, msg: &str) {
        self.bar.finish_and_clear();
        if !msg.is_empty() {
            println!("  {} {}", style("✓").green(), msg);
        }
    }
}
