//! The Usage page: token usage, findings, time and chunk reuse of review runs over a window of
//! calendar days, by day, pull request, repository, model, provider and account.
//!
//! Runs are read once per request and aggregated here; a 90-day window holds at most a few
//! thousand runs. A run's tokens are the sum of its stages when it has any: the run's own columns
//! are written only when it finishes without being superseded, so they read 0 for runs that were
//! superseded, interrupted or are still running. Models, providers and accounts come from the
//! per-execution attempts (`review_run_usage_attempts`); runs from before those were recorded fall
//! back to `review_run_model_usage` for the model, and the rest of each run's tokens is reported as
//! unattributed, so every breakdown adds up to the total.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::*;

const DAY_MS: i64 = 86_400_000;
pub const USAGE_RANGES: [i64; 3] = [7, 30, 90];
const UNATTRIBUTED: &str = "unattributed";

/// The window and scope of a usage request.
#[derive(Debug, Clone, Copy)]
pub struct UsageFilter {
    /// Calendar days ending today: 7, 30 or 90.
    pub days: i64,
    /// The viewer's offset from UTC in minutes, east positive; days are the viewer's days.
    pub tz_minutes: i64,
    pub repo_id: Option<i64>,
    pub now: i64,
}

impl UsageFilter {
    fn offset_ms(&self) -> i64 {
        self.tz_minutes * 60_000
    }

    fn local_day(&self, at: i64) -> i64 {
        (at + self.offset_ms()).div_euclid(DAY_MS)
    }

