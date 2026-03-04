<p align="center">
  <img src="src-tauri/icons/icon.png" width="120" alt="RAG Search Logo"/>
</p>

<h1 align="center">RAG Search</h1>
<p align="center">
  <strong>Tìm kiếm tài liệu thông minh với AI — Chạy 100% cục bộ, bảo mật tuyệt đối</strong>
</p>

<p align="center">
  <a href="https://github.com/azoom-pham-the-tho/rag-search/releases"><img src="https://img.shields.io/github/v/release/azoom-pham-the-tho/rag-search?style=flat-square&color=blue" alt="Release"/></a>
  <a href="https://github.com/azoom-pham-the-tho/rag-search/actions"><img src="https://img.shields.io/github/actions/workflow/status/azoom-pham-the-tho/rag-search/release.yml?style=flat-square" alt="Build"/></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey?style=flat-square" alt="Platform"/>
  <img src="https://img.shields.io/badge/license-proprietary-red?style=flat-square" alt="License"/>
</p>

---

## 📖 Tổng quan

**RAG Search** là ứng dụng desktop cho phép tìm kiếm và hỏi đáp trên tài liệu cá nhân bằng AI. Toàn bộ dữ liệu được index và lưu trữ trên máy — không upload lên cloud, không rò rỉ dữ liệu.

Chỉ API request tới Gemini gửi đi **keyword + context snippet** (đã lọc PII), không gửi toàn bộ tài liệu.

### 🎯 Use Cases

- Tìm thông tin trong hàng trăm file PDF, Excel, Word nhanh chóng
- Hỏi AI câu hỏi về nội dung tài liệu (với citation nguồn)
- OCR ảnh chụp → tìm kiếm nội dung trong ảnh
- Theo dõi thay đổi file realtime (auto re-index)

---

## 🏗️ Kiến trúc hệ thống

```
┌─────────────────────────────────────────────────────────────┐
│                    Frontend (Vanilla JS + CSS)               │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────────┐ │
│  │ Chat UI  │  │ Search   │  │ Folder   │  │  Settings   │ │
│  │ (chat.js)│  │(search.js│  │(folder.js│  │(settings.js)│ │
│  └──────┬───┘  └────┬─────┘  └────┬─────┘  └──────┬──────┘ │
│         │           │             │                │        │
│  ┌──────┴───────────┴─────────────┴────────────────┴──────┐ │
│  │                  Tauri IPC Bridge (api.js)              │ │
│  └────────────────────────┬───────────────────────────────┘ │
└───────────────────────────┼─────────────────────────────────┘
                            │ invoke()
┌───────────────────────────┼─────────────────────────────────┐
│                    Backend (Rust + Tauri v2)                  │
│                           │                                  │
│  ┌────────────────────────▼───────────────────────────────┐  │
│  │              AI Chat Pipeline (pipeline.rs)             │  │
│  │  ┌─────────┐  ┌────────────┐  ┌────────────────────┐  │  │
│  │  │  L0:    │→ │ L1: AI     │→ │ L2: Hybrid Search  │  │  │
│  │  │  Fast   │  │ Keyword    │  │ BM25 + Vector      │  │  │
│  │  │Followup │  │ Extract    │  │ (parallel)         │  │  │
│  │  └─────────┘  └────────────┘  └────────┬───────────┘  │  │
│  │                                         │              │  │
│  │  ┌──────────────────────────────────────▼───────────┐  │  │
│  │  │        Context Builder (context.rs)               │  │  │
│  │  │  • Compound term matching                        │  │  │
│  │  │  • AND-majority scoring (≥50% keywords)          │  │  │
│  │  │  • Coverage expansion                            │  │  │
│  │  │  • Dynamic budget per intent                     │  │  │
│  │  │  • PII sanitization                              │  │  │
│  │  └──────────────────────────┬───────────────────────┘  │  │
│  │                              │                         │  │
│  │  ┌───────────────────────────▼──────────────────────┐  │  │
│  │  │     AI Response (Gemini Streaming + Citation)     │  │  │
│  │  └──────────────────────────────────────────────────┘  │  │
│  └────────────────────────────────────────────────────────┘  │
│                                                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐   │
│  │  Tantivy     │  │ HNSW Vector  │  │  SQLite          │   │
│  │  (BM25 FTS)  │  │ (768-dim)    │  │  (metadata +     │   │
│  │              │  │              │  │   settings)       │   │
│  └──────────────┘  └──────────────┘  └──────────────────┘   │
│                                                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐   │
│  │  Kreuzberg   │  │  Gemini      │  │  File Watcher    │   │
│  │  Parser      │  │  Embedding   │  │  (OS-native      │   │
│  │  (75+ fmts)  │  │  (768-dim)   │  │   FSEvents/RDC)  │   │
│  │  + OCR       │  │              │  │                   │   │
│  └──────────────┘  └──────────────┘  └──────────────────┘   │
└──────────────────────────────────────────────────────────────┘
```

