import { useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Plus, Smartphone } from "lucide-react";
import { useCallback, useState } from "react";
import {
  ClaudeSignIn,
  ClaudeTokenSignIn,
  CodexSignIn,
  CopyCode,
  friendlyError,
  OpencodeKeySignIn,
  useGithubFlow,
} from "../components/AccessConnect";
import { BrandLoadingIcon } from "../components/BrandLoading";
import { ProviderGlyph } from "../platform/components/AccountsPanel";
import { setupApi } from "../services/setupApi";
import type { AiProvider } from "../types";
import {
  ChoiceList,
  ChoiceRow,
  ctaPrimary,
  ctaSecondary,
  rowAction,
  rowActionQuiet,
  StatusPill,
  StepActions,
  StepHeader,
} from "./OnboardingParts";

export const PROVIDERS_QUERY_KEY = ["setup", "providers"] as const;

function GithubMark() {
  return (
    <svg viewBox="0 0 16 16" className="size-5 fill-fg" aria-hidden="true">
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
    </svg>
  );
}

export function GithubStep({
  connected,
  login,
  onChanged,
  onContinue,
}: {
  connected: boolean;
  login?: string;
  onChanged: () => void;
  onContinue: () => void;
}) {
  const flow = useGithubFlow(connected, onChanged);
  const { prompt } = flow;
  const busy = flow.running || flow.start.isPending;
  return (
    <>
      <StepHeader
        title="Connect GitHub"
        subtitle="OpenCodeReview reads your pull requests and posts its reviews as you."
      />
      <ChoiceList label="Code host">
        <ChoiceRow
          glyph={<GithubMark />}
          name="GitHub"
          detail={
            connected ? (login ?? "Connected") : "Uses the GitHub CLI sign-in, kept in OpenCodeReview's Docker volume."
          }
          status={
            connected ? (
              <StatusPill tone="success">Connected</StatusPill>
            ) : !busy ? (
              <StatusPill tone="warning">Not connected</StatusPill>
            ) : undefined
          }
          action={
            !connected && !busy ? (
              <button type="button" className={rowAction} onClick={() => flow.start.mutate()}>
                {flow.failure ? "Try again" : "Connect"}
              </button>
            ) : undefined
          }
        >
          {!connected && (busy || flow.failure || flow.error) ? (
            <div className="grid gap-3">
              {busy &&
                (prompt.code ? (
                  <>
                    <p className="text-small font-medium text-fg">Enter this code on GitHub to approve access</p>
                    <CopyCode code={prompt.code} />
                    <div className="flex flex-wrap items-center gap-3">
                      <p className="flex items-center gap-2 text-small text-fg-muted">
                        <BrandLoadingIcon />
                        <span>Waiting for approval. This page updates by itself.</span>
                      </p>
                      {prompt.url && (
                        <a
                          className={rowAction}
                          href={prompt.url}
                          target="_blank"
                          rel="noreferrer"
                          onClick={() => void navigator.clipboard?.writeText(prompt.code ?? "")}
                        >
                          Open GitHub
                        </a>
                      )}
                    </div>
                  </>
                ) : (
                  <p className="flex items-center gap-2 text-small text-fg-muted">
                    <BrandLoadingIcon />
                    <span>Starting GitHub sign-in…</span>
                  </p>
                ))}
              {!busy && flow.failure && (
                <p className="text-small font-semibold text-danger" role="alert">
                  {flow.failure}
                </p>
              )}
              {flow.error && (
                <p className="text-small font-semibold text-danger" role="alert">
                  {friendlyError(flow.error)}
                </p>
              )}
            </div>
          ) : null}
        </ChoiceRow>
      </ChoiceList>
      <StepActions
        primary={
          <button type="button" className={ctaPrimary} disabled={!connected} onClick={onContinue}>
            Continue
          </button>
        }
        hint={connected ? undefined : "Connect GitHub to continue."}
      />
    </>
  );
}

type SignIn = { provider: AiProvider["provider"]; method: "oauth" | "code" };

const planDetail: Record<AiProvider["provider"], string> = {
  claude: "Pro, Max or Team plan",
  codex: "ChatGPT Plus or Pro plan",
  opencode: "Free models, or your Go key",
};

/** The second way into each provider, offered from the row's menu. */
const alternative: Record<
  AiProvider["provider"],
  { label: string; hint: string; icon: React.ReactNode; method: SignIn["method"] }
> = {
  claude: {
    label: "Paste a token",
    hint: "From claude setup-token on another computer",
    icon: <KeyRound />,
    method: "code",
  },
  codex: {
    label: "Sign in with a code",
    hint: "Enter a one-time code on any device",
    icon: <Smartphone />,
    method: "code",
  },
  opencode: {
    label: "Add an OpenCode Go key",
    hint: "Unlocks the paid Go models",
    icon: <KeyRound />,
    method: "oauth",
  },
};

