# 技術生態雷達 — 完整生成流程

> 對應程式碼：`src/main.rs::run_terminal_radar` → `src/radar/mod.rs` → `src/radar/terminal.rs`

---

## 整體流程概覽

```
使用者輸入關鍵字
       │
       ▼
 ① 抓取技術訊號          fetch_radar_signals()
       │
       ▼
 ② LLM 提取 Blip        extract_blips()
       │
       ▼
 ③ 進階模型審核（×2）    review_and_augment()
       │
       ▼
 ④ GitHub 活躍度校正     check_oss_activity()
       │
       ▼
 ⑤ 建構 ASCII 格子       build_radar_grid()
       │
       ▼
 ⑥ 輸出雷達圖            render_radar()
       │
       ▼
 ⑦ 輸出清單表格          render_legend()
       │
       ▼
 ⑧ 互動式瀏覽 + 競品分析
```

---

## ① 抓取技術訊號

**位置：** `main.rs:run_terminal_radar`

```rust
let fetch_n = cfg.max_results.max(12);
let items = fetch_radar_signals(kw, fetch_n).await;
```

- 同時呼叫 GitHub 熱門 repo 與 GitHub 新興 repo 搜尋
- 結果去重後作為雷達的技術訊號輸入
- 最少抓取 12 筆（雷達圖需要足夠上下文給 LLM 判斷生態）

---

## ② LLM 提取 Blip

**位置：** `radar/mod.rs::extract_blips`

### 2.1 組合 Prompt

取前 10 筆技術訊號，格式化為：
```
- [GitHub] vllm-project/vllm | ⭐ 48512 | Python | LLM serving engine
- [GitHub] kubeflow/kubeflow | ⭐ 14321 | Go | Machine learning toolkit for Kubernetes
...
```

注入 `RADAR_PROMPT`，要求 LLM：
1. 找出技術訊號中所有開源/閉源專案、模型、工具、框架
2. 補充 LLM 自身知識中該領域重要項目
3. 為 4 個象限命名（依領域特性，例如 AI 領域：模型/框架/工具/技術）
4. 為每個項目判斷成熟度環形

### 2.2 環形定義

| 環形 | 代表意義 |
|------|---------|
| `adopt` | 生產環境主流，業界廣泛採用 |
| `trial` | 有成功案例，值得新專案採用 |
| `assess` | 值得關注，仍在快速發展 |
| `hold` | 已被取代或有重大疑慮 |

> ⚠️ 閉源服務（如 OpenAI API、Claude API）若業界廣泛使用，仍可列為 `adopt`；
> 「閉源」風險記錄在 `cons` 欄位，不用來壓低環形評級。

### 2.3 LLM 呼叫

```rust
let response = llm.invoke_with_limit(&prompt, 16384).await?;
```

使用 **16,384 token 輸出上限**（一個完整雷達圖可包含 15–40 個 blip，JSON 較大）。

### 2.4 JSON 解析與正規化

```
LLM 回應
    │
    ├── strip_code_fences()    移除 ```json ... ``` 包裝
    │
    ├── extract_json()         用 regex 抓取 { ... } 主體
    │
    └── serde_json::from_str() 解析為 RadarResponse { quadrant_names, blips }
```

每個 blip 正規化：
- `ring` 和 `quadrant` → 強制小寫、去空白
- 若 `ring` 不在合法值 → 強制改為 `assess`
- 若 `quadrant` 不在 `q1`–`q4` → 強制改為 `q1`

### 2.5 去重（deduplicate）

避免 LLM 重複列出同一技術的不同稱呼（如 `LangChain` 與 `langchain`、`Kubernetes` 與 `K8s`）。

**判定為重複的條件（任一成立）：**

```
key(A) 是 key(B) 的前綴
  OR
key(B) 是 key(A) 的前綴
  OR
tokens(A) ⊆ tokens(B)  或  tokens(B) ⊆ tokens(A)
```

其中：
- `key()` = 只保留英數字、轉小寫
- `tokens()` = 以空格/點/連字號/底線分詞，過濾 stopwords（`&`, `and`, `the`, `by`, `for`）

**重複時保留規則：**
1. 名稱較長者（保留更完整的正式名稱）
2. 同長則保留 ring_rank 較低者（更成熟：adopt < trial < assess < hold）

---

## ③ 進階模型審核（最多 2 輪）

**位置：** `main.rs:run_terminal_radar`（loop）→ `radar/mod.rs::review_and_augment`

