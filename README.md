<p align="center">
  <img src="src-tauri/icons/icon.png" width="120" alt="RAG Search Logo"/>
</p>

<h1 align="center">RAG Search</h1>
<p align="center">
  <strong>Tìm kiếm tài liệu thông minh với AI — 100% cục bộ, bảo mật tuyệt đối</strong>
</p>

<p align="center">
  <a href="https://github.com/azoom-pham-the-tho/rag-search/releases"><img src="https://img.shields.io/github/v/release/azoom-pham-the-tho/rag-search?style=flat-square&color=blue" alt="Release"/></a>
  <a href="https://github.com/azoom-pham-the-tho/rag-search/actions"><img src="https://img.shields.io/github/actions/workflow/status/azoom-pham-the-tho/rag-search/release.yml?style=flat-square" alt="Build"/></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey?style=flat-square" alt="Platform"/>
</p>

---

## 📖 RAG là gì? Tại sao cần RAG Search?

**RAG (Retrieval-Augmented Generation)** là kỹ thuật kết hợp "tìm kiếm" + "AI" để trả lời câu hỏi dựa trên tài liệu thực tế.

> **Vấn đề**: ChatGPT / Gemini rất giỏi, nhưng chúng **không biết nội dung tài liệu riêng của bạn** (file công ty, hóa đơn, báo cáo...).

> **Giải pháp RAG**: Trước khi hỏi AI, hệ thống **tìm kiếm** trong tài liệu của bạn → lấy phần liên quan nhất → **đưa vào prompt** cho AI đọc → AI trả lời chính xác kèm nguồn trích dẫn.

### So sánh nhanh

|                            | Hỏi AI thường   | RAG Search                                |
| -------------------------- | --------------- | ----------------------------------------- |
| AI biết nội dung file bạn? | ❌ Không        | ✅ Có — tìm và đọc file trước khi trả lời |
| Có nguồn trích dẫn?        | ❌ Không        | ✅ Có — ghi rõ từ file nào, trang nào     |
| Dữ liệu lưu ở đâu?         | Gửi lên cloud   | 🏠 100% trên máy bạn                      |
| Hỗ trợ file gì?            | Copy-paste text | 📄 75+ format: PDF, Excel, Word, ảnh...   |

---

## 🎯 Ứng dụng thực tế

- 📊 Tra cứu nhanh trong hàng trăm file Excel hóa đơn, báo cáo
- 📝 Hỏi tóm tắt nội dung file Word/PDF dài
- 🔍 Tìm thông tin cụ thể (tên, số tiền, ngày tháng) trong chồng tài liệu
- 📸 OCR ảnh chụp tài liệu → tìm kiếm nội dung trong ảnh
- 🗂️ Thêm thư mục → hệ thống tự đồng bộ khi file thay đổi

---

## 🏗️ Cách hệ thống hoạt động

RAG Search có **3 luồng xử lý chính**:

### Luồng 1: Nạp dữ liệu (Data Ingestion)

> _Khi bạn thêm thư mục tài liệu vào app_

<p align="center">
  <img src="docs/flow-ingestion.png" width="700" alt="Data Ingestion Flow"/>
</p>

**Giải thích từng bước:**

1. **Thêm thư mục** — Bạn chọn thư mục chứa tài liệu cần tìm kiếm
2. **Quét file** — Hệ thống phát hiện tất cả file được hỗ trợ (PDF, DOCX, XLSX, ảnh, HTML, v.v.)
3. **Parse & Extract** — Mỗi file được đọc bởi engine `Kreuzberg`:
   - PDF → extract text + giữ cấu trúc bảng
   - Excel → đọc từng sheet, giữ header
   - Ảnh (JPG, PNG) → chạy OCR (Tesseract) nhận dạng chữ
4. **Tạo chunks** — Text được cắt thành đoạn nhỏ ~2KB (có overlap để không mất ngữ cảnh giữa 2 đoạn)
5. **Index kép** — Cùng 1 bộ chunks được đưa vào **2 index song song**:
   - 🔤 **BM25 Index** (Tantivy): Đánh chỉ mục theo từ khóa — tìm chính xác theo keyword
   - 🧠 **Vector Index** (HNSW): Gọi Gemini API chuyển text → vector 768 chiều — tìm theo ngữ nghĩa (hiểu đồng nghĩa, câu tương tự)

