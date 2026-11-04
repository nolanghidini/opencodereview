//! The ReviewBench agent contract (github.com/review-bench/ReviewBench, `AGENT_CONTRACT.md`):
//! the pull request is mounted at `/work/repo` and `/work/pr`, the findings go to `RB_OUT`. The
//! AACR-Bench and Martian adapters in `bench/adapters` mount their pull requests the same way.
//!
//! Exit codes follow the contract: a setup failure (credential, usage limit, runtime) exits non-zero
//! so the benchmark retries it; a review that fails validation or runs out of time writes no
//! findings and exits 0, with the reason in `ocr-findings.json` and on stderr.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;

use super::export::{self, Finding};
use super::{Engine, PullRequest};

/// Findings written to `RB_OUT`; the contract warns against unbounded output.
const MAX_FINDINGS: usize = 50;
const DEFAULT_DEADLINE_SECS: u64 = 25 * 60;
const DEFAULT_MAX_FILES: i64 = 800;
const DEFAULT_FOCUS_FILES: i64 = 100;

#[derive(Serialize)]
struct Output<'a> {
    pr: OutputPr<'a>,
    agent: &'a str,
    findings: Vec<OutputFinding<'a>>,
}

#[derive(Serialize)]
struct OutputPr<'a> {
    repo: String,
    pr_number: i64,
    base: &'a str,
    head: &'a str,
}

#[derive(Serialize)]
struct OutputFinding<'a> {
    file: &'a str,
    start_line: i64,
    end_line: i64,
    message: String,
    producer: &'a str,
}

