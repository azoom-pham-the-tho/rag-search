use instant_distance::{Builder, Hnsw, Point, PointId, Search};
use std::path::Path;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Embedding vector — implements instant_distance::Point
#[derive(Clone, Debug)]
pub struct EmbeddingPoint {
    pub vector: Vec<f32>,
}

impl Point for EmbeddingPoint {
    fn distance(&self, other: &Self) -> f32 {
        // Cosine distance = 1 - cosine_similarity
        // instant-distance uses DISTANCE (smaller = closer), not similarity
        let dot: f32 = self
            .vector
            .iter()
            .zip(other.vector.iter())
            .map(|(a, b)| a * b)
            .sum();
        let norm_a: f32 = self.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = other.vector.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return 1.0; // Max distance
        }

        1.0 - (dot / (norm_a * norm_b))
    }
}

/// Metadata stored alongside each vector in the HNSW index
#[derive(Clone, Debug)]
pub struct VectorMeta {
    pub chunk_id: String,
    pub file_path: String,
    pub file_name: String,
    pub content: String,
    pub section: Option<String>,
}

/// HNSW-based vector index for fast semantic search
pub struct VectorIndex {
    /// The built HNSW graph — None if no vectors yet
    hnsw: RwLock<Option<Hnsw<EmbeddingPoint>>>,
    /// Mapping: PointId index → metadata index
    pid_to_idx: RwLock<Vec<PointId>>,
    /// Metadata for each point
    metadata: RwLock<Vec<VectorMeta>>,
    /// All embedding points (needed for rebuild)
    points: RwLock<Vec<EmbeddingPoint>>,
    /// Directory for persistence
    index_dir: std::path::PathBuf,
    /// Dirty flag — HNSW needs rebuild before next search
    dirty: AtomicBool,
}

/// Search result from vector index
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    pub chunk_id: String,
    pub file_path: String,
    pub file_name: String,
    pub content: String,
    pub section: Option<String>,
    pub distance: f32,   // Lower = more similar
    pub similarity: f32, // 1 - distance (higher = more similar)
}

impl VectorIndex {
    /// Create a new (empty) vector index
    pub fn new(index_dir: std::path::PathBuf) -> Self {
        std::fs::create_dir_all(&index_dir).ok();
        VectorIndex {
            hnsw: RwLock::new(None),
            pid_to_idx: RwLock::new(Vec::new()),
            metadata: RwLock::new(Vec::new()),
            points: RwLock::new(Vec::new()),
            index_dir,
            dirty: AtomicBool::new(false),
        }
    }

    /// Rebuild HNSW graph from current points
    fn rebuild_hnsw(points: &[EmbeddingPoint]) -> Option<(Hnsw<EmbeddingPoint>, Vec<PointId>)> {
        if points.is_empty() {
            return None;
        }
        let (hnsw, pids) = Builder::default().build_hnsw(points.to_vec());
        Some((hnsw, pids))
    }

    /// Add vectors + metadata in batch — LAZY rebuild (chỉ build khi search)
    /// Batch indexing 10 files × 10 chunks = 100 inserts → 0 rebuilds
    /// Rebuild xảy ra tự động trước lần search() đầu tiên
    pub fn add_vectors(&self, vectors: Vec<Vec<f32>>, metas: Vec<VectorMeta>) {
        if vectors.is_empty() {
            return;
        }

        let mut points_guard = self.points.write().unwrap();
        let mut meta_guard = self.metadata.write().unwrap();

        let added = vectors.len();
        for (vec, meta) in vectors.into_iter().zip(metas.into_iter()) {
            points_guard.push(EmbeddingPoint { vector: vec });
            meta_guard.push(meta);
        }

        // Mark dirty — rebuild sẽ xảy ra khi search() hoặc save()
        self.dirty.store(true, Ordering::Release);
        log::info!(
            "[VectorIndex] Added {} vectors (total: {}), HNSW rebuild deferred",
            added,
            points_guard.len()
        );
    }

    /// Ensure HNSW graph is up-to-date (rebuild if dirty)
    fn ensure_hnsw_built(&self) {
        if !self.dirty.load(Ordering::Acquire) {
            return;
        }

        let points_guard = self.points.read().unwrap();
        let start = std::time::Instant::now();
        if let Some((hnsw, pids)) = Self::rebuild_hnsw(&points_guard) {
            log::info!(
                "[VectorIndex] Lazy rebuild: {} points HNSW in {}ms",
                points_guard.len(),
                start.elapsed().as_millis()
            );
            *self.pid_to_idx.write().unwrap() = pids;
            *self.hnsw.write().unwrap() = Some(hnsw);
        }
        self.dirty.store(false, Ordering::Release);
    }

