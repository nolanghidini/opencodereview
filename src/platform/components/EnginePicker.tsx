import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, ChevronDown, Plus } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { cx } from "../../components/DesignSystem";
import { setupApi } from "../../services/setupApi";
import type { AiAccount } from "../../types";
import { ACCOUNTS_QUERY_KEY, nextAccountId, ProviderGlyph, providerName, usageQuery } from "./AccountsPanel";
import { AccountUsageBar } from "./AccountUsageBar";
import { AccountChoices, AddAccount } from "./EngineAccounts";

export const ENGINES_QUERY_KEY = ["review-engines"] as const;

type Provider = AiAccount["provider"];
const PROVIDERS: Provider[] = ["claude", "codex", "opencode"];

const effortLabel: Record<string, string> = {
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
  ultra: "Ultra",
};
const labelEffort = (effort: string) => effortLabel[effort] ?? effort;

/** The account, model and effort the next reviews run on, switchable from the top bar. */
export function EnginePicker() {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  // The provider the menu shows, which starts as the next review's; null follows it.
  const [view, setView] = useState<Provider | null>(null);
  const [adding, setAdding] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const accounts = useQuery({ queryKey: ACCOUNTS_QUERY_KEY, queryFn: setupApi.listAccounts, refetchInterval: 60_000 });
  const engines = useQuery({ queryKey: ENGINES_QUERY_KEY, queryFn: setupApi.reviewEngines });
  const all = accounts.data ?? [];
  const next = all.find((account) => account.id === nextAccountId(all));
  const engine = engines.data?.find((item) => item.provider === next?.provider);
  // New reviews start on the first ready account, so choosing one moves it to the front, turning
  // it back on first if it was paused.
  const choose = useMutation({
    mutationFn: async (account: AiAccount) => {
      if (!account.enabled) await setupApi.updateAccount(account.id, { enabled: true });
      return setupApi.reorderAccounts([
        account.id,
        ...all.filter((other) => other.id !== account.id).map((other) => other.id),
      ]);
    },
    onSuccess: (data) => queryClient.setQueryData(ACCOUNTS_QUERY_KEY, data),
  });
  const save = useMutation({
    mutationFn: (choice: { provider: string; model: string; effort: string }) => setupApi.saveReviewEngine(choice),
    onSuccess: (data) => queryClient.setQueryData(ENGINES_QUERY_KEY, data),
  });
  useEffect(() => {
    if (!open) return;
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent ? event.key === "Escape" : !root.current?.contains(event.target as Node))
        setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open]);
  if (!next || !engine) return null;
  const model = engine.models.find((item) => item.id === engine.model);
  // The trigger shows the model; effort and account are in its label and in the menu.
  const summary = [model?.label ?? engine.model, engine.effort && labelEffort(engine.effort), next.label]
    .filter(Boolean)
    .join(" · ");
  const provider = view ?? next.provider;
  const shown = engines.data?.find((item) => item.provider === provider) ?? engine;
  const shownModel = shown.models.find((item) => item.id === shown.model);
  const models = shown.models.filter((item) => item.label.toLowerCase().includes(search.trim().toLowerCase()));
  const chooseModel = (id: string) => {
    const efforts = shown.models.find((item) => item.id === id)?.efforts ?? [];
    save.mutate({ provider: shown.provider, model: id, effort: efforts.includes(shown.effort) ? shown.effort : "" });
  };
  const connected = PROVIDERS.filter((item) => all.some((account) => account.provider === item));
  const added = (item: Provider) => {
    setAdding(false);
    setView(item);
    void queryClient.invalidateQueries({ queryKey: ACCOUNTS_QUERY_KEY });
    void queryClient.invalidateQueries({ queryKey: usageQuery.queryKey });
  };
  return (
    <div ref={root} className="relative ml-auto flex items-center gap-3">
      <AccountUsageBar account={next} />
      <button
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={summary}
        title={`New reviews run on ${summary}`}
        className="flex h-8 max-w-[min(20rem,50vw)] cursor-pointer items-center gap-2 rounded-lg px-2 text-small text-fg transition-colors hover:bg-surface-muted"
        onClick={() => {
          setView(null);
          setAdding(false);
          setOpen(!open);
        }}
      >
        <ProviderGlyph provider={engine.provider} />
        <span className="truncate font-medium">{model?.label ?? engine.model}</span>
        <ChevronDown
          className={cx("-ml-0.5 size-3.5 flex-none text-fg-subtle transition-transform", open && "rotate-180")}
        />
      </button>
      {open && (
        <div
          role="dialog"
          aria-label="Reviews run on"
          className="absolute top-full right-0 z-50 mt-2 flex max-h-[min(40rem,85vh)] w-[min(24rem,92vw)] overflow-hidden rounded-2xl border border-line-strong bg-surface shadow-pop"
        >
          <div className="flex w-14 flex-none flex-col items-center gap-1.5 overflow-y-auto border-r border-line bg-rail py-2.5">
            {connected.map((item) => (
              <button
                key={item}
                type="button"
                aria-label={providerName[item]}
                aria-pressed={!adding && item === provider}
                title={providerName[item]}
                className={cx(
                  "relative grid size-10 flex-none cursor-pointer place-items-center rounded-xl transition-colors [&_svg]:size-5",
                  !adding && item === provider
                    ? "bg-surface-muted ring-1 ring-line-strong"
                    : "opacity-70 hover:bg-surface hover:opacity-100",
                )}
                onClick={() => {
                  setAdding(false);
                  setSearch("");
                  setView(item);
                }}
              >
                <ProviderGlyph provider={item} />
              </button>
            ))}
            <button
              type="button"
              aria-label="Add an account"
              aria-pressed={adding}
              title="Add an account"
              className={cx(
                "grid size-10 flex-none cursor-pointer place-items-center rounded-xl text-fg-subtle hover:bg-surface hover:text-fg",
                adding && "bg-surface-muted text-fg ring-1 ring-line-strong",
              )}
              onClick={() => setAdding(true)}
            >
              <Plus className="size-4" />
            </button>
          </div>
          {adding ? (
            <AddAccount onDone={added} onCancel={() => setAdding(false)} />
          ) : (
            <div className="flex min-w-0 flex-1 flex-col overflow-y-auto">
              <div className="px-4 pt-3.5 pb-1.5">
                <div className="truncate text-small font-semibold text-fg">{providerName[provider]}</div>
                <div className="truncate text-caption text-fg-subtle">
                  {provider === next.provider
                    ? "New reviews start on the chosen account"
                    : `New reviews run on ${providerName[next.provider]} · ${next.label}`}
                </div>
              </div>
              <AccountChoices
                accounts={all.filter((account) => account.provider === provider)}
                nextId={next.id}
                busy={choose.isPending}
                onChoose={(account) => choose.mutate(account)}
              />
              <div className="flex flex-col gap-2.5 border-t border-line px-4 pt-3 pb-2">
                <span className="text-caption text-fg-subtle">Model</span>
                {shown.models.length > 1 && (
                  <input
                    aria-label="Search models"
                    placeholder="Search models"
                    value={search}
                    onChange={(event) => setSearch(event.target.value)}
                    className="h-9 w-full rounded-lg bg-surface-muted px-3 text-small text-fg outline-none placeholder:text-fg-subtle focus:ring-2 focus:ring-brand/20"
                  />
                )}
              </div>
              <div className="flex flex-col px-2 pb-2" role="radiogroup" aria-label="Model">
                {models.map((item) => {
                  const selected = item.id === shown.model;
                  return (
                    // biome-ignore lint/a11y/useSemanticElements: radio rows styled as a list; a native radio cannot hold this layout
                    <button
                      key={item.id}
                      type="button"
                      role="radio"
                      aria-checked={selected}
                      disabled={save.isPending}
                      className="flex h-9 w-full flex-none cursor-pointer items-center gap-2.5 rounded-lg px-2 text-left text-small text-fg transition-colors hover:bg-surface-muted/60"
                      onClick={() => !selected && chooseModel(item.id)}
                    >
                      <span
                        className={cx(
                          "grid size-4.5 flex-none place-items-center rounded-full border",
                          selected ? "border-fg bg-fg text-canvas" : "border-line-strong",
                        )}
                      >
                        {selected && <Check className="size-3" strokeWidth={3} />}
                      </span>
                      <span className="truncate">{item.label}</span>
                      {shown.models.length > 1 && item.id === shown.models[0].id && (
                        <span className="rounded-md bg-surface-muted px-1.5 py-0.5 text-caption text-fg-subtle">
                          default
                        </span>
                      )}
                    </button>
                  );
                })}
                {models.length === 0 && <p className="px-2 py-3 text-caption text-fg-subtle">No model matches.</p>}
              </div>
              {shownModel && shownModel.efforts.length > 0 && (
                <div className="flex flex-col gap-2 border-t border-line px-4 py-3">
                  <span className="text-caption text-fg-subtle">Effort</span>
                  <div className="flex rounded-lg bg-surface-muted p-0.5">
                    {shownModel.efforts.map((effort) => (
                      <button
                        key={effort}
                        type="button"
                        aria-pressed={effort === shown.effort}
                        disabled={save.isPending}
                        className={cx(
                          "h-8 min-w-0 flex-1 cursor-pointer truncate rounded-md px-1.5 text-caption font-medium transition-colors",
                          effort === shown.effort ? "bg-surface text-fg shadow-card" : "text-fg-muted hover:text-fg",
                        )}
                        onClick={() => save.mutate({ provider: shown.provider, model: shown.model, effort })}
                      >
                        {labelEffort(effort)}
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
