use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: String,
    pub source_path: String,
    pub output_path: String,
    pub preset: String,
    pub status: String,
    pub original_size: Option<i64>,
    pub converted_size: Option<i64>,
    pub kept_file: Option<String>,
    pub space_saved: Option<i64>,
    pub error_message: Option<String>,
    pub queue_order: i32,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub preset: String,
    pub cleanup_mode: String,
    pub launch_at_login: bool,
    pub handbrake_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySummary {
    pub total_saved_bytes: i64,
    pub total_files: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryPage {
    pub jobs: Vec<JobInfo>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderScanResult {
    pub file_count: usize,
    pub folder_name: String,
    pub folder_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedPaths {
    pub files: Vec<String>,
    pub folders: Vec<FolderScanResult>,
}
