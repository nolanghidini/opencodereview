import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, LogIn, Pencil, Play, Plus, Smartphone, Trash2 } from "lucide-react";
import { useState } from "react";
import { ClaudeSignIn, ClaudeTokenSignIn, CodexSignIn, OpencodeKeySignIn } from "../../components/AccessConnect";
import { BrandLoading } from "../../components/BrandLoading";
import { cx, DSButton, DSError, DSInput, DSMenu, DSSwitchIndicator } from "../../components/DesignSystem";
import { setupApi } from "../../services/setupApi";
import type { AccountUsage, AiAccount, UsageWindow } from "../../types";
import { formatError } from "../lib/utils";

export const ACCOUNTS_QUERY_KEY = ["accounts"] as const;
const USAGE_QUERY_KEY = ["accounts", "usage"] as const;
export const usageQuery = {
  queryKey: USAGE_QUERY_KEY,
  queryFn: () => setupApi.accountUsage(),
  staleTime: 60_000,
  refetchInterval: 300_000,
};

type Health = "ready" | "resting" | "signed-out" | "paused";

export function accountHealth(account: AiAccount, now = Date.now()): Health {
  if (!account.signedIn || account.status === "auth_failed") return "signed-out";
  // A limit outranks a pause: resuming an account still at its limit would not start reviews on it.
  if (account.status === "limited" && (account.limitedUntil ?? 0) > now) return "resting";
  if (!account.enabled) return "paused";
  return "ready";
}

type Provider = AiAccount["provider"];
export const providerName: Record<Provider, string> = {
  claude: "Claude",
  codex: "ChatGPT",
  opencode: "OpenCode",
};

/** Claude's asterisk, Codex's knot or OpenCode's brackets. */
export function ProviderGlyph({ provider }: { provider: Provider }) {
  if (provider === "opencode")
    return (
      <svg
        viewBox="0 0 24 24"
        className="size-4 text-fg"
        fill="none"
        stroke="currentColor"
        strokeWidth={1.9}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        <path d="M8.5 6.5 3.5 12l5 5.5M15.5 6.5l5 5.5-5 5.5" />
      </svg>
    );
  return provider === "codex" ? (
    <svg
      viewBox="0 0 24 24"
      className="size-4 text-fg"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {[0, 60, 120, 180, 240, 300].map((angle) => (
        <path key={angle} d="M12 4.2a4 4 0 0 1 6.9 2.6l-.1 5.4-4.6 2.7" transform={`rotate(${angle} 12 12)`} />
      ))}
    </svg>
  ) : (
    <span className="text-[0.95rem] leading-none text-[#d97757]">✳</span>
  );
}

export function relative(ms: number) {
  const minutes = Math.max(1, Math.round(Math.abs(ms) / 60_000));
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  return minutes % 60 ? `${hours} h ${minutes % 60} min` : `${hours} h`;
}

/** The account the next review starts on, as the server picks it: the first ready one of the first
 * usable account's provider, then of the other providers. */
export function nextAccountId(accounts: AiAccount[], now = Date.now()) {
  const usable = accounts.filter((account) => account.enabled && account.status !== "auth_failed" && account.signedIn);
  const preferred = usable[0]?.provider;
  const ready = usable.filter((account) => accountHealth(account, now) === "ready");
  return (ready.find((account) => account.provider === preferred) ?? ready[0])?.id;
}

export function windowName(minutes: number | null) {
  if (minutes === 300) return "5-hour";
  if (minutes === 10_080) return "Weekly";
  if (!minutes) return "Usage";
  return minutes % 1440 === 0
    ? `${minutes / 1440}-day`
    : minutes % 60 === 0
      ? `${minutes / 60}-hour`
      : `${minutes}-min`;
}

export const percentLeft = (window: UsageWindow) => Math.max(0, Math.min(100, Math.round(100 - window.usedPercent)));