    /// From local midnight `days - 1` days ago to now.
    pub fn window(&self) -> (i64, i64) {
        let first = self.local_day(self.now) - (self.days - 1);
        (first * DAY_MS - self.offset_ms(), self.now + 1)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageTokens {
    /// Input + output + cache creation. Cache reads are reported apart.
    pub tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

impl UsageTokens {
    fn new(input: i64, output: i64, cache_read: i64, cache_creation: i64) -> Self {
        Self {
            tokens: input + output + cache_creation,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
        }
    }

    fn add(&mut self, other: UsageTokens) {
        *self = Self::new(
            self.input_tokens + other.input_tokens,
            self.output_tokens + other.output_tokens,
            self.cache_read_tokens + other.cache_read_tokens,
            self.cache_creation_tokens + other.cache_creation_tokens,
        );
    }

    /// What `self` has beyond `other`, never below zero.
    fn beyond(self, other: UsageTokens) -> Self {
        Self::new(
            (self.input_tokens - other.input_tokens).max(0),
            (self.output_tokens - other.output_tokens).max(0),
            (self.cache_read_tokens - other.cache_read_tokens).max(0),
            (self.cache_creation_tokens - other.cache_creation_tokens).max(0),
        )
    }

    fn is_empty(&self) -> bool {
        self.tokens == 0 && self.cache_read_tokens == 0
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    #[serde(flatten)]
    pub usage: UsageTokens,
    pub runs: i64,
    pub prs: i64,
    /// Findings and questions in published reviews.
    pub findings: i64,
    pub questions: i64,
    /// Findings and questions the review kept, published or not.
    pub findings_found: i64,
    pub questions_found: i64,
    pub median_duration_ms: Option<i64>,
    pub reused_chunks: i64,
    pub chunks: i64,
    pub overhead_tokens: i64,
    pub runs_without_usage: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDay {
    pub day: String,
    pub tokens: i64,
    pub cache_read_tokens: i64,
    pub runs: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageGroupRow {
    pub key: String,
    pub label: String,
    pub provider: Option<String>,
    #[serde(flatten)]
    pub usage: UsageTokens,
    pub runs: i64,
    pub findings: Option<i64>,
    pub findings_found: Option<i64>,
    pub median_duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageRepoOption {
    pub id: i64,
    pub full_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub days: i64,
    pub from: i64,
    pub to: i64,
    pub totals: UsageTotals,
    pub by_day: Vec<UsageDay>,
    pub by_repo: Vec<UsageGroupRow>,
    pub by_model: Vec<UsageGroupRow>,
    pub by_account: Vec<UsageGroupRow>,
    pub by_provider: Vec<UsageGroupRow>,
    pub repos: Vec<UsageRepoOption>,
    pub has_running: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStripRun {
    pub run_id: i64,
    pub tokens: i64,
    pub outcome: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePrRow {
    pub pr_id: i64,
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub url: Option<String>,
    #[serde(flatten)]
    pub usage: UsageTokens,
    pub runs: i64,
    pub findings: i64,
    pub findings_found: i64,
    pub median_duration_ms: Option<i64>,
    pub last_run_at: i64,
    pub strip: Vec<UsageStripRun>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePrPage {
    pub items: Vec<UsagePrRow>,
    pub total: i64,
    pub next_offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRun {
    pub id: i64,
    pub outcome: &'static str,
    pub status: String,
    pub head_sha: String,
    pub started_at: i64,
    pub duration_ms: Option<i64>,
    #[serde(flatten)]
    pub usage: UsageTokens,
    pub findings: i64,
    pub questions: i64,
    pub findings_found: i64,
    pub questions_found: i64,
    pub models: Vec<String>,
    pub accounts: Vec<String>,
    pub reused_chunks: i64,
    pub chunks: i64,
    pub usage_reported: bool,
}

/// A run in the window, with what the report needs from it.
#[derive(Debug, Clone)]
struct RunRow {
    id: i64,
    pr_id: i64,
    repo_id: i64,
    repo: String,
    number: i64,
    title: String,
    url: Option<String>,
    status: String,
    head_sha: String,
    created_at: i64,
    outcome: &'static str,
    published: bool,
    usage: UsageTokens,
    usage_reported: bool,
    duration_ms: Option<i64>,
    findings: i64,
    questions: i64,
    findings_found: i64,
    questions_found: i64,
    chunks: i64,
    reused_chunks: i64,
}

impl RunRow {
    /// Superseded before it was published, failed or cancelled: tokens that bought no review.
    fn is_overhead(&self) -> bool {
        matches!(self.outcome, "failed" | "cancelled") || (self.outcome == "superseded" && !self.published)
    }
}

/// Tokens of one run attributed to one model and account.
#[derive(Debug, Clone)]
struct Attribution {
    run_id: i64,
    model: String,
    provider: Option<String>,
    account: Option<(i64, String)>,
    usage: UsageTokens,
}

/// What a run's status and publication mean on the Usage page.
pub(crate) fn run_outcome(status: &str, published: bool) -> &'static str {
    match status {
        "pending" | "queued" | "running" | "posting" | "cancelling" => "running",
        "posted" | "approved" | "resolved" => "posted",
        "superseded" => "superseded",
        "failed" => "failed",
        "cancelled" => "cancelled",
        "skipped" => "skipped",
        _ if published => "posted",
        _ => "preview",
    }
}

/// The provider behind a model name recorded before accounts were attributed. Gemini is no longer
/// offered, but its past runs keep their provider.
fn provider_of_model(model: &str) -> Option<&'static str> {
    if model.starts_with("claude") {
        Some("claude")
    } else if model.starts_with("gemini") {
        Some("gemini")
    } else if model == "codex" || model.starts_with("gpt-") {
        Some("codex")
    } else if model == "opencode" {
        Some("opencode")
    } else {
        None
    }
}

fn median(mut values: Vec<i64>) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2
    } else {
        values[middle]
    })
}

/// `YYYY-MM-DD` of a day counted from 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_date(day: i64) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn sorted_groups(groups: impl IntoIterator<Item = UsageGroupRow>) -> Vec<UsageGroupRow> {
    let mut rows: Vec<_> = groups
        .into_iter()
        .filter(|row| !row.usage.is_empty() || row.runs > 0)
        .collect();
    // Unattributed last, the rest by tokens.
    rows.sort_by(|a, b| {
        (a.key == UNATTRIBUTED)
            .cmp(&(b.key == UNATTRIBUTED))
            .then(b.usage.tokens.cmp(&a.usage.tokens))
            .then(a.label.cmp(&b.label))
    });
    rows
}

fn db_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl Database {
    /// Records what one execution of a stage spent, per model, on the account it ran on.
    pub fn insert_usage_attempts(
        &self,
        run_id: i64,
        pr_id: i64,
        stage: &str,
        account: &AiAccount,
        by_model: &[crate::review::ModelUsageEntry],
    ) -> Result<(), String> {
        let now = now_millis();
        for entry in by_model {
            self.conn
                .execute(
                    "INSERT INTO review_run_usage_attempts (run_id, pr_id, stage, account_id, account_label, provider, model,
                       input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                    params![
                        run_id,
                        pr_id,
                        stage,
                        account.id,
                        account.label,
                        account.provider,
                        entry.model,
                        entry.input_tokens,
                        entry.output_tokens,
                        entry.cache_read_tokens,
                        entry.cache_creation_tokens,
                        now,
                    ],
                )
                .map_err(db_error)?;
        }
        Ok(())
    }

    fn usage_runs(&self, filter: &UsageFilter, pr_id: Option<i64>) -> Result<Vec<RunRow>, String> {
        let (from, to) = filter.window();
        let mut statement = self
            .conn
            .prepare(
                "SELECT r.id, r.pr_id, p.repo_id, repos.full_name, p.number, p.title, p.url, r.status,
                        COALESCE(r.reviewed_head_sha, r.head_sha), r.created_at,
                        (r.status IN ('posted', 'approved', 'resolved') OR EXISTS (
                           SELECT 1 FROM review_results rr WHERE rr.run_id = r.id AND rr.posted_at IS NOT NULL)) AS published,
                        r.comments_json,
                        r.input_tokens, r.output_tokens, r.cache_read_tokens, r.cache_creation_tokens,
                        COALESCE(st.stages, 0), COALESCE(st.executed, 0),
                        COALESCE(st.input_tokens, 0), COALESCE(st.output_tokens, 0),
                        COALESCE(st.cache_read_tokens, 0), COALESCE(st.cache_creation_tokens, 0),
                        st.first_start, st.last_end, COALESCE(st.unfinished, 0),
                        COALESCE(st.chunks, 0), COALESCE(st.reused_chunks, 0),
                        (SELECT a.audit_json FROM review_finding_audits a WHERE a.run_id = r.id)
                 FROM review_runs r
                 JOIN pull_requests p ON p.id = r.pr_id
                 JOIN repos ON repos.id = p.repo_id
                 LEFT JOIN LATERAL (
                   SELECT COUNT(*) AS stages,
                          COUNT(*) FILTER (WHERE s.cache_hit = 0) AS executed,
                          SUM(s.input_tokens)::BIGINT AS input_tokens,
                          SUM(s.output_tokens)::BIGINT AS output_tokens,
                          SUM(s.cache_read_tokens)::BIGINT AS cache_read_tokens,
                          SUM(s.cache_creation_tokens)::BIGINT AS cache_creation_tokens,
                          MIN(s.created_at) AS first_start,
                          MAX(s.finished_at) AS last_end,
                          COUNT(*) FILTER (WHERE s.finished_at IS NULL) AS unfinished,
                          COUNT(DISTINCT s.chunk_id) FILTER (WHERE s.stage = 'chunk') AS chunks,
                          COUNT(DISTINCT s.chunk_id) FILTER (WHERE s.stage = 'chunk' AND s.cache_hit <> 0) AS reused_chunks
                   FROM review_run_stages s WHERE s.run_id = r.id
                 ) st ON true
                 WHERE r.created_at >= $1 AND r.created_at < $2
                   AND ($3::BIGINT IS NULL OR p.repo_id = $3)
                   AND ($4::BIGINT IS NULL OR r.pr_id = $4)
                 ORDER BY r.created_at, r.id",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![from, to, filter.repo_id, pr_id], |row| {
                let status: String = row.get(7)?;
                let published: bool = row.get(10)?;
                let comments_json: Option<String> = row.get(11)?;
                let stages: i64 = row.get(16)?;
                let executed: i64 = row.get(17)?;
                let usage = if stages > 0 {
                    UsageTokens::new(row.get(18)?, row.get(19)?, row.get(20)?, row.get(21)?)
                } else {
                    UsageTokens::new(row.get(12)?, row.get(13)?, row.get(14)?, row.get(15)?)
                };
                let outcome = run_outcome(&status, published);
                let first_start: Option<i64> = row.get(22)?;
                let last_end: Option<i64> = row.get(23)?;
                let unfinished: i64 = row.get(24)?;
                let duration_ms = match (first_start, last_end) {
                    (Some(start), Some(end)) if unfinished == 0 && outcome != "running" => Some((end - start).max(0)),
                    _ => None,
                };
                let audit_json: Option<String> = row.get(27)?;
                let selected = crate::review::findings::point_counts(comments_json.as_deref());
                let (findings, questions) = if published { selected } else { (0, 0) };
                // What the review kept, published or not: its finding audit when it has one,
                // otherwise the comments it selected.
                let (findings_found, questions_found) = audit_json
                    .as_deref()
                    .and_then(crate::review::findings::candidate_counts)
                    .unwrap_or(selected);
                Ok(RunRow {
                    id: row.get(0)?,
                    pr_id: row.get(1)?,
                    repo_id: row.get(2)?,
                    repo: row.get(3)?,
                    number: row.get(4)?,
                    title: row.get(5)?,
                    url: row.get(6)?,
                    head_sha: row.get(8)?,
                    created_at: row.get(9)?,
                    status,
                    outcome,
                    published,
                    // A run that executed stages yet reported nothing lost its usage; one that
                    // executed none (skipped, cancelled early, every chunk reused) truly spent 0.
                    usage_reported: !(usage.is_empty() && executed > 0 && outcome != "running"),
                    usage,
                    duration_ms,
                    findings,
                    questions,
                    findings_found,
                    questions_found,
                    chunks: row.get(25)?,
                    reused_chunks: row.get(26)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    /// Model and account attributions for `runs`, plus each run's unattributed remainder.
    fn usage_attributions(&self, runs: &[RunRow]) -> Result<Vec<Attribution>, String> {
        let ids: Vec<i64> = runs.iter().map(|run| run.id).collect();
        let mut attributions = Vec::new();
        let mut statement = self
            .conn
            .prepare(
                "SELECT run_id, model, provider, account_id, account_label,
                        SUM(input_tokens)::BIGINT, SUM(output_tokens)::BIGINT,
                        SUM(cache_read_tokens)::BIGINT, SUM(cache_creation_tokens)::BIGINT
                 FROM review_run_usage_attempts WHERE run_id = ANY($1)
                 GROUP BY run_id, model, provider, account_id, account_label
                 ORDER BY run_id, model, account_id",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![ids], |row| {
                let account_id: Option<i64> = row.get(3)?;
                let account_label: Option<String> = row.get(4)?;
                Ok(Attribution {
                    run_id: row.get(0)?,
                    model: row.get(1)?,
                    provider: row.get(2)?,
                    account: account_id.map(|id| (id, account_label.unwrap_or_else(|| format!("Account {id}")))),
                    usage: UsageTokens::new(row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?),
                })
            })
            .map_err(db_error)?;
        attributions.extend(rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?);
        let attempted: HashSet<i64> = attributions.iter().map(|row| row.run_id).collect();
        let legacy: Vec<i64> = ids.iter().copied().filter(|id| !attempted.contains(id)).collect();
        if !legacy.is_empty() {
            let mut statement = self
                .conn
                .prepare(
                    "SELECT run_id, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
                     FROM review_run_model_usage WHERE run_id = ANY($1) ORDER BY run_id, model",
                )
                .map_err(db_error)?;
            let rows = statement
                .query_map(params![legacy], |row| {
                    let model: String = row.get(1)?;
                    Ok(Attribution {
                        run_id: row.get(0)?,
                        provider: provider_of_model(&model).map(str::to_string),
                        model,
                        account: None,
                        usage: UsageTokens::new(row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?),
                    })
                })
                .map_err(db_error)?;
            attributions.extend(rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?);
        }
        Ok(attributions)
    }

    fn usage_repo_options(&self, filter: &UsageFilter) -> Result<Vec<UsageRepoOption>, String> {
        let (from, to) = filter.window();
        let mut statement = self
            .conn
            .prepare(
                "SELECT repos.id, repos.full_name FROM repos
                 WHERE repos.id = $3 OR EXISTS (
                   SELECT 1 FROM review_runs r JOIN pull_requests p ON p.id = r.pr_id
                   WHERE p.repo_id = repos.id AND r.created_at >= $1 AND r.created_at < $2)
                 ORDER BY repos.full_name",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![from, to, filter.repo_id], |row| {
                Ok(UsageRepoOption {
                    id: row.get(0)?,
                    full_name: row.get(1)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn usage_report(&self, filter: &UsageFilter) -> Result<UsageReport, String> {
        let runs = self.usage_runs(filter, None)?;
        let attributions = self.usage_attributions(&runs)?;
        let (from, to) = filter.window();

        let mut totals = UsageTotals::default();
        let mut durations = Vec::new();
        let mut prs = HashSet::new();
        for run in &runs {
            totals.usage.add(run.usage);
            totals.runs += 1;
            totals.findings += run.findings;
            totals.questions += run.questions;
            totals.findings_found += run.findings_found;
            totals.questions_found += run.questions_found;
            totals.chunks += run.chunks;
            totals.reused_chunks += run.reused_chunks;
            if run.is_overhead() {
                totals.overhead_tokens += run.usage.tokens;
            }
            if !run.usage_reported {
                totals.runs_without_usage += 1;
            }
            durations.extend(run.duration_ms);
            prs.insert(run.pr_id);
        }
        totals.prs = prs.len() as i64;
        totals.median_duration_ms = median(durations);

        let first_day = filter.local_day(from);
        let last_day = filter.local_day(filter.now);
        let mut by_day: BTreeMap<i64, UsageDay> = (first_day..=last_day)
            .map(|day| {
                (
                    day,
                    UsageDay {
                        day: civil_date(day),
                        tokens: 0,
                        cache_read_tokens: 0,
                        runs: 0,
                    },
                )
            })
            .collect();
        for run in &runs {
            if let Some(day) = by_day.get_mut(&filter.local_day(run.created_at)) {
                day.tokens += run.usage.tokens;
                day.cache_read_tokens += run.usage.cache_read_tokens;
                day.runs += 1;
            }
        }

        let mut repos: BTreeMap<i64, (String, UsageTokens, i64, i64, i64, Vec<i64>)> = BTreeMap::new();
        for run in &runs {
            let entry = repos
                .entry(run.repo_id)
                .or_insert_with(|| (run.repo.clone(), UsageTokens::default(), 0, 0, 0, Vec::new()));
            entry.1.add(run.usage);
            entry.2 += 1;
            entry.3 += run.findings;
            entry.4 += run.findings_found;
            entry.5.extend(run.duration_ms);
        }
        let by_repo = sorted_groups(repos.into_iter().map(
            |(id, (label, usage, count, findings, found, durations))| UsageGroupRow {
                key: id.to_string(),
                label,
                provider: None,
                usage,
                runs: count,
                findings: Some(findings),
                findings_found: Some(found),
                median_duration_ms: median(durations),
            },
        ));

        let by_model = attributed_groups(&runs, &attributions, |row| {
            Some((row.model.clone(), row.model.clone(), row.provider.clone()))
        });
        let by_provider = attributed_groups(&runs, &attributions, |row| {
            row.provider
                .clone()
                .map(|provider| (provider.clone(), provider.clone(), Some(provider)))
        });
        let by_account = attributed_groups(&runs, &attributions, |row| {
            row.account
                .as_ref()
                .map(|(id, label)| (id.to_string(), label.clone(), row.provider.clone()))
        });

        Ok(UsageReport {
            days: filter.days,
            from,
            to,
            has_running: runs.iter().any(|run| run.outcome == "running"),
            totals,
            by_day: by_day.into_values().collect(),
            by_repo,
            by_model,
            by_account,
            by_provider,
            repos: self.usage_repo_options(filter)?,
        })
    }

    pub fn usage_prs(&self, filter: &UsageFilter, offset: i64, limit: i64) -> Result<UsagePrPage, String> {
        let runs = self.usage_runs(filter, None)?;
        let mut grouped: HashMap<i64, Vec<&RunRow>> = HashMap::new();
        for run in &runs {
            grouped.entry(run.pr_id).or_default().push(run);
        }
        let mut items: Vec<UsagePrRow> = grouped
            .into_values()
            .map(|runs| {
                let first = runs[0];
                let mut usage = UsageTokens::default();
                for run in &runs {
                    usage.add(run.usage);
                }
                UsagePrRow {
                    pr_id: first.pr_id,
                    repo: first.repo.clone(),
                    number: first.number,
                    title: first.title.clone(),
                    url: first.url.clone(),
                    usage,
                    runs: runs.len() as i64,
                    findings: runs.iter().map(|run| run.findings).sum(),
                    findings_found: runs.iter().map(|run| run.findings_found).sum(),
                    median_duration_ms: median(runs.iter().filter_map(|run| run.duration_ms).collect()),
                    last_run_at: runs.iter().map(|run| run.created_at).max().unwrap_or_default(),
                    strip: runs
                        .iter()
                        .map(|run| UsageStripRun {
                            run_id: run.id,
                            tokens: run.usage.tokens,
                            outcome: run.outcome,
                        })
                        .collect(),
                }
            })
            .collect();
        items.sort_by(|a, b| {
            b.usage
                .tokens
                .cmp(&a.usage.tokens)
                .then(b.last_run_at.cmp(&a.last_run_at))
                .then(a.pr_id.cmp(&b.pr_id))
        });
        let total = items.len() as i64;
        let offset = offset.clamp(0, total);
        let end = (offset + limit.max(1)).min(total);
        Ok(UsagePrPage {
            items: items.drain(offset as usize..end as usize).collect(),
            total,
            next_offset: (end < total).then_some(end),
        })
    }

    /// The runs of one pull request in the window, newest first.
    pub fn pr_usage_runs(&self, pr_id: i64, filter: &UsageFilter) -> Result<Vec<UsageRun>, String> {
        let filter = UsageFilter {
            repo_id: None,
            ..*filter
        };
        let runs = self.usage_runs(&filter, Some(pr_id))?;
        let attributions = self.usage_attributions(&runs)?;
        let mut items: Vec<UsageRun> = runs
            .into_iter()
            .map(|run| {
                let mine = attributions.iter().filter(|row| row.run_id == run.id);
                let mut models: Vec<String> = mine.clone().map(|row| row.model.clone()).collect();
                let mut accounts: Vec<String> = mine
                    .filter_map(|row| row.account.as_ref().map(|(_, label)| label.clone()))
                    .collect();
                accounts.sort();
                accounts.dedup();
                models.sort();
                models.dedup();
                UsageRun {
                    id: run.id,
                    outcome: run.outcome,
                    status: run.status,
                    head_sha: run.head_sha,
                    started_at: run.created_at,
                    duration_ms: run.duration_ms,
                    usage: run.usage,
                    findings: run.findings,
                    questions: run.questions,
                    findings_found: run.findings_found,
                    questions_found: run.questions_found,
                    models,
                    accounts,
                    reused_chunks: run.reused_chunks,
                    chunks: run.chunks,
                    usage_reported: run.usage_reported,
                }
            })
            .collect();
        items.reverse();
        Ok(items)
    }
}

/// Groups attributed tokens by `key_of` (key, label, provider); tokens with no group, including each
/// run's remainder beyond its attributions, go to one unattributed row.
fn attributed_groups(
    runs: &[RunRow],
    attributions: &[Attribution],
    key_of: impl Fn(&Attribution) -> Option<(String, String, Option<String>)>,
) -> Vec<UsageGroupRow> {
    let mut groups: BTreeMap<String, (String, Option<String>, UsageTokens, HashSet<i64>)> = BTreeMap::new();
    let mut unattributed = (UsageTokens::default(), HashSet::new());
    let mut attributed_per_run: HashMap<i64, UsageTokens> = HashMap::new();
    for row in attributions {
        attributed_per_run.entry(row.run_id).or_default().add(row.usage);
        match key_of(row) {
            Some((key, label, provider)) => {
                let entry = groups
                    .entry(key)
                    .or_insert_with(|| (label, provider, UsageTokens::default(), HashSet::new()));
                entry.2.add(row.usage);
                entry.3.insert(row.run_id);
            }
            None => {
                unattributed.0.add(row.usage);
                unattributed.1.insert(row.run_id);
            }
        }
    }
    for run in runs {
        let rest = run
            .usage
            .beyond(attributed_per_run.get(&run.id).copied().unwrap_or_default());
        if !rest.is_empty() {
            unattributed.0.add(rest);
            unattributed.1.insert(run.id);
        }
    }
    let mut rows: Vec<UsageGroupRow> = groups
        .into_iter()
        .map(|(key, (label, provider, usage, runs))| UsageGroupRow {
            key,
            label,
            provider,
            usage,
            runs: runs.len() as i64,
            findings: None,
            findings_found: None,
            median_duration_ms: None,
        })
        .collect();
    rows.push(UsageGroupRow {
        key: UNATTRIBUTED.into(),
        label: "Unattributed".into(),
        provider: None,
        usage: unattributed.0,
        runs: unattributed.1.len() as i64,
        findings: None,
        findings_found: None,
        median_duration_ms: None,
    });
    sorted_groups(rows)
}

#[cfg(test)]
#[path = "usage_report_tests.rs"]
mod tests;
