export interface Settings {
  theme: "light" | "dark" | "system";
  fontSize: number;
  sidebarCollapsed: boolean;
  missionWorkspaceConfigured: boolean;
  missionIndexDone: boolean;
  pollIntervalSeconds: number;
  autoSkipKeywords: string[];
  reviewAutoEnabled: boolean;
  reviewConcurrency: number;
  reviewDepthMode: "economic" | "auto" | "deep";
  reviewDeepThreshold: number;
  reviewChunkSize: number;
  reviewParallelChunks: number;
  reviewMaxFiles: number;
  /** Lists lower-priority suggestions in a collapsed section of the review summary. */
  reviewSuggestionsEnabled: boolean;
  dismissStaleApprovals: boolean;
  autoIndexStaleRepos: boolean;
  /** Auto-post mode for reviews WITH findings: "off" | "comment" | "request_changes". */
  autoPostMode: "off" | "comment" | "request_changes";
  reviewAssignedOnly: boolean;
  reviewTargetFilterEnabled: boolean;
  reviewTargetLogins: string[];
  reviewTargetTeams: string[];
  autoApproveEnabled: boolean;
  autoApproveMaxRiskScore: number;
  indexStrategy: "graph";
  prSummaryCommentEnabled: boolean;
  reviewResultMode: "silent" | "result_comment";
  reviewArchiveRetentionDays: number;
}

export interface AiAccount {
  id: number;
  provider: "claude" | "codex" | "opencode";
  label: string;
  priority: number;
  enabled: boolean;
  status: "ok" | "limited" | "auth_failed";
  limitedUntil: number | null;
  lastError: string | null;
  lastUsedAt: number | null;
  createdAt: number;
  signedIn: boolean;
}

/** A model a provider can review with, and the reasoning efforts it takes. */
export interface EngineModel {
  id: string;
  label: string;
  efforts: string[];
  defaultEffort: string;
}

/** The model and effort a provider reviews with now, and its choices. */
export interface ReviewEngine {
  provider: AiAccount["provider"];
  name: string;
  model: string;
  effort: string;
  label: string;
  models: EngineModel[];
}

export interface UsageWindow {
  windowMins: number | null;
  usedPercent: number;
  /** Milliseconds since the epoch, or an ISO-8601 string. */
  resetsAt: number | string | null;
}

export interface AccountUsage {
  accountId: number;
  windows: UsageWindow[];
  error?: string;
}

export interface AiProvider {
  provider: AiAccount["provider"];
  name: string;
  installed: boolean;
  version: string | null;
  accounts: number;
  ready: number;
  label: string | null;
}

export interface PollStatus {
  pollIntervalSeconds: number;
  running: boolean;
  lastPollAt: number | null;
  nextPollAt: number | null;
  lastError: string | null;
  monitoredRepoCount: number;
}

export interface Workspace {
  id: number;
  login: string;
  kind: string;
  selected: boolean;
  repoCount: number;
  reviewSkillRepoCount: number;
  refreshedAt: number | null;
  monitorAll: boolean;
}

export interface Repo {
  id: number;
  workspaceId: number | null;
  owner: string;
  name: string;
  fullName: string;
  description: string | null;
  isPrivate: boolean;
  defaultBranch: string | null;
  skillPath: string | null;
  skillStatus: "unknown" | "found" | "missing" | "error";
  skillCheckedAt: number | null;
  skillError: string | null;
  subscriptionEnabled: boolean;
  reviewMode: "manual" | "automatic";
  skipDraftPrs: boolean;
  skipSelfPrs: boolean;
  pollError: string | null;
  pollErrorUrl: string | null;
  pollErrorAt: number | null;
}

/** `gh auth status` for the Settings GitHub-access panel. */
export interface GitHubAuthStatus {
  loggedIn: boolean;
  login: string | null;
  host: string;
  detail: string;
}

/** Exactly what a review decision will post to GitHub — for a confirm/cancel step. */
export interface SkillVersion {
  id: number;
  version: number;
  note: string | null;
  charCount: number;
  active: boolean;
  createdAt: number;
}
