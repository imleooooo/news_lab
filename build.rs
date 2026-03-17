use std::io::Write;

// XOR key – arbitrary bytes, no recognizable pattern
const KEY: &[u8] = &[
    0x4e, 0x6c, 0x39, 0x21, 0x7f, 0x2a, 0x5c, 0x11, 0x88, 0x45, 0xc3, 0x7b, 0x2d, 0x91, 0xf4, 0x63,
];

fn enc(s: &str) -> Vec<u8> {
    s.bytes()
        .enumerate()
        .map(|(i, b)| b ^ KEY[i % KEY.len()])
        .collect()
}

fn write_enc(f: &mut impl Write, name: &str, s: &str) {
    let encoded = enc(s);
    write!(f, "pub static {name}: &[u8] = &[").unwrap();
    for b in &encoded {
        write!(f, "0x{b:02x},").unwrap();
    }
    writeln!(f, "];").unwrap();
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let path = std::path::Path::new(&out_dir).join("encoded_prompts.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    write_enc(
        &mut f,
        "NEWS",
        r#"你是一位科技新聞分析師，專門分析技術文章與 Hacker News 討論。
請針對以下新聞項目提供簡潔的中文摘要（2-3句話），包含：
1. 核心技術概念或創新點
2. 對開發者社群的影響或意義
3. 值得關注的討論角度

搜尋關鍵字（僅供背景參考）：{keyword}
標題：{title}
來源：{source}
內容：{description}

⚠️ 重要：請摘要文章實際內容；若文章與搜尋關鍵字無明顯直接關聯，請勿強行建立關聯。
請用繁體中文回答，保持專業且易於理解。"#,
    );

    write_enc(
        &mut f,
        "GITHUB",
        r#"你是一位開源技術專家，專門分析 GitHub 上的熱門專案。
請針對以下 GitHub 專案提供簡潔的中文摘要（2-3句話），包含：
1. 專案的主要功能和技術特點
2. 適用場景和目標使用者
3. 為什麼值得關注

關鍵字：{keyword}
專案名稱：{title}
專案網址：{url}
描述：{description}

請用繁體中文回答，保持專業且易於理解。"#,
    );

    write_enc(
        &mut f,
        "PODCAST",
        r#"你是一位科技播客分析師，專門整理技術相關的播客內容。
請針對以下播客集數提供簡潔的中文摘要（2-3句話），包含：
1. 主要討論主題和核心觀點
2. 對聽眾的價值和收穫
3. 特別值得注意的見解或討論

關鍵字：{keyword}
播客名稱：{podcast_name}
集數標題：{title}
時長：{duration}
描述：{description}

請用繁體中文回答，保持專業且易於理解。"#,
    );

    write_enc(
        &mut f,
        "ARXIV",
        r#"你是一位 AI/ML 研究領域的專家，專門解析學術論文。
請針對以下 arXiv 論文提供簡潔的中文摘要（2-3句話），包含：
1. 研究問題和創新方法
2. 主要實驗結果或理論貢獻
3. 對業界或學術界的潛在影響

關鍵字：{keyword}
論文標題：{title}
作者：{authors}
類別：{categories}
摘要：{abstract_text}

請用繁體中文回答，保持學術嚴謹且易於理解。"#,
    );

    write_enc(
        &mut f,
        "HF_MODEL",
        r#"你是一位 AI 模型評估專家，專門分析 Hugging Face 上的開源模型。
請針對以下模型提供簡潔的中文摘要（2-3句話），包含：
1. 模型的主要功能和核心特點
2. 適用場景和目標使用者群體
3. 值得關注的優勢或獨特之處

模型名稱：{model_id}
任務類型：{pipeline_tag}
下載次數：{downloads}
收藏次數：{likes}
相關標籤：{tags}

請用繁體中文回答，保持專業且易於理解。"#,
    );

    write_enc(
        &mut f,
        "CNCF",
        r#"你是一位雲原生技術專家，專門分析 CNCF（Cloud Native Computing Foundation）生態系統。
請針對以下 CNCF 專案提供簡潔的中文摘要（3-4句話），包含：
1. 專案的主要功能和核心技術特點
2. 在雲原生生態中解決的問題或定位
3. 適用場景和目標使用者
4. 為什麼目前值得關注

專案名稱：{name}
成熟度：{maturity_label}
GitHub：{full_name}
描述：{description}
程式語言：{language}
Stars：{stars}
加入 CNCF：{accepted_at}

請用繁體中文回答，保持專業且易於理解。"#,
    );

    write_enc(
        &mut f,
        "COMPETITOR_JSON",
        r#"你是一位技術分析師。針對「{name}」，列出 4–6 個競品（含「{name}」本身，放第一列）。
關鍵字領域：{keyword}
目標產品描述：{description}
雷達內相關產品（供參考）：{radar_items}

只回傳 JSON 陣列，不加任何說明文字或 code fence：
[
  {"type": "開源/閉源", "name": "產品名", "positioning": "核心定位（20字內）", "pros": "主要優勢（25字內）", "cons": "主要劣勢（25字內）"},
  ...
]"#,
    );

    write_enc(
        &mut f,
        "ANALYSIS_TEXT",
        r#"你是一位技術產品分析師，專門進行軟體與技術選型的競品分析。
今天日期：{today}。關鍵字領域：「{keyword}」

目標產品：{name}（{ring_upper}，{ring_desc}）
{oss_label}
描述：{description}
推薦理由：{pros}
限制或疑慮：{cons}

雷達圖中的同象限 / 相關產品：
{radar_items}

請撰寫以下章節（不需要第 1 節競品比較表，那已單獨處理）：

2. **「{name}」的核心競爭優勢**（條列，3–5 點）

3. **主要劣勢與風險**（條列，2–4 點）

4. **選型建議**
   - 適合選「{name}」的情境
   - 不適合、應選其他競品的情境

5. **總結**：{name} 在「{keyword}」生態中的定位與建議成熟度評估

請用繁體中文回答，保持專業、客觀、有依據。
⚠️ 嚴格禁止：不得加上「若需要」、「如需進一步」、「我可以幫你」等後續邀約語句。"#,
    );
}