---

## 🔧 Tech Stack & Skills

### Backend — Rust

| Component            | Technology                | Mô tả                                              |
| -------------------- | ------------------------- | -------------------------------------------------- |
| **Framework**        | Tauri v2                  | Desktop app framework, IPC bridge                  |
| **Full-text Search** | Tantivy                   | BM25 ranking, inverted index                       |
| **Vector Search**    | HNSW (instant-distance)   | 768-dim cosine similarity, lazy rebuild            |
| **Database**         | SQLite (rusqlite)         | File tracking, settings, chat history              |
| **Document Parser**  | Kreuzberg                 | 75+ formats: PDF, DOCX, XLSX, HTML, images...      |
| **OCR Engine**       | Tesseract (static-linked) | Eng + Việt + Nhật, bundled trong app               |
| **AI Integration**   | Gemini API                | Streaming chat, keyword extraction, embedding      |
| **File Watcher**     | notify (OS-native)        | FSEvents (macOS) / ReadDirectoryChangesW (Windows) |
| **Concurrency**      | Tokio + Rayon             | Async I/O + parallel CPU processing                |

### Frontend — Vanilla JS + CSS

| Component        | Mô tả                                     |
| ---------------- | ----------------------------------------- |
| **UI Framework** | Vanilla JS (zero dependencies)            |
| **Styling**      | Custom CSS with dark theme, glassmorphism |
| **Icons**        | Lucide Icons                              |
| **Markdown**     | Custom renderer for AI responses          |
| **Real-time**    | Tauri event streaming (SSE-like)          |

### DevOps / CI/CD

| Tool          | Mô tả                                   |
| ------------- | --------------------------------------- |
| **CI/CD**     | GitHub Actions — auto build on tag push |
| **Platforms** | macOS (ARM64 + Intel) + Windows x64     |
| **Package**   | DMG (macOS) + MSI/EXE (Windows)         |

---

## 🧠 Core Algorithms & Design Patterns

### 1. Hybrid Search (BM25 + Vector)

```
Query → ┬→ BM25 Search (Tantivy)     → top 10 by term frequency
        └→ Vector Search (HNSW)       → top 15 by semantic similarity
                    ↓
           Merge & Re-rank (composite score)
                    ↓
           Context Builder → AI
```

- **BM25**: Exact keyword matching, fast, good for specific terms
- **Vector**: Semantic similarity, handles synonyms and paraphrasing
- **Parallel execution**: Both run concurrently via `tokio::join!`

### 2. Multi-layer Chat Pipeline

```
L0: Fast Follow-up Detection (<1ms)
  ├─ Pronoun references ("nó", "đó", "file này")
  ├─ Follow-up indicators ("thêm", "tiếp", "chi tiết")
  ├─ Keyword overlap (>40%)
  └─ → Skip search, reuse context

L1: AI Keyword Extraction (Gemini Flash, 1.5s timeout)
  ├─ Context-aware: understands follow-up from history
  ├─ Compound term detection ("Test 123")
  └─ Fallback: heuristic extraction if AI timeout

L2: Hybrid Search + Context Building
  ├─ BM25 + Vector parallel search
  ├─ Compound term expansion (load ALL chunks)
  ├─ AND-majority scoring (≥50% keywords present)
  ├─ Dynamic budget per intent type
  └─ PII sanitization before sending to AI

L3: AI Streaming Response
  ├─ Gemini streaming (SSE)
  ├─ Multi-key rotation (rate limit handling)
  ├─ Citation extraction & verification
  └─ History context injection
```