    /// Remove all vectors for a given file_path, then rebuild
    pub fn remove_file(&self, file_path: &str) {
        let mut points_guard = self.points.write().unwrap();
        let mut meta_guard = self.metadata.write().unwrap();

        // Find indices to remove (reverse order)
        let remove_indices: Vec<usize> = meta_guard
            .iter()
            .enumerate()
            .filter(|(_, m)| m.file_path == file_path)
            .map(|(i, _)| i)
            .collect();

        if remove_indices.is_empty() {
            return;
        }

        for &idx in remove_indices.iter().rev() {
            points_guard.swap_remove(idx);
            meta_guard.swap_remove(idx);
        }

        // Rebuild if points remain
        if points_guard.is_empty() {
            *self.hnsw.write().unwrap() = None;
            self.pid_to_idx.write().unwrap().clear();
        } else {
            if let Some((hnsw, pids)) = Self::rebuild_hnsw(&points_guard) {
                *self.pid_to_idx.write().unwrap() = pids;
                *self.hnsw.write().unwrap() = Some(hnsw);
            }
        }
    }

    /// Remove ALL vectors whose file_path starts with folder_path prefix
    /// Dùng khi reindex: xóa sạch vectors của folder dù file_tracking trống
    pub fn remove_files_in_folder(&self, folder_path: &str) -> usize {
        let folder_prefix = if folder_path.ends_with('/') {
            folder_path.to_string()
        } else {
            format!("{}/", folder_path)
        };

        let mut points_guard = self.points.write().unwrap();
        let mut meta_guard = self.metadata.write().unwrap();

        let before = meta_guard.len();

        // Collect indices to remove in reverse
        let remove_indices: Vec<usize> = meta_guard
            .iter()
            .enumerate()
            .filter(|(_, m)| m.file_path.starts_with(&folder_prefix) || m.file_path == folder_path)
            .map(|(i, _)| i)
            .collect();

        for &idx in remove_indices.iter().rev() {
            points_guard.swap_remove(idx);
            meta_guard.swap_remove(idx);
        }

        let removed = before - meta_guard.len();

        if removed > 0 {
            if points_guard.is_empty() {
                *self.hnsw.write().unwrap() = None;
                self.pid_to_idx.write().unwrap().clear();
            } else if let Some((hnsw, pids)) = Self::rebuild_hnsw(&points_guard) {
                *self.pid_to_idx.write().unwrap() = pids;
                *self.hnsw.write().unwrap() = Some(hnsw);
            }
        }

        removed
    }

    /// Search for nearest neighbors — auto-rebuild HNSW if dirty
    pub fn search(&self, query_vector: &[f32], top_n: usize) -> Vec<VectorSearchResult> {
        // Lazy rebuild: chỉ build HNSW khi cần search
        self.ensure_hnsw_built();
        let hnsw_guard = self.hnsw.read().unwrap();
        let hnsw = match hnsw_guard.as_ref() {
            Some(h) => h,
            None => return vec![],
        };

        let meta_guard = self.metadata.read().unwrap();
        let points_guard = self.points.read().unwrap();
        let query = EmbeddingPoint {
            vector: query_vector.to_vec(),
        };
        let mut search = Search::default();

        // instant-distance Item.pid maps to the index in the original points array
        // which matches our metadata array order
        let results: Vec<VectorSearchResult> = hnsw
            .search(&query, &mut search)
            .take(top_n)
            .filter_map(|item| {
                let idx = item.pid.into_inner() as usize;
                if idx < meta_guard.len() && idx < points_guard.len() {
                    let meta = &meta_guard[idx];
                    let distance = item.distance;
                    Some(VectorSearchResult {
                        chunk_id: meta.chunk_id.clone(),
                        file_path: meta.file_path.clone(),
                        file_name: meta.file_name.clone(),
                        content: meta.content.clone(),
                        section: meta.section.clone(),
                        distance,
                        similarity: 1.0 - distance.min(1.0),
                    })
                } else {
                    None
                }
            })
            .collect();

        results
    }

    /// Number of vectors in the index
    pub fn len(&self) -> usize {
        self.points.read().unwrap().len()
    }

    /// Get all chunks for a specific file (for chunk visualization UI)
    pub fn get_chunks_for_file(&self, file_path: &str) -> Vec<VectorMeta> {
        let meta_guard = self.metadata.read().unwrap();
        meta_guard
            .iter()
            .filter(|m| m.file_path == file_path)
            .cloned()
            .collect()
    }

