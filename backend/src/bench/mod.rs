//! `ocr-bench`: the headless reviewer of the public benchmark image (`docker/Dockerfile.bench`).
//! It reviews one pull request from a local clone with the product's review pipeline, against a
//! throwaway Postgres and one sandboxed runner in the same container, and writes the findings in
//! the benchmark's format. It has no HTTP API and never talks to GitHub.

mod export;
mod reviewbench;

pub use reviewbench::run_from_env as run_reviewbench;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::db::Db;
use crate::review;

/// The pull request under review, pinned to exact commits.
pub(crate) struct PullRequest {
    pub nwo: String,
    pub number: i64,
    pub base: String,
    pub head: String,
    pub title: String,
    pub body: String,
}

/// The provider a run uses, and the model and effort when the benchmark configuration names them
/// (the product defaults otherwise).
pub(crate) struct Engine {
    pub provider: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub(crate) struct Reviewed {
    pub output: review::ReviewOutput,
    /// `provider/model/effort` as the review recorded it.
    pub engine: String,
}

/// Where a benchmark mounts the credential file when the environment names none.
const DEFAULT_CREDENTIAL_FILE: &str = "/run/ocr-bench/credential";

/// The one credential of a run, read from the environment before any thread starts and removed
/// from it, so no process the reviewer spawns inherits it: the file `OCR_BENCH_CREDENTIAL_FILE`
/// names (or `/run/ocr-bench/credential`), `OCR_BENCH_CREDENTIAL`, or for Claude
/// `CLAUDE_CODE_OAUTH_TOKEN`. The value is the provider's sign-in file contents, or a Claude
/// sign-in token.
pub(crate) fn take_credential(provider: &str) -> Result<String, String> {
    let path = std::env::var("OCR_BENCH_CREDENTIAL_FILE")
        .ok()
        .filter(|path| !path.is_empty())
        .or_else(|| {
            Path::new(DEFAULT_CREDENTIAL_FILE)
                .is_file()
                .then(|| DEFAULT_CREDENTIAL_FILE.into())
        });
    let from_file = path
        .map(|path| std::fs::read_to_string(path).map_err(|_| "The credential file is unreadable".to_string()))
        .transpose()?;
    let inline = std::env::var("OCR_BENCH_CREDENTIAL").ok();
    let claude = std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok();
    for name in [
        "OCR_BENCH_CREDENTIAL_FILE",
        "OCR_BENCH_CREDENTIAL",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        std::env::remove_var(name);
    }
    from_file
        .or(inline)
        .or(claude.filter(|_| provider == "claude"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("No {provider} credential: set OCR_BENCH_CREDENTIAL_FILE or OCR_BENCH_CREDENTIAL"))
}

/// Registers the run's only account, so a review can never move to another provider.
pub(crate) fn seed_account(db: &Db, engine: &Engine, credential: &str) -> Result<(), String> {
    let database = db.open()?;
    let account = database.create_ai_account(&engine.provider, "Benchmark")?;
    match engine.provider.as_str() {
        "claude" => crate::accounts::store_claude_token(account.id, credential)?,
        "codex" => crate::accounts::store_codex_auth(account.id, credential)?,
        "opencode" => crate::accounts::store_opencode_auth(account.id, credential)?,
        _ => return Err("Unsupported provider".into()),
    }
    if engine.model.is_some() || engine.effort.is_some() {
        let (model, effort) = database.review_engine(&engine.provider)?;
        database.save_review_engine(
            &engine.provider,
            engine.model.as_deref().unwrap_or(&model),
            engine.effort.as_deref().unwrap_or(&effort),
        )?;
    }
    Ok(())
}

/// The `provider/model/effort` label reviews on `provider` run with.
pub(crate) fn engine_label(db: &Db, provider: &str) -> Result<String, String> {
    let (model, effort) = db.open()?.review_engine(provider)?;
    Ok(crate::providers::engines::label(provider, &model, &effort))
}

/// Reviews `pr` from the clone at `repo` with the product pipeline, cancelling after `deadline`.
/// `budget` ends before the deadline and keeps what finished chunks validated.
pub(crate) fn review(
    db: &Db,
    repo: &Path,
    pr: &PullRequest,
    archive_dir: PathBuf,
    deadline: Duration,
    budget: Arc<review::budget::Budget>,
) -> Result<Reviewed, String> {
    let database = db.open()?;
    let conn = db.connection()?;
    let now = crate::sandbox::now();
    let (owner, name) = pr
        .nwo
        .split_once('/')
        .filter(|(owner, name)| {
            [owner, name].iter().all(|part| {
                !part.is_empty()
                    && !part.starts_with('.')
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            })
        })
        .ok_or("RB_NWO must be owner/repo")?;
    let repo_id: i64 = conn
        .query_row(
            "INSERT INTO repos(owner, name, full_name, created_at, updated_at) VALUES($1, $2, $3, $4, $4) RETURNING id",
            crate::pg_params![owner, name, pr.nwo, now],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let pr_id: i64 = conn
        .query_row(
            "INSERT INTO pull_requests(repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at)
             VALUES($1, $2, $3, 'benchmark', $4, $5, $5, $5) RETURNING id",
            crate::pg_params![repo_id, pr.number, pr.title, pr.head, now],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let provider = database
        .pick_ai_account(&[])?
        .ok_or("The benchmark account is missing")?
        .provider;
    let engine = engine_label(db, &provider)?;
    let run_id: i64 = conn
        .query_row(
            "INSERT INTO review_runs(pr_id, head_sha, status, created_at, updated_at, started_at, model)
             VALUES($1, $2, 'running', $3, $3, $3, $4) RETURNING id",
            crate::pg_params![pr_id, pr.head, now, engine],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    drop(conn);
    let settings = database.settings()?;
    // The pipeline expects the server's clone layout, `<root>/<owner>/<name>`.
    let link = archive_dir.with_file_name("repos").join(&pr.nwo);
    std::fs::create_dir_all(link.parent().ok_or("Invalid repository location")?).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(repo, &link).map_err(|error| format!("Could not link the clone: {error}"))?;
    let repo = link.as_path();
    let inputs = review::ReviewInputs {
        frozen: None,
        run_id,
        repo_id,
        pr_id,
        repo_path: repo.to_path_buf(),
        full_name: pr.nwo.clone(),
        number: pr.number,
        base_ref: pr.base.clone(),
        fixed_head_sha: Some(pr.head.clone()),
        offline: true,
        expected_scope: None,
        prior_scope: None,
        last_reviewed_sha: None,
        force_full: true,
        reprocess: false,
        skill_content: database.active_skill_content(),
        unresolved_review_head: None,
        unresolved_review_body: None,
        pr_title: pr.title.clone(),
        pr_body: pr.body.clone(),
        model: crate::providers::claude::MODEL.to_string(),
        review_depth_mode: settings.review_depth_mode.clone(),
        review_deep_threshold: settings.review_deep_threshold.max(2) as usize,
        review_chunk_size: settings.review_chunk_size.clamp(2, 20) as usize,
        // One chunk per runner. A runner inside this container is the only one there, because
        // its process check covers the whole container; runners in containers of their own
        // (`OCR_REVIEW_RUNNER_SOCKETS` lists them) take chunks side by side, as Compose's do.
        review_parallel_chunks: runner_count().clamp(1, 20),
        db_handle: Some(db.clone()),
        archive_dir,
        budget: Some(budget),
    };
    drop(database);
    let cancel = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    {
        let (cancel, finished) = (cancel.clone(), finished.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            while !finished.load(Ordering::Relaxed) {
                if started.elapsed() > deadline {
                    eprintln!(
                        "ocr-bench: review deadline of {}s reached; cancelling",
                        deadline.as_secs()
                    );
                    cancel.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    }
    let mut last = String::new();
    let mut progress = |_phase: &str, tail: &str| {
        let headline = tail.lines().next().unwrap_or_default();
        if !headline.is_empty() && headline != last {
            eprintln!("ocr-bench: {headline}");
            last = headline.to_string();
        }
    };
    let result = review::execute(&inputs, &cancel, &mut progress);
    finished.store(true, Ordering::Relaxed);
    let engine = db
        .open()
        .ok()
        .and_then(|db| db.review_run_by_id(run_id).ok().flatten())
        .and_then(|run| run.model)
        .unwrap_or(engine);
    Ok(Reviewed {
        output: result?,
        engine,
    })
}

/// Whether a failed review is the run's setup (sign-in, usage limits), which the benchmark should
/// retry, rather than this pull request's review, which it scores as finding nothing.
pub(crate) fn is_setup_failure(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "sign in",
        "signed out",
        "usage limit",
        "connect an ai account",
        "no account is ready",
        "isolation runtime",
        "runner socket",
        "postgres",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}

/// How many runners `OCR_REVIEW_RUNNER_SOCKETS` names.
fn runner_count() -> usize {
    std::env::var("OCR_REVIEW_RUNNER_SOCKETS")
        .map(|sockets| sockets.split(',').filter(|socket| !socket.trim().is_empty()).count())
        .unwrap_or(1)
}

/// The comments the finished chunks of a cancelled review validated, merged as one
/// `{"comments": [...]}` without repeats: what a review the deadline stopped still found.
pub(crate) fn finished_chunk_comments(budget: &review::budget::Budget) -> Option<String> {
    let mut seen = std::collections::BTreeSet::new();
    let comments = budget
        .finished_chunks()
        .iter()
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .flat_map(|value| value["comments"].as_array().cloned().unwrap_or_default())
        .filter(|comment| {
            seen.insert(
                serde_json::json!([comment["path"], comment["line"], comment["side"], comment["body"]]).to_string(),
            )
        })
        .collect::<Vec<_>>();
    (!comments.is_empty()).then(|| serde_json::json!({ "comments": comments }).to_string())
}
