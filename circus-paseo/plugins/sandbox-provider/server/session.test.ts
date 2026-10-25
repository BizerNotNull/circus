import { expect, test } from "vitest";
import type { ProviderEvent, ProviderSessionConfig } from "@getpaseo/plugin/server/provider";
import { SandboxSession, type TaskClient } from "./session.js";
import { ArtifactStore } from "./artifacts.js";
import type {
  CircusTaskEvent,
  CircusTaskResult,
  CleanupStatus,
  CircusTaskRequest,
} from "./bridge/stdio-client.js";
import { launchConfig } from "./provider.js";

const config: ProviderSessionConfig = {
  cwd: "/private/host/project",
  env: { SECRET: "private" },
  systemPrompt: "host-only-system-prompt",
  mcpServers: { host: { type: "http", url: "http://localhost:6767" } },
  settings: {},
  persist: false,
  providerOptions: {
    inputFiles: [{ path: "answer.py", text: "print(42)" }],
    outputPaths: ["result.txt"],
    timeoutMs: 60_000,
  },
};

function fixture(events: CircusTaskEvent[] = []) {
  let complete!: (result: CircusTaskResult) => void;
  const result = new Promise<CircusTaskResult>((resolve) => {
    complete = resolve;
  });
  let terminal!: () => void;
  const finished = new Promise<void>((resolve) => {
    terminal = resolve;
  });
  const observed: ProviderEvent[] = [];
  const requests: CircusTaskRequest[] = [];
  let cancelled = 0;
  let closed = 0;
  let cleanup: CleanupStatus = "confirmed";
  const client: TaskClient = {
    async start(request) {
      requests.push(request);
      return {
        id: "task-1",
        result,
        events: {
          async *[Symbol.asyncIterator]() {
            yield* events;
            await result;
          },
        },
        async cancel() {
          cancelled += 1;
        },
        async retryCleanup() {
          return cleanup;
        },
      };
    },
    async close() {
      closed += 1;
    },
  };
  const session = new SandboxSession({
    id: "session-1",
    config,
    client,
    artifacts: new ArtifactStore(),
    emit(event) {
      observed.push(event);
      if (event.type === "session.turn" && event.state !== "started") terminal();
    },
  });
  function prompt(text = "Fix the project") {
    session.prompt({
      clientMessageId: `message-${observed.length}`,
      delivery: "auto",
      input: { type: "message", content: [{ type: "text", text }] },
    });
  }
  return {
    session,
    observed,
    requests,
    complete,
    finished,
    prompt,
    setCleanup(value: CleanupStatus) {
      cleanup = value;
    },
    cancelled: () => cancelled,
    closed: () => closed,
  };
}

test("runs once, passes only supplied bytes and reports success after confirmed cleanup", async () => {
  const f = fixture([{ kind: "text_delta", text: "Fixed" }]);
  f.prompt();
  expect(f.requests).toEqual([
    {
      prompt: "Fix the project",
      inputFiles: { "answer.py": Buffer.from("print(42)") },
      outputPaths: ["result.txt"],
      timeoutMs: 60_000,
    },
  ]);
  expect(
    f.observed.filter((event) => event.type === "session.turn").map((event) => event.state),
  ).toEqual(["started"]);
  f.complete({
    status: "succeeded",
    cleanup: "confirmed",
    artifacts: { "result.txt": Buffer.from("42\n") },
  });
  await f.finished;
  expect(f.closed()).toBe(1);
  expect(
    f.observed.filter((event) => event.type === "session.turn").map((event) => event.state),
  ).toEqual(["started", "completed"]);
  f.prompt("Continue");
  expect(f.requests).toHaveLength(1);
  expect(f.observed.at(-1)).toMatchObject({
    type: "session.prompt_result",
    result: { type: "failed", error: { code: "single_task" } },
  });
});

test("Stop waits for cancellation outcome and refuses unconfirmed cleanup", async () => {
  const f = fixture();
  f.prompt();
  const stopped = f.session.interrupt();
  f.complete({
    status: "cancelled",
    cleanup: "pending",
    error: { code: "cleanup", message: "Cleanup requires retry" },
  });
  await expect(stopped).rejects.toThrow("Sandbox stop is not confirmed");
  expect(f.cancelled()).toBe(1);
  expect(f.closed()).toBe(0);
  expect(
    f.observed.findLast((event) => event.type === "timeline.item" && event.item.type === "plugin"),
  ).toMatchObject({
    item: {
      data: {
        status: "cancelled",
        cleanup: "pending",
        message: "Task stopped. Start a new sandbox task to run again.",
      },
    },
  });
  expect(await f.session.retryCleanup()).toBe("confirmed");
  expect(f.closed()).toBe(1);
});

test("keeps model errors visible and rejects host attachments", async () => {
  const f = fixture();
  f.session.prompt({
    clientMessageId: "upload",
    delivery: "auto",
    input: {
      type: "message",
      content: [
        {
          type: "uploaded_file",
          id: "f",
          path: "/private/secret",
          fileName: "secret",
          size: 2,
          mimeType: "text/plain",
        },
      ],
    },
  });
  expect(f.requests).toHaveLength(0);
  expect(f.observed.at(-1)).toMatchObject({
    type: "session.prompt_result",
    result: { type: "failed" },
  });
  f.prompt();
  f.complete({
    status: "failed",
    cleanup: "confirmed",
    error: { code: "model_rejected", message: "Model request rejected" },
  });
  await f.finished;
  expect(
    f.observed.findLast((event) => event.type === "timeline.item" && event.item.type === "plugin"),
  ).toMatchObject({
    item: {
      data: {
        status: "failed",
        cleanup: "confirmed",
        message: "Model request rejected (model_rejected)",
      },
    },
  });
  expect(f.observed.at(-1)).toMatchObject({
    type: "session.turn",
    state: "failed",
    error: { code: "model_rejected", message: "Model request rejected" },
  });
});

test("bridge launch strips host credentials, HOME, proxies and task env", () => {
  expect(
    launchConfig({
      command: "/opt/circus-stdio-bridge",
      args: [],
      env: {
        CIRCUS_RUNNER_CONFIG: "/private/runner.json",
        HOME: "/private/home",
        OPENAI_API_KEY: "secret",
        HTTPS_PROXY: "http://proxy",
        PATH: "/bin",
      },
    }),
  ).toEqual({
    executable: "/opt/circus-stdio-bridge",
    configFile: "/private/runner.json",
    env: { PATH: "/bin" },
  });
});
