#![allow(dead_code)]
pub mod handler;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::path::PathBuf;
// use std::sync::Arc; // Removed unused import
use thiserror::Error;
use tokio::sync::mpsc;

#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum WatcherError {
    #[error("Watcher error: {0}")]
    NotifyError(#[from] notify::Error),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum FileEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

/// OS-native file watcher
#[allow(dead_code)]
pub struct FolderWatcher {
    watcher: notify::RecommendedWatcher,
    watched_paths: Vec<PathBuf>,
}

#[allow(dead_code)]
impl FolderWatcher {
    /// Tạo watcher mới, trả về (watcher, receiver channel)
    pub fn new() -> Result<(Self, mpsc::UnboundedReceiver<FileEvent>), WatcherError> {
        let (tx, rx) = mpsc::unbounded_channel();

        let supported_extensions: Vec<&str> =
            vec!["txt", "md", "pdf", "docx", "xlsx", "xls", "pptx", "svg"];

        let watcher = notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
            match result {
                Ok(event) => {
                    // Filter only supported file types
                    for path in &event.paths {
                        let ext = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_lowercase())
                            .unwrap_or_default();

                        if !supported_extensions.contains(&ext.as_str()) {
                            continue;
                        }

                        let file_event = match event.kind {
                            EventKind::Create(_) => Some(FileEvent::Created(path.clone())),
                            EventKind::Modify(_) => Some(FileEvent::Modified(path.clone())),
                            EventKind::Remove(_) => Some(FileEvent::Deleted(path.clone())),
                            _ => None,
                        };

                        if let Some(fe) = file_event {
                            let _ = tx.send(fe);
                        }
                    }
                }
                Err(e) => {
                    log::error!("Watcher error: {}", e);
                }
            }
        })?;

        Ok((
            FolderWatcher {
                watcher,
                watched_paths: Vec::new(),
            },
            rx,
        ))
    }

    /// Thêm folder để theo dõi
    pub fn watch_folder(&mut self, path: PathBuf) -> Result<(), WatcherError> {
        self.watcher.watch(&path, RecursiveMode::Recursive)?;
        self.watched_paths.push(path);
        Ok(())
    }

    /// Ngừng theo dõi folder
    pub fn unwatch_folder(&mut self, path: &PathBuf) -> Result<(), WatcherError> {
        self.watcher.unwatch(path)?;
        self.watched_paths.retain(|p| p != path);
        Ok(())
    }

    /// Danh sách folders đang watch
    pub fn watched_folders(&self) -> &[PathBuf] {
        &self.watched_paths
    }
}

/// Startup diff — phát hiện files thay đổi khi app tắt
/// So sánh file_modified trong DB với file thực trên disk
#[allow(dead_code)]
pub fn detect_changed_files(
    folder_path: &PathBuf,
    known_files: &std::collections::HashMap<String, i64>, // path → last_modified timestamp
) -> Vec<FileEvent> {
    let mut events = Vec::new();

    let supported_extensions: Vec<&str> =
        vec!["txt", "md", "pdf", "docx", "xlsx", "xls", "pptx", "svg"];

    // Walk directory
    if let Ok(entries) = walkdir(folder_path, &supported_extensions) {
        let current_files: std::collections::HashSet<String> =
            entries.iter().map(|(p, _)| p.clone()).collect();

        // Check new or modified files
        for (path, modified) in &entries {
            match known_files.get(path) {
                None => {
                    // New file
                    events.push(FileEvent::Created(PathBuf::from(path)));
                }
                Some(last_known) => {
                    if *modified > *last_known {
                        // Modified since last index
                        events.push(FileEvent::Modified(PathBuf::from(path)));
                    }
                }
            }
        }

        // Check deleted files
        for known_path in known_files.keys() {
            if !current_files.contains(known_path) {
                events.push(FileEvent::Deleted(PathBuf::from(known_path)));
            }
        }
    }

    events
}

/// Walk directory recursively, return (path, modified_timestamp) for supported files
#[allow(dead_code)]
fn walkdir(dir: &PathBuf, extensions: &[&str]) -> Result<Vec<(String, i64)>, std::io::Error> {
    let mut results = Vec::new();

    fn visit(
        dir: &PathBuf,
        extensions: &[&str],
        results: &mut Vec<(String, i64)>,
    ) -> Result<(), std::io::Error> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Skip hidden directories
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with('.'))
                    .unwrap_or(false)
                {
                    continue;
                }
                visit(&path, extensions, results)?;
            } else if path.is_file() {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();

                if extensions.contains(&ext.as_str()) {
                    let modified = path
                        .metadata()?
                        .modified()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);

                    results.push((path.to_string_lossy().to_string(), modified));
                }
            }
        }
        Ok(())
    }

    visit(dir, extensions, &mut results)?;
    Ok(results)
}
