import { useMutation, useQuery } from "@tanstack/react-query";
import { ArrowUpRight } from "lucide-react";
import { useEffect, useState } from "react";
import { setupApi } from "../services/setupApi";
import type { OpencodeModels } from "../types";
import { BrandLoadingIcon } from "./BrandLoading";

const secondaryButton =
  "inline-flex h-9 items-center justify-center rounded-md border border-line bg-surface px-3 text-[0.8125rem] font-medium text-fg transition hover:border-line-strong hover:bg-surface disabled:cursor-not-allowed disabled:opacity-45";
const verificationCode =
  "inline-flex rounded-lg border border-[color-mix(in_srgb,var(--accent)_30%,var(--border))] bg-surface px-4 py-3 font-mono text-2xl font-semibold tracking-[0.16em] text-fg";
type FlowEvent = { kind: string; code?: string; url?: string; message?: string; success?: boolean };

const offlineMessage = "OpenCodeReview's server did not answer. Check that Docker is running, then try again.";
export function friendlyError(error: unknown) {
  const message = error instanceof Error ? error.message : String(error ?? "");
  return /failed to fetch|networkerror|load failed/i.test(message) ? offlineMessage : message;
}

const copy = (code?: string) => (code ? navigator.clipboard?.writeText(code) : undefined);

export function CopyCode({ code }: { code: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="access-code-row flex flex-wrap items-center gap-2.5">
      <code className={verificationCode}>{code}</code>
      <button type="button" className={secondaryButton} onClick={() => void copy(code)?.then(() => setCopied(true))}>
        {copied ? "Copied" : "Copy code"}
      </button>
    </div>
  );
}

export function useGithubFlow(connected: boolean, onChanged: () => void) {
  const events = useQuery({
    queryKey: ["setup", "auth-events"],
    queryFn: () => setupApi.githubAuthEvents(),
    enabled: !connected,
    refetchInterval: (query) => (query.state.data?.running ? 1000 : false),
  });
  const start = useMutation({ mutationFn: () => setupApi.githubAuthStart(), onSuccess: () => void events.refetch() });
  const list: FlowEvent[] = events.data?.events ?? [];
  const prompt: FlowEvent = Object.assign({}, ...list.filter((event) => event.kind === "prompt"));
  const done = list.find((event) => event.kind === "done");
  const running = Boolean(events.data?.running) || start.isPending;
  useEffect(() => {
    if (done?.success) onChanged();
  }, [done?.success, onChanged]);
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(onChanged, 3000);
    return () => window.clearInterval(timer);
  }, [running, onChanged]);
  return {
    start,
    prompt,
    running,
    failure: done && !done.success ? (done.message ?? "GitHub sign-in did not finish.") : null,
    error: start.error ?? events.error,
  };
}

/** The calls behind a sign-in that opens a provider page and takes back the code it shows. */
type PasteFlowApi = {
  key: string;
  name: string;
  start: (accountId?: number) => Promise<unknown>;
  events: () => Promise<{ running: boolean; events: FlowEvent[] }>;
  input: (code: string) => Promise<unknown>;
  cancel: () => Promise<unknown>;
};
const claudeApi: PasteFlowApi = {
  key: "claude-auth-events",
  name: "Claude",
  start: (id) => setupApi.claudeAuthStart(id),
  events: () => setupApi.claudeAuthEvents(),
  input: (code) => setupApi.claudeAuthInput(code),
  cancel: () => setupApi.claudeAuthCancel(),
};
function usePasteFlow(api: PasteFlowApi, onChanged: () => void, accountId?: number) {
  const [started, setStarted] = useState(false);
  const [code, setCode] = useState("");
  const events = useQuery({
    queryKey: ["setup", api.key],
    queryFn: () => api.events(),
    enabled: started,
    refetchInterval: (query) => (query.state.data?.running !== false ? 1000 : false),
  });
  const start = useMutation({
    mutationFn: () => api.start(accountId),
    onSuccess: () => {
      setStarted(true);
      setCode("");
      void events.refetch();
    },
  });
  const submit = useMutation({ mutationFn: () => api.input(code.trim()), onSuccess: () => void events.refetch() });
  const cancel = useMutation({
    mutationFn: () => api.cancel(),
    onSuccess: () => {
      setStarted(false);
      void events.refetch();
    },
  });
  const list: FlowEvent[] = started ? (events.data?.events ?? []) : [];
  const url = list.find((event) => event.kind === "prompt")?.url;
  const submitted = list.some((event) => event.kind === "submitted");
  const done = list.find((event) => event.kind === "done");
  useEffect(() => {
    if (done?.success) onChanged();
  }, [done?.success, onChanged]);
  return {
    code,
    setCode,
    url,
    submitted,
    start,
    submit,
    cancel,
    running: started && !done,
    failure: done && !done.success ? (done.message ?? `${api.name} sign-in did not finish.`) : null,
    error: start.error ?? submit.error ?? events.error,
  };
}