```rust
let review_llm = LLMClient::new("gpt-5.4-2026-03-05")?;
for round in 1..=2u8 {
    let satisfied = review_and_augment(&mut blips, &q_names, kw, &review_llm).await;
    if satisfied { break; }
}
```

### 審核流程

1. 將現有 blip 清單摘要為純文字（name | 象限 | ring | 開/閉源）
2. 送入 `REVIEW_PROMPT`，要求進階模型評估生態完整性

**回傳兩種情況：**

```json
// 已完整 → 停止審核
{"satisfied": true}

// 有缺漏 → 補充新 blip
{
  "satisfied": false,
  "reason": "缺少向量資料庫類項目",
  "blips": [ {...新項目...} ]
}
```

3. 新項目同樣正規化後，追加至 blip 清單，再執行一次 `deduplicate()`
4. 若 LLM 呼叫失敗 → 視為 `satisfied: true`，跳過本輪

---

## ④ GitHub 活躍度校正

**位置：** `radar/mod.rs::check_oss_activity`

只對 `is_open_source: true` 的 blip 執行。

### 查詢邏輯

```
GitHub Search API
  q=<blip.name>+in:name   ← +in:name 限定 repo 名稱包含關鍵字
  sort=stars&per_page=5
         │
         └── 從 5 個候選中選 最佳匹配：
             min_by_key((name_mismatch, days_since_push))
             │
             ├── name_mismatch = 0：repo 名稱與 blip 名稱有包含關係
             └── name_mismatch = 1：無法對應（退而求其次）
```

### 環形調降規則

| 最後更新距今 | 動作 |
|-------------|------|
| > 365 天 | 環形下調 **2 級**（adopt → assess、trial → hold…） |
| 181–365 天 | 環形下調 **1 級** |
| ≤ 180 天 | 不調整 |

調降時在 `rationale` 末尾追加警告：
```
⚠️ GitHub 最後更新 423 天前，活躍度極低，從 TRIAL 下調兩級。
```

如遇 GitHub API 403（超過限速）→ 立即停止，其餘 blip 維持原環形。

---

## ⑤ 建構 ASCII 格子

**位置：** `radar/terminal.rs::build_radar_grid`

### 格子尺寸

```
ROWS = 23（高）
COLS = 71（寬）
中心：CR = 11, CC = 35
最大半徑：MR = 10（行）, MC = 20（列，2:1 長寬比）
```

### 極座標轉格子座標（p2g）

角度以**順時針從正上方**為 0°（0°=上、90°=右、180°=下、270°=左）：

```
row = CR - r_frac × MR × cos(angle)
col = CC + r_frac × MC × sin(angle)
```

2:1 長寬比是因為終端機字元高度約為寬度的兩倍，補償後視覺上為正圓。

### 繪製順序

```
① 環形圓弧（r_frac = 0.25 / 0.50 / 0.75 / 1.00）
   字元：·  顏色：grey30

② 象限分隔線（0° / 90° / 180° / 270°）
   0°/180° → │   90°/270° → ─   顏色：grey42

③ 中心點：+（顏色：grey42）

④ 環形標籤（放在 90° 方向的最右側）
   adopt → A   trial → T   assess → S   hold → H

⑤ 放置 Blip 數字
```

### Blip 放置演算法

**分組：** 按 (quadrant, ring) 形成 sector。

**遍歷順序：** q1 → q2 → q3 → q4，每個象限內 adopt → trial → assess → hold，依序編號 1, 2, 3…

**角度分配：**

```
象限中心角：q1=45°, q2=315°, q3=225°, q4=135°
ring 半徑：adopt=0.20, trial=0.45, assess=0.70, hold=0.92

同一 sector 有 n 個 blip：
  spread = min(38°, max(10°, n × 11°))
  angle[i] = center_angle - spread/2 + spread × i/(n-1)
```

**碰撞迴避（9 個偏移位置，依序嘗試）：**

```
(0,0) → (0,+1) → (0,-1) → (-1,0) → (+1,0)
→ (0,+2) → (0,-2) → (-1,+1) → (+1,+1)
```

找到第一個所有數字位都是空白（` ` 或 `·`）的位置後放置。若全部失敗，強制覆蓋原始位置。

**顏色：**
- 開源（`is_open_source: true`）→ `bright_green`（▲）
- 閉源 → `bright_red`（●）

---

## ⑥ 輸出雷達圖

**位置：** `radar/terminal.rs::render_radar`