> 💡 **Tại sao cần 2 index?**
>
> - BM25 giỏi tìm chính xác: "hóa đơn số 12345" → match đúng "12345"
> - Vector giỏi hiểu ngữ nghĩa: "chi phí vận chuyển" ≈ "phí ship hàng"
> - Kết hợp cả hai → kết quả tốt nhất

---

### Luồng 2: Chat với AI (AI Chat Pipeline)

> _Khi bạn gõ câu hỏi và nhấn Enter_

<p align="center">
  <img src="docs/flow-chat.png" width="600" alt="AI Chat Pipeline"/>
</p>

**Giải thích từng layer:**

**L0 — Fast Follow-up Check** `(<1ms)`

- Nếu câu hỏi là follow-up ("thêm chi tiết", "còn gì nữa không", "nó là gì") → **bỏ qua tìm kiếm**, dùng lại context cũ
- Phát hiện bằng: đại từ ("nó", "đó"), từ tiếp nối ("thêm", "tiếp tục"), hoặc keyword trùng >40%
- **Nếu hỏi topic mới** → luôn search lại, KHÔNG dùng context cũ

**L1 — AI Keyword Extraction** `(~1.5s)`

- Gọi Gemini Flash trích từ khóa thông minh
- VD: _"cho tôi hóa đơn tháng 3 của công ty ABC"_ → keywords: `"hóa đơn", "tháng 3", "ABC"`
- Hiểu context từ lịch sử chat → biết "nó" chỉ cái gì
- Nếu AI chậm/lỗi → fallback sang heuristic (cắt stop words)

**L2 — Hybrid Search** `(~40ms)`

- Chạy **song song** 2 engine:
  - BM25: tìm theo từ khóa chính xác
  - Vector: tìm theo ngữ nghĩa (cosine similarity)
- Merge kết quả → xếp hạng theo composite score

**L3 — Context Builder** `(~5ms)`

- Chọn top chunks phù hợp nhất (scoring: chunk phải chứa ≥50% keywords)
- Giới hạn context theo loại câu hỏi:
  - Tra cứu nhanh → 40K chars
  - Tóm tắt → 120K chars (cần đọc nhiều)
- **PII masking**: che số điện thoại, email trước khi gửi AI

**L4 — AI Streaming Response**

- Gửi context + câu hỏi tới Gemini
- Nhận response kiểu streaming (từng từ một, như ChatGPT)
- AI tự cite nguồn: _"Theo file invoices.xlsx [1]..."_
- Xử lý rate limit: nếu key bị giới hạn → tự rotate sang key khác

---

### Luồng 3: Đồng bộ thay đổi (File Watcher)

> _Khi file trong thư mục bị thay đổi (thêm, sửa, xóa)_

<p align="center">
  <img src="docs/flow-watcher.png" width="600" alt="File Watcher Flow"/>
</p>

**Giải thích từng bước:**

1. **OS File Watcher** — Dùng API hệ điều hành để lắng nghe thay đổi:
   - macOS: FSEvents (kernel-level, cực nhanh)
   - Windows: ReadDirectoryChangesW
2. **Phát hiện thay đổi** — File mới, file sửa, file xóa
3. **Debounce 2s** — Đợi 2 giây gom batch (tránh xử lý từng file khi copy hàng loạt)
4. **Xử lý**:
   - File mới/sửa → parse lại + cập nhật BM25 + tạo vector mới
   - File xóa → xóa khỏi tất cả index
5. **Thông báo UI** — Gửi event cho frontend hiển thị badge

**Bonus — Startup Diff**: Khi mở app, hệ thống so sánh DB vs filesystem → phát hiện file thay đổi khi app đang tắt → tự re-index.

---

## 🔧 Công nghệ sử dụng

### Backend — Rust

