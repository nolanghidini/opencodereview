import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { friendlyError, OpencodeModelSelect } from "../../components/AccessConnect";
import { setupApi } from "../../services/setupApi";
import type { AiAccount } from "../../types";
import { ENGINES_QUERY_KEY } from "./EnginePicker";

/** One OpenCode account's model, picked from the models OpenCode offers its key or plan. */
function AccountModel({ account }: { account: AiAccount }) {
  const queryClient = useQueryClient();
  const queryKey = ["opencode-models", account.id];
  const models = useQuery({
    queryKey,
    queryFn: () => setupApi.opencodeModels(undefined, account.id),
    staleTime: 60_000,
    retry: false,
  });
  const save = useMutation({
    mutationFn: (model: string) => setupApi.opencodeKeySave(undefined, account.id, model),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey });
      // The top bar names the model the next review runs on.
      void queryClient.invalidateQueries({ queryKey: ENGINES_QUERY_KEY });
    },
  });
  const error = save.error ?? models.error;
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
      <div className="min-w-0">
        <div className="text-small font-medium text-fg">{account.label}</div>
        <div className={error ? "text-caption text-danger" : "text-caption text-fg-subtle"}>
          {error
            ? friendlyError(error)
            : models.data
              ? `${models.data.models.length} models on this ${models.data.plan === "opencode-go" ? "Go key" : "free plan"}`
              : "Asking OpenCode for the models…"}
        </div>
      </div>
      {models.data && (
        <div className="flex w-72 max-w-full">
          <OpencodeModelSelect
            models={models.data.models}
            value={models.data.current}
            disabled={save.isPending}
            onChange={(model) => save.mutate(model)}
          />
        </div>
      )}
    </div>
  );
}

/** The model each OpenCode account reviews with: every account keeps its own. */
export function OpencodeAccountModels({ accounts }: { accounts: AiAccount[] }) {
  return accounts
    .filter((account) => account.provider === "opencode" && account.signedIn)
    .map((account) => <AccountModel key={account.id} account={account} />);
}
