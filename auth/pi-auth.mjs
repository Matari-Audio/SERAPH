// Uses Pi's production ModelRuntime for OAuth, locked credential storage, and refresh.
// Protocol output never includes refresh tokens.
import readline from "node:readline";
import { homedir } from "node:os";
import { join } from "node:path";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";

const codexProvider = "openai-codex";
const authPath = process.env.SERAPH_AUTH_PATH || join(homedir(), ".seraph", "auth.json");
const runtime = await ModelRuntime.create({ authPath, modelsPath: null, refreshOnCreate: false });
let login;
let pendingPrompt;

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function tokenClaims(accessToken) {
  try {
    const payload = accessToken.split(".")[1];
    return JSON.parse(Buffer.from(payload, "base64url").toString("utf8"));
  } catch {
    return {};
  }
}

async function tokens(id, force = false) {
  const resolved = await runtime.getAuth(codexProvider, { minOAuthValidityMs: force ? 3_500_000 : 30_000 });
  const accessToken = resolved?.auth.apiKey;
  if (!accessToken) return send({ id, result: null });
  const claims = tokenClaims(accessToken);
  const auth = claims["https://api.openai.com/auth"] || {};
  send({
    id,
    result: {
      accessToken,
      chatgptAccountId: auth.chatgpt_account_id,
      chatgptPlanType: auth.chatgpt_plan_type || null,
    },
  });
}

function providers(id) {
  const result = runtime.getProviders().flatMap((provider) => {
    const status = runtime.getProviderAuthStatus(provider.id);
    return [
      provider.auth.oauth && {
        provider: provider.id,
        authType: "oauth",
        label: `${provider.name} — ${provider.auth.oauth.loginLabel || provider.auth.oauth.name}`,
        signedIn: status.configured && runtime.isUsingOAuth(provider.id),
      },
      provider.auth.apiKey?.login && {
        provider: provider.id,
        authType: "api_key",
        label: `${provider.name} — ${provider.auth.apiKey.name}`,
        signedIn: status.configured && !runtime.isUsingOAuth(provider.id),
      },
    ].filter(Boolean);
  }).sort((a, b) => a.authType === b.authType
    ? a.label.localeCompare(b.label)
    : a.authType === "oauth" ? -1 : 1);
  send({ id, result });
}

function models(id, provider = codexProvider) {
  send({ id, result: runtime.getModels(provider).map((model) => ({
    id: model.id,
    name: model.name,
    reasoning: model.reasoning === true,
    thinkingLevelMap: model.thinkingLevelMap || {},
  })) });
}

function startLogin(id, provider, authType) {
  if (login) return send({ id, error: "Login already in progress" });
  const auth = runtime.getProvider(provider)?.auth;
  if (!(authType === "oauth" ? auth?.oauth : authType === "api_key" ? auth?.apiKey : undefined)) {
    return send({ id, error: `Unsupported login: ${provider}/${authType}` });
  }
  const controller = new AbortController();
  login = controller;
  runtime.login(provider, authType, {
    signal: controller.signal,
    notify: (event) => send({ event: "auth", ...event }),
    prompt: (prompt) => {
      send({
        event: "prompt",
        prompt: {
          type: prompt.type,
          message: prompt.message,
          placeholder: prompt.placeholder,
          options: prompt.options,
        },
      });
      return new Promise((resolve, reject) => {
        let settled = false;
        const cleanup = () => {
          prompt.signal?.removeEventListener("abort", abort);
          controller.signal.removeEventListener("abort", abort);
        };
        const abort = () => {
          if (settled) return;
          settled = true;
          cleanup();
          pendingPrompt = undefined;
          reject(new Error("Login cancelled"));
        };
        prompt.signal?.addEventListener("abort", abort, { once: true });
        controller.signal.addEventListener("abort", abort, { once: true });
        pendingPrompt = (value) => {
          if (settled) return;
          settled = true;
          cleanup();
          pendingPrompt = undefined;
          resolve(value);
        };
      });
    },
  }).then(
    () => provider === codexProvider ? tokens(id) : send({ id, result: { provider } }),
    (error) => send({ id, error: error instanceof Error ? error.message : String(error) }),
  ).finally(() => {
    login = undefined;
    pendingPrompt = undefined;
  });
}

readline.createInterface({ input: process.stdin }).on("line", async (line) => {
  let request;
  try {
    request = JSON.parse(line);
    if (request.method === "tokens") await tokens(request.id, request.force === true);
    else if (request.method === "providers") providers(request.id);
    else if (request.method === "models") models(request.id, request.provider);
    else if (request.method === "login") startLogin(request.id, request.provider, request.authType);
    else if (request.method === "prompt") {
      if (!pendingPrompt) throw new Error("No login prompt is waiting");
      pendingPrompt(String(request.value || ""));
      send({ id: request.id, result: null });
    }
    else if (request.method === "cancel") {
      login?.abort();
      send({ id: request.id, result: null });
    } else if (request.method === "logout") {
      await runtime.logout(request.provider || codexProvider);
      send({ id: request.id, result: null });
    } else send({ id: request.id, error: `Unknown method: ${request.method}` });
  } catch (error) {
    send({ id: request?.id, error: error instanceof Error ? error.message : String(error) });
  }
});
