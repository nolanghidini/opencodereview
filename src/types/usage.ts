/** Range options the Usage page offers, in calendar days ending today. */
export type UsageRange = 7 | 30 | 90;

/**
 * How a run ended, from its status and publication. `running` covers queued, running and posting
 * runs; `preview` is a finished run waiting for a decision.
 */
export type UsageOutcome = "running" | "preview" | "posted" | "superseded" | "failed" | "cancelled" | "skipped";

/** Token counts. `tokens` = input + output + cache creation; cache reads are reported apart. */
interface UsageTokens {
  tokens: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
}

export interface UsageTotals extends UsageTokens {
  runs: number;
  prs: number;
  /** Findings in published reviews. */
  findings: number;
  /** Questions in published reviews. */
  questions: number;
  /** Findings and questions the reviews kept, published or not. */
  findingsFound: number;
  questionsFound: number;
  /** Median execution time of finished runs (first stage start to last stage end). */
  medianDurationMs: number | null;
  /** Chunks whose earlier result was reused, and all chunks reviewed. */
  reusedChunks: number;
  chunks: number;
  /** Tokens spent on runs that were superseded, failed or cancelled. */
  overheadTokens: number;
  /** Finished runs that reported no token usage. */
  runsWithoutUsage: number;
}

export interface UsageDay {
  /** Local calendar day, `YYYY-MM-DD`. */
  day: string;
  tokens: number;
  cacheReadTokens: number;
  runs: number;
}

/** One row of the Repositories, Models or Accounts breakdown. */
export interface UsageGroupRow extends UsageTokens {
  /** Stable key; `unattributed` for tokens no repository, model or account accounts for. */
  key: string;
  label: string;
  /** Provider id (`claude`, `codex` or `opencode`) for model and account rows. */
  provider: string | null;
  runs: number;
  /** Repositories only: findings published, and kept whether published or not. */
  findings: number | null;
  findingsFound: number | null;
  medianDurationMs: number | null;
}

export interface UsageRepoOption {
  id: number;
  fullName: string;
}

export interface UsageReport {
  days: UsageRange;
  /** Window start (local midnight) and end, epoch milliseconds. */
  from: number;
  to: number;
  totals: UsageTotals;
  byDay: UsageDay[];
  byRepo: UsageGroupRow[];
  byModel: UsageGroupRow[];
  byAccount: UsageGroupRow[];
  byProvider: UsageGroupRow[];
  /** Repositories with runs in the window, for the filter. */
  repos: UsageRepoOption[];
  /** A run in the window is still in progress. */
  hasRunning: boolean;
}

export interface UsageStripRun {
  runId: number;
  tokens: number;
  outcome: UsageOutcome;
}

export interface UsagePrRow extends UsageTokens {
  prId: number;
  repo: string;
  number: number;
  title: string;
  url: string | null;
  runs: number;
  findings: number;
  findingsFound: number;
  medianDurationMs: number | null;
  lastRunAt: number;
  /** The PR's runs in the window, oldest first. */
  strip: UsageStripRun[];
}

export interface UsagePrPage {
  items: UsagePrRow[];
  total: number;
  nextOffset: number | null;
}

export interface UsageRun extends UsageTokens {
  id: number;
  outcome: UsageOutcome;
  status: string;
  headSha: string;
  startedAt: number;
  durationMs: number | null;
  findings: number;
  questions: number;
  findingsFound: number;
  questionsFound: number;
  models: string[];
  accounts: string[];
  /** Chunks reused from an earlier run, of all chunks. */
  reusedChunks: number;
  chunks: number;
  /** False when the run finished without reporting usage; show "—", not 0. */
  usageReported: boolean;
}

export interface UsageQuery {
  days: UsageRange;
  repoId: number | null;
  /** Minutes east of UTC (`-new Date().getTimezoneOffset()`). */
  tz: number;
}
