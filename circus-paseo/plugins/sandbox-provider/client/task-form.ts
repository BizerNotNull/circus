import {
  taskOptionsSchema,
  inputFileSchema,
  type InputFile,
  type TaskOptions,
} from "../shared/task.js";
import { z } from "zod";

interface TaskSubmission {
  prompt: string;
  options: TaskOptions;
}
export const taskFormDraftSchema = z.strictObject({
  prompt: z.string(),
  outputPaths: z.string(),
  timeoutSeconds: z.string(),
  filePath: z.string(),
  fileText: z.string(),
  files: z.array(inputFileSchema),
});
export type TaskFormDraft = z.infer<typeof taskFormDraftSchema>;
type SubmissionState =
  | { status: "editing" }
  | { status: "submitting" }
  | { status: "failed"; message: string }
  | { status: "created"; agentId: string };
interface FormState {
  prompt: string;
  outputPaths: string;
  timeoutSeconds: string;
  filePath: string;
  fileText: string;
  files: InputFile[];
  submission: SubmissionState;
}
interface FormOptions {
  create(input: TaskSubmission): Promise<string>;
  initial?: Partial<TaskFormDraft>;
  onDraftChange?(draft: TaskFormDraft): void;
}

export function openTaskForm(options: FormOptions) {
  const initial = {
    prompt: "",
    outputPaths: "",
    timeoutSeconds: "300",
    filePath: "",
    fileText: "",
    files: [],
    ...options.initial,
  } satisfies TaskFormDraft;
  let state: FormState = {
    ...initial,
    submission: { status: "editing" },
  };
  const listeners = new Set<() => void>();
  let closed = false;
  function publish(next: FormState): void {
    if (closed) return;
    state = next;
    options.onDraftChange?.({
      prompt: next.prompt,
      outputPaths: next.outputPaths,
      timeoutSeconds: next.timeoutSeconds,
      filePath: next.filePath,
      fileText: next.fileText,
      files: next.files,
    });
    for (const listener of listeners) listener();
  }
  function fail(message: string): void {
    publish({ ...state, submission: { status: "failed", message } });
  }
  return {
    getState: () => state,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    close() {
      closed = true;
      listeners.clear();
    },
    set(
      field: "prompt" | "outputPaths" | "timeoutSeconds" | "filePath" | "fileText",
      value: string,
    ) {
      if (state.submission.status === "submitting") return;
      publish({ ...state, [field]: value, submission: { status: "editing" } });
    },
    addTextFile() {
      const parsed = inputFileSchema.safeParse({ path: state.filePath, text: state.fileText });
      if (!parsed.success) {
        fail(parsed.error.issues[0]!.message);
        return;
      }
      this.addFiles([parsed.data]);
    },
    addFiles(files: InputFile[]) {
      if (state.submission.status === "submitting") return;
      const parsed = taskOptionsSchema.safeParse({ inputFiles: [...state.files, ...files] });
      if (!parsed.success) {
        fail(parsed.error.issues[0]!.message);
        return;
      }
      publish({ ...state, files: parsed.data.inputFiles, submission: { status: "editing" } });
    },
    removeFile(path: string) {
      if (state.submission.status === "submitting") return;
      publish({
        ...state,
        files: state.files.filter((file) => file.path !== path),
        submission: { status: "editing" },
      });
    },
    fail,
    async submit() {
      if (state.submission.status === "submitting") return;
      const promptBytes = new TextEncoder().encode(state.prompt).length;
      if (!state.prompt.trim() || promptBytes > 65536 || state.prompt.includes("\0")) {
        fail("Enter a prompt of at most 64 KiB.");
        return;
      }
      const parsed = taskOptionsSchema.safeParse({
        inputFiles: state.files,
        outputPaths: state.outputPaths
          .split("\n")
          .map((path) => path.trim())
          .filter(Boolean),
        timeoutMs: Number(state.timeoutSeconds) * 1000,
      });
      if (!parsed.success) {
        fail(parsed.error.issues[0]!.message);
        return;
      }
      publish({ ...state, submission: { status: "submitting" } });
      try {
        const agentId = await options.create({ prompt: state.prompt, options: parsed.data });
        publish({ ...state, submission: { status: "created", agentId } });
      } catch (error) {
        fail(error instanceof Error ? error.message : "Could not create the sandbox task.");
      }
    },
  };
}
