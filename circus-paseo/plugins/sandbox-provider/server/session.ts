import { randomUUID } from "node:crypto";
import type {
  ProviderEvent,
  ProviderPrompt,
  ProviderSessionConfig,
  ProviderTimelineItem,
} from "@getpaseo/plugin/server/provider";
import { taskOptionsSchema, SINGLE_TASK_NOTICE } from "../shared/task.js";
import type { TaskResultView } from "../shared/artifacts.js";
import { ArtifactStore, ArtifactError } from "./artifacts.js";
import {
  CircusBridgeError,
  type CircusStdioClient,
  type CircusTask,
  type CleanupStatus,
} from "./bridge/stdio-client.js";
import { SandboxProviderError } from "./errors.js";
import { TaskTimeline } from "./timeline.js";

export type TaskClient = Pick<CircusStdioClient, "start" | "close">;

interface SessionOptions {
  id: string;
  config: ProviderSessionConfig;
  client: TaskClient;
  artifacts: ArtifactStore;
  emit(event: ProviderEvent): void;
}

type RunState =
  | { status: "ready" }
  | {
      status: "active" | "finished";
      task: Promise<CircusTask>;
      done: Promise<void>;
      cleanup: CleanupStatus;
    };

export class SandboxSession {
  private state: RunState = { status: "ready" };
  private result: TaskResultView | null = null;
  private readonly options;

  constructor(private readonly context: SessionOptions) {
    this.options = taskOptionsSchema.parse(context.config.providerOptions ?? {});
  }

  prompt(prompt: ProviderPrompt): void {
    const { id, emit } = this.context;
    const rejection = this.validatePrompt(prompt);
    if (rejection) {
      emit({
        type: "session.prompt_result",
        sessionId: id,
        clientMessageId: prompt.clientMessageId,
        result: { type: "failed", error: { code: "single_task", message: rejection } },
      });
      return;
    }
    if (prompt.input.type !== "message") return;
    const text = prompt.input.content
      .map((part) => (part.type === "text" ? part.text : ""))
      .join("\n");
    const turnId = randomUUID();
    this.item({
      type: "user_message",
      id: `${turnId}:user`,
      clientMessageId: prompt.clientMessageId,
      text,
    });
    emit({
      type: "session.prompt_result",
      sessionId: id,
      clientMessageId: prompt.clientMessageId,
      result: { type: "turn", turnId },
    });
    emit({ type: "session.turn", sessionId: id, turnId, state: "started" });
    this.item({
      type: "notification",
      id: `${turnId}:boundary`,
      level: "info",
      message: SINGLE_TASK_NOTICE,
    });
    const inputFiles = Object.fromEntries(
      this.options.inputFiles.map((file) => {
        const bytes = "text" in file ? Buffer.from(file.text) : Buffer.from(file.base64, "base64");
        return [file.path, bytes];
      }),
    );
    // Only connection/tool IDs cross this boundary. Host MCP configurations,
    // credentials, system prompts, env and cwd never enter the task request.
    const task = this.context.client.start({
      prompt: text,
      inputFiles,
      outputPaths: this.options.outputPaths,
      timeoutMs: this.options.timeoutMs,
      mcpGrants: this.options.mcpGrants,
    });
    const state: Exclude<RunState, { status: "ready" }> = {
      status: "active",
      task,
      done: Promise.resolve(),
      cleanup: "unknown",
    };
    this.state = state;
    state.done = this.consume(task, turnId);
  }

  private validatePrompt(prompt: ProviderPrompt): string | null {
    if (this.state.status !== "ready")
      return "This sandbox task has already started. Create a new sandbox task; conversation and project state are not retained.";
    if (prompt.delivery === "steer") return "Sandbox tasks do not support steering.";
    if (prompt.input.type !== "message" || prompt.outputSchema)
      return "Sandbox tasks accept a text prompt and explicitly configured input files.";
    if (prompt.input.content.some((part) => part.type !== "text" || "mimeType" in part))
      return "Add files in the Sandbox task panel. Host attachments and image prompts are not imported automatically.";
    return null;
  }