pub fn run_from_env() -> Result<(), String> {
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let required = |name: &str| env(name).ok_or_else(|| format!("{name} is required"));
    let out = PathBuf::from(required("RB_OUT")?);
    let agent = env("RB_AGENT").unwrap_or_else(|| "opencodereview".into());
    let repo = PathBuf::from(env("RB_REPO").unwrap_or_else(|| "/work/repo".into()));
    let pr_json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(env("RB_PR_JSON").unwrap_or_else(|| "/work/pr/pr.json".into()))
            .map_err(|_| "RB_PR_JSON is unreadable")?,
    )
    .map_err(|_| "RB_PR_JSON is not JSON")?;
    let pr = PullRequest {
        nwo: required("RB_NWO")?,
        number: required("RB_PR_NUMBER")?
            .parse()
            .map_err(|_| "RB_PR_NUMBER must be a number")?,
        base: sha(&required("RB_BASE")?)?,
        head: sha(&required("RB_HEAD")?)?,
        title: pr_json["title"].as_str().unwrap_or_default().to_string(),
        body: pr_json["body"].as_str().unwrap_or_default().to_string(),
    };
    let engine = Engine {
        provider: env("RB_CONFIG_PROVIDER").unwrap_or_else(|| "claude".into()),
        model: env("RB_CONFIG_MODEL"),
        effort: env("RB_CONFIG_EFFORT"),
    };
    if !matches!(engine.provider.as_str(), "claude" | "codex" | "opencode") {
        return Err(format!("Unsupported RB_CONFIG_PROVIDER {}", engine.provider));
    }
    let deadline = Duration::from_secs(
        env("RB_CONFIG_DEADLINE_SECS")
            .or_else(|| env("OCR_BENCH_DEADLINE_SECS"))
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| "The review deadline must be a number of seconds")
            })
            .transpose()?
            .unwrap_or(DEFAULT_DEADLINE_SECS),
    );
    let credential = super::take_credential(&engine.provider)?;
    let db = crate::db::Db::from_env()?;
    super::seed_account(&db, &engine, &credential)?;
    drop(credential);
    // Settings → Review → maximum files. A pull request the product declines scores nothing here.
    // Above the focus limit, the highest-priority files are reviewed and the rest are listed, so
    // a very large pull request still finishes inside the benchmark's time limit. The lens pass
    // adds a second look per stage at what a first pass usually leaves out. Suggestions follow the
    // product default, off, unless asked for.
    for (variable, key, default) in [
        ("RB_CONFIG_MAX_FILES", "review_max_files", DEFAULT_MAX_FILES.to_string()),
        (
            "RB_CONFIG_FOCUS_FILES",
            "review_focus_files",
            DEFAULT_FOCUS_FILES.to_string(),
        ),
        ("RB_CONFIG_LENS_PASS", "review_lens_pass", "true".to_string()),
        (
            "RB_CONFIG_SUGGESTIONS",
            "review_suggestions_enabled",
            "false".to_string(),
        ),
    ] {
        let value = env(variable).unwrap_or(default);
        db.open()?
            .save_setting(key, &value)
            .map_err(|error| format!("{variable}: {error}"))?;
    }
    let label = super::engine_label(&db, &engine.provider)?;
    // The contract reports any configuration label whose value never appears in the output.
    println!("provider={} engine={label}", engine.provider);
    for (key, value) in std::env::vars().filter(|(key, _)| key.starts_with("RB_CONFIG_")) {
        println!("{}={value}", key.trim_start_matches("RB_CONFIG_").to_ascii_lowercase());
    }
    let diff = match env("RB_DIFF").and_then(|path| std::fs::read_to_string(path).ok()) {
        Some(diff) => diff,
        None => git_diff(&repo, &pr)?,
    };
    let archive_dir = PathBuf::from(env("OCR_DATA_DIR").unwrap_or_else(|| "/ocr/data".into())).join("archives");
    let started = Instant::now();
    // Optional work stops early enough that the deadline rarely cancels a review; when it does,
    // the chunks that finished still count.
    let budget = crate::review::budget::Budget::new(deadline.saturating_sub(Duration::from_secs(120)));
    let result = super::review(&db, &repo, &pr, archive_dir, deadline, budget.clone());
    let elapsed = started.elapsed().as_millis() as i64;
    let (status, error, points, usage, engine_label, summary) = match result {
        Ok(reviewed) => {
            let mut points = match reviewed.output.comments_json.as_deref() {
                Some(raw) => export::points(raw, &diff)?,
                None => Vec::new(),
            };
            points.extend(export::suggestions(&reviewed.output.body));
            let usage = &reviewed.output.usage;
            (
                "ok",
                None,
                points,
                json!({
                    "inputTokens": usage.input_tokens,
                    "outputTokens": usage.output_tokens,
                    "cacheReadTokens": usage.cache_read_tokens,
                    "cacheCreationTokens": usage.cache_creation_tokens,
                }),
                reviewed.engine,
                reviewed.output.body,
            )
        }
        Err(error) if super::is_setup_failure(&error) => return Err(error),
        Err(error) if error == "cancelled" => {
            let points = match super::finished_chunk_comments(&budget) {
                Some(raw) => export::points(&raw, &diff)?,
                None => Vec::new(),
            };
            eprintln!(
                "ocr-bench: review deadline reached, reporting the {} findings its finished chunks validated",
                points.len()
            );
            ("timeout", Some(error), points, json!(null), label, String::new())
        }
        Err(error) => {
            eprintln!("ocr-bench: review failed, reporting no findings: {error}");
            ("failed", Some(error), Vec::new(), json!(null), label, String::new())
        }
    };
    let (findings, questions): (Vec<&Finding>, Vec<&Finding>) =
        points.iter().partition(|point| point.kind != "question");
    let export = json!({
        "schema": "ocr-bench/v1",
        "pr": {"nwo": pr.nwo, "number": pr.number, "base": pr.base, "head": pr.head},
        "engine": {
            "label": engine_label,
            "engineVersion": crate::sandbox::engine_version(),
            "promptVersion": crate::review::REVIEW_PROMPT_VERSION,
        },
        "status": status,
        "error": error,
        "durationMs": elapsed,
        "usage": usage,
        "summary": summary,
        "findings": findings,
        "questions": questions,
    });
    let directory = out.parent().ok_or("RB_OUT has no directory")?;
    write_atomic(
        &directory.join("ocr-findings.json"),
        &serde_json::to_vec_pretty(&export).map_err(|error| error.to_string())?,
    )?;
    let output = Output {
        pr: OutputPr {
            repo: format!("https://github.com/{}", pr.nwo),
            pr_number: pr.number,
            base: &pr.base,
            head: &pr.head,
        },
        agent: &agent,
        findings: findings
            .iter()
            .take(MAX_FINDINGS)
            .map(|finding| OutputFinding {
                file: &finding.path,
                start_line: finding.start_line,
                end_line: finding.end_line,
                message: finding.message(),
                producer: &agent,
            })
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&output).map_err(|error| error.to_string())?;
    // The contract fails a pull request on malformed output; check it before leaving.
    serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| error.to_string())?;
    write_atomic(&out, &bytes)?;
    eprintln!(
        "ocr-bench: {status}: {} findings, {} questions in {}s",
        findings.len(),
        questions.len(),
        elapsed / 1000
    );
    Ok(())
}

fn sha(value: &str) -> Result<String, String> {
    if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err("RB_BASE and RB_HEAD must be full 40-character commit SHAs".into())
    }
}

fn git_diff(repo: &Path, pr: &PullRequest) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            &format!("{}...{}", pr.base, pr.head),
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("Could not diff the pull request in the local clone".into());
    }
    // As the product reads a pull request diff: a file in another encoding never fails the review.
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Writes `path` readable by the benchmark driver, which reads the mount as another user.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let temporary = path.with_extension("partial");
    std::fs::write(&temporary, bytes)
        .and_then(|_| std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o644)))
        .and_then(|_| std::fs::rename(&temporary, path))
        .map_err(|error| format!("Could not write {}: {error}", path.display()))
}
