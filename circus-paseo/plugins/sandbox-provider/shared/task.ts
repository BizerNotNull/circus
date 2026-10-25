import { z } from "zod";

export const SANDBOX_PROVIDER_ID = "sandbox";
export const SANDBOX_HOST_MODEL_ID = "host-configured";
export const SANDBOX_PROVIDER_SELECTION = `${SANDBOX_PROVIDER_ID}/${SANDBOX_HOST_MODEL_ID}`;

export const projectPathSchema = z
  .string()
  .min(1)
  .max(1024)
  .refine((path) => {
    if (/[\\:\p{Cc}]/u.test(path)) return false;
    if (new TextEncoder().encode(path).length > 1024) return false;
    return path.split("/").every((part) => part !== "" && part !== "." && part !== "..");
  }, "Use a project-relative file path without empty, . or .. components");

export const inputFileSchema = z.union([
  z.strictObject({ path: projectPathSchema, text: z.string() }),
  z.strictObject({
    path: projectPathSchema,
    base64: z.string().regex(/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/),
  }),
]);
export type InputFile = z.infer<typeof inputFileSchema>;

export const taskOptionsSchema = z
  .strictObject({
    inputFiles: z.array(inputFileSchema).max(64).default([]),
    outputPaths: z.array(projectPathSchema).max(32).default([]),
    timeoutMs: z.number().int().min(1).max(3_600_000).default(300_000),
  })
  .superRefine((task, context) => {
    const inputs = task.inputFiles.map((file) => file.path);
    if (new Set(inputs).size !== inputs.length) {
      context.addIssue({
        code: "custom",
        path: ["inputFiles"],
        message: "Input file paths must be unique",
      });
    }
    if (new Set(task.outputPaths).size !== task.outputPaths.length) {
      context.addIssue({
        code: "custom",
        path: ["outputPaths"],
        message: "Output paths must be unique",
      });
    }
    const bytes = task.inputFiles.reduce((total, file) => total + inputFileBytes(file), 0);
    if (bytes > 8 * 1024 * 1024) {
      context.addIssue({
        code: "custom",
        path: ["inputFiles"],
        message: "Input files exceed 8 MiB",
      });
    }
  });

export type TaskOptions = z.infer<typeof taskOptionsSchema>;

export function inputFileBytes(file: InputFile): number {
  if ("text" in file) return new TextEncoder().encode(file.text).length;
  const padding = file.base64.endsWith("==") ? 2 : Number(file.base64.endsWith("="));
  return (file.base64.length * 3) / 4 - padding;
}

export const SINGLE_TASK_NOTICE =
  "This sandbox accepts one task. Only the files you supply are imported. Exported files are kept separately; your project is not changed. After it finishes or stops, create a new task. Conversation and workspace continuation are not available.";
