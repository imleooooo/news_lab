# News Lab

```
  ██╗   ██╗███████╗██╗    ██╗███████╗      ██╗      █████╗ ██████╗
  ████╗  ██║██╔════╝██║    ██║██╔════╝      ██║     ██╔══██╗██╔══██╗
  ██╔██╗ ██║█████╗  ██║ █╗ ██║███████╗      ██║     ███████║██████╔╝
  ██║╚██╗██║██╔══╝  ██║███╗██║╚════██║      ██║     ██╔══██║██╔══██╗
  ██║ ╚████║███████╗╚███╔███╔╝███████║      ███████╗██║  ██║██████╔╝
  ╚═╝  ╚═══╝╚══════╝ ╚══╝╚══╝ ╚══════╝      ╚══════╝╚═╝  ╚═╝╚═════╝
```

科技新聞摘要 + 技術雷達的終端機 CLI，以 Rust 撰寫，透過 OpenAI API 對各大技術來源進行 AI 摘要與分析。

---

## 功能

| 功能 | 說明 |
|------|------|
| **新聞摘要** | 同時抓取 Hacker News、InfoQ、iThome、The Register、Ars Technica、TechCrunch，依時間排序並以繁體中文摘要 |
| **開源專案摘要** | GitHub 搜尋（近期熱門 / 新興專案），摘要專案特色與適用場景 |
| **arXiv 論文摘要** | LLM 自動擴展搜尋關鍵字，摘要近 30 天論文的研究問題與貢獻 |
| **Podcast 摘要** | 透過 iTunes Search API + RSS Feed 搜尋技術播客並摘要集數 |
| **知識圖譜** | 分析技術資料，在終端機以 ASCII 繪製技術分類與關係圖 |
| **技術生態雷達 + 競品分析** | 根據 GitHub 技術訊號生成互動式 ASCII 雷達圖（Adopt / Trial / Assess / Hold），點選項目可進行 AI 驅動的競品比較表與選型建議 |
| **HuggingFace 模型整理** | 抓取 HuggingFace 前 20 名熱門模型（依熱門趨勢 / 下載數 / 收藏數），以繁體中文摘要 |
| **CNCF 專案整理** | 從 CNCF TOC GitHub 整理最近值得關注的 Graduated / Incubating / Sandbox 專案 |

---

## 系統需求

- macOS 12 Monterey 以上（Apple Silicon 或 Intel）
- **OpenAI API 金鑰**（必要）
- GitHub Personal Access Token（選用，避免 API 限速）

---

## 安裝

### 方法一：安裝 .pkg（推薦）

從發布的 `News Lab-<版本>.pkg` 雙擊安裝，App 會安裝至 `/Applications/News Lab.app`。

> **首次啟動**：macOS Gatekeeper 會攔截未知開發者的 App。請**右鍵 → 打開**即可。之後可正常雙擊。

### 方法二：從原始碼建置

```bash
# 1. 安裝 Rust（若尚未安裝）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. 建置
cd news-app/news-rs
cargo build --release

# 3. 執行（OpenAI 模式需設定 OPENAI_API_KEY）
OPENAI_API_KEY=sk-... ./target/release/news_lab
```

---

## 設定

在 `news-app/news-rs/` 或 `news-app/` 目錄下建立 `.env` 檔：

```env
# OpenAI Provider 使用；若啟動時選「自定義 API」則不需要
OPENAI_API_KEY=sk-...

# 選用（提高 GitHub API 速率上限，CNCF 功能建議設定）
GITHUB_TOKEN=ghp_...

# 選用（技術雷達「進階審核」使用的模型，預設 gpt-5.4-2026-03-05）
REVIEW_MODEL=gpt-4o
```

`.pkg` 安裝版會自動將 `.env` 打包至 App bundle，無需額外設定。

啟動後可選擇 `OpenAI` 或 `自定義 API`。自定義 API 需相容 OpenAI Chat Completions 格式，並在互動提示中輸入：

