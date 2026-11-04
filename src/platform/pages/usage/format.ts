import type { Tone } from "../../../components/DesignSystem";
import type { AiAccount, UsageGroupRow, UsageOutcome } from "../../../types";
import { formatError } from "../../lib/utils";

const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

const trimmed = (value: number, digits: number) => String(Number(value.toFixed(digits)));

/** Token counts at a glance: `887`, `887.3K`, `12.42M`, `1.2B`. */
export function compactTokens(tokens: number) {
  const n = Math.max(0, Math.round(tokens));
  if (n >= 999_995_000) return `${trimmed(n / 1e9, 2)}B`;
  if (n >= 999_950) return `${trimmed(n / 1e6, 2)}M`;
  if (n >= 1000) return `${trimmed(n / 1e3, 1)}K`;
  return String(n);
}

/** `part` as a share of `whole`; `0%` when there is nothing to divide. */
export function pct(part: number, whole: number, digits = 0) {
  if (!(whole > 0)) return "0%";
  return `${trimmed((part / whole) * 100, digits)}%`;
}

/** Axis label for a local `YYYY-MM-DD` day, such as `OCT 25`. */
export function dayAxisLabel(day: string) {
  const [, month, date] = day.split("-").map(Number);
  return month >= 1 && month <= 12 && date ? `${MONTHS[month - 1]} ${date}` : day;
}

/** Tooltip label for a local `YYYY-MM-DD` day, such as `Oct 25, 2026`. */
export function dayLabel(day: string) {
  const [year, month, date] = day.split("-").map(Number);
  if (!(month >= 1 && month <= 12 && date)) return day;
  const name = MONTHS[month - 1];
  return `${name[0]}${name.slice(1).toLowerCase()} ${date}, ${year}`;
}

/** Minutes east of UTC, as the API expects in `tz`. */
export const timezoneOffset = (now = new Date()) => -now.getTimezoneOffset();

const PROVIDERS: string[] = ["claude", "codex", "opencode"] satisfies AiAccount["provider"][];
export const isProvider = (value: string | null): value is AiAccount["provider"] =>
  value !== null && PROVIDERS.includes(value);

/** Rows the API could attribute, without the `unattributed` remainder. */
export const attributedRows = (rows: UsageGroupRow[]) => rows.filter((row) => row.key !== "unattributed");

export const UNATTRIBUTED_HINT =
  "Runs from before OpenCodeReview tracked usage per account and model. Their tokens still count in the totals.";

export const outcomeMeta: Record<UsageOutcome, { label: string; tone: Tone; segment: string }> = {
  running: { label: "Running", tone: "brand", segment: "bg-brand/40" },
  preview: { label: "Preview", tone: "warning", segment: "bg-amber-500" },
  posted: { label: "Posted", tone: "success", segment: "bg-brand" },
  superseded: { label: "Superseded", tone: "neutral", segment: "bg-surface-sunken" },
  failed: { label: "Failed", tone: "danger", segment: "bg-rose-500" },
  cancelled: { label: "Cancelled", tone: "neutral", segment: "bg-slate-400" },
  skipped: { label: "Skipped", tone: "neutral", segment: "bg-slate-400" },
};

/** Maps an old server's missing routes to the Docker fix, like `reviewError`. */
export function usageError(cause: unknown) {
  const message = formatError(cause);
  return message.includes("route not found")
    ? "Usage is not available in the running server. Rebuild the Docker backend (docker compose up --build server)."
    : message;
}
