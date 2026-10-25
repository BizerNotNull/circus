import { chmodSync, existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import { openCommandCenter } from "../support/helpers/command-center";
import { seedWorkspace } from "../support/helpers/seed-client";
import { getServerId } from "../support/helpers/server-id";
import { switchWorkspaceViaSidebar } from "../support/helpers/workspace-ui";

const fixtureDirectory = mkdtempSync(join(tmpdir(), "paseo-sandbox-stdio-fixture-"));
const bridge = join(fixtureDirectory, "controlled-bridge");
const runnerConfig = join(fixtureDirectory, "runner.json");
const marker = `${runnerConfig}.seen`;

writeFileSync(runnerConfig, "{}\n", "utf8");
writeFileSync(
  bridge,
  `#!/usr/bin/env node
const fs = require("node:fs");
const readline = require("node:readline");
const write = (frame) => process.stdout.write(JSON.stringify(frame) + "\\n");
write({ v: 1, type: "ready", maxFrameBytes: 12 * 1024 * 1024 });
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const frame = JSON.parse(line);
  if (frame.type !== "start") return;
  const configIndex = process.argv.indexOf("--config");
  fs.writeFileSync(process.argv[configIndex + 1] + ".seen", "fixture\\n");
  write({ v: 1, type: "accepted", requestId: frame.requestId, taskId: "fixture-task" });
  write({ v: 1, type: "event", requestId: frame.requestId, taskId: "fixture-task", seq: 1, event: { kind: "phase", phase: "preparing" } });
  write({ v: 1, type: "result", requestId: frame.requestId, taskId: "fixture-task", result: { status: "succeeded", cleanup: "confirmed", artifacts: [{ path: "result.txt", data: "NDIK" }] } });
});
`,
  "utf8",
);
chmodSync(bridge, 0o755);

test.use({
  e2eDaemonConfig: {
    version: 1,
    agents: {
      providers: {
        sandbox: {
          enabled: true,
          command: [bridge],
          env: {
            CIRCUS_RUNNER_CONFIG: runnerConfig,
            CIRCUS_SANDBOX_STATE_DIR: join(fixtureDirectory, "state"),
          },
        },
      },
    },
  },
});

test.afterAll(() => {
  rmSync(fixtureDirectory, { recursive: true, force: true });
});

async function openSandboxTask(page: Page, workspaceId: string) {
  await gotoAppShell(page);
  await switchWorkspaceViaSidebar({ page, serverId: getServerId(), workspaceId });
  const command = await openCommandCenter(page);
  await command.getByTestId("command-center-input").fill("New sandbox task");
  await command.getByRole("button", { name: "New sandbox task", exact: true }).click();
  await expect(page.getByText(/Sandbox sessions continue completed turns only/)).toBeVisible();
}

test("creates through agents.create with a controlled stdio provider fixture", async ({
  page,
}, info) => {
  const workspace = await seedWorkspace({ repoPrefix: "sandbox-provider-" });
  try {
    await page.setViewportSize({ width: 1200, height: 900 });
    await openSandboxTask(page, workspace.workspaceId);
    await page.getByLabel("Sandbox prompt").fill("Return the fixture result");
    await page.getByLabel("Output file paths").fill("result.txt");
    await page.getByRole("button", { name: "Start task", exact: true }).click();
    await expect(page.getByText("Sandbox task created.", { exact: true })).toBeVisible();
    await expect.poll(() => existsSync(marker)).toBe(true);
    await page.getByRole("button", { name: "Open task", exact: true }).click();
    await expect(page.getByText("Sandbox cleanup: confirmed", { exact: true })).toBeVisible();
    await expect(page.getByText("result.txt · 3 bytes", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Preview", exact: true }).click();
    await expect(page.getByText("42\n", { exact: true })).toBeVisible();
    await page.screenshot({ path: info.outputPath("sandbox-result-card-preview.png") });
  } finally {
    await workspace.cleanup();
  }
});
