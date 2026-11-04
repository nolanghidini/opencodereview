import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, Check } from "lucide-react";
import { useState } from "react";
import { ClaudeSignIn, ClaudeTokenSignIn, CodexSignIn, OpencodeKeySignIn } from "../../components/AccessConnect";
import { cx } from "../../components/DesignSystem";
import type { AiAccount, UsageWindow } from "../../types";
import {
  accountHealth,
  accountStatusText,
  ProviderGlyph,
  percentLeft,
  providerName,
  relative,
  usageQuery,
} from "./AccountsPanel";

type Provider = AiAccount["provider"];

/** When the account has plan left again: the latest reset among its used-up windows, if any is ahead. */
function usedUpUntil(windows: UsageWindow[], now = Date.now()) {
  const resets = windows
    .filter((window) => percentLeft(window) === 0)
    .map((window) => (window.resetsAt === null ? NaN : new Date(window.resetsAt).getTime()))
    .filter((at) => Number.isFinite(at) && at > now);
  return resets.length ? Math.max(...resets) : null;
}

/** What a row says about an account: how much is left beside its name, or, under it, why it
 * can't take reviews now. The full status is in the row's tooltip. */
function accountNote(account: AiAccount, windows: UsageWindow[], left: number | null, now = Date.now()) {
  const health = accountHealth(account, now);
  if (health === "signed-out") return "Signed out";
  if (health === "resting") return `Limit reached · resets in ${relative((account.limitedUntil ?? now) - now)}`;
  const until = usedUpUntil(windows, now);
  const amount = until === null ? (left === null ? null : `${left}% left`) : `Resets in ${relative(until - now)}`;
  if (health === "paused") return amount === null ? "Paused" : `Paused · ${amount}`;
  return amount ?? "";
}

/** The provider's signed-in accounts; choosing one makes new reviews start on it. */
export function AccountChoices({
  accounts,
  nextId,
  busy,
  onChoose,
}: {
  accounts: AiAccount[];
  nextId: number;
  busy: boolean;
  onChoose: (account: AiAccount) => void;
}) {
  const usage = useQuery(usageQuery).data;
  return (
    <div className="flex flex-col px-2 pb-1" role="radiogroup" aria-label="Account">
      {accounts.map((account) => {
        const selected = account.id === nextId;
        const health = accountHealth(account);
        const windows = usage?.find((item) => item.accountId === account.id)?.windows ?? [];
        const left = windows.length ? Math.min(...windows.map(percentLeft)) : null;
        const unavailable = health === "signed-out" || health === "resting";
        return (
          // biome-ignore lint/a11y/useSemanticElements: radio rows styled as a list; a native radio cannot hold this layout
          <button
            key={account.id}
            type="button"
            role="radio"
            aria-checked={selected}
            title={accountStatusText(account)}
            disabled={busy || unavailable}
            className="flex min-h-10 w-full flex-none cursor-pointer items-center gap-2.5 rounded-lg px-2 py-1.5 text-left text-small text-fg transition-colors hover:bg-surface-muted/60 disabled:cursor-not-allowed disabled:opacity-60"
            onClick={() => !selected && onChoose(account)}
          >
            <span
              className={cx(
                "grid size-4.5 flex-none place-items-center rounded-full border",
                selected ? "border-fg bg-fg text-canvas" : "border-line-strong",
              )}
            >
              {selected && <Check className="size-3" strokeWidth={3} />}
            </span>
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="truncate">{account.label}</span>
              {unavailable && (
                <span className={cx("truncate text-caption", health === "resting" ? "text-warning" : "text-danger")}>
                  {accountNote(account, windows, left)}
                </span>
              )}
            </span>
            {!unavailable && (
              <span
                className={cx(
                  "flex-none text-caption tabular-nums",
                  left !== null && left <= 10 ? "text-danger" : "text-fg-subtle",
                )}
              >
                {accountNote(account, windows, left)}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

/** Signs in a new Claude or ChatGPT account without leaving the picker. */
export function AddAccount({ onDone, onCancel }: { onDone: (provider: Provider) => void; onCancel: () => void }) {
  const [choice, setChoice] = useState<{ provider: Provider; alternative: boolean } | null>(null);
  const done = () => choice && onDone(choice.provider);
  const back = () => setChoice(null);
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-4 py-3.5">
      <div className="flex items-center gap-2">
        <button
          type="button"
          aria-label="Back"
          className="grid size-7 flex-none cursor-pointer place-items-center rounded-lg text-fg-subtle hover:bg-surface-muted hover:text-fg"
          onClick={choice ? back : onCancel}
        >
          <ArrowLeft className="size-4" />
        </button>
        <span className="text-small font-semibold text-fg">
          {choice ? `Add a ${providerName[choice.provider]} account` : "Add an account"}
        </span>
      </div>
      {!choice ? (
        <div className="flex flex-col gap-1.5">
          {(["claude", "codex", "opencode"] as const).map((provider) => (
            <button
              key={provider}
              type="button"
              className="flex h-11 cursor-pointer items-center gap-2.5 rounded-lg bg-surface-muted/60 px-3 text-left text-small font-medium text-fg transition-colors hover:bg-surface-muted"
              onClick={() => setChoice({ provider, alternative: false })}
            >
              <ProviderGlyph provider={provider} />
              {providerName[provider]}
              <span className="ml-auto text-caption font-normal text-fg-subtle">
                {provider === "claude" ? "Pro, Max or Team" : provider === "codex" ? "Plus or Pro" : "Free or Go key"}
              </span>
            </button>
          ))}
        </div>
      ) : (
        <>
          {choice.provider === "codex" ? (
            <CodexSignIn mode={choice.alternative ? "device" : "browser"} onDone={done} onCancel={back} />
          ) : choice.provider === "opencode" ? (
            <OpencodeKeySignIn onDone={done} onCancel={back} />
          ) : choice.alternative ? (
            <ClaudeTokenSignIn onDone={done} onCancel={back} />
          ) : (
            <ClaudeSignIn onDone={done} onCancel={back} />
          )}
          {choice.provider !== "opencode" && (
            <button
              type="button"
              className="cursor-pointer self-start text-caption text-fg-subtle underline-offset-2 hover:text-fg hover:underline"
              onClick={() => setChoice({ ...choice, alternative: !choice.alternative })}
            >
              {choice.alternative
                ? `Sign in to ${providerName[choice.provider]} in the browser instead`
                : choice.provider === "claude"
                  ? "Paste a token from claude setup-token instead"
                  : "Sign in with a one-time code instead"}
            </button>
          )}
        </>
      )}
    </div>
  );
}
