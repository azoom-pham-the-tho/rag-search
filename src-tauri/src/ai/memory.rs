use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MemoryError {
    #[error("Database error: {0}")]
    DbError(#[from] rusqlite::Error),
}

/// Chat session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub citations: Option<String>, // JSON serialized
    pub model: Option<String>,
    pub timestamp: i64,
}

/// Conversation memory — lưu và truy xuất chat history
pub struct ConversationMemory;

impl ConversationMemory {
    /// Tạo session mới
    pub fn create_session(
        conn: &Connection,
        id: &str,
        title: Option<&str>,
    ) -> Result<(), MemoryError> {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO chat_sessions (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, title, now, now],
        )?;
        Ok(())
    }

    /// Lưu message
    pub fn save_message(conn: &Connection, msg: &ChatMessage) -> Result<(), MemoryError> {
        conn.execute(
            "INSERT INTO chat_messages (id, session_id, role, content, citations, model, timestamp) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                msg.id,
                msg.session_id,
                msg.role,
                msg.content,
                msg.citations,
                msg.model,
                msg.timestamp,
            ],
        )?;

        // Update session timestamp
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, msg.session_id],
        )?;

        Ok(())
    }

    /// Lấy lịch sử chat của session
    pub fn get_history(
        conn: &Connection,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>, MemoryError> {
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, citations, model, timestamp
             FROM chat_messages
             WHERE session_id = ?1
             ORDER BY timestamp ASC",
        )?;

        let messages = stmt
            .query_map(rusqlite::params![session_id], |row| {
                Ok(ChatMessage {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    citations: row.get(4)?,
                    model: row.get(5)?,
                    timestamp: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(messages)
    }

    /// Lấy messages dưới dạng (role, content) cho Gemini
    pub fn get_context_pairs(
        conn: &Connection,
        session_id: &str,
    ) -> Result<Vec<(String, String)>, MemoryError> {
        let messages = Self::get_history(conn, session_id)?;
        Ok(messages.into_iter().map(|m| (m.role, m.content)).collect())
    }

    /// Xóa lịch sử chat
    pub fn clear_session(conn: &Connection, session_id: &str) -> Result<(), MemoryError> {
        conn.execute(
            "DELETE FROM chat_messages WHERE session_id = ?1",
            rusqlite::params![session_id],
        )?;
        conn.execute(
            "DELETE FROM chat_sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }

    /// Danh sách sessions gần đây
    pub fn list_sessions(conn: &Connection, limit: usize) -> Result<Vec<ChatSession>, MemoryError> {
        let mut stmt = conn.prepare(
            "SELECT id, title, created_at, updated_at
             FROM chat_sessions
             ORDER BY updated_at DESC
             LIMIT ?1",
        )?;

        let sessions = stmt
            .query_map(rusqlite::params![limit], |row| {
                Ok(ChatSession {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(sessions)
    }

    /// Auto-generate title từ tin nhắn đầu tiên
    /// Auto generate title for session (mock for now)
    #[allow(dead_code)]
    pub fn auto_title(conn: &Connection, session_id: &str) -> Result<(), MemoryError> {
        let messages = Self::get_history(conn, session_id)?;
        if let Some(first) = messages.first() {
            let title: String = first.content.chars().take(50).collect();
            let title = if first.content.len() > 50 {
                format!("{}...", title)
            } else {
                title
            };

            conn.execute(
                "UPDATE chat_sessions SET title = ?1 WHERE id = ?2",
                rusqlite::params![title, session_id],
            )?;
        }
        Ok(())
    }
}
