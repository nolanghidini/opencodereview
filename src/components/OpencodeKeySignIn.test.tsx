import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { setupApi } from "../services/setupApi";
import { OpencodeKeySignIn } from "./AccessConnect";

vi.mock("../services/setupApi", () => ({
  setupApi: { opencodeModels: vi.fn(), opencodeKeySave: vi.fn() },
}));

const goModels = {
  plan: "opencode-go" as const,
  current: "opencode-go/glm-5.3",
  default: "opencode-go/glm-5.3",
  models: [
    { id: "opencode-go/deepseek-v4.1-flash", label: "deepseek-v4.1-flash" },
    { id: "opencode-go/glm-5.3", label: "glm-5.3" },
  ],
};

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(setupApi.opencodeModels).mockResolvedValue(goModels);
  vi.mocked(setupApi.opencodeKeySave).mockResolvedValue({ ok: true });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.clearAllMocks();
});

async function render(onDone = vi.fn()) {
  const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <OpencodeKeySignIn onDone={onDone} onCancel={vi.fn()} />
      </QueryClientProvider>,
    );
  });
  return onDone;
}

const button = (name: string) =>
  [...container.querySelectorAll("button")].find((item) => item.textContent === name) as HTMLButtonElement;

async function type(input: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

it("checks the key, offers OpenCode's models and saves the one picked", async () => {
  const onDone = await render();
  await type(
    container.querySelector('input[aria-label="OpenCode Go key"]') as HTMLInputElement,
    "sk-go-key-0123456789",
  );
  await act(async () => button("Check key").click());
  expect(setupApi.opencodeModels).toHaveBeenCalledWith("sk-go-key-0123456789");

  const select = container.querySelector('select[aria-label="OpenCode model"]') as HTMLSelectElement;
  expect([...select.options].map((option) => option.textContent)).toEqual(["deepseek-v4.1-flash", "glm-5.3"]);
  // The account's current model is picked until the user chooses another.
  expect(select.value).toBe("opencode-go/glm-5.3");
  expect(container.textContent).toContain("OpenCode accepted the key: 2 models");

  await act(async () => {
    select.value = "opencode-go/deepseek-v4.1-flash";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await act(async () => button("Save").click());
  expect(setupApi.opencodeKeySave).toHaveBeenCalledWith(
    "sk-go-key-0123456789",
    undefined,
    "opencode-go/deepseek-v4.1-flash",
  );
  expect(onDone).toHaveBeenCalled();
});

it("lists the free models without a key and saves no key with them", async () => {
  vi.mocked(setupApi.opencodeModels).mockResolvedValue({
    plan: "opencode",
    current: "opencode/big-pickle",
    default: "opencode/big-pickle",
    models: [{ id: "opencode/big-pickle", label: "big-pickle" }],
  });
  await render();
  await act(async () => button("Use free models").click());
  expect(setupApi.opencodeModels).toHaveBeenCalledWith(undefined);
  await act(async () => button("Save").click());
  expect(setupApi.opencodeKeySave).toHaveBeenCalledWith(undefined, undefined, "opencode/big-pickle");
});

it("shows OpenCode's answer to a rejected key and keeps the key form", async () => {
  vi.mocked(setupApi.opencodeModels).mockRejectedValue(new Error("OpenCode rejected that key"));
  await render();
  await type(
    container.querySelector('input[aria-label="OpenCode Go key"]') as HTMLInputElement,
    "sk-bad-key-0123456789",
  );
  await act(async () => button("Check key").click());
  await vi.waitFor(() => expect(container.textContent).toContain("OpenCode rejected that key"));
  expect(container.querySelector('select[aria-label="OpenCode model"]')).toBeNull();
  expect(setupApi.opencodeKeySave).not.toHaveBeenCalled();
});
