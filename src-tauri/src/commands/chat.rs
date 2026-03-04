use serde::{Deserialize, Serialize};
use tauri::State;
use crate::AppState;
use crate::ai::memory::ConversationMemory;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Citation {
    pub file_path: String,
    pub file_name: String,
    pub section: Option<String>,
    pub content_snippet: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub id: String,
    pub role: String, // "user" | "assistant"
    pub content: String,
    pub citations: Vec<Citation>,
    pub model: Option<String>,
    pub timestamp: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    pub model: String,         // "gemini-2.0-flash" etc.
    pub session_id: String,
}

/// Gửi message cho AI chat (RAG pipeline)
#[tauri::command]
pub async fn send_message(
    request: ChatRequest,
    state: State<'_, AppState>,
) -> Result<ChatMessage, String> {
    let now = chrono::Utc::now().timestamp();
    let msg_id = uuid::Uuid::new_v4().to_string();

    // Save user message to database
    {
        let db = state.db.lock().await;

        // Ensure session exists
        let _ = ConversationMemory::create_session(
            &db.conn,
            &request.session_id,
            Some(&request.message.chars().take(50).collect::<String>()),
        ); // Ignore error if session already exists

        let user_msg = crate::ai::memory::ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: request.session_id.clone(),
            role: "user".to_string(),
            content: request.message.clone(),
            citations: None,
            model: None,
            timestamp: now,
        };
        let _ = ConversationMemory::save_message(&db.conn, &user_msg);
    }

    // TODO: Full RAG pipeline — for now, try BM25 search + format response
    // When API key is configured, use GeminiClient for actual AI response
    let response_content = {
        let tantivy_guard = state.tantivy.lock().await;
        if let Some(tantivy) = tantivy_guard.as_ref() {
            match tantivy.search(&request.message, 3) {
                Ok(results) if !results.is_empty() => {
                    let mut answer = String::from("Dựa trên tài liệu, tôi tìm thấy:\n\n");
                    for (i, (_score, chunk)) in results.iter().enumerate() {
                        let preview: String = chunk.content.chars().take(200).collect();
                        answer.push_str(&format!(
                            "**[Nguồn {}]** `{}`\n{}\n\n",
                            i + 1,
                            chunk.file_name,
                            preview
                        ));
                    }
                    answer.push_str("\n⚠️ *Để có phân tích AI chi tiết, cấu hình Gemini API key trong Cài đặt.*");
                    answer
                }
                _ => {
                    "Không tìm thấy tài liệu liên quan. Hãy:\n\n• Thêm thư mục và đợi index xong\n• Cấu hình Gemini API key trong Cài đặt\n• Thử từ khóa khác".to_string()
                }
            }
        } else {
            "Hệ thống search chưa sẵn sàng. Vui lòng thêm thư mục và đợi index.".to_string()
        }
    };

    let assistant_msg = ChatMessage {
        id: msg_id,
        role: "assistant".to_string(),
        content: response_content,
        citations: vec![],
        model: Some(request.model.clone()),
        timestamp: chrono::Utc::now().timestamp(),
    };

    // Save assistant message
    {
        let db = state.db.lock().await;
        let db_msg = crate::ai::memory::ChatMessage {
            id: assistant_msg.id.clone(),
            session_id: request.session_id.clone(),
            role: "assistant".to_string(),
            content: assistant_msg.content.clone(),
            citations: None,
            model: Some(request.model),
            timestamp: assistant_msg.timestamp,
        };
        let _ = ConversationMemory::save_message(&db.conn, &db_msg);
    }

    Ok(assistant_msg)
}

/// Lấy lịch sử chat của session
#[tauri::command]
pub async fn get_chat_history(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ChatMessage>, String> {
    let db = state.db.lock().await;

    let messages = ConversationMemory::get_history(&db.conn, &session_id)
        .map_err(|e| format!("Lỗi đọc lịch sử: {}", e))?;

    Ok(messages.into_iter().map(|m| ChatMessage {
        id: m.id,
        role: m.role,
        content: m.content,
        citations: vec![], // TODO: deserialize from JSON
        model: m.model,
        timestamp: m.timestamp,
    }).collect())
}

/// Xóa lịch sử chat
#[tauri::command]
pub async fn clear_chat(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    ConversationMemory::clear_session(&db.conn, &session_id)
        .map_err(|e| format!("Lỗi xóa chat: {}", e))?;
    log::info!("[Chat] Cleared session: {}", session_id);
    Ok(())
}

/// Danh sách sessions gần đây
#[tauri::command]
pub async fn list_chat_sessions(
    state: State<'_, AppState>,
) -> Result<Vec<crate::ai::memory::ChatSession>, String> {
    let db = state.db.lock().await;
    ConversationMemory::list_sessions(&db.conn, 50)
        .map_err(|e| format!("Lỗi đọc sessions: {}", e))
}

/// Xóa 1 session
#[tauri::command]
pub async fn delete_chat_session(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    ConversationMemory::clear_session(&db.conn, &session_id)
        .map_err(|e| format!("Lỗi xóa session: {}", e))?;
    log::info!("[Chat] Deleted session: {}", session_id);
    Ok(())
}
