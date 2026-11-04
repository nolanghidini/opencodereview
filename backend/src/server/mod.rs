use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Condvar, Mutex,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::db::{Database, Db, Repo, ReviewResult, ReviewResultInput};
use crate::state::{IndexProgressEvent, ReviewJob};
use crate::{github, indexer, local, review};

mod comment_activation;
mod http;
mod routes;
mod runtime;
mod stream;
use comment_activation::*;

mod accounts;
mod claude_auth;
mod codex_auth;
mod indexing;
mod opencode_auth;
mod polling;
mod publication;
mod queue_acknowledgements;
mod response;
mod review_http;
mod review_jobs;
mod status_comments;
mod summary;
mod usage_http;
mod workspace;

pub(crate) use accounts::*;
pub(crate) use claude_auth::*;
pub(crate) use codex_auth::*;
pub(crate) use http::*;
pub(crate) use indexing::*;
pub(crate) use opencode_auth::*;
pub(crate) use polling::*;
pub(crate) use publication::*;
pub(crate) use queue_acknowledgements::*;
pub(crate) use response::*;
pub(crate) use review_http::*;
pub(crate) use review_jobs::*;
pub(crate) use routes::*;
pub(crate) use runtime::*;
pub(crate) use status_comments::*;
pub(crate) use stream::*;
pub(crate) use summary::*;
pub(crate) use usage_http::*;
pub(crate) use workspace::*;
#[cfg(test)]
mod progress_tests;

const OCR_REPOSITORY_URL: &str = "https://github.com/BridgLabs/opencodereview";

/// Closing line of every visible OpenCodeReview comment. Reviews post from the
/// user's own GitHub account, so each one says where it came from.
fn ocr_footer() -> String {
    format!("<sub>Reviewed by [OpenCodeReview]({OCR_REPOSITORY_URL}).</sub>")
}

#[derive(Clone)]
pub struct ServerConfig {
    pub bind: String,
    pub db: Db,
    /// Server state on disk: repositories, review archives, sign-ins.
    pub data_dir: PathBuf,
    pub repos_dir: PathBuf,
    pub auto_reindex: bool,
    pub poll_interval_seconds: u64,
    pub api_token: Option<String>,
}

impl ServerConfig {
    pub fn archive_dir(&self) -> PathBuf {
        crate::review_archive::archive_dir(&self.data_dir)
    }

