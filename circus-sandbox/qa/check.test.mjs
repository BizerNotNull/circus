import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const qaSource = path.dirname(fileURLToPath(import.meta.url));
const copied = ["check.mjs", ".markdownlint.json", ".prettierrc.json"];

function write(file, text) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

function mandatoryDocs(root) {
  const text = "# QA smoke\n";
  write(path.join(root, "README.md"), text);
  write(path.join(root, "AGENTS.md"), text);
  write(path.join(root, "circus-sandbox", "AGENTS.md"), text);
}

function removeFixture(temp) {
  const linked = path.join(temp, "circus-sandbox", "qa", "node_modules");
  try {
    fs.lstatSync(linked);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    fs.rmSync(temp, { recursive: true, force: true });
    return;
  }
  // Unlink before recursive removal so a junction cannot follow into the real tree.
  fs.unlinkSync(linked);
  fs.rmSync(temp, { recursive: true, force: true });
}

function linkJunction(from, to) {
  try {
    fs.symlinkSync(from, to, "junction");
  } catch (error) {
    const code = error && error.code ? error.code : "unknown";
    throw new Error(
      `node_modules junction could not be created (${code}); local permission to create a junction or symlink is required`,
      { cause: error },
    );
  }
  if (!fs.lstatSync(to).isSymbolicLink()) {
    throw new Error(
      "node_modules junction was not created; local permission to create a junction or symlink is required",
    );
  }
}

// check.mjs treats its grandparent as the repo root, so the copy stays at circus-sandbox/qa.
function withRepo(run) {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "circus-qa-"));
  const qa = path.join(temp, "circus-sandbox", "qa");
  fs.mkdirSync(qa, { recursive: true });
  try {
    for (const name of copied) {
      fs.copyFileSync(path.join(qaSource, name), path.join(qa, name));
    }
    linkJunction(path.join(qaSource, "node_modules"), path.join(qa, "node_modules"));
    mandatoryDocs(temp);
    return run(temp);
  } finally {
    removeFixture(temp);
  }
}

function runCheck(temp) {
  return spawnSync(process.execPath, [path.join(temp, "circus-sandbox", "qa", "check.mjs")], {
    cwd: temp,
    encoding: "utf8",
    timeout: 60_000,
    windowsHide: true,
  });
}

function assertExit(result, expected) {
  if (!result.error && result.status === expected) return;
  const output = `${result.stdout ?? ""}${result.stderr ?? ""}`.slice(0, 2000);
  assert.fail(
    `expected exit ${expected}, got ${result.status ?? "none"}${
      result.error ? ` (${result.error.message})` : ""
    }${output ? `\n${output}` : ""}`,
  );
}

function linkReadme(extraLine) {
  const lines = [
    "# QA smoke",
    "",
    "[参考][target]",
    "",
    "[target]: circus-sandbox/%E7%9B%AE%E6%A0%87%20%E6%96%87%E6%A1%A3.md#中文标题",
    "",
    "[Duplicate](circus-sandbox/%E7%9B%AE%E6%A0%87%20%E6%96%87%E6%A1%A3.md#repeat-1)",
    "",
    "[HTML heading](circus-sandbox/heading.md#hello-world)",
    "",
    "[Directory](circus-sandbox/dir#section)",
  ];
  if (extraLine) lines.push("", extraLine);
  return `${lines.join("\n")}\n`;
}

function writeLinkTargets(root) {
  write(
    path.join(root, "circus-sandbox", "目标 文档.md"),
    "# 中文标题\n\n<!-- markdownlint-disable MD024 -->\n\n## Repeat\n\n## Repeat\n",
  );
  write(path.join(root, "circus-sandbox", "heading.md"), "# Hello <em>world</em>\n");
  write(path.join(root, "circus-sandbox", "dir", "README.md"), "# Section\n");
}

function writeRust(root, text) {
  write(path.join(root, "circus-sandbox", "probe.rs"), text);
}

test("local links accept encoded references, directory READMEs, and HTML headings", () => {
  withRepo((root) => {
    writeLinkTargets(root);
    write(path.join(root, "README.md"), linkReadme());
    assertExit(runCheck(root), 0);
  });
  withRepo((root) => {
    writeLinkTargets(root);
    write(path.join(root, "README.md"), linkReadme("[Missing](missing-file.md)"));
    assertExit(runCheck(root), 1);
  });
  withRepo((root) => {
    writeLinkTargets(root);
    write(
      path.join(root, "README.md"),
      linkReadme("[Missing fragment](circus-sandbox/heading.md#does-not-exist)"),
    );
    assertExit(runCheck(root), 1);
  });
  withRepo((root) => {
    writeLinkTargets(root);
    write(path.join(root, "README.md"), linkReadme("[Broken line](circus-sandbox/heading.md#L1)"));
    assertExit(runCheck(root), 1);
  });
});

test("missing mandatory guidance fails", () => {
  withRepo((root) => {
    assertExit(runCheck(root), 0);
  });
  withRepo((root) => {
    fs.unlinkSync(path.join(root, "AGENTS.md"));
    assertExit(runCheck(root), 1);
  });
});

test("handwritten Rust line limit and generated comment header", () => {
  const handwritten = "// handwritten fixture\n";
  withRepo((root) => {
    writeRust(root, handwritten.repeat(1200));
    assertExit(runCheck(root), 0);
  });
  withRepo((root) => {
    writeRust(root, `${handwritten.repeat(1200)}// boundary\n`);
    assertExit(runCheck(root), 1);
  });
  withRepo((root) => {
    writeRust(
      root,
      'const MESSAGE: &str = "@generated DO NOT EDIT";\n' + "// handwritten\n".repeat(1200),
    );
    assertExit(runCheck(root), 1);
  });
  withRepo((root) => {
    writeRust(root, "// @generated by QA fixture\n" + "// generated\n".repeat(1200));
    assertExit(runCheck(root), 0);
  });
});

test("a .rs symlink counts even when its payload is not named .rs", () => {
  const body = "// handwritten\n".repeat(1201);
  withRepo((root) => {
    write(path.join(root, "circus-sandbox", "payload.txt"), body);
    assertExit(runCheck(root), 0);
  });
  withRepo((root) => {
    write(path.join(root, "circus-sandbox", "payload.txt"), body);
    const link = path.join(root, "circus-sandbox", "probe.rs");
    try {
      fs.symlinkSync("payload.txt", link, "file");
    } catch (error) {
      const code = error && error.code ? error.code : "unknown";
      throw new Error(
        `file symlink could not be created (${code}); local permission to create symlinks is required. This case must not pass silently.`,
        { cause: error },
      );
    }
    if (!fs.lstatSync(link).isSymbolicLink()) {
      throw new Error(
        "file symlink was not created; local permission to create symlinks is required. This case must not pass silently.",
      );
    }
    assertExit(runCheck(root), 1);
  });
});