  private async consume(pending: Promise<CircusTask>, turnId: string): Promise<void> {
    const timeline = new TaskTimeline(turnId);
    let terminal: "completed" | "failed" | "canceled" = "failed";
    let error: { message: string; code: string } | undefined;
    let taskId = turnId;
    try {
      const task = await pending;
      taskId = task.id;
      const drain = async () => {
        for await (const event of task.events) {
          const item = timeline.accept(event);
          if (item) this.item(item);
        }
      };
      const [outcome] = await Promise.all([task.result, drain()]);
      if (this.state.status !== "ready") this.state.cleanup = outcome.cleanup;
      if (outcome.status === "succeeded") {
        const artifacts = await this.context.artifacts.save(outcome.artifacts);
        this.result = {
          taskId,
          status: "succeeded",
          cleanup: "confirmed",
          artifacts,
          message:
            "Task complete. Sandbox cleanup confirmed. Download exports before the daemon restarts.",
        };
        terminal = "completed";
      } else {
        const message =
          outcome.status === "cancelled"
            ? "Task stopped. Start a new sandbox task to run again."
            : `${outcome.error.message} (${outcome.error.code})`;
        this.result = {
          taskId,
          status: outcome.status,
          cleanup: outcome.cleanup,
          artifacts: [],
          message,
        };
        error = outcome.error;
        terminal = outcome.status === "cancelled" ? "canceled" : "failed";
      }
      // Keep the bridge alive while it owns a retryable cleanup handle.
      if (outcome.cleanup === "confirmed") await this.context.client.close();
    } catch (failure) {
      terminal = "failed";
      error = safeError(failure);
      const cleanup = this.state.status === "ready" ? "unknown" : this.state.cleanup;
      this.result = { taskId, status: "failed", cleanup, artifacts: [], message: error.message };
      if (cleanup !== "pending") {
        try {
          await this.context.client.close();
        } catch (closeError) {
          this.result.message += ` ${safeError(closeError).message}`;
        }
      }
    }
    if (this.state.status !== "ready") this.state.status = "finished";
    if (terminal !== "completed") for (const item of timeline.finish(terminal)) this.item(item);
    this.publishResult();
    this.context.emit({
      type: "session.turn",
      sessionId: this.context.id,
      turnId,
      state: terminal,
      error,
    });
  }

  async interrupt(): Promise<void> {
    const state = this.state;
    if (state.status === "ready") return;
    if (state.status === "active") {
      const task = await state.task;
      await task.cancel();
      await state.done;
    }
    if (state.cleanup !== "confirmed")
      throw new SandboxProviderError(
        "cleanup_unconfirmed",
        "Sandbox stop is not confirmed. Retry cleanup from the result card.",
      );
  }

  async retryCleanup(): Promise<CleanupStatus> {
    const state = this.state;
    if (state.status !== "finished")
      throw new SandboxProviderError(
        "not_finished",
        "Wait for the task result before retrying cleanup.",
      );
    const task = await state.task;
    state.cleanup = await task.retryCleanup();
    if (this.result) this.result = { ...this.result, cleanup: state.cleanup };
    this.publishResult();
    if (state.cleanup === "confirmed") await this.context.client.close();
    return state.cleanup;
  }

  matchesTask(id: string): boolean {
    return this.result?.taskId === id;
  }

  async close(): Promise<void> {
    if (this.state.status === "active") await this.interrupt();
    await this.context.client.close();
  }

  private publishResult(): void {
    if (!this.result) return;
    this.item({
      type: "plugin",
      id: `${this.result.taskId}:result`,
      pluginId: "sandbox-provider",
      kind: "sandbox-result",
      version: 1,
      data: this.result,
    });
  }

  private item(item: ProviderTimelineItem): void {
    this.context.emit({ type: "timeline.item", sessionId: this.context.id, item });
  }
}

export function safeError(error: unknown): { message: string; code: string } {
  if (error instanceof CircusBridgeError || error instanceof SandboxProviderError)
    return { message: error.message, code: error.code };
  if (error instanceof ArtifactError) return { message: error.message, code: "artifact_storage" };
  return {
    code: "provider",
    message: "Sandbox task failed. Check the host's sandbox configuration and start a new task.",
  };
}
