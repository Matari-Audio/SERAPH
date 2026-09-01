import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { Type } from "typebox";

export default function (pi) {
  let child;
  let nextId = 1;
  const pending = new Map();

  function stop(error = new Error("SERAPH kernel stopped")) {
    child?.kill();
    child = undefined;
    for (const { reject } of pending.values()) reject(error);
    pending.clear();
  }

  function start() {
    if (child) return child;
    child = spawn(process.env.SERAPH_EXE ?? "seraph", ["__kernel"], {
      env: { ...process.env, SERAPH_KERNEL_CHILD: "1" },
      stdio: ["pipe", "pipe", "pipe"],
    });
    child.stderr.resume();
    createInterface({ input: child.stdout }).on("line", (line) => {
      let response;
      try { response = JSON.parse(line); } catch { return stop(new Error("invalid SERAPH kernel response")); }
      const request = pending.get(response.id);
      if (!request) return;
      pending.delete(response.id);
      response.ok ? request.resolve(response) : request.reject(new Error(response.error));
    });
    child.once("error", stop);
    child.once("exit", () => stop());
    return child;
  }

  function requestKernel(code, signal) {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      const kernel = start();
      pending.set(id, { resolve, reject });
      signal?.addEventListener("abort", () => stop(new Error("SERAPH Python execution cancelled")), { once: true });
      kernel.stdin.write(`${JSON.stringify({ id, code })}\n`);
    });
  }

  pi.registerTool({
    name: "seraph_python",
    label: "SERAPH Python",
    description: "Execute Python in this agent's persistent SERAPH kernel. Variables survive later calls. Use emit(value) for structured results.",
    promptSnippet: "seraph_python: persistent Python computation and programmatic orchestration",
    parameters: Type.Object({ code: Type.String({ description: "Python code to execute" }) }),
    async execute(_toolCallId, { code }, signal) {
      const result = await requestKernel(code, signal);
      const parts = [result.stdout, result.stderr, result.background_stdout, result.background_stderr].filter(Boolean);
      if (result.emitted.length) parts.push(JSON.stringify(result.emitted));
      if (result.truncated) parts.push("[output truncated]");
      return { content: [{ type: "text", text: parts.join("\n") || "ok" }], details: result };
    },
  });

  if (!process.env.SERAPH_AGENT_CHILD) pi.registerTool({
    name: "seraph_agent",
    label: "SERAPH Agent",
    description: "Spawn an independent SERAPH coding agent. Calls may run in parallel. The agent streams progress and returns its final answer.",
    promptSnippet: "seraph_agent: delegate independent work to parallel agents",
    parameters: Type.Object({ prompt: Type.String({ description: "Complete task for the child agent" }) }),
    async execute(toolCallId, { prompt }, signal, onUpdate) {
      const agent = spawn(process.env.SERAPH_EXE ?? "seraph", ["__agent"], {
        env: {
          ...process.env,
          SERAPH_AGENT_CHILD: "1",
          SERAPH_AGENT_ID: String(BigInt(process.pid) * 1000000n + BigInt(nextId++)),
        },
        stdio: ["pipe", "pipe", "pipe"],
      });
      agent.stderr.resume();
      let streamed = "";
      const done = new Promise((resolve, reject) => {
        createInterface({ input: agent.stdout }).on("line", (line) => {
          let event;
          try { event = JSON.parse(line); } catch { return; }
          if (event.type === "message_delta") {
            streamed += event.text;
            onUpdate?.({ content: [{ type: "text", text: streamed }], details: event });
          } else if (event.type === "idle") {
            agent.stdin.write(`${JSON.stringify({ type: "shutdown" })}\n`);
            resolve(event.result);
          } else if (event.type === "failed") {
            reject(new Error(event.error));
          }
        });
        agent.once("error", reject);
        agent.once("exit", (code) => { if (code && code !== 0) reject(new Error(`SERAPH agent exited with ${code}`)); });
      });
      signal?.addEventListener("abort", () => agent.kill(), { once: true });
      agent.stdin.write(`${JSON.stringify({ type: "start", prompt })}\n`);
      const result = await done;
      return { content: [{ type: "text", text: result || streamed || "done" }], details: { toolCallId } };
    },
  });

  pi.on("session_shutdown", async () => stop());
}