### 3. Smart Query Intent Classification

| Intent        | Budget     | Strategy                   |
| ------------- | ---------- | -------------------------- |
| **Lookup**    | 40K chars  | Tìm vài dòng cụ thể        |
| **Summarize** | 120K chars | Cần đọc nhiều → budget cao |
| **Compare**   | 80K chars  | So sánh 2+ mục             |
| **Aggregate** | 120K chars | Thống kê, tổng hợp         |
| **Extract**   | 80K chars  | Trích xuất dữ liệu         |
| **Verify**    | 40K chars  | Xác nhận thông tin         |

### 4. Embedding Pipeline

```
Documents → Kreuzberg → Chunks (2KB/4KB)
    ↓
Gemini Embedding API (768-dim)
    ↓
HNSW Index (lazy rebuild)
    ↓
Binary persistence (bincode)
```

- **Batch processing**: 100 texts/request, rate limit aware
- **Separate Mutex**: Watcher embedding ≠ Chat embedding (zero contention)
- **Lazy HNSW rebuild**: Index only rebuilds before first search after changes

### 5. Real-time File Watching

```
OS Events (FSEvents/RDC)
    ↓
Debounce (2s)
    ↓
Parse + Chunk + Embed + Index
    ↓
UI Notification ("files-changed" event)
```

- **Startup diff**: Detect changes made while app was closed
- **Delta re-index**: Only re-process changed files
- **OS-native**: FSEvents on macOS, ReadDirectoryChangesW on Windows

### 6. Privacy-First Architecture

```
                    ┌──────────────┐
   User's Machine   │   Gemini API  │
┌─────────────────┐ │              │
│ Documents       │ │ Receives:    │
│ (never leaves)  │ │ • keywords   │
│                 │→│ • context    │
│ Index (local)   │ │   snippets   │
│ SQLite (local)  │ │ • sanitized  │
│ Vectors (local) │ │   (PII masked│
└─────────────────┘ └──────────────┘
```

---

## 📂 Project Structure

```
ragSearch/
├── src/                          # Frontend
│   ├── index.html                # Main HTML (SPA)
│   ├── js/
│   │   ├── api.js                # Tauri IPC wrapper
│   │   ├── app.js                # App initialization
│   │   ├── chat.js               # AI chat UI (streaming, citations)
│   │   ├── search.js             # Search UI
│   │   ├── folder.js             # Folder management
│   │   ├── settings.js           # Settings panel
│   │   └── chunk-viewer.js       # Chunk inspection tool
│   └── styles/
│       ├── main.css              # Design system, dark theme
│       ├── chat.css              # Chat bubble styles
│       ├── sidebar.css           # Navigation
│       └── settings.css          # Settings panel
│
├── src-tauri/                    # Backend (Rust)
│   ├── src/
│   │   ├── lib.rs                # App setup, state init
│   │   ├── ai/
│   │   │   ├── gemini.rs         # Gemini API client (streaming + JSON)
│   │   │   ├── memory.rs         # Chat history (SQLite)
│   │   │   ├── model_registry.rs # Auto-discover available models
│   │   │   └── structured.rs     # Structured output (JSON schema)
│   │   ├── commands/
│   │   │   └── search/
│   │   │       ├── pipeline.rs   # Main chat pipeline (1570 LOC)
│   │   │       ├── context.rs    # Context builder + ranking (960 LOC)
│   │   │       ├── keyword.rs    # Keyword extraction (heuristic)
│   │   │       ├── prompt.rs     # Prompt engineering per intent
│   │   │       └── decompose.rs  # Complex query decomposition
│   │   ├── embedding/
│   │   │   ├── pipeline.rs       # Batch embed + rate limit
│   │   │   └── gemini_embed.rs   # Gemini Embedding API
│   │   ├── indexer/
│   │   │   ├── tantivy_index.rs  # BM25 full-text search
│   │   │   └── chunker.rs       # Smart text chunking
│   │   ├── parser/
│   │   │   └── mod.rs            # Document parser (75+ formats + OCR)
│   │   ├── search/
│   │   │   ├── hybrid.rs         # BM25 + Vector merger
│   │   │   └── vector_index.rs   # HNSW vector search (768-dim)
│   │   ├── watcher/
│   │   │   ├── mod.rs            # OS-native file watcher
│   │   │   └── handler.rs        # File change event handler
│   │   └── db/
│   │       └── sqlite.rs         # SQLite schemas + queries
│   ├── tessdata/                 # OCR training data (eng + vie + jpn)
│   └── tauri.conf.json           # Tauri config
│
└── .github/
    └── workflows/
        └── release.yml           # CI/CD: auto build macOS + Windows
```

