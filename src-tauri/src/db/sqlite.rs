use super::DbError;
use rusqlite::Connection;

/// Folder CRUD operations
pub mod folders {
    use super::*;

    pub fn insert(conn: &Connection, id: &str, path: &str, name: &str) -> Result<(), DbError> {
        conn.execute(
            "INSERT OR IGNORE INTO folders (id, path, name) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, path, name],
        )?;
        Ok(())
    }

    pub fn delete(conn: &Connection, id: &str) -> Result<(), DbError> {
        conn.execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> Result<Vec<(String, String, String)>, DbError> {
        let mut stmt = conn.prepare(
            "SELECT id, path, name FROM folders WHERE is_active = 1 ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    #[allow(dead_code)]
    pub fn get_path(conn: &Connection, id: &str) -> Result<Option<String>, DbError> {
        let result = conn.query_row(
            "SELECT path FROM folders WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        );
        match result {
            Ok(path) => Ok(Some(path)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(DbError::SqliteError(e)),
        }
    }
}

/// File tracking operations
pub mod file_tracking {
    use super::*;

    pub fn upsert(
        conn: &Connection,
        file_path: &str,
        folder_id: &str,
        file_hash: &str,
        file_modified: i64,
        file_size: i64,
        chunk_count: usize,
        has_vectors: bool,
    ) -> Result<(), DbError> {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT OR REPLACE INTO file_tracking
             (file_path, folder_id, file_hash, file_modified, file_size, last_indexed, chunk_count, status, has_vectors)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'indexed', ?8)",
            rusqlite::params![file_path, folder_id, file_hash, file_modified, file_size, now, chunk_count as i64, has_vectors as i64],
        )?;
        Ok(())
    }

    /// Update has_vectors flag for a file (after successful embedding)
    pub fn update_has_vectors(
        conn: &Connection,
        file_path: &str,
        has_vectors: bool,
    ) -> Result<(), DbError> {
        conn.execute(
            "UPDATE file_tracking SET has_vectors = ?1 WHERE file_path = ?2",
            rusqlite::params![has_vectors as i64, file_path],
        )?;
        Ok(())
    }

    /// Get files that are indexed but missing vectors (for resume embedding)
    pub fn get_files_without_vectors(
        conn: &Connection,
        folder_id: &str,
    ) -> Result<Vec<String>, DbError> {
        let mut stmt = conn.prepare(
            "SELECT file_path FROM file_tracking WHERE folder_id = ?1 AND has_vectors = 0 AND status = 'indexed'"
        )?;
        let paths = stmt
            .query_map(rusqlite::params![folder_id], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(paths)
    }

    /// Get ALL files without vectors across all folders (for startup auto-retry)
    pub fn get_all_files_without_vectors(
        conn: &Connection,
    ) -> Result<Vec<String>, DbError> {
        let mut stmt = conn.prepare(
            "SELECT file_path FROM file_tracking WHERE has_vectors = 0 AND status = 'indexed'"
        )?;
        let paths = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(paths)
    }

    pub fn delete_by_path(conn: &Connection, file_path: &str) -> Result<(), DbError> {
        conn.execute(
            "DELETE FROM file_tracking WHERE file_path = ?1",
            rusqlite::params![file_path],
        )?;
        Ok(())
    }

    pub fn delete_by_folder(conn: &Connection, folder_id: &str) -> Result<(), DbError> {
        conn.execute(
            "DELETE FROM file_tracking WHERE folder_id = ?1",
            rusqlite::params![folder_id],
        )?;
        Ok(())
    }

    /// Get all tracked files as (file_path, last_modified_timestamp)
    pub fn get_known_files(
        conn: &Connection,
        folder_id: &str,
    ) -> Result<std::collections::HashMap<String, i64>, DbError> {
        let mut stmt = conn
            .prepare("SELECT file_path, file_modified FROM file_tracking WHERE folder_id = ?1")?;
        let map = stmt
            .query_map(rusqlite::params![folder_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(map)
    }

    /// Count files by status
    pub fn count_by_status(conn: &Connection) -> Result<(usize, usize, usize), DbError> {
        let total: usize =
            conn.query_row("SELECT COUNT(*) FROM file_tracking", [], |row| row.get(0))?;
        let indexed: usize = conn.query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status = 'indexed'",
            [],
            |row| row.get(0),
        )?;
        let errors: usize = conn.query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status = 'error'",
            [],
            |row| row.get(0),
        )?;
        Ok((total, indexed, errors))
    }

    /// Count files for a specific folder
    pub fn count_by_folder(conn: &Connection, folder_id: &str) -> Result<usize, DbError> {
        let count: usize = conn.query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE folder_id = ?1",
            rusqlite::params![folder_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    pub fn clear_all_tracking(conn: &Connection) -> Result<(), DbError> {
        conn.execute("DELETE FROM file_tracking", [])?;
        conn.execute("DELETE FROM embeddings", [])?;
        Ok(())
    }

    /// List all files in a folder with details
    pub fn list_files_by_folder(
        conn: &Connection,
        folder_id: &str,
    ) -> Result<Vec<(String, i64, i64, String)>, DbError> {
        // Returns: (file_path, file_size, chunk_count, status)
        let mut stmt = conn.prepare(
            "SELECT file_path, file_size, chunk_count, status 
             FROM file_tracking 
             WHERE folder_id = ?1 
             ORDER BY file_path ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }
}

/// Settings key-value store
pub mod settings {
    use super::*;

    pub fn get(conn: &Connection, key: &str) -> Result<Option<String>, DbError> {
        let result = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get(0),
        );
        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(DbError::SqliteError(e)),
        }
    }

    pub fn set(conn: &Connection, key: &str, value: &str) -> Result<(), DbError> {
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    pub fn delete(conn: &Connection, key: &str) -> Result<(), DbError> {
        conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            rusqlite::params![key],
        )?;
        Ok(())
    }
}
