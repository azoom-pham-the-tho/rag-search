pub mod sqlite;

use rusqlite::Connection;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("Lỗi database: {0}")]
    SqliteError(#[from] rusqlite::Error),
    #[error("Lỗi IO: {0}")]
    IoError(#[from] std::io::Error),
}

/// Database chính của ứng dụng
pub struct Database {
    pub conn: Connection,
}

impl Database {
    /// Tạo hoặc mở database tại đường dẫn file trực tiếp
    /// VD: Database::new("/path/to/ragsearch.db")
    pub fn new(db_path: PathBuf) -> Result<Self, DbError> {
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;

        // Enable WAL mode for better concurrent read/write
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        let db = Database { conn };
        db.init_tables()?;
        Ok(db)
    }

    /// Tạo tất cả tables cần thiết
    fn init_tables(&self) -> Result<(), DbError> {
        self.conn.execute_batch(
            "
            -- Quản lý folders
            CREATE TABLE IF NOT EXISTS folders (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                is_active INTEGER NOT NULL DEFAULT 1
            );

            -- Theo dõi files đã index
            CREATE TABLE IF NOT EXISTS file_tracking (
                file_path TEXT PRIMARY KEY,
                folder_id TEXT NOT NULL,
                file_hash TEXT NOT NULL,
                file_modified INTEGER,
                file_size INTEGER,
                last_indexed INTEGER NOT NULL,
                chunk_count INTEGER DEFAULT 0,
                status TEXT DEFAULT 'indexed',
                FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE CASCADE
            );

            -- Vector embeddings
            CREATE TABLE IF NOT EXISTS embeddings (
                chunk_id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                embedding BLOB NOT NULL,
                model_name TEXT NOT NULL DEFAULT 'all-MiniLM-L6-v2-int8',
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                FOREIGN KEY (file_path) REFERENCES file_tracking(file_path) ON DELETE CASCADE
            );

            -- Chat history
            CREATE TABLE IF NOT EXISTS chat_messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                citations TEXT,
                model TEXT,
                timestamp INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            );

            -- Chat sessions
            CREATE TABLE IF NOT EXISTS chat_sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            );

            -- App settings (key-value store)
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- Indexes for performance
            CREATE INDEX IF NOT EXISTS idx_file_tracking_folder
                ON file_tracking(folder_id);
            CREATE INDEX IF NOT EXISTS idx_file_tracking_status
                ON file_tracking(status);
            CREATE INDEX IF NOT EXISTS idx_embeddings_file
                ON embeddings(file_path);
            CREATE INDEX IF NOT EXISTS idx_chat_messages_session
                ON chat_messages(session_id);
            ",
        )?;

        // Migrations — safe to run multiple times
        self.run_migrations()?;

        Ok(())
    }

    /// Run database migrations (ALTER TABLE is idempotent via IF NOT EXISTS workaround)
    fn run_migrations(&self) -> Result<(), DbError> {
        // Migration: add has_vectors to file_tracking (0 = not embedded, 1 = has vectors)
        let has_col: bool = self
            .conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('file_tracking') WHERE name='has_vectors'",
            )?
            .query_row([], |row| row.get::<_, i64>(0))
            .unwrap_or(0)
            > 0;

        if !has_col {
            self.conn.execute_batch(
                "ALTER TABLE file_tracking ADD COLUMN has_vectors INTEGER DEFAULT 0;",
            )?;
            log::info!("[DB] Migration: added has_vectors column to file_tracking");
        }

        Ok(())
    }
}