```
  Q2（框架 & 函式庫）│ Q1（模型 & 演算法）
  ─────────────────────────────────────────
  [23 × 71 格子，ANSI 上色]
  ─────────────────────────────────────────
  Q3（工具 & 平台）  │ Q4（技術 & 方法）

  ▲=開源 ●=閉源   A=Adopt T=Trial S=Assess H=Hold
```

---

## ⑦ 輸出清單表格

**位置：** `radar/terminal.rs::render_legend`

- `comfy-table`，寬度 78
- 排序：象限順序（q1→q4）→ 環形成熟度（adopt→hold）→ 編號
- 每個象限前插入分組標題列

```
┌───┬────┬──────────────────┬────────┬──────────────┐
│ # │    │ 專案             │ 成熟度 │ 象限         │
├───┼────┼──────────────────┼────────┼──────────────┤
│   │    │ ── 模型 & 演算法 ──│        │              │
│ 1 │ ●  │ GPT-4o           │ ADOPT  │ 模型 & 演算法 │
│ 2 │ ▲  │ Llama 3          │ ADOPT  │ 模型 & 演算法 │
...
```

---

## ⑧ 互動式瀏覽 + 競品分析

**位置：** `main.rs:run_terminal_radar`（loop）

```
inquire::Select 列出所有 blip
       │
       ├── ← 返回主選單  → 結束
       │
       └── 選擇 blip #N
               │
               ▼
        show_blip_detail()
        ┌─────────────────────────┐
        │ #N  專案名稱             │
        │ ▲開源  ADOPT  象限  授權 │
        │                         │
        │ 描述：...               │
        │ ⬆ 上游依賴：...         │
        │ ⬇ 下游生態：...         │
        │ GitHub 活躍度 🟢 14天前 │
        │ ✅ 推薦理由             │
        │ ⚠️  不推薦理由          │
        │ 📌 分類依據             │
        └─────────────────────────┘
               │
               ▼
        inquire::Select（子選單）
        ┌─────────────────────┐
        │ ⚔ 對「X」進行競品分析│
        │ ← 後退              │
        └─────────────────────┘
               │
    ┌──────────┴──────────────┐
    │ 競品分析                 │
    │  tokio::join!(           │
    │    invoke(COMPETITOR_JSON)  → 競品對比表（JSON）  │
    │    invoke_with_limit(       → 選型建議文字       │
    │      ANALYSIS_TEXT, 4096)                        │
    │  )                       │
    │                          │
    │  render_competitor_table()  comfy-table 寬 92    │
    │  panel("⚔ 競品分析：X")     text panel           │
    └──────────────────────────┘
```

### 競品分析 LLM 呼叫

兩個呼叫**並行執行**（`tokio::join!`）：

| 呼叫 | Prompt | 輸出 | 用途 |
|------|--------|------|------|
| 第一個 | `COMPETITOR_JSON` | JSON 陣列（4–6 列競品） | comfy-table 對比表 |
| 第二個 | `ANALYSIS_TEXT`（上限 4096 tokens） | 純文字（章節 2–5） | panel 顯示選型建議 |

JSON 解析流程：
```
LLM 回應 → strip_code_fence() → 找 [ ... ] 邊界 → serde_json::from_str::<Vec<CompetitorRow>>()
```

---

## 資料結構總覽

```rust
struct Blip {
    name: String,               // 專案名稱（≤20 字元）
    quadrant: String,           // "q1" | "q2" | "q3" | "q4"
    ring: String,               // "adopt" | "trial" | "assess" | "hold"
    is_open_source: bool,
    description: String,        // 一句話 + 條列重點
    license: String,            // "Apache 2.0" 等
    upstream: Vec<String>,      // 上游依賴
    downstream: Vec<String>,    // 下游生態
    pros: Vec<String>,          // 推薦理由
    cons: Vec<String>,          // 限制或疑慮
    rationale: String,          // 環形/象限分類依據
    github_repo: String,        // "owner/repo"（check_oss_activity 填入）
    github_days: Option<i64>,   // 距最後 push 天數（check_oss_activity 填入）
    number: usize,              // 圖上編號（build_radar_grid 填入）
}
```

---

## 各階段 LLM 使用

| 階段 | 模型 | max_tokens | 用途 |
|------|------|-----------|------|
| extract_blips | `cfg.model`（預設 gpt-4o-mini）| 16,384 | 生成完整雷達 JSON |
| review_and_augment | `gpt-5.4-2026-03-05` | 8,192 | 審核完整性並補充缺漏 |
| analyze_competition（JSON）| `cfg.model` | 2,048（預設）| 競品 JSON 陣列 |
| analyze_competition（text）| `cfg.model` | 4,096 | 選型建議文字 |