    /// Connects to Postgres (and migrates it); call before any thread starts.
    pub fn from_env() -> Result<Self, String> {
        let data_dir = std::env::var("OCR_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data"));
        Ok(Self {
            bind: std::env::var("OCR_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string()),
            db: Db::from_env()?,
            repos_dir: std::env::var("OCR_REPOS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| data_dir.join("repos")),
            auto_reindex: std::env::var("OCR_AUTO_REINDEX")
                .map(|value| value != "false" && value != "0")
                .unwrap_or(true),
            poll_interval_seconds: std::env::var("OCR_POLL_INTERVAL_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(300),
            api_token: std::env::var("OCR_API_TOKEN")
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| resolve_persisted_token(&data_dir)),
            data_dir,
        })
    }
}

pub(crate) struct ServerState {
    pub(crate) config: ServerConfig,
    index_jobs: Arc<Mutex<HashMap<i64, Arc<AtomicBool>>>>,
    index_progress: Arc<Mutex<HashMap<i64, IndexProgressEvent>>>,
    review_jobs: Arc<Mutex<HashMap<i64, ReviewJob>>>,
    /// Reviewable file counts of pull requests over the file limit, per repository and number,
    /// for the head and base they were counted at.
    reviewable_file_counts: Mutex<HashMap<(i64, i64), ((String, Option<String>), FileListing)>>,
    github_auth_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    github_auth_events: Arc<Mutex<Vec<serde_json::Value>>>,
    github_auth_flow_secret: Arc<Mutex<Option<String>>>,
    poll_running: Arc<AtomicBool>,
    review_sse_connections: AtomicUsize,
    pub(crate) archive_leases: crate::review_archive::retention::ArchiveLeaseTracker,
    pub(crate) archive_storage_status: Arc<Mutex<crate::review_archive::retention::ArchiveStorageStatus>>,
    pub(crate) retention_worker_notify: Arc<(Mutex<bool>, Condvar)>,
}

pub(crate) type SharedState = Arc<ServerState>;

impl ServerState {
    pub(crate) fn new(config: ServerConfig) -> Self {
        Self {
            config,
            index_jobs: Arc::new(Mutex::new(HashMap::new())),
            index_progress: Arc::new(Mutex::new(HashMap::new())),
            review_jobs: Arc::new(Mutex::new(HashMap::new())),
            reviewable_file_counts: Mutex::new(HashMap::new()),
            github_auth_cancel: Arc::new(Mutex::new(None)),
            github_auth_events: Arc::new(Mutex::new(Vec::new())),
            github_auth_flow_secret: Arc::new(Mutex::new(None)),
            poll_running: Arc::new(AtomicBool::new(false)),
            review_sse_connections: AtomicUsize::new(0),
            archive_leases: crate::review_archive::retention::ArchiveLeaseTracker::new(),
            archive_storage_status: Arc::new(Mutex::new(
                crate::review_archive::retention::ArchiveStorageStatus::default(),
            )),
            retention_worker_notify: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HealthResponse {
    ok: bool,
    message: String,
    repos_dir: String,
    github: ToolStatus,
    git: ToolStatus,
    claude: ToolStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolStatus {
    ok: bool,
    detail: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SelectWorkspacesRequest {
    logins: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthStartRequest {
    action: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SaveSettingRequest {
    key: String,
    value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubscribeRepoRequest {
    enabled: bool,
    skip_draft_prs: Option<bool>,
    skip_self_prs: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartReviewRequest {
    force_full: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DecideReviewRequest {
    decision: String,
    run_id: Option<i64>,
    selection_revision: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FindingSelectionRequest {
    revision: i64,
    selected_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateSkillVersionRequest {
    content: String,
    note: Option<String>,
}

/// Runs archive maintenance now instead of at its next hourly pass.
pub(crate) fn wake_retention_worker(state: &SharedState) {
    let (lock, cvar) = &*state.retention_worker_notify;
    if let Ok(mut wake) = lock.lock() {
        *wake = true;
        cvar.notify_one();
    }
}

fn resolve_persisted_token(data_dir: &Path) -> Option<String> {
    let token_path = data_dir.join("api-token");
    if let Ok(existing) = std::fs::read_to_string(&token_path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return Some(existing);
        }
    }
    let mut file = std::fs::File::open("/dev/urandom").ok()?;
    let mut bytes = [0u8; 32];
    Read::read_exact(&mut file, &mut bytes).ok()?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let _ = std::fs::create_dir_all(data_dir);
    if let Err(err) = std::fs::write(&token_path, &token) {
        eprintln!("ERROR: could not persist API token to {}: {err}", token_path.display());
        return None;
    }
    Some(token)
}

pub fn run_from_env() -> Result<(), String> {
    run(ServerConfig::from_env()?)
}

pub fn run(mut config: ServerConfig) -> Result<(), String> {
    std::fs::create_dir_all(&config.repos_dir).map_err(|error| error.to_string())?;
    let instance = Instance::new()?;
    config.db = config.db.with_instance(&instance.id);
    let db = config.db.open()?;
    db.register_instance(&instance.id, &instance.host)?;
    // Only work whose instance is gone: other instances sharing the database keep theirs.
    db.reconcile_interrupted_jobs()?;
    db.reap_snapshot_indexes()?;
    crate::accounts::remove_unsupported(&db)?;
    crate::accounts::adopt_legacy_token(&db)?;
    drop(db);
    cleanup_runtime_artifacts();

    let state = Arc::new(ServerState::new(config.clone()));
    if config.auto_reindex {
        start_reindex_worker(state.clone());
    }
    start_instance_heartbeat(config.db.clone(), instance);
    start_orphan_reaper(config.db.clone(), config.repos_dir.clone());
    start_review_queue_drain(state.clone());
    start_untracked_run_sweeper(state.clone());
    start_review_result_reconciliation(config.db.clone(), config.repos_dir.clone());
    start_review_request_restoration(config.db.clone());
    start_poll_scheduler(state.clone());
    start_runtime_cleanup_worker();
    crate::review_archive::retention::start_archive_retention_worker(state.clone());

    if config.api_token.is_none() {
        eprintln!("WARNING: OCR_API_TOKEN is not set — all authenticated endpoints will reject requests");
    }

    let listener =
        TcpListener::bind(&config.bind).map_err(|error| format!("could not bind {}: {error}", config.bind))?;
    if config.api_token.is_some() {
        println!("API authentication enabled");
    } else {
        return Err(
            "API token required: set OCR_API_TOKEN or ensure the data directory is writable for auto-generation"
                .to_string(),
        );
    }
    println!("OpenCodeReview server listening on http://{}", config.bind);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = state.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_stream(stream, state) {
                        eprintln!("request failed: {error}");
                    }
                });
            }
            Err(error) => eprintln!("connection failed: {error}"),
        }
    }
    Ok(())
}

pub(crate) struct Request {
    method: String,
    path: String,
    /// The raw query string, without `?`.
    query: String,
    origin: Option<String>,
    authorization: Option<String>,
    content_type: Option<String>,
    flow_secret: Option<String>,
    body: Vec<u8>,
}

pub(crate) struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

#[cfg(test)]
mod tests;
