import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { isValidPrTitle } from "./pr-title.mjs";

const cli = fileURLToPath(new URL("./pr-title.mjs", import.meta.url));

function runCli(title) {
  const env = { ...process.env };
  delete env.PR_TITLE;
  if (title !== undefined) env.PR_TITLE = title;
  const result = spawnSync(process.execPath, [cli], {
    env,
    encoding: "utf8",
    timeout: 10_000,
    windowsHide: true,
  });
  assert.ifError(result.error);
  assert.equal(result.signal, null);
  return result;
}

test("accepts the required forms without a type allowlist or extra scope policy", () => {
  for (const title of [
    "ci: update gates",
    "feat(workspace): add bounded writes",
    "fix(生命周期): 修复资源回收",
    "a: x",
    "custom-type9: arbitrary type",
    "a0--: digits and hyphens after the first letter",
    "ci(scope: with spaces / punctuation): update gates",
    "ci( ): nonempty whitespace scope",
    "ci: subject: with (punctuation) and 中文 🚀",
    "ci: subject with trailing whitespace \t",
    "ci: $(printf unsafe) `echo unsafe` ::warning::untrusted",
  ]) {
    assert.equal(isValidPrTitle(title), true, JSON.stringify(title));
  }
});

test("rejects missing prefixes and types outside the specified ASCII grammar", () => {
  for (const title of [
    "update gates",
    ": update gates",
    "CI: update gates",
    "cI: update gates",
    "1ci: update gates",
    "-ci: update gates",
    "ci_name: update gates",
    "修复: update gates",
    "cí: update gates",
    "ci!: update gates",
    " ci: update gates",
    "ci : update gates",
  ]) {
    assert.equal(isValidPrTitle(title), false, JSON.stringify(title));
  }
});

test("rejects empty, nested, or malformed scopes", () => {
  for (const title of [
    "ci(): update gates",
    "ci(scope: update gates",
    "ci(scope)): update gates",
    "ci((scope)): update gates",
    "ci(a(b)c): update gates",
    "ci(scope)(other): update gates",
    "ci(scope) : update gates",
  ]) {
    assert.equal(isValidPrTitle(title), false, JSON.stringify(title));
  }
});

test("requires exactly one separator space and a nonblank description", () => {
  for (const title of [
    "",
    "ci:",
    "ci: ",
    "ci:    ",
    "ci: \t",
    "ci:subject",
    "ci:\tsubject",
    "ci:\u00a0subject",
    "ci:  subject",
    "ci: \tsubject",
    "ci: \u3000subject",
    "ci(scope): ",
  ]) {
    assert.equal(isValidPrTitle(title), false, JSON.stringify(title));
  }
  for (const value of [undefined, null, 0, false, {}, ["ci: subject"]]) {
    assert.equal(isValidPrTitle(value), false);
  }
});

test("rejects line breaks anywhere, including an otherwise valid trailing newline", () => {
  for (const separator of ["\n", "\r", "\r\n", "\u2028", "\u2029"]) {
    for (const title of [
      `${separator}ci: subject`,
      `c${separator}i: subject`,
      `ci(scope${separator}other): subject`,
      `ci:${separator} subject`,
      `ci: first${separator}second`,
      `ci: subject${separator}`,
    ]) {
      assert.equal(isValidPrTitle(title), false, JSON.stringify(title));
    }
  }
});

test("CLI reads PR_TITLE, succeeds for valid metadata, and fails for invalid or missing metadata", () => {
  const valid = runCli("fix(生命周期): 修复资源回收");
  assert.equal(valid.status, 0);
  assert.equal(valid.stdout, "PR title is valid.\n");
  assert.equal(valid.stderr, "");

  for (const title of [undefined, "", "missing prefix", "ci(): empty scope"]) {
    const invalid = runCli(title);
    assert.equal(invalid.status, 1);
    assert.equal(invalid.stdout, "");
    assert.match(invalid.stderr, /PR title must use type:/u);
  }
});

test("CLI never echoes metadata or workflow commands, even on malformed input", () => {
  const sentinel = "TITLE_CONTENT_MUST_NOT_APPEAR";
  for (const title of [
    `ci: ${sentinel} $(echo unsafe) ::warning::unsafe`,
    `${sentinel}\n::error::injected`,
    `ci: subject\n::add-mask::${sentinel}`,
  ]) {
    const result = runCli(title);
    assert.equal(result.status, isValidPrTitle(title) ? 0 : 1);
    const output = result.stdout + result.stderr;
    assert.equal(output.includes(sentinel), false);
    assert.equal(output.includes("::"), false);
  }
});