| Thành phần       | Công nghệ                     | Vai trò                                       |
| ---------------- | ----------------------------- | --------------------------------------------- |
| Framework        | **Tauri v2**                  | App desktop, IPC bridge (Rust ↔ JS)           |
| Full-text Search | **Tantivy**                   | BM25 ranking, inverted index ~40ms            |
| Vector Search    | **HNSW** (instant-distance)   | Cosine similarity 768-dim, O(log n)           |
| Database         | **SQLite** (rusqlite)         | File tracking, settings, chat history         |
| Document Parser  | **Kreuzberg**                 | Parse 75+ formats (PDF, DOCX, XLSX...)        |
| OCR Engine       | **Tesseract** (static-linked) | Nhận dạng chữ trong ảnh (EN + VI + JP)        |
| AI               | **Gemini API**                | Chat streaming, keyword extraction, embedding |
| File Watcher     | **notify**                    | OS-native file change detection               |
| Async Runtime    | **Tokio**                     | Non-blocking I/O, parallel tasks              |
| CPU Parallelism  | **Rayon**                     | Parallel parse + chunk trên multi-core        |

### Frontend — Vanilla JS

| Thành phần | Mô tả                                        |
| ---------- | -------------------------------------------- |
| UI         | Vanilla JS — zero framework, zero build step |
| Styling    | Custom CSS, dark theme, glassmorphism        |
| Streaming  | Real-time token-by-token rendering           |
| Icons      | Lucide Icons                                 |

### CI/CD

| Công cụ        | Mô tả                                |
| -------------- | ------------------------------------ |
| GitHub Actions | Auto build khi push tag              |
| Output         | macOS `.dmg` + Windows `.msi`/`.exe` |

---

## 📊 Hiệu năng

| Metric                     | Giá trị                       |
| -------------------------- | ----------------------------- |
| Tìm kiếm (BM25 + Vector)   | ~40ms                         |
| Fast follow-up             | <1ms                          |
| AI keyword extraction      | ~1.5s                         |
| HNSW vector search         | O(log n) — sub-millisecond    |
| File watch → re-index      | <2s debounce                  |
| Phát hiện thay đổi offline | <100ms khi khởi động          |
| RAM                        | ~50MB base + kích thước index |

---

## 🛡️ Bảo mật & Quyền riêng tư

| Nguyên tắc                | Chi tiết                                       |
| ------------------------- | ---------------------------------------------- |
| 📁 Tài liệu KHÔNG rời máy | File chỉ được đọc local, không upload          |
| 🔒 PII masking            | Số điện thoại, email được che trước khi gửi AI |
| 📤 Gửi gì cho AI?         | Chỉ keywords + đoạn trích nhỏ (đã filter)      |
| 🔑 API key                | Lưu local trong SQLite                         |
| 🏠 Index                  | Tantivy + HNSW + SQLite — 100% trên ổ cứng     |

---

## 🚀 Cài đặt & Sử dụng

### Download