---

## ⚡ Performance Characteristics

| Metric             | Value                               |
| ------------------ | ----------------------------------- |
| **Search latency** | ~40ms (BM25 + Vector parallel)      |
| **Chat pipeline**  | ~1.5s (keyword extraction) + stream |
| **Fast follow-up** | <1ms (context reuse)                |
| **HNSW search**    | O(log n) — sub-millisecond          |
| **File watch**     | <2s debounce → auto re-index        |
| **Startup diff**   | Detect offline changes in <100ms    |
| **Memory**         | ~50MB base + index size             |

---

## 🚀 Getting Started

### Prerequisites

- [Gemini API key](https://aistudio.google.com/apikey) (miễn phí)

### Download

Tải bản mới nhất tại [Releases](https://github.com/azoom-pham-the-tho/rag-search/releases):

- **macOS (Apple Silicon)**: `.dmg`
- **macOS (Intel)**: `.dmg`
- **Windows**: `.msi` installer

### Cài đặt

1. **macOS**: Mở `.dmg` → kéo vào Applications
2. **Windows**: Chạy `.msi` installer
3. Mở app → Nhập API key → Thêm thư mục tài liệu

### Development

```bash
# Prerequisites
brew install rust node tesseract

# Clone & run
git clone git@github.com:azoom-pham-the-tho/rag-search.git
cd rag-search
npm install
npm run tauri dev
```

---

## 🛡️ Security & Privacy

- 📁 **Tài liệu KHÔNG BAO GIỜ rời khỏi máy** — chỉ keyword + snippet gửi tới AI
- 🔒 **PII masking** — số điện thoại, email được che trước khi gửi
- 🏠 **Index lưu local** — Tantivy + HNSW + SQLite trên ổ cứng
- 🔑 **API key lưu local** — trong SQLite database (không gửi đi đâu)

---

## 📊 Skills & Technologies Demonstrated

### Systems Programming (Rust)

- Ownership & borrowing, lifetime management
- Async/await with Tokio runtime
- Thread-safe state management (Arc, Mutex, RwLock, AtomicBool)
- FFI integration (C++ Tesseract via kreuzberg)
- Binary serialization (bincode for HNSW persistence)

### Information Retrieval

- BM25 ranking algorithm (Tantivy)
- Vector embeddings + Cosine similarity (HNSW)
- Hybrid search (lexical + semantic fusion)
- Compound term matching + whitespace normalization
- AND-majority relevance scoring

### AI/ML Engineering

- RAG (Retrieval-Augmented Generation) pipeline
- Prompt engineering per query intent
- Streaming LLM responses (SSE parsing)
- Multi-model support + API key rotation
- Rate limit handling + exponential backoff
- Context window management (token budgeting)

### Desktop Application Development

- Tauri v2 (Rust + Web frontend)
- Native file system integration
- OS-level file watching (FSEvents, ReadDirectoryChangesW)
- Cross-platform bundling (macOS DMG, Windows MSI)
- OCR integration (Tesseract static-linked)

### Frontend Development

- Vanilla JS architecture (zero framework, zero build step)
- Real-time streaming UI (token-by-token rendering)
- Dark theme + glassmorphism design
- Responsive layout
- Keyboard shortcuts + accessibility

### DevOps & CI/CD

- GitHub Actions multi-platform build
- Automated release + binary distribution
- Cross-compilation handling
- Resource bundling (tessdata, icons)

---

<p align="center">
  Built with ❤️ using <strong>Rust + Tauri + Gemini AI</strong>
</p>