- API base URL，例如 `https://example.com/v1`（不要填完整 `/chat/completions` endpoint）
- API key
- model name

---

## 使用方式

啟動後會進入互動式選單：

```
輸入搜尋關鍵字 (英文): AI on Kubernetes
```

接著選擇功能：

```
關鍵字: "AI on Kubernetes" — 選擇功能:
❯ 新聞摘要
  開源專案摘要
  arXiv 論文摘要
  Podcast 摘要
  知識圖譜
  技術生態雷達和競品分析 (請使用如 AI on K8s 去提問)
  其他功能 ▶
  調整筆數 (目前: 10)
  更換關鍵字
  離開
```

**技術生態雷達**使用建議：輸入較具體的技術領域，例如：
- `AI on K8s`、`LLM inference`、`vector database`、`platform engineering`

選擇項目後可進一步執行**競品分析**，產生競品比較表與選型建議。

---

## 目錄結構

```
news-rs/
├── build.rs               # 編譯期加密 LLM Prompt（XOR）
├── Cargo.toml
├── resources/             # App 圖示
├── scripts/
│   ├── build_app.sh       # 建置 ~/Applications/News Lab.app
│   └── make_installer.sh  # 建置 .pkg 安裝檔
└── src/
    ├── main.rs            # 主迴圈 + 選單 + 各功能入口
    ├── config.rs          # 設定精靈（模型選擇、筆數）
    ├── llm.rs             # LLMClient（async-openai）
    ├── ui.rs              # 終端機 UI 元件（panel、spinner、separator）
    ├── summarizer.rs      # AI 摘要函式（prompt 在 build.rs 加密）
    ├── fetcher/
    │   ├── tech.rs        # Hacker News、InfoQ、iThome、各科技媒體
    │   ├── arxiv.rs       # arXiv Atom XML 解析
    │   ├── podcast.rs     # iTunes Search + RSS Feed
    │   ├── huggingface.rs # HuggingFace API
    │   └── cncf.rs        # CNCF TOC GitHub Issues
    ├── radar/
    │   ├── mod.rs         # Blip 結構、LLM 提取、GitHub 活躍度檢查
    │   └── terminal.rs    # ASCII 23×71 雷達圖渲染
    └── knowledge/
        ├── mod.rs         # 知識圖譜提取
        └── terminal.rs    # ASCII 知識圖譜渲染
```

---

## 建置與發布

```bash
# 建置 macOS .app（安裝至 ~/Applications/）
./scripts/build_app.sh

# 建置 .pkg 安裝檔（可分享給他人）
./scripts/make_installer.sh
```

產生的 `.pkg` 包含 ad-hoc 程式碼簽名，同事安裝後首次右鍵 → 打開即可正常使用。

---

## 技術棧

| 套件 | 用途 |
|------|------|
| `tokio` | 非同步執行時期 |
| `async-openai` | OpenAI Chat Completion API |
| `reqwest` | HTTP 客戶端（rustls-tls） |
| `quick-xml` | arXiv / Podcast RSS XML 解析 |
| `inquire` | 互動式終端機選單 |
| `indicatif` + `console` | 進度條、ANSI 樣式 |
| `comfy-table` | 終端機表格（競品分析） |
| `serde` + `serde_json` | JSON 序列化 |
| `chrono` | 日期時間處理 |

---

## 技術文件

- [技術生態雷達 — 完整生成流程](docs/radar-flow.md)

---

## 版本紀錄

| 版本 | 更新內容 |
|------|---------|
| 0.1.3 | LLM Prompt 編譯期 XOR 加密、macOS ad-hoc 程式碼簽名 |
| 0.1.2 | 修正 HN 連結、GitHub 活躍度檢查、新增 HuggingFace 與 CNCF 功能 |
| 0.1.1 | 新增技術雷達競品分析（comfy-table 競品對比表） |
| 0.1.0 | 初始版本：新聞、arXiv、Podcast、技術雷達 |