function providerDetail(provider: AiProvider) {
  if (!provider.installed) return `The ${provider.name} CLI is missing from this OpenCodeReview install.`;
  if (provider.ready > 0)
    return [
      provider.label && provider.label !== provider.name ? provider.label : planDetail[provider.provider],
      provider.ready > 1 ? `+${provider.ready - 1} more` : "",
    ]
      .filter(Boolean)
      .join(" · ");
  if (provider.accounts > 0) return "Signed out";
  return planDetail[provider.provider];
}

export function PlanStep({ onBack, onContinue }: { onBack: () => void; onContinue: () => void }) {
  const queryClient = useQueryClient();
  const [signIn, setSignIn] = useState<SignIn | null>(null);
  const providers = useQuery({ queryKey: PROVIDERS_QUERY_KEY, queryFn: () => setupApi.aiProviders() });
  const refresh = useCallback(() => {
    for (const key of [PROVIDERS_QUERY_KEY, ["setup", "claude-auth"], ["setup", "status"], ["accounts"]])
      void queryClient.invalidateQueries({ queryKey: key });
  }, [queryClient]);
  const finish = useCallback(() => {
    setSignIn(null);
    refresh();
  }, [refresh]);
  const list = providers.data ?? [];
  const ready = list.some((provider) => provider.ready > 0);
  const hint = signIn ? "Finish signing in above." : ready ? "You can add more accounts later in Settings." : undefined;
  return (
    <>
      <StepHeader title="Connect your plan" subtitle="Reviews run on the AI plan you already pay for." />
      {providers.isLoading ? (
        <p className="flex items-center justify-center gap-2 text-small text-fg-muted">
          <BrandLoadingIcon />
          Checking the AI providers on this machine…
        </p>
      ) : providers.error ? (
        <p className="text-center text-small font-semibold text-danger" role="alert">
          {friendlyError(providers.error)}
        </p>
      ) : (
        <ChoiceList label="Choose your AI provider">
          {list.map((provider) => {
            const open = signIn?.provider === provider.provider ? signIn : null;
            const connected = provider.ready > 0;
            const menu = provider.installed
              ? [
                  ...(connected
                    ? [
                        {
                          label: "Add another account",
                          icon: <Plus />,
                          hint: "Reviews switch to it when this one hits a limit",
                          onSelect: () => setSignIn({ provider: provider.provider, method: "oauth" }),
                        },
                      ]
                    : []),
                  ...(alternative[provider.provider].method === "oauth"
                    ? []
                    : [
                        {
                          ...alternative[provider.provider],
                          onSelect: () =>
                            setSignIn({ provider: provider.provider, method: alternative[provider.provider].method }),
                        },
                      ]),
                ]
              : [];
            return (
              <ChoiceRow
                key={provider.provider}
                glyph={
                  <span className="scale-125">
                    <ProviderGlyph provider={provider.provider} />
                  </span>
                }
                name={provider.name}
                detail={providerDetail(provider)}
                detailTone={provider.installed ? undefined : "warning"}
                status={
                  !provider.installed ? (
                    <StatusPill tone="warning">Not installed</StatusPill>
                  ) : connected ? (
                    <StatusPill tone="success">Connected</StatusPill>
                  ) : undefined
                }
                action={
                  provider.installed && !connected && !open ? (
                    <button
                      type="button"
                      className={rowActionQuiet}
                      onClick={() => setSignIn({ provider: provider.provider, method: "oauth" })}
                    >
                      Connect
                    </button>
                  ) : undefined
                }
                menu={open ? undefined : menu}
              >
                {open &&
                  (provider.provider === "codex" ? (
                    <CodexSignIn
                      mode={open.method === "code" ? "device" : "browser"}
                      onDone={finish}
                      onCancel={() => setSignIn(null)}
                    />
                  ) : provider.provider === "opencode" ? (
                    <OpencodeKeySignIn onDone={finish} onCancel={() => setSignIn(null)} />
                  ) : open.method === "code" ? (
                    <ClaudeTokenSignIn onDone={finish} onCancel={() => setSignIn(null)} />
                  ) : (
                    <ClaudeSignIn onDone={finish} onCancel={() => setSignIn(null)} />
                  ))}
              </ChoiceRow>
            );
          })}
        </ChoiceList>
      )}
      <StepActions
        primary={
          <button type="button" className={ctaPrimary} disabled={!ready} onClick={onContinue}>
            Continue
          </button>
        }
        secondary={
          <button type="button" className={ctaSecondary} onClick={onBack}>
            Back
          </button>
        }
        hint={hint}
      />
    </>
  );
}