export function usageSummary(usage?: AccountUsage) {
  return (
    usage?.windows.map((window) => `${windowName(window.windowMins)} ${percentLeft(window)}% left`).join(" · ") ?? ""
  );
}

export function resetText(resetsAt: UsageWindow["resetsAt"], now = Date.now()) {
  const at = resetsAt === null ? NaN : new Date(resetsAt).getTime();
  return Number.isFinite(at) && at > now ? `resets in ${relative(at - now)}` : "";
}

function UsageMeters({ usage }: { usage?: AccountUsage }) {
  if (!usage) return null;
  if (usage.windows.length === 0) return null;
  return (
    <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1">
      {usage.windows.map((window) => {
        const left = percentLeft(window);
        return (
          <div
            key={window.windowMins ?? 0}
            className="flex items-center gap-2 text-caption text-fg-subtle"
            title={resetText(window.resetsAt)}
          >
            <span>{windowName(window.windowMins)}</span>
            {/* biome-ignore lint/a11y/useSemanticElements: slim custom bar; a native <meter> renders with browser-specific styling */}
            <span
              className="h-1.5 w-16 overflow-hidden rounded-full bg-surface-sunken"
              role="meter"
              aria-label={`${windowName(window.windowMins)} usage left`}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={left}
            >
              <span
                className={cx(
                  "block h-full rounded-full",
                  left <= 10 ? "bg-danger" : left <= 30 ? "bg-warning" : "bg-success",
                )}
                style={{ width: `${left}%` }}
              />
            </span>
            <span className="tabular-nums text-fg-muted">{left}% left</span>
          </div>
        );
      })}
    </div>
  );
}

export function accountStatusText(account: AiAccount, now = Date.now()) {
  switch (accountHealth(account, now)) {
    case "signed-out":
      return account.status === "auth_failed"
        ? `${providerName[account.provider]} rejected this sign-in · sign in again`
        : "Signed out · sign in again";
    case "paused":
      return "Paused · reviews skip this account";
    case "resting":
      return `Usage limit reached · resets in ${relative((account.limitedUntil ?? now) - now)}`;
    default:
      return account.lastUsedAt ? `Ready · last used ${relative(now - account.lastUsedAt)} ago` : "Ready";
  }
}