const brandButton =
  "inline-flex h-9 w-fit flex-none cursor-pointer items-center justify-center gap-1.5 rounded-lg bg-brand px-3.5 text-small font-semibold text-brand-fg no-underline transition hover:bg-brand-hover disabled:cursor-not-allowed disabled:opacity-50";
const quietButton =
  "inline-flex h-9 w-fit flex-none cursor-pointer items-center justify-center rounded-lg bg-surface-sunken px-3.5 text-small font-semibold text-fg transition hover:bg-line-strong disabled:cursor-not-allowed disabled:opacity-50";
const codeInput =
  "access-code-input h-9 min-w-0 flex-1 basis-56 rounded-lg border border-line bg-surface px-3 font-mono text-small text-fg outline-none focus:border-brand focus:ring-3 focus:ring-brand/15";

function Waiting({ children }: { children: React.ReactNode }) {
  return (
    <span className="inline-flex items-center gap-2 text-small text-fg-muted">
      <BrandLoadingIcon size={16} />
      {children}
    </span>
  );
}

function Problem({ children }: { children: React.ReactNode }) {
  return (
    <p className="access-flow-error text-small font-semibold text-danger" role="alert">
      {children}
    </p>
  );
}

/** Shared frame of every sign-in: one line of guidance, then the actions in a single row. */
function SignInFrame({
  hint,
  children,
  problems,
}: {
  hint: React.ReactNode;
  children: React.ReactNode;
  problems?: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-3">
      <p className="text-small text-fg-muted">{hint}</p>
      {children}
      {problems}
    </div>
  );
}

function Retry({ busy, onRetry, onCancel }: { busy: boolean; onRetry?: () => void; onCancel: () => void }) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      {onRetry && (
        <button type="button" className={brandButton} disabled={busy} onClick={onRetry}>
          Try again
        </button>
      )}
      <button type="button" className={quietButton} onClick={onCancel}>
        Cancel
      </button>
    </div>
  );
}

/** Opens the provider's sign-in page, then takes the code it shows, saving the sign-in to
 * `accountId` or to a new account. */
function PasteCodeSignIn({
  api,
  plan,
  accountId,
  onDone,
  onCancel,
}: {
  api: PasteFlowApi;
  plan: string;
  accountId?: number;
  onDone: () => void;
  onCancel: () => void;
}) {
  const flow = usePasteFlow(api, onDone, accountId);
  const { mutate: start } = flow.start;
  useEffect(() => {
    start();
  }, [start]);
  const locked = !flow.url || flow.submitted || flow.submit.isPending;
  const problems = (
    <>
      {!flow.running && flow.failure && <Problem>{flow.failure}</Problem>}
      {flow.error && <Problem>{friendlyError(flow.error)}</Problem>}
    </>
  );
  if (!flow.running)
    return (
      <SignInFrame hint={plan} problems={problems}>
        <Retry
          busy={flow.start.isPending}
          onRetry={flow.failure || flow.error ? () => flow.start.mutate() : undefined}
          onCancel={onCancel}
        />
      </SignInFrame>
    );
  return (
    <SignInFrame hint={`Sign in to ${api.name} in the browser, then paste the code it shows you.`} problems={problems}>
      <div className="flex flex-wrap items-center gap-2">
        {flow.url ? (
          <a className={brandButton} href={flow.url} target="_blank" rel="noreferrer">
            Open {api.name} sign-in
            <ArrowUpRight className="size-3.5" aria-hidden="true" />
          </a>
        ) : (
          <Waiting>Preparing sign-in…</Waiting>
        )}
      </div>
      <form
        className="access-code-row flex flex-wrap items-center gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          flow.submit.mutate();
        }}
      >
        <input
          aria-label={`${api.name} authentication code`}
          className={codeInput}
          value={flow.code}
          autoComplete="off"
          spellCheck={false}
          disabled={locked}
          onChange={(event) => flow.setCode(event.target.value)}
          placeholder="Paste the code"
        />
        <button type="submit" className={brandButton} disabled={locked || !flow.code.trim()}>
          {flow.submitted || flow.submit.isPending ? "Checking…" : "Connect"}
        </button>
        <button
          type="button"
          className={quietButton}
          disabled={flow.cancel.isPending}
          onClick={() => flow.cancel.mutate(undefined, { onSettled: onCancel })}
        >
          Cancel
        </button>
      </form>
      {flow.submitted && <Waiting>Checking the code with {api.name}…</Waiting>}
    </SignInFrame>
  );
}

