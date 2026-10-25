import { access, constants } from "node:fs/promises";
import { isAbsolute } from "node:path";
import {
  negotiateProviderCapabilities,
  requireProviderCapabilities,
  type ProviderConnection,
  type ProviderEvent,
  type ProviderInput,
  type ProviderLaunch,
  type ProviderRegistration,
} from "@getpaseo/plugin/server/provider";
import { CircusStdioClient, type CircusClientConfig } from "./bridge/stdio-client.js";
import { ArtifactStore } from "./artifacts.js";
import { SandboxProviderError } from "./errors.js";
import { SandboxSession, safeError } from "./session.js";
import { SANDBOX_HOST_MODEL_ID, SANDBOX_PROVIDER_ID } from "../shared/task.js";
import { readMcpCatalog } from "./mcp-catalog.js";

const capabilities = ["prompt.message", "timeline.plugin"] as const;
const hostConfiguredModel = {
  id: SANDBOX_HOST_MODEL_ID,
  label: "Host configured",
  description: "The host runner selects the configured agent and model",
  isDefault: true,
} as const;
const modes = [
  {
    id: "single-task",
    label: "Single task",
    description: "New sandbox; no conversation or workspace continuation",
    isUnattended: true,
  },
];

export class SandboxProvider {
  private readonly sessions = new Set<SandboxSession>();
  private configFile: string | null = null;

  constructor(private readonly artifacts: ArtifactStore) {}

  registration(): ProviderRegistration {
    return {
      id: SANDBOX_PROVIDER_ID,
      label: "Sandbox",
      icon: "icon.svg",
      requiresConfiguration: true,
      description:
        "One agent task in a disposable sandbox, using explicitly supplied files and tools",
      command: ["circus-stdio-bridge"],
      status: async ({ launch }) => {
        try {
          const config = launchConfig(launch);
          await access(config.executable, constants.X_OK);
          await access(config.configFile, constants.R_OK);
          this.configFile = config.configFile;
          return { available: true };
        } catch {
          this.configFile = null;
          return {
            available: false,
            diagnostic:
              "Configure the sandbox provider with an absolute circus-stdio-bridge command and CIRCUS_RUNNER_CONFIG pointing to a readable host configuration file.",
          };
        }
      },
      connect: async (request) => {
        if (!request.versions.includes(1))
          throw new SandboxProviderError("version", "Provider protocol version 1 is required");
        const config = launchConfig(request.launch);
        this.configFile = config.configFile;
        return this.connect(
          config,
          negotiateProviderCapabilities(request.capabilities, capabilities),
        );
      },
    };
  }

  mcpCatalog() {
    return readMcpCatalog(this.configFile);
  }

  async retryCleanup(taskId: string) {
    const session = [...this.sessions].find((candidate) => candidate.matchesTask(taskId));
    if (!session)
      throw new SandboxProviderError(
        "task_unavailable",
        "This task is no longer connected. Check sandbox cleanup on the host.",
      );
    return { cleanup: await session.retryCleanup() };
  }