    /// Save index to disk (binary format)
    pub fn save(&self) -> Result<(), String> {
        let points_guard = self.points.read().unwrap();
        let meta_guard = self.metadata.read().unwrap();

        if points_guard.is_empty() {
            return Ok(());
        }

        // Save points as flat f32 arrays
        let dim = if points_guard.is_empty() {
            0
        } else {
            points_guard[0].vector.len()
        };
        let points_path = self.index_dir.join("vectors.bin");
        let meta_path = self.index_dir.join("metadata.bin");

        // Serialize vectors: [dim: u32][count: u32][f32 * dim * count]
        let count = points_guard.len();
        let mut vec_data: Vec<u8> = Vec::with_capacity(8 + count * dim * 4);
        vec_data.extend_from_slice(&(dim as u32).to_le_bytes());
        vec_data.extend_from_slice(&(count as u32).to_le_bytes());
        for p in points_guard.iter() {
            for f in &p.vector {
                vec_data.extend_from_slice(&f.to_le_bytes());
            }
        }
        std::fs::write(&points_path, &vec_data).map_err(|e| format!("Save vectors: {}", e))?;

        // Serialize metadata as JSON lines (simple, human-readable)
        let meta_lines: Vec<String> = meta_guard
            .iter()
            .map(|m| {
                serde_json::json!({
                    "chunk_id": m.chunk_id,
                    "file_path": m.file_path,
                    "file_name": m.file_name,
                    "content_len": m.content.len(),
                    "content": m.content,
                    "section": m.section,
                })
                .to_string()
            })
            .collect();
        std::fs::write(&meta_path, meta_lines.join("\n"))
            .map_err(|e| format!("Save metadata: {}", e))?;

        log::info!(
            "[VectorIndex] Saved {} vectors + metadata to {:?}",
            count,
            self.index_dir
        );
        Ok(())
    }

    /// Load index from disk
    pub fn load(index_dir: &Path) -> Result<Self, String> {
        let points_path = index_dir.join("vectors.bin");
        let meta_path = index_dir.join("metadata.bin");

        if !points_path.exists() || !meta_path.exists() {
            return Ok(Self::new(index_dir.to_path_buf()));
        }

        let start = std::time::Instant::now();

        // Load vectors
        let vec_data = std::fs::read(&points_path).map_err(|e| format!("Read vectors: {}", e))?;
        if vec_data.len() < 8 {
            return Ok(Self::new(index_dir.to_path_buf()));
        }

        let dim = u32::from_le_bytes([vec_data[0], vec_data[1], vec_data[2], vec_data[3]]) as usize;
        let count =
            u32::from_le_bytes([vec_data[4], vec_data[5], vec_data[6], vec_data[7]]) as usize;

        let mut points = Vec::with_capacity(count);
        let mut offset = 8;
        for _ in 0..count {
            let mut vector = Vec::with_capacity(dim);
            for _ in 0..dim {
                if offset + 4 > vec_data.len() {
                    break;
                }
                let f = f32::from_le_bytes([
                    vec_data[offset],
                    vec_data[offset + 1],
                    vec_data[offset + 2],
                    vec_data[offset + 3],
                ]);
                vector.push(f);
                offset += 4;
            }
            points.push(EmbeddingPoint { vector });
        }

        // Load metadata
        let meta_text =
            std::fs::read_to_string(&meta_path).map_err(|e| format!("Read metadata: {}", e))?;
        let metadata: Vec<VectorMeta> = meta_text
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).ok()?;
                Some(VectorMeta {
                    chunk_id: v["chunk_id"].as_str()?.to_string(),
                    file_path: v["file_path"].as_str()?.to_string(),
                    file_name: v["file_name"].as_str()?.to_string(),
                    content: v["content"].as_str().unwrap_or("").to_string(),
                    section: v["section"].as_str().map(|s| s.to_string()),
                })
            })
            .collect();

        // Build HNSW
        let (hnsw, pids) = if points.is_empty() {
            (None, Vec::new())
        } else {
            let (h, p) = Builder::default().build_hnsw(points.clone());
            (Some(h), p)
        };

        log::info!(
            "[VectorIndex] Loaded {} vectors from disk in {}ms",
            points.len(),
            start.elapsed().as_millis()
        );

        Ok(VectorIndex {
            hnsw: RwLock::new(hnsw),
            pid_to_idx: RwLock::new(pids),
            metadata: RwLock::new(metadata),
            points: RwLock::new(points),
            index_dir: index_dir.to_path_buf(),
            dirty: AtomicBool::new(false),
        })
    }

    /// Clear all data
    pub fn clear(&self) {
        *self.hnsw.write().unwrap() = None;
        self.pid_to_idx.write().unwrap().clear();
        self.points.write().unwrap().clear();
        self.metadata.write().unwrap().clear();
        // Remove files
        let _ = std::fs::remove_file(self.index_dir.join("vectors.bin"));
        let _ = std::fs::remove_file(self.index_dir.join("metadata.bin"));
    }
}