/** Signs in to Claude right away, saving the token to `accountId` or to a new account. */
export function ClaudeSignIn(props: { accountId?: number; onDone: () => void; onCancel: () => void }) {
  return <PasteCodeSignIn api={claudeApi} plan="Uses your Claude Pro, Max or Team plan." {...props} />;
}

const selectInput =
  "h-9 min-w-0 flex-1 basis-56 cursor-pointer rounded-lg border border-line bg-surface px-3 text-small text-fg outline-none focus:border-brand focus:ring-3 focus:ring-brand/15 disabled:cursor-not-allowed disabled:opacity-50";

/** The models OpenCode offers to a key, an account or the free plan, as a dropdown. */
export function OpencodeModelSelect({
  models,
  value,
  disabled,
  onChange,
}: {
  models: OpencodeModels["models"];
  value: string;
  disabled?: boolean;
  onChange: (model: string) => void;
}) {
  return (
    <select
      aria-label="OpenCode model"
      className={selectInput}
      value={value}
      disabled={disabled}
      onChange={(event) => onChange(event.target.value)}
    >
      {models.map((model) => (
        <option key={model.id} value={model.id}>
          {model.label}
        </option>
      ))}
    </select>
  );
}

/** Connects OpenCode, to `accountId` or to a new account. A pasted Go key is checked with
 * OpenCode, which answers with the models it unlocks; the free models need no key. Either way
 * the account is saved with the model picked from that list. */
export function OpencodeKeySignIn({
  accountId,
  onDone,
  onCancel,
}: {
  accountId?: number;
  onDone: () => void;
  onCancel: () => void;
}) {
  const [key, setKey] = useState("");
  const [model, setModel] = useState("");
  const check = useMutation({
    mutationFn: (withKey: boolean) => setupApi.opencodeModels(withKey ? key.trim() : undefined),
    onSuccess: (list) =>
      setModel(list.models.some((item) => item.id === list.current) ? list.current : (list.models[0]?.id ?? "")),
  });
  const list = check.data;
  const save = useMutation({
    mutationFn: () =>
      setupApi.opencodeKeySave(list?.plan === "opencode-go" ? key.trim() : undefined, accountId, model || undefined),
    onSuccess: onDone,
  });
  const error = save.error ?? check.error;
  const problems = error && <Problem>{friendlyError(error)}</Problem>;
  if (list)
    return (
      <SignInFrame
        hint={
          list.plan === "opencode-go"
            ? `OpenCode accepted the key: ${list.models.length} models. Pick the one reviews run on.`
            : `${list.models.length} free models. Pick the one reviews run on.`
        }
        problems={problems}
      >
        <form
          className="flex flex-wrap items-center gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            save.mutate();
          }}
        >
          <OpencodeModelSelect models={list.models} value={model} disabled={save.isPending} onChange={setModel} />
          <button type="submit" className={brandButton} disabled={!model || save.isPending}>
            {save.isPending ? "Saving…" : "Save"}
          </button>
          <button type="button" className={quietButton} disabled={save.isPending} onClick={() => check.reset()}>
            Back
          </button>
        </form>
      </SignInFrame>
    );
  return (
    <SignInFrame hint="Free models need no account. A key unlocks the paid Go models." problems={problems}>
      <form
        className="flex flex-wrap items-center gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          check.mutate(true);
        }}
      >
        <input
          aria-label="OpenCode Go key"
          type="password"
          className={codeInput}
          value={key}
          autoComplete="off"
          spellCheck={false}
          placeholder="Paste your OpenCode Go key"
          onChange={(event) => setKey(event.target.value)}
        />
        <button type="submit" className={brandButton} disabled={!key.trim() || check.isPending}>
          {check.isPending && check.variables ? "Checking the key…" : "Check key"}
        </button>
      </form>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <button type="button" className={quietButton} disabled={check.isPending} onClick={() => check.mutate(false)}>
          {check.isPending && !check.variables ? "Loading free models…" : "Use free models"}
        </button>
        <button type="button" className={quietButton} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </SignInFrame>
  );
}

/** Pastes a token made with `claude setup-token`, saving it to `accountId` or to a new account. */
export function ClaudeTokenSignIn({
  accountId,
  onDone,
  onCancel,
}: {
  accountId?: number;
  onDone: () => void;
  onCancel: () => void;
}) {
  const [token, setToken] = useState("");
  const save = useMutation({ mutationFn: () => setupApi.claudeTokenSave(token.trim(), accountId), onSuccess: onDone });
  return (
    <SignInFrame
      hint={
        <>
          Run{" "}
          <code className="rounded bg-surface-sunken px-1.5 py-0.5 font-mono text-caption text-fg">
            claude setup-token
          </code>{" "}
          on any computer signed in to Claude, then paste the token.
        </>
      }
      problems={save.error && <Problem>{friendlyError(save.error)}</Problem>}
    >
      <form
        className="flex flex-wrap items-center gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <input
          aria-label="Claude token"
          type="password"
          className={codeInput}
          value={token}
          autoComplete="off"
          spellCheck={false}
          placeholder="sk-ant-oat01-…"
          onChange={(event) => setToken(event.target.value)}
        />
        <button type="submit" className={brandButton} disabled={!token.trim() || save.isPending}>
          {save.isPending ? "Saving…" : "Save token"}
        </button>
        <button type="button" className={quietButton} onClick={onCancel}>
          Cancel
        </button>
      </form>
    </SignInFrame>
  );
}

