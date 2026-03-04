use std::path::PathBuf;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexReader, IndexWriter};
use thiserror::Error;

use super::chunker::Chunk;

#[derive(Error, Debug)]
pub enum TantivyError {
    #[error("Tantivy error: {0}")]
    Internal(#[from] tantivy::TantivyError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Tantivy full-text search index
pub struct TantivyIndex {
    pub index: Index,
    pub schema: Schema,
    // Field handles
    pub field_chunk_id: Field,
    pub field_file_path: Field,
    pub field_file_name: Field,
    pub field_content: Field,
    pub field_section: Field,
    pub field_folder_id: Field,
}

impl TantivyIndex {
    /// Tạo hoặc mở Tantivy index tại thư mục cho trước
    pub fn new(index_dir: PathBuf) -> Result<Self, TantivyError> {
        std::fs::create_dir_all(&index_dir)?;

        // Build schema
        let mut schema_builder = Schema::builder();

        let field_chunk_id = schema_builder.add_text_field("chunk_id", STRING | STORED);
        let field_file_path = schema_builder.add_text_field("file_path", STRING | STORED);
        let field_file_name = schema_builder.add_text_field("file_name", TEXT | STORED);
        let field_content = schema_builder.add_text_field("content", TEXT | STORED);
        let field_section = schema_builder.add_text_field("section", STRING | STORED);
        let field_folder_id = schema_builder.add_text_field("folder_id", STRING | STORED);

        let schema = schema_builder.build();

        // Open or create index
        let index = if index_dir.join("meta.json").exists() {
            Index::open_in_dir(&index_dir)?
        } else {
            Index::create_in_dir(&index_dir, schema.clone())?
        };

        Ok(TantivyIndex {
            index,
            schema,
            field_chunk_id,
            field_file_path,
            field_file_name,
            field_content,
            field_section,
            field_folder_id,
        })
    }

    /// Thêm chunks vào index (batch) — tạo writer mới mỗi lần
    pub fn add_chunks(&self, chunks: &[Chunk], folder_id: &str) -> Result<(), TantivyError> {
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;

        for chunk in chunks {
            writer.add_document(doc!(
                self.field_chunk_id => chunk.id.clone(),
                self.field_file_path => chunk.file_path.clone(),
                self.field_file_name => chunk.file_name.clone(),
                self.field_content => chunk.content.clone(),
                self.field_section => chunk.section.clone().unwrap_or_default(),
                self.field_folder_id => folder_id.to_string(),
            ))?;
        }

        writer.commit()?;
        Ok(())
    }

    /// Tạo writer dùng chung cho batch indexing
    pub fn create_writer(&self) -> Result<IndexWriter, TantivyError> {
        Ok(self.index.writer(50_000_000)?)
    }

    /// Thêm chunks vào writer đã tạo sẵn (không commit)
    pub fn add_chunks_to_writer(
        &self,
        writer: &IndexWriter,
        chunks: &[Chunk],
        folder_id: &str,
    ) -> Result<(), TantivyError> {
        for chunk in chunks {
            writer.add_document(doc!(
                self.field_chunk_id => chunk.id.clone(),
                self.field_file_path => chunk.file_path.clone(),
                self.field_file_name => chunk.file_name.clone(),
                self.field_content => chunk.content.clone(),
                self.field_section => chunk.section.clone().unwrap_or_default(),
                self.field_folder_id => folder_id.to_string(),
            ))?;
        }
        Ok(())
    }

    /// Xóa chunks của file bằng writer đã tạo sẵn (không commit)
    pub fn delete_file_with_writer(&self, writer: &IndexWriter, file_path: &str) {
        let term = tantivy::Term::from_field_text(self.field_file_path, file_path);
        writer.delete_term(term);
    }

    pub fn delete_file_chunks(&self, file_path: &str) -> Result<(), TantivyError> {
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        let term = tantivy::Term::from_field_text(self.field_file_path, file_path);
        writer.delete_term(term);
        writer.commit()?;
        Ok(())
    }

    /// Xóa toàn bộ index
    pub fn clear_index(&self) -> Result<(), TantivyError> {
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        writer.delete_all_documents()?;
        writer.commit()?;
        Ok(())
    }

    /// Search BM25 — trả về top N kết quả
    pub fn search(&self, query_str: &str, top_n: usize) -> Result<Vec<(f32, Chunk)>, TantivyError> {
        self.search_internal(query_str, top_n, None)
    }

    /// Search BM25 scoped to a specific folder
    pub fn search_in_folder(
        &self,
        query_str: &str,
        top_n: usize,
        folder_id: &str,
    ) -> Result<Vec<(f32, Chunk)>, TantivyError> {
        self.search_internal(query_str, top_n, Some(folder_id))
    }

    /// Internal search with optional folder filter
    fn search_internal(
        &self,
        query_str: &str,
        top_n: usize,
        folder_id: Option<&str>,
    ) -> Result<Vec<(f32, Chunk)>, TantivyError> {
        let reader: IndexReader = self.index.reader()?;
        let searcher = reader.searcher();

        let query_parser = tantivy::query::QueryParser::for_index(
            &self.index,
            vec![self.field_content, self.field_file_name],
        );

        // Sanitize: escape ký tự đặc biệt của Tantivy query syntax
        let sanitized = sanitize_query(query_str);

        let text_query: Box<dyn tantivy::query::Query> = match query_parser.parse_query(&sanitized)
        {
            Ok(q) => q,
            Err(_) => {
                let (q, _errors) = query_parser.parse_query_lenient(&sanitized);
                q
            }
        };

        // If folder filter → BooleanQuery(MUST text + MUST folder_id)
        let final_query: Box<dyn tantivy::query::Query> = if let Some(fid) = folder_id {
            let folder_term = tantivy::Term::from_field_text(self.field_folder_id, fid);
            let folder_query = tantivy::query::TermQuery::new(
                folder_term,
                tantivy::schema::IndexRecordOption::Basic,
            );
            Box::new(tantivy::query::BooleanQuery::new(vec![
                (tantivy::query::Occur::Must, text_query),
                (tantivy::query::Occur::Must, Box::new(folder_query)),
            ]))
        } else {
            text_query
        };

        let top_docs =
            searcher.search(&final_query, &tantivy::collector::TopDocs::with_limit(top_n))?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: tantivy::TantivyDocument = searcher.doc(doc_address)?;

            let chunk_id = doc
                .get_first(self.field_chunk_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let file_path = doc
                .get_first(self.field_file_path)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let file_name = doc
                .get_first(self.field_file_name)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = doc
                .get_first(self.field_content)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let section = doc
                .get_first(self.field_section)
                .and_then(|v| v.as_str())
                .map(|s| {
                    if s.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    }
                })
                .unwrap_or(None);

            results.push((
                score,
                Chunk {
                    id: chunk_id,
                    file_path,
                    file_name,
                    content,
                    section,
                    position: 0,
                    char_start: 0,
                    char_end: 0,
                },
            ));
        }

        Ok(results)
    }

    /// Load ALL chunks belonging to a specific file (by exact file_path)
    pub fn get_chunks_by_file_path(&self, file_path: &str) -> Result<Vec<Chunk>, TantivyError> {
        let reader: IndexReader = self.index.reader()?;
        let searcher = reader.searcher();

        let term = tantivy::Term::from_field_text(self.field_file_path, file_path);
        let query = tantivy::query::TermQuery::new(term, tantivy::schema::IndexRecordOption::Basic);

        let top_docs = searcher.search(&query, &tantivy::collector::TopDocs::with_limit(500))?;

        let mut results = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc: tantivy::TantivyDocument = searcher.doc(doc_address)?;

            let content = doc
                .get_first(self.field_content)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let file_name = doc
                .get_first(self.field_file_name)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let chunk_id = doc
                .get_first(self.field_chunk_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let section = doc
                .get_first(self.field_section)
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    if s.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    }
                });

            results.push(Chunk {
                id: chunk_id,
                file_path: file_path.to_string(),
                file_name,
                content,
                section,
                position: 0,
                char_start: 0,
                char_end: 0,
            });
        }

        Ok(results)
    }
}

/// Loại bỏ ký tự đặc biệt của Tantivy query syntax
/// Tránh lỗi parse khi user nhập tên file có [], (), {} v.v.
fn sanitize_query(query: &str) -> String {
    query
        .chars()
        .map(|c| match c {
            '[' | ']' | '(' | ')' | '{' | '}' | '~' | '^' | '!' | ':' | '\\' | '/' => ' ',
            _ => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