👉 [**Releases**](https://github.com/azoom-pham-the-tho/rag-search/releases) — chọn file phù hợp:

- **macOS (Apple Silicon)**: `RAG Search_x.x.x_aarch64.dmg`
- **macOS (Intel)**: `RAG Search_x.x.x_x64.dmg`
- **Windows**: `RAG Search_x.x.x_x64-setup.exe`

### Yêu cầu

- [Gemini API key](https://aistudio.google.com/apikey) (miễn phí)

### Cài đặt

1. **macOS**: Mở `.dmg` → kéo vào Applications
2. **Windows**: Chạy `.msi` hoặc `.exe`
3. Mở app → Settings → Nhập API key
4. Thêm thư mục tài liệu → Đợi index xong → Bắt đầu tìm kiếm!

### Development

```bash
# Yêu cầu: Rust, Node.js, Tesseract
brew install rust node tesseract

# Clone & chạy
git clone git@github.com:azoom-pham-the-tho/rag-search.git
cd rag-search
npm install
npm run tauri dev
```

---

## 📂 Cấu trúc dự án

```
ragSearch/
├── src/                              # 🖥️ Frontend (Vanilla JS)
│   ├── index.html                    # SPA entry point
│   ├── js/
│   │   ├── api.js                    # Tauri IPC wrapper
│   │   ├── chat.js                   # AI chat UI (streaming)
│   │   ├── search.js                 # Search UI
│   │   ├── folder.js                 # Quản lý thư mục
│   │   └── settings.js              # Cài đặt
│   └── styles/                       # CSS (dark theme)
│
├── src-tauri/                        # ⚙️ Backend (Rust)
│   ├── src/
│   │   ├── ai/                       # 🧠 Gemini integration
│   │   │   ├── gemini.rs             # API client (streaming + JSON)
│   │   │   ├── memory.rs             # Chat history management
│   │   │   └── model_registry.rs     # Auto-discover models
│   │   ├── commands/search/          # 🔍 Search pipeline (core)
│   │   │   ├── pipeline.rs           # Main chatbot pipeline
│   │   │   ├── context.rs            # Context builder + ranking
│   │   │   ├── keyword.rs            # Keyword extraction
│   │   │   └── prompt.rs             # Prompt engineering
│   │   ├── embedding/                # 📊 Vector embedding
│   │   │   └── pipeline.rs           # Gemini Embedding API
│   │   ├── indexer/                   # 📚 Indexing
│   │   │   ├── tantivy_index.rs      # BM25 full-text search
│   │   │   └── chunker.rs            # Smart text chunking
│   │   ├── parser/                    # 📄 Document parsing
│   │   │   └── mod.rs                # 75+ formats + OCR
│   │   ├── search/                    # 🎯 Search engines
│   │   │   ├── hybrid.rs             # BM25 + Vector merger
│   │   │   └── vector_index.rs       # HNSW (768-dim)
│   │   └── watcher/                   # 👁️ File watching
│   │       ├── mod.rs                # OS-native watcher
│   │       └── handler.rs            # Change event handler
│   └── tessdata/                      # OCR training data
│
└── .github/workflows/
    └── release.yml                    # 🚀 CI/CD auto-build
```

---

## 🎓 Kỹ năng áp dụng

### Systems Programming (Rust)

- **Ownership & Borrowing** — quản lý bộ nhớ an toàn không cần garbage collector
- **Async/Await** — xử lý I/O không chặn với Tokio runtime
- **Thread-safe state** — `Arc<Mutex<T>>`, `RwLock`, `AtomicBool` cho concurrent access
- **FFI** — tích hợp C++ (Tesseract OCR) qua kreuzberg bindings
- **Zero-cost abstractions** — trait, generics, enum cho type-safe code

### Information Retrieval (IR)

- **BM25 ranking** — thuật toán xếp hạng theo tần suất từ khóa
- **Vector embeddings** — chuyển text → không gian 768 chiều, tìm bằng cosine similarity
- **Hybrid search** — kết hợp lexical + semantic search, ưu điểm cả hai
- **HNSW** (Hierarchical Navigable Small World) — cấu trúc graph cho approximate nearest neighbor search
- **Compound term matching** — xử lý cụm từ có khoảng trắng ("Test 123")

### AI Engineering

- **RAG pipeline** — Retrieval → Augment → Generate
- **Prompt engineering** — prompt khác nhau theo intent (tra cứu, tóm tắt, so sánh)
- **Streaming LLM** — parse SSE stream real-time
- **Multi-model support** — auto-discover + key rotation
- **Rate limit handling** — retry + fallback + key cycling
- **Token budgeting** — quản lý context window theo loại câu hỏi

### Desktop Application

- **Tauri v2** — native app (Rust backend + Web frontend)
- **OS-level file watching** — FSEvents (macOS), ReadDirectoryChangesW (Windows)
- **Cross-platform build** — CI/CD cho macOS ARM64 + Intel + Windows
- **OCR bundling** — Tesseract static-linked, tessdata bundled

### Frontend

- **Zero-dependency** — Vanilla JS, không framework
- **Real-time streaming UI** — token-by-token rendering
- **Event-driven architecture** — Tauri events cho IPC

---

<p align="center">
  Built with ❤️ using <strong>Rust</strong> + <strong>Tauri v2</strong> + <strong>Gemini AI</strong>
</p>