function AccountRow({
  account,
  usage,
  next,
  busy,
  onToggle,
  onRename,
  onSignIn,
  onRemove,
  onPick,
}: {
  account: AiAccount;
  usage?: AccountUsage;
  next: boolean;
  busy: boolean;
  onToggle: () => void;
  onRename: (label: string) => void;
  onSignIn: () => void;
  onRemove: () => void;
  onPick: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [label, setLabel] = useState(account.label);
  const health = accountHealth(account);
  return (
    <div
      className="account-row flex items-center gap-3 border-b border-line px-4 py-3 last:border-b-0"
      data-account={account.id}
    >
      <span className="grid size-9 flex-none place-items-center rounded-full bg-surface-muted" aria-hidden="true">
        <ProviderGlyph provider={account.provider} />
      </span>
      <div className="min-w-0 flex-1">
        {editing ? (
          <form
            className="flex items-center gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              if (label.trim()) onRename(label.trim());
              setEditing(false);
            }}
          >
            <DSInput
              aria-label="Account name"
              value={label}
              maxLength={80}
              autoFocus
              onChange={(event) => setLabel(event.target.value)}
              className="h-8"
            />
            <DSButton type="submit" size="sm" tone="primary">
              Save
            </DSButton>
            <DSButton
              size="sm"
              tone="ghost"
              onClick={() => {
                setLabel(account.label);
                setEditing(false);
              }}
            >
              Cancel
            </DSButton>
          </form>
        ) : (
          <>
            <div className="flex items-baseline gap-2">
              <span className="truncate text-small font-medium text-fg">{account.label}</span>
              {account.label !== providerName[account.provider] && (
                <span className="flex-none text-caption text-fg-subtle">{providerName[account.provider]}</span>
              )}
              {next && (
                <span
                  className="flex-none self-center rounded-full bg-brand-soft px-2 py-0.5 text-caption font-semibold text-brand-hover"
                  title="The next review starts on this account"
                >
                  Next review
                </span>
              )}
            </div>
            {health !== "ready" && (
              <div
                className={cx(
                  "truncate text-caption",
                  health === "signed-out" ? "text-danger" : health === "resting" ? "text-warning" : "text-fg-subtle",
                )}
              >
                {accountStatusText(account)}
              </div>
            )}
            <UsageMeters usage={usage} />
          </>
        )}
      </div>
      {!editing && (
        <div className="flex flex-none items-center gap-0.5">
          {health === "signed-out" && (
            <DSButton size="sm" onClick={onSignIn}>
              <LogIn className="size-3.5" />
              Sign in
            </DSButton>
          )}
          <button
            type="button"
            className="ml-1 cursor-pointer disabled:cursor-not-allowed disabled:opacity-50"
            aria-label={account.enabled ? `Pause ${account.label}` : `Use ${account.label}`}
            aria-pressed={account.enabled}
            disabled={busy}
            onClick={onToggle}
          >
            <DSSwitchIndicator checked={account.enabled} onLabel="" offLabel="" />
          </button>
          <DSMenu
            label={`${account.label} actions`}
            vertical
            size="iconSm"
            items={[
              ...(next
                ? []
                : [
                    {
                      label: "Use for new reviews",
                      hint: "New reviews start on this account",
                      icon: <Play />,
                      disabled: busy || health === "signed-out" || health === "resting",
                      onSelect: onPick,
                    },
                  ]),
              { label: "Rename", icon: <Pencil />, onSelect: () => setEditing(true) },
              { label: "Sign in again", icon: <LogIn />, onSelect: onSignIn },
              {
                label: "Remove account",
                icon: <Trash2 />,
                tone: "danger",
                hint: "Deletes its sign-in from this machine",
                onSelect: onRemove,
              },
            ]}
          />
        </div>
      )}
    </div>
  );
}

/** The ways to add an account of each provider: its usual sign-in first. */
function addOptions(provider: Provider, start: (code?: boolean) => void) {
  const main = {
    label: `Sign in to ${providerName[provider]}`,
    icon: <ProviderGlyph provider={provider} />,
    onSelect: () => start(),
  };
  if (provider === "claude")
    return [
      main,
      {
        label: "Paste a token",
        hint: "From claude setup-token on another computer",
        icon: <KeyRound />,
        onSelect: () => start(true),
      },
    ];
  if (provider === "codex")
    return [
      main,
      {
        label: "Sign in with a code",
        hint: "Enter a one-time code on any device",
        icon: <Smartphone />,
        onSelect: () => start(true),
      },
    ];
  return [main];
}

/**
 * Settings: the AI accounts reviews can use, in the order they are tried. With `provider`, only
 * that provider's accounts, still ordered within the global list.
 */
