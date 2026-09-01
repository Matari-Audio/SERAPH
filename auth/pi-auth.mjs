// Uses Pi's production ModelRuntime for OAuth, locked credential storage, and refresh.
// Protocol output never includes refresh tokens.
import readline from "node:readline";
import { homedir } from "node:os";
import { join } from "node:path";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";

const provider = "openai-codex";
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
  const resolved = await runtime.getAuth(provider, { minOAuthValidityMs: force ? 3_500_000 : 30_000 });
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

function startLogin(id, method) {
  if (login) return send({ id, error: "Login already in progress" });
  const controller = new AbortController();
  login = controller;
  runtime.login(provider, "oauth", {
    signal: controller.signal,
    notify: (event) => send({ event: "auth", ...event }),
    prompt: (prompt) => {
      if (prompt.type === "select" && method) return Promise.resolve(method);
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
        const abort = () => reject(new Error("Login cancelled"));
        prompt.signal?.addEventListener("abort", abort, { once: true });
        pendingPrompt = (value) => {
          prompt.signal?.removeEventListener("abort", abort);
          pendingPrompt = undefined;
          resolve(value);
        };
      });
    },
  }).then(
    () => tokens(id),
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
    else if (request.method === "login") startLogin(request.id, request.loginMethod);
    else if (request.method === "prompt") {
      if (!pendingPrompt) throw new Error("No login prompt is waiting");
      pendingPrompt(String(request.value || ""));
      send({ id: request.id, result: null });
    }
    else if (request.method === "cancel") {
      login?.abort();
      send({ id: request.id, result: null });
    } else if (request.method === "logout") {
      await runtime.logout(provider);
      send({ id: request.id, result: null });
    } else send({ id: request.id, error: `Unknown method: ${request.method}` });
  } catch (error) {
    send({ id: request?.id, error: error instanceof Error ? error.message : String(error) });
  }
});
