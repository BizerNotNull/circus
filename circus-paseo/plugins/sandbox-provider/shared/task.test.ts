import { expect, test } from "vitest";
import { taskOptionsSchema } from "./task.js";

test("accepts only explicit project-relative files and bounded task settings", () => {
  expect(
    taskOptionsSchema.parse({
      inputFiles: [{ path: "src/answer.py", text: "answer = 42\n" }],
      outputPaths: ["result.txt"],
      timeoutMs: 60_000,
    }),
  ).toEqual({
    inputFiles: [{ path: "src/answer.py", text: "answer = 42\n" }],
    outputPaths: ["result.txt"],
    timeoutMs: 60_000,
  });
  for (const path of [
    "/etc/passwd",
    "../secret",
    "a/../b",
    "a//b",
    "a\\b",
    "C:secret",
    "a\n.txt",
    ".",
    "",
  ]) {
    expect(taskOptionsSchema.safeParse({ outputPaths: [path] }).success).toBe(false);
  }
  expect(
    taskOptionsSchema.safeParse({
      inputFiles: [
        { path: "a", text: "x" },
        { path: "a", text: "y" },
      ],
    }).success,
  ).toBe(false);
  expect(taskOptionsSchema.safeParse({ outputPaths: ["a", "a"] }).success).toBe(false);
  expect(taskOptionsSchema.safeParse({ timeoutMs: 3_600_001 }).success).toBe(false);
  expect(taskOptionsSchema.safeParse({ cwd: "/home/user" }).success).toBe(false);
});