export function AccountsPanel({ provider }: { provider?: Provider } = {}) {
  const queryClient = useQueryClient();
  const accountsQuery = useQuery({
    queryKey: ACCOUNTS_QUERY_KEY,
    queryFn: setupApi.listAccounts,
    refetchInterval: 30_000,
  });
  const [signIn, setSignIn] = useState<{ provider: Provider; accountId?: number; code?: boolean } | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<AiAccount | null>(null);
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ACCOUNTS_QUERY_KEY });
    void queryClient.invalidateQueries({ queryKey: ["setup"] });
    // ENGINES_QUERY_KEY: an OpenCode account's model names the engine the top bar shows.
    void queryClient.invalidateQueries({ queryKey: ["review-engines"] });
  };
  const settle = {
    onSuccess: (accounts: AiAccount[]) => {
      queryClient.setQueryData(ACCOUNTS_QUERY_KEY, accounts);
      void queryClient.invalidateQueries({ queryKey: ["setup"] });
    },
  };
  const update = useMutation({
    mutationFn: ({ id, ...change }: { id: number; label?: string; enabled?: boolean }) =>
      setupApi.updateAccount(id, change),
    ...settle,
  });
  const reorder = useMutation({ mutationFn: (ids: number[]) => setupApi.reorderAccounts(ids), ...settle });
  const remove = useMutation({
    mutationFn: (id: number) => setupApi.deleteAccount(id),
    ...settle,
    onSettled: () => setConfirmRemove(null),
  });
  const usageQueryResult = useQuery(usageQuery);
  const all = accountsQuery.data ?? [];
  const accounts = provider ? all.filter((account) => account.provider === provider) : all;
  const next = nextAccountId(all);
  const busy = update.isPending || reorder.isPending || remove.isPending;
  const pick = async (account: AiAccount) => {
    if (!account.enabled) await update.mutateAsync({ id: account.id, enabled: true });
    reorder.mutate([account.id, ...all.filter((other) => other.id !== account.id).map((other) => other.id)]);
  };
  const error = accountsQuery.error ?? update.error ?? reorder.error ?? remove.error;
  return (
    <section className="settings-group flex flex-col gap-2.5">
      <header className="flex items-end justify-between gap-4 px-1">
        <div>
          <h2 className="text-small font-semibold text-fg">Accounts</h2>
          <p className="mt-0.5 max-w-2xl text-caption text-fg-subtle">
            New reviews start on the account marked Next review. When it hits its usage limit, they continue on the next
            account of the same provider, and only then on another provider's.
          </p>
        </div>
        {!signIn && provider && accounts.length > 0 && (
          <DSMenu
            label="Add account"
            text="Add account"
            items={addOptions(provider, (code) => setSignIn({ provider, code }))}
          />
        )}
        {!signIn && !provider && accounts.length > 0 && (
          <DSMenu
            label="Add account"
            text="Add account"
            items={[
              {
                label: "Claude",
                hint: "Claude Pro, Max, Team or Enterprise subscription",
                icon: <ProviderGlyph provider="claude" />,
                onSelect: () => setSignIn({ provider: "claude" }),
              },
              {
                label: "ChatGPT",
                hint: "ChatGPT plan with Codex",
                icon: <ProviderGlyph provider="codex" />,
                onSelect: () => setSignIn({ provider: "codex" }),
              },
              {
                label: "OpenCode",
                hint: "Free models, or an OpenCode Go key",
                icon: <ProviderGlyph provider="opencode" />,
                onSelect: () => setSignIn({ provider: "opencode" }),
              },
              {
                label: "Claude with a token",
                hint: "From claude setup-token on another computer",
                icon: <KeyRound />,
                onSelect: () => setSignIn({ provider: "claude", code: true }),
              },
              {
                label: "ChatGPT with a code",
                hint: "Enter a one-time code on any device",
                icon: <Smartphone />,
                onSelect: () => setSignIn({ provider: "codex", code: true }),
              },
            ]}
          />
        )}
      </header>
      {error && <DSError>{formatError(error)}</DSError>}
      <div className="settings-group-body flex flex-col rounded-xl border border-line/70 bg-surface">
        {accountsQuery.isLoading ? (
          <BrandLoading size="compact" label="Loading accounts…" />
        ) : provider && accounts.length === 0 && !signIn ? (
          <div className="flex flex-col items-start gap-3 px-4 py-5">
            <p className="text-small text-fg-muted">No {providerName[provider]} account yet.</p>
            <div className="flex flex-wrap gap-2">
              {addOptions(provider, (code) => setSignIn({ provider, code })).map((option, index) => (
                <DSButton key={option.label} tone={index === 0 ? "primary" : "secondary"} onClick={option.onSelect}>
                  {index === 0 && <Plus className="size-3.5" />}
                  {option.label}
                </DSButton>
              ))}
            </div>
          </div>
        ) : accounts.length === 0 && !signIn ? (
          <div className="flex flex-col items-start gap-3 px-4 py-5">
            <p className="text-small text-fg-muted">
              Connect an AI account so OpenCodeReview can review pull requests. Add more later to keep reviewing when
              one hits its limit.
            </p>
            <div className="flex flex-wrap gap-2">
              <DSButton tone="primary" onClick={() => setSignIn({ provider: "claude" })}>
                <Plus className="size-3.5" />
                Add Claude account
              </DSButton>
              <DSButton onClick={() => setSignIn({ provider: "codex" })}>
                <Plus className="size-3.5" />
                Add ChatGPT account
              </DSButton>
              <DSButton onClick={() => setSignIn({ provider: "opencode" })}>
                <Plus className="size-3.5" />
                Add OpenCode
              </DSButton>
            </div>
          </div>
        ) : (
          accounts.map((account) => (
            <AccountRow
              key={account.id}
              account={account}
              usage={usageQueryResult.data?.find((usage) => usage.accountId === account.id)}
              next={account.id === next}
              busy={busy}
              onToggle={() => update.mutate({ id: account.id, enabled: !account.enabled })}
              onRename={(label) => update.mutate({ id: account.id, label })}
              onSignIn={() => setSignIn({ provider: account.provider, accountId: account.id })}
              onRemove={() => setConfirmRemove(account)}
              onPick={() => void pick(account)}
            />
          ))
        )}
        {confirmRemove && (
          <div
            role="alertdialog"
            aria-label="Remove account"
            className="flex flex-wrap items-center gap-3 border-t border-line bg-rose-50 px-4 py-3"
          >
            <p className="min-w-0 flex-1 text-small text-rose-900">
              Remove “{confirmRemove.label}”? Its sign-in is deleted from this machine.
            </p>
            <DSButton size="sm" tone="ghost" onClick={() => setConfirmRemove(null)}>
              Keep
            </DSButton>
            <DSButton
              size="sm"
              tone="danger"
              disabled={remove.isPending}
              onClick={() => remove.mutate(confirmRemove.id)}
            >
              {remove.isPending ? "Removing…" : "Remove"}
            </DSButton>
          </div>
        )}
        {signIn && (
          <div className="border-t border-line px-4 py-4 first:border-t-0">
            <h3 className="mb-3 text-small font-semibold text-fg">
              {signIn.accountId
                ? `Sign in again to “${accounts.find((account) => account.id === signIn.accountId)?.label ?? providerName[signIn.provider]}”`
                : signIn.provider === "opencode"
                  ? "Add OpenCode"
                  : `Add a ${providerName[signIn.provider]} account`}
            </h3>
            {signIn.provider === "codex" ? (
              <CodexSignIn
                accountId={signIn.accountId}
                mode={signIn.code ? "device" : "browser"}
                onDone={() => {
                  setSignIn(null);
                  refresh();
                }}
                onCancel={() => setSignIn(null)}
              />
            ) : signIn.provider === "opencode" ? (
              <OpencodeKeySignIn
                accountId={signIn.accountId}
                onDone={() => {
                  setSignIn(null);
                  refresh();
                }}
                onCancel={() => setSignIn(null)}
              />
            ) : signIn.code ? (
              <ClaudeTokenSignIn
                accountId={signIn.accountId}
                onDone={() => {
                  setSignIn(null);
                  refresh();
                }}
                onCancel={() => setSignIn(null)}
              />
            ) : (
              <ClaudeSignIn
                accountId={signIn.accountId}
                onDone={() => {
                  setSignIn(null);
                  refresh();
                }}
                onCancel={() => setSignIn(null)}
              />
            )}
          </div>
        )}
      </div>
    </section>
  );
}