  private connect(config: CircusClientConfig, negotiated: readonly string[]): ProviderConnection {
    const sessions = new Map<string, SandboxSession>();
    const listeners = new Set<(event: ProviderEvent) => void>();
    let closed = false;
    function emit(event: ProviderEvent): void {
      for (const listener of listeners) listener(event);
    }
    const dispatch = async (input: ProviderInput) => {
      if (input.type === "catalog") {
        emit({
          type: "catalog",
          requestId: input.requestId,
          catalog: {
            models: [hostConfiguredModel],
            modes,
            defaultModel: SANDBOX_HOST_MODEL_ID,
            defaultMode: "single-task",
          },
        });
        return;
      }
      if (input.type === "session.open") {
        if (sessions.has(input.sessionId))
          throw new SandboxProviderError("duplicate", "Sandbox session already exists");
        if (input.persistence)
          throw new SandboxProviderError(
            "unsupported",
            "Sandbox tasks cannot be resumed. Create a new task.",
          );
        if (input.config.toolPolicy)
          throw new SandboxProviderError(
            "unsupported",
            "Sandbox tasks do not accept host tool policies",
          );
        if (input.config.model !== SANDBOX_HOST_MODEL_ID)
          throw new SandboxProviderError(
            "unsupported",
            `Sandbox model must be ${SANDBOX_HOST_MODEL_ID}; the host runner selects the real model`,
          );
        const session = new SandboxSession({
          id: input.sessionId,
          config: input.config,
          artifacts: this.artifacts,
          client: new CircusStdioClient(config),
          emit,
        });
        sessions.set(input.sessionId, session);
        this.sessions.add(session);
        emit({
          type: "session.opened",
          requestId: input.requestId,
          sessionId: input.sessionId,
          capabilities: negotiated,
          restoration: "core",
          cwd: input.config.cwd,
        });
        emit({
          type: "session.config",
          sessionId: input.sessionId,
          config: {
            model: SANDBOX_HOST_MODEL_ID,
            models: [hostConfiguredModel],
            modes,
            mode: "single-task",
            thinkingOptions: [],
            settings: [],
          },
        });
        emit({ type: "session.ready", requestId: input.requestId, sessionId: input.sessionId });
        return;
      }
      if (!("sessionId" in input))
        throw new SandboxProviderError("unsupported", "Unsupported sandbox operation");
      const session = sessions.get(input.sessionId);
      if (!session)
        throw new SandboxProviderError("missing_session", "Sandbox session is not open");
      switch (input.type) {
        case "session.prompt":
          session.prompt(input.prompt);
          return;
        case "session.interrupt":
          await session.interrupt();
          break;
        case "session.close":
          await session.close();
          sessions.delete(input.sessionId);
          this.sessions.delete(session);
          emit({ type: "session.closed", sessionId: input.sessionId });
          break;
        default:
          throw new SandboxProviderError("unsupported", "Unsupported sandbox operation");
      }
      emit({ type: "request.completed", requestId: input.requestId });
    };
    return {
      version: 1,
      capabilities: negotiated,
      onEvent(listener) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      async send(input) {
        if (closed) throw new SandboxProviderError("closed", "Sandbox provider is closed");
        try {
          requireProviderCapabilities(negotiated, input);
          await dispatch(input);
        } catch (failure) {
          const error = safeError(failure);
          if (input.type === "session.prompt")
            emit({
              type: "session.prompt_result",
              sessionId: input.sessionId,
              clientMessageId: input.prompt.clientMessageId,
              result: { type: "failed", error },
            });
          else if ("requestId" in input)
            emit({ type: "request.failed", requestId: input.requestId, error });
          else throw failure;
        }
      },
      close: async () => {
        if (closed) return;
        const results = await Promise.allSettled(
          [...sessions.values()].map((session) => session.close()),
        );
        const failed = results.find((result) => result.status === "rejected");
        if (failed?.status === "rejected") throw failed.reason;
        for (const session of sessions.values()) this.sessions.delete(session);
        sessions.clear();
        listeners.clear();
        closed = true;
      },
    };
  }
}

export function launchConfig(launch: ProviderLaunch | undefined): CircusClientConfig {
  const configFile = launch?.env.CIRCUS_RUNNER_CONFIG;
  if (
    !launch ||
    !configFile ||
    !isAbsolute(configFile) ||
    !isAbsolute(launch.command) ||
    launch.args.length !== 0
  ) {
    throw new SandboxProviderError(
      "configuration",
      "Set an absolute bridge executable and CIRCUS_RUNNER_CONFIG; command arguments are not supported.",
    );
  }
  const env: Record<string, string> = {};
  // The host config supplies absolute Python/model-key paths. No login, proxy or HOME inheritance.
  for (const key of ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR"]) {
    const value = launch.env[key];
    if (value !== undefined) env[key] = value;
  }
  return { executable: launch.command, configFile, env };
}