/** Signs in to Codex right away, in the browser or with a one-time code, saving it to `accountId` or to a new account. */
export function CodexSignIn({
  accountId,
  mode = "browser",
  onDone,
  onCancel,
}: {
  accountId?: number;
  mode?: "browser" | "device";
  onDone: () => void;
  onCancel: () => void;
}) {
  const [started, setStarted] = useState(false);
  const [copied, setCopied] = useState(false);
  const events = useQuery({
    queryKey: ["setup", "codex-auth-events"],
    queryFn: () => setupApi.codexAuthEvents(),
    enabled: started,
    refetchInterval: (query) => (query.state.data?.running !== false ? 1000 : false),
  });
  const start = useMutation({
    mutationFn: () => setupApi.codexAuthStart(accountId, mode),
    onSuccess: () => {
      setStarted(true);
      void events.refetch();
    },
  });
  const cancel = useMutation({
    mutationFn: () => setupApi.codexAuthCancel(),
    onSettled: () => {
      setStarted(false);
      onCancel();
    },
  });
  const { mutate: begin } = start;
  useEffect(() => {
    begin();
  }, [begin]);
  const list: FlowEvent[] = started ? (events.data?.events ?? []) : [];
  const prompt = list.find((event) => event.kind === "prompt");
  const done = list.find((event) => event.kind === "done");
  useEffect(() => {
    if (done?.success) onDone();
  }, [done?.success, onDone]);
  const failure = done && !done.success ? (done.message ?? "ChatGPT sign-in did not finish.") : null;
  const error = start.error ?? events.error;
  const problems = (
    <>
      {failure && <Problem>{failure}</Problem>}
      {error && <Problem>{friendlyError(error)}</Problem>}
    </>
  );
  const copy = async () => {
    if (!prompt?.code) return;
    await navigator.clipboard.writeText(prompt.code).catch(() => undefined);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  };
  if (failure || error)
    return (
      <SignInFrame hint="Uses your ChatGPT plan's Codex usage." problems={problems}>
        <Retry busy={start.isPending} onRetry={() => start.mutate()} onCancel={onCancel} />
      </SignInFrame>
    );
  const cancelButton = (
    <button type="button" className={quietButton} disabled={cancel.isPending} onClick={() => cancel.mutate()}>
      Cancel
    </button>
  );
  if (mode === "browser")
    return (
      <SignInFrame hint="Sign in to ChatGPT in the browser. This page updates by itself when you're done.">
        <div className="flex flex-wrap items-center gap-2">
          {prompt?.url ? (
            <a className={brandButton} href={prompt.url} target="_blank" rel="noreferrer">
              Open ChatGPT sign-in
              <ArrowUpRight className="size-3.5" aria-hidden="true" />
            </a>
          ) : (
            <Waiting>Preparing sign-in…</Waiting>
          )}
          {cancelButton}
          {prompt && (
            <span className="ml-1">
              <Waiting>Waiting for the browser…</Waiting>
            </span>
          )}
        </div>
      </SignInFrame>
    );
  return (
    <SignInFrame hint="Open the ChatGPT sign-in page on any device and enter this one-time code.">
      {prompt?.code ? (
        <div className="flex flex-wrap items-center gap-2">
          {/* biome-ignore lint/a11y/useAriaPropsSupportedByRole: the label names the one-time code for tests and screen readers that honour it; the code text stays visible */}
          <code
            aria-label="Codex sign-in code"
            className="rounded-lg bg-surface-sunken px-3.5 py-1.5 font-mono text-body font-semibold tracking-[0.14em] text-fg"
          >
            {prompt.code}
          </code>
          <button type="button" className={quietButton} onClick={() => void copy()}>
            {copied ? "Copied" : "Copy"}
          </button>
        </div>
      ) : (
        <Waiting>Preparing a code…</Waiting>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {prompt?.url && (
          <a className={brandButton} href={prompt.url} target="_blank" rel="noreferrer">
            Open sign-in page
            <ArrowUpRight className="size-3.5" aria-hidden="true" />
          </a>
        )}
        {cancelButton}
        {prompt?.code && (
          <span className="ml-1">
            <Waiting>Waiting for approval…</Waiting>
          </span>
        )}
      </div>
    </SignInFrame>
  );
}
