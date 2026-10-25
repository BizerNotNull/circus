import { mkdtemp, mkdir, readFile, rm, stat, symlink, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, test } from "vitest";
import { createSandboxSetup } from "./setup.js";
import type { CommandInput, RunCommand } from "./command.js";

const roots: string[] = [];
afterEach(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "circus-desktop-setup-"));
  roots.push(root);
  const home = join(root, "home");
  const bin = join(root, "bin");
  await mkdir(home);
  await mkdir(bin);
  await Promise.all(
    ["docker", "python3", "circus-stdio-bridge"].map((name) =>
      writeFile(join(bin, name), "", { mode: 0o700 }),
    ),
  );
  await writeFile(
    join(home, "config.json"),
    JSON.stringify({
      daemon: { listen: "127.0.0.1:19001" },
      agents: { providers: { codex: { enabled: false } } },
    }),
  );
  const secrets = new Map<string, string>();
  const commands: CommandInput[] = [];
  const requests: RequestInit[] = [];
  let reloads = 0;
  let keychainWritable = true;
  const run: RunCommand = async (command) => {
    commands.push(command);
    const args = command.args;
    if (command.executable === "/usr/bin/security") {
      if (args[0] === "-i") {
        if (!keychainWritable) return { ok: true, stdout: "" };
        const values = command.input?.match(/"(?:\\.|[^"\\])*"/g);
        if (!values || values.length !== 3) throw new Error("Invalid Keychain input");
        const decoded = values.map((value) => String(JSON.parse(value)));
        secrets.set(`${decoded[0]}:${decoded[1]}`, decoded[2]!);
        return { ok: true, stdout: "" };
      }
      const key = `${args[2]}:${args[4]}`;
      if (args[0] === "delete-generic-password") {
        secrets.delete(key);
        return { ok: true, stdout: "" };
      }
      const secret = secrets.get(key);
      return { ok: secret !== undefined, stdout: secret === undefined ? "" : `${secret}\n` };
    }
    if (args.includes("info")) return { ok: true, stdout: "linux\n" };
    if (args.includes("inspect")) return { ok: true, stdout: `sha256:${"a".repeat(64)}\n` };
    return { ok: true, stdout: "" };
  };
  const dependencies = {
    home,
    userHome: root,
    platform: "darwin",
    sourceCandidates: [],
    binaryDirectories: [bin],
    run,
    reload: async () => {
      reloads += 1;
    },
    request: async (_url: string | URL | Request, init?: RequestInit) => {
      if (init) requests.push(init);
      const rpc = JSON.parse(String(init?.body));
      if (rpc.method === "notifications/initialized") return new Response(null, { status: 202 });
      if (rpc.method === "initialize")
        return new Response(
          JSON.stringify({
            jsonrpc: "2.0",
            id: rpc.id,
            result: { protocolVersion: "2025-03-26", capabilities: { tools: {} } },
          }),
        );
      if (rpc.method === "tools/list")
        return new Response(
          JSON.stringify({
            jsonrpc: "2.0",
            id: rpc.id,
            result: {
              tools: [
                {
                  name: "issue_read",
                  inputSchema: {
                    type: "object",
                    properties: { method: {}, owner: {}, repo: {}, issue_number: {} },
                  },
                },
              ],
            },
          }),
        );
      return new Response(
        JSON.stringify({ id: "resp_fixture", object: "response", status: "completed", output: [] }),
        {
          status: 200,
        },
      );
    },
  };
  const service = createSandboxSetup(dependencies);
  return {
    service,
    home,
    secrets,
    commands,
    requests,
    dependencies,
    reloads: () => reloads,
    refuseKeychain: () => {
      keychainWritable = false;
    },
  };
}

const model = {
  baseUrl: "https://model.example/v1",
  model: "fixture-model",
  apiKey: 'fixture-only-secret\\with"quote',
  preset: "compact" as const,
};

test("commits the runner and Keychain credential after successful save", async () => {
  const f = await fixture();
  await expect(f.service.save({ ...model, agent: "claude-code" })).resolves.toMatchObject({
    ok: true,
  });

  const persisted = JSON.parse(await readFile(join(f.home, "config.json"), "utf8"));
  const runnerPath = persisted.agents.providers.sandbox.env.CIRCUS_RUNNER_CONFIG;
  const runner = JSON.parse(await readFile(runnerPath, "utf8"));
  expect(runner.agent).toBe("claude-code");
  expect(
    f.secrets.get(`${runner.model.apiKeyKeychain.service}:${runner.model.apiKeyKeychain.account}`),
  ).toBe(model.apiKey);
  expect(await f.service.snapshot()).toMatchObject({
    settings: { agent: "claude-code", hasCredential: true, credentialStorage: "keychain" },
  });
});
test("saves a usable provider without storing a credential in config, argv, or snapshot", async () => {
  const f = await fixture();
  expect((await f.service.snapshot()).ready).toBe(true);
  expect(await f.service.save(model)).toMatchObject({ ok: true });
  const persistedText = await readFile(join(f.home, "config.json"), "utf8");
  const persisted = JSON.parse(persistedText);
  expect(persisted.daemon.listen).toBe("127.0.0.1:19001");
  expect(persisted.agents.providers.codex.enabled).toBe(false);
  expect(persisted.agents.providers.sandbox.command[0]).toMatch(/circus-stdio-bridge$/);
  expect(persisted.agents.providers.sandbox.env.CIRCUS_SANDBOX_STATE_DIR).toBe(
    join(f.home, "circus/sandbox-state"),
  );
  const runnerPath = persisted.agents.providers.sandbox.env.CIRCUS_RUNNER_CONFIG;
  const runner = JSON.parse(await readFile(runnerPath, "utf8"));
  expect(runner.backend).toEqual({
    kind: "docker",
    endpoint: "unix:///var/run/docker.sock",
    image: `sha256:${"a".repeat(64)}`,
    runtime: "runc",
  });
  expect(runner.image).toEqual({
    platform: { os: "linux", architecture: "amd64" },
    pullPolicy: "local-only",
    prepareTimeoutMs: 30000,
  });
  expect(runner).not.toHaveProperty("dockerEndpoint");
  expect(runner).not.toHaveProperty("agentImage");
  expect(runner.model.apiKeyFile).toBeUndefined();
  expect(
    f.secrets.get(`${runner.model.apiKeyKeychain.service}:${runner.model.apiKeyKeychain.account}`),
  ).toBe(model.apiKey);
  // Windows stat mode bits do not model ACLs; the credential stays in Keychain.
  if (process.platform !== "win32") expect((await stat(runnerPath)).mode & 0o777).toBe(0o600);
  expect(
    JSON.stringify([
      persisted,
      runner,
      await f.service.snapshot(),
      f.commands.map((command) => command.args),
    ]),
  ).not.toContain("fixture-only-secret");
  expect(f.reloads()).toBe(1);
});

test("prepares a first-run runtime while the model fields are still blank", async () => {
  const f = await fixture();
  const source = join(f.home, "source");
  await mkdir(join(source, "circus-sandbox/images/codex"), { recursive: true });
  await writeFile(join(source, "circus-sandbox/images/codex/Dockerfile"), "");
  await f.service.selectSource(source);
  await expect(
    f.service.prepare({ agent: "codex", baseUrl: "", model: "", preset: "compact" }),
  ).resolves.toMatchObject({ ok: true });
});

test("saving Codex after Claude preparation keeps snapshots on the persisted agent", async () => {
  const f = await fixture();
  const source = join(f.home, "source");
  for (const agent of ["codex", "claude-code"]) {
    const imageDirectory = join(source, `circus-sandbox/images/${agent}`);
    await mkdir(imageDirectory, { recursive: true });
    await writeFile(join(imageDirectory, "Dockerfile"), "");
  }
  await f.service.selectSource(source);
  await expect(
    f.service.prepare({ agent: "claude-code", baseUrl: "", model: "", preset: "compact" }),
  ).resolves.toMatchObject({ ok: true });
  await expect(f.service.save({ ...model, agent: "codex" })).resolves.toMatchObject({ ok: true });
  await expect(f.service.snapshot()).resolves.toMatchObject({ settings: { agent: "codex" } });

  await expect(
    f.service.save({ ...model, agent: "codex", apiKey: "", model: "updated-codex-model" }),
  ).resolves.toMatchObject({ ok: true });
  await expect(f.service.snapshot()).resolves.toMatchObject({
    settings: { agent: "codex", model: "updated-codex-model" },
  });
});

test("resaves an existing Claude Code configuration", async () => {
  const f = await fixture();
  const claudeModel = { ...model, agent: "claude-code" as const };
  await f.service.save(claudeModel);
  const reloaded = createSandboxSetup(f.dependencies);
  await expect(
    reloaded.save({ ...claudeModel, apiKey: "", model: "updated-claude-model" }),
  ).resolves.toMatchObject({ ok: true });
  const runner = JSON.parse(await readFile(join(f.home, "circus/runner.json"), "utf8"));
  expect(runner.agent).toBe("claude-code");
  expect(runner.model.model).toBe("updated-claude-model");
});

test("reloads saved settings and uses the stored credential for model checks and updates", async () => {
  const f = await fixture();
  await f.service.save(model);
  const reloaded = createSandboxSetup(f.dependencies);
  expect(await reloaded.snapshot()).toMatchObject({
    ready: true,
    settings: {
      baseUrl: model.baseUrl,
      model: model.model,
      preset: "compact",
      hasCredential: true,
      credentialStorage: "keychain",
    },
  });
  expect(await reloaded.testConnection({ ...model, apiKey: "" })).toMatchObject({ ok: true });
  expect(f.requests[0]?.headers).toMatchObject({ Authorization: `Bearer ${model.apiKey}` });
  expect(JSON.parse(String(f.requests[0]?.body)).model).toBe(model.model);
  expect(
    await reloaded.save({ ...model, apiKey: "", model: "updated-model", preset: "balanced" }),
  ).toMatchObject({ ok: true });
  expect(await createSandboxSetup(f.dependencies).snapshot()).toMatchObject({
    settings: { model: "updated-model", preset: "balanced", credentialStorage: "keychain" },
  });
});

test("inspects only the configured Docker endpoint and image after reload", async () => {
  const f = await fixture();
  await f.service.save(model);
  const path = join(f.home, "circus/runner.json");
  const config = JSON.parse(await readFile(path, "utf8"));
  const endpoint = "unix:///custom/docker.sock";
  const image = `sha256:${"b".repeat(64)}`;
  config.backend.endpoint = endpoint;
  config.backend.image = image;
  await writeFile(path, JSON.stringify(config));
  const previous = await readFile(path, "utf8");
  const keys = [...f.secrets.keys()];
  const run = f.dependencies.run;
  const dockerCommands: CommandInput[] = [];
  let available = true;
  const service = createSandboxSetup({
    ...f.dependencies,
    run: async (input) => {
      if (input.args.includes("info") || input.args.includes("inspect")) {
        dockerCommands.push(input);
        if (input.args[input.args.indexOf("--host") + 1] !== endpoint || !available)
          return { ok: false, stdout: "" };
        if (input.args.includes("inspect"))
          return { ok: input.args.at(-1) === image, stdout: `${image}\n` };
      }
      return run(input);
    },
  });
  expect((await service.snapshot()).ready).toBe(true);
  expect(dockerCommands.some((command) => command.args.at(-1) === image)).toBe(true);
  available = false;
  expect((await service.snapshot()).ready).toBe(false);
  await expect(service.save({ ...model, apiKey: "" })).rejects.toThrow();
  expect(dockerCommands.every((command) => command.args.includes(endpoint))).toBe(true);
  expect(await readFile(path, "utf8")).toBe(previous);
  expect([...f.secrets.keys()]).toEqual(keys);
});

interface MutableRunnerConfigFixture {
  backend: {
    kind: string;
    endpoint: string;
    image: string;
    socket?: string;
  };
  image: {
    prepareTimeoutMs: number;
    pullPolicy: string;
  };
  model: { agentImage?: string };
  dockerEndpoint?: string;
  agentImage?: string;
}

test.each([
  [
    "legacy backend fields",
    (config: MutableRunnerConfigFixture) => {
      config.dockerEndpoint = config.backend.endpoint;
      config.agentImage = config.backend.image;
      const malformed: Partial<MutableRunnerConfigFixture> = config;
      delete malformed.backend;
      delete malformed.image;
    },
  ],
  [
    "obsolete aliases alongside tagged backend",
    (config: MutableRunnerConfigFixture) => {
      config.dockerEndpoint = config.backend.endpoint;
      config.agentImage = config.backend.image;
    },
  ],
  [
    "unknown backend kind",
    (config: MutableRunnerConfigFixture) => {
      config.backend.kind = "unknown";
    },
  ],
  [
    "unexpected backend field",
    (config: MutableRunnerConfigFixture) => {
      config.backend.socket = "/run/containerd/containerd.sock";
    },
  ],
  [
    "missing preparation settings",
    (config: MutableRunnerConfigFixture) => {
      const malformed: Partial<MutableRunnerConfigFixture> = config;
      delete malformed.image;
    },
  ],
  [
    "invalid preparation deadline",
    (config: MutableRunnerConfigFixture) => {
      config.image.prepareTimeoutMs = 0;
    },
  ],
  [
    "invalid pull policy",
    (config: MutableRunnerConfigFixture) => {
      config.image.pullPolicy = "always";
    },
  ],
  [
    "unexpected model field",
    (config: MutableRunnerConfigFixture) => {
      config.model.agentImage = "obsolete";
    },
  ],
] as const)("preserves malformed configuration: %s", async (_name, mutate) => {
  const f = await fixture();
  await f.service.save(model);
  const path = join(f.home, "circus/runner.json");
  const config: MutableRunnerConfigFixture = JSON.parse(await readFile(path, "utf8"));
  mutate(config);
  const serialized = JSON.stringify(config);
  await writeFile(path, serialized);
  const provider = await readFile(join(f.home, "config.json"), "utf8");
  const keys = [...f.secrets.keys()];
  const reloaded = createSandboxSetup(f.dependencies);
  await expect(reloaded.snapshot()).rejects.toThrow();
  await expect(reloaded.save(model)).rejects.toThrow();
  await expect(reloaded.testConnection({ ...model, apiKey: "" })).rejects.toThrow();
  expect(await readFile(path, "utf8")).toBe(serialized);
  expect(await readFile(join(f.home, "config.json"), "utf8")).toBe(provider);
  expect([...f.secrets.keys()]).toEqual(keys);
  expect(f.requests).toHaveLength(0);
});

test("does not reinterpret or overwrite a configured containerd backend", async () => {
  const f = await fixture();
  await f.service.save(model);
  const path = join(f.home, "circus/runner.json");
  const config = JSON.parse(await readFile(path, "utf8"));
  config.backend = {
    kind: "containerd",
    socket: "/run/containerd/containerd.sock",
    namespace: "circus",
    image: "docker.io/library/circus-agent-codex:0.164.0",
    runtime: "io.containerd.runc.v2",
    snapshotter: "overlayfs",
  };
  const serialized = JSON.stringify(config);
  await writeFile(path, serialized);
  const provider = await readFile(join(f.home, "config.json"), "utf8");
  const keys = [...f.secrets.keys()];
  f.commands.length = 0;
  const service = createSandboxSetup(f.dependencies);
  await expect(service.snapshot()).rejects.toThrow();
  await expect(service.prepare()).rejects.toThrow();
  await expect(service.save(model)).rejects.toThrow();
  await expect(service.testConnection(model)).rejects.toThrow();
  await expect(
    service.saveGithub({
      id: "github",
      url: "https://mcp.example/mcp/",
      token: "fixture-token",
      owner: "team",
      repo: "approved",
    }),
  ).rejects.toThrow();
  expect(await readFile(path, "utf8")).toBe(serialized);
  expect(await readFile(join(f.home, "config.json"), "utf8")).toBe(provider);
  expect([...f.secrets.keys()]).toEqual(keys);
  expect(f.commands).toHaveLength(0);
  expect(f.requests).toHaveLength(0);
});

test("does not trust security's zero exit code without reading back the saved key", async () => {
  const f = await fixture();
  f.refuseKeychain();
  await expect(f.service.save(model)).rejects.toThrow("Could not save the credential");
  expect(
    JSON.parse(await readFile(join(f.home, "config.json"), "utf8")).agents.providers.sandbox,
  ).toBeUndefined();
  expect(f.secrets.size).toBe(0);
});

test("stored credentials cannot be sent to a changed service URL or redirect", async () => {
  const f = await fixture();
  await f.service.save(model);
  await expect(
    f.service.testConnection({ ...model, apiKey: "", baseUrl: "https://other.example/v1" }),
  ).rejects.toThrow("never sent to a different service URL");
  expect(f.requests).toHaveLength(0);
  expect(await f.service.testConnection({ ...model, apiKey: "" })).toMatchObject({ ok: true });
  expect(f.requests[0]?.redirect).toBe("error");
});

test("failed config application rolls back the runner and new Keychain item", async () => {
  const f = await fixture();
  await f.service.save(model);
  const runnerPath = join(f.home, "circus/runner.json");
  const previous = await readFile(runnerPath, "utf8");
  const keys = [...f.secrets.keys()];
  const originalRun = f.dependencies.run;
  const service = createSandboxSetup({
    ...f.dependencies,
    run: async (input) => {
      const result = await originalRun(input);
      if (input.args[0] === "find-generic-password") {
        const config = JSON.parse(await readFile(join(f.home, "config.json"), "utf8"));
        config.agents.providers.sandbox.label = "Changed concurrently";
        await writeFile(join(f.home, "config.json"), JSON.stringify(config));
      }
      return result;
    },
  });
  await expect(service.save({ ...model, model: "another-model" })).rejects.toThrow(
    "changed while saving",
  );
  expect(await readFile(runnerPath, "utf8")).toBe(previous);
  expect([...f.secrets.keys()]).toEqual(keys);
});

test("unsupported platforms and malformed service addresses fail before credentials are written", async () => {
  const f = await fixture();
  const unsupported = createSandboxSetup({ ...f.dependencies, platform: "linux" });
  await expect(unsupported.save(model)).rejects.toThrow("macOS Keychain");
  for (const baseUrl of [
    "http://model.example/v1",
    "https://user:pass@model.example/v1",
    "https://model.example/v1?token=unsafe",
  ]) {
    await expect(f.service.save({ ...model, baseUrl })).rejects.toThrow("HTTPS URL");
  }
  expect(f.secrets.size).toBe(0);
});

test("preserves configured MCP policies and unrelated model settings on credential migration", async () => {
  const f = await fixture();
  await f.service.save(model);
  const runnerPath = join(f.home, "circus/runner.json");
  const runner = JSON.parse(await readFile(runnerPath, "utf8"));
  runner.mcpConnections = [{ id: "existing", url: "https://mcp.example", tools: [] }];
  runner.model.caFile = "/trusted/certificate.pem";
  await writeFile(runnerPath, JSON.stringify(runner));
  await f.service.save({ ...model, apiKey: "" });
  const updated = JSON.parse(await readFile(runnerPath, "utf8"));
  expect(updated.mcpConnections).toEqual(runner.mcpConnections);
  expect(updated.model.caFile).toBe("/trusted/certificate.pem");
});

test("verifies a GitHub connection then saves a host-enforced repository policy and Keychain reference", async () => {
  const f = await fixture();
  await f.service.save(model);
  const github = {
    id: "github",
    url: "https://mcp.example/mcp/",
    token: "fixture-github-token",
    owner: "team",
    repo: "approved",
  };
  expect(await f.service.saveGithub(github)).toMatchObject({ ok: true });
  const serialized = await readFile(join(f.home, "circus/runner.json"), "utf8");
  const runner = JSON.parse(serialized);
  expect(runner.mcpConnections).toHaveLength(1);
  const connection = runner.mcpConnections[0];
  expect(connection.url).toBe(github.url);
  expect(connection.tools).toEqual([
    {
      name: "issue_read",
      allowedArguments: ["issue_number"],
      fixedArguments: { method: "get", owner: "team", repo: "approved" },
    },
  ]);
  expect(serialized).not.toContain(github.token);
  expect(
    f.secrets.get(`${connection.headersKeychain.service}:${connection.headersKeychain.account}`),
  ).toBe(JSON.stringify({ Authorization: `Bearer ${github.token}` }));
  await expect(f.service.saveGithub(github)).rejects.toThrow("already exists");
  expect(
    JSON.parse(await readFile(join(f.home, "circus/runner.json"), "utf8")).mcpConnections,
  ).toEqual(runner.mcpConnections);
});

test("failed MCP authentication creates neither a connection nor a Keychain entry", async () => {
  const f = await fixture();
  await f.service.save(model);
  const keys = [...f.secrets.keys()];
  const service = createSandboxSetup({
    ...f.dependencies,
    request: async () => new Response("private failure", { status: 403 }),
  });
  expect(
    await service.saveGithub({
      id: "github",
      url: "https://mcp.example/mcp/",
      token: "fixture-github-token",
      owner: "team",
      repo: "approved",
    }),
  ).toMatchObject({ ok: false });
  expect([...f.secrets.keys()]).toEqual(keys);
  expect(
    JSON.parse(await readFile(join(f.home, "circus/runner.json"), "utf8")).mcpConnections,
  ).toBeUndefined();
});

test.each([
  "<html>login required</html>",
  JSON.stringify({
    id: "resp_failed",
    object: "response",
    status: "failed",
    output: [],
    error: { message: "private provider error" },
  }),
  "x".repeat(65537),
])("does not report a successful model check for an invalid 200 response", async (body) => {
  const f = await fixture();
  const service = createSandboxSetup({
    ...f.dependencies,
    request: async () => new Response(body),
  });
  const result = await service.testConnection(model);
  expect(result.ok).toBe(false);
  expect(result.message).not.toContain("private provider error");
});

test.skipIf(process.platform === "win32")(
  "refuses FIFO and final symlink configuration paths without blocking",
  async () => {
    const f = await fixture();
    await f.service.save(model);
    const path = join(f.home, "circus/runner.json");
    const original = await readFile(path, "utf8");
    await rm(path);
    execFileSync("mkfifo", [path]);
    await expect(f.service.snapshot()).rejects.toThrow("configuration could not be read");
    await rm(path);
    const target = join(f.home, "original.json");
    await writeFile(target, original);
    await symlink(target, path);
    await expect(f.service.snapshot()).rejects.toThrow("configuration could not be read");
  },
);

test("uses a verified macOS certificate bundle when Python has no default trust store", async () => {
  const f = await fixture();
  const run = f.dependencies.run;
  const service = createSandboxSetup({
    ...f.dependencies,
    run: async (input) => {
      if (input.args[2]?.includes("ssl.create_default_context"))
        return { ok: input.args[3] === "/etc/ssl/cert.pem", stdout: "" };
      return run(input);
    },
  });
  expect((await service.snapshot()).checks.find((check) => check.id === "tls")?.ready).toBe(true);
  await service.save(model);
  const runner = JSON.parse(await readFile(join(f.home, "circus/runner.json"), "utf8"));
  expect(runner.model.caFile).toBe("/etc/ssl/cert.pem");
  expect(runner.model.apiKeyKeychain).toBeDefined();
});

test("blocks saving when no trusted certificates can be loaded", async () => {
  const f = await fixture();
  const run = f.dependencies.run;
  const service = createSandboxSetup({
    ...f.dependencies,
    run: async (input) => {
      if (input.args[2]?.includes("ssl.create_default_context")) return { ok: false, stdout: "" };
      return run(input);
    },
  });
  const snapshot = await service.snapshot();
  expect(snapshot.ready).toBe(false);
  expect(snapshot.checks.find((check) => check.id === "tls")?.ready).toBe(false);
  await expect(service.save(model)).rejects.toThrow("environment checks");
  expect(f.secrets.size).toBe(0);
});

test("finds a first-run Codex image when the Engine requires its canonical repository name", async () => {
  const f = await fixture();
  const run = f.dependencies.run;
  const service = createSandboxSetup({
    ...f.dependencies,
    run: async (input) => {
      if (
        input.args.includes("inspect") &&
        input.args.at(-1) !== "docker.io/library/circus-agent-codex:0.164.0"
      ) {
        return { ok: false, stdout: "" };
      }
      return run(input);
    },
  });
  expect((await service.snapshot()).ready).toBe(true);
  await service.save(model);
  const saved = JSON.parse(await readFile(join(f.home, "circus/runner.json"), "utf8"));
  expect(saved.backend.image).toBe(`sha256:${"a".repeat(64)}`);
});

test("MCP selects its own trusted CA without inheriting the model service's private CA", async () => {
  const f = await fixture();
  await f.service.save(model);
  const path = join(f.home, "circus/runner.json");
  const config = JSON.parse(await readFile(path, "utf8"));
  config.model.caFile = "/private/model-only.pem";
  await writeFile(path, JSON.stringify(config));
  const probes: string[] = [];
  const run = f.dependencies.run;
  const service = createSandboxSetup({
    ...f.dependencies,
    run: async (input) => {
      if (input.args[2]?.includes("ssl.create_default_context")) {
        probes.push(input.args[3]!);
        return { ok: input.args[3] === "/etc/ssl/cert.pem", stdout: "" };
      }
      return run(input);
    },
  });
  expect(
    await service.saveGithub({
      id: "github",
      url: "https://mcp.example/mcp/",
      token: "fixture-token",
      owner: "team",
      repo: "repo",
    }),
  ).toMatchObject({ ok: true });
  const saved = JSON.parse(await readFile(path, "utf8"));
  expect(saved.model.caFile).toBe("/private/model-only.pem");
  expect(saved.mcpConnections[0].caFile).toBe("/etc/ssl/cert.pem");
  expect(probes).toEqual(["", "/etc/ssl/cert.pem"]);
});

test("MCP refuses to save if its Python connector cannot load trusted certificates", async () => {
  const f = await fixture();
  await f.service.save(model);
  const keys = [...f.secrets.keys()];
  const run = f.dependencies.run;
  const service = createSandboxSetup({
    ...f.dependencies,
    run: async (input) =>
      input.args[2]?.includes("ssl.create_default_context")
        ? { ok: false, stdout: "" }
        : run(input),
  });
  await expect(
    service.saveGithub({
      id: "github",
      url: "https://mcp.example/mcp/",
      token: "fixture-token",
      owner: "team",
      repo: "repo",
    }),
  ).rejects.toThrow("trusted certificates");
  expect([...f.secrets.keys()]).toEqual(keys);
  expect(f.requests).toHaveLength(0);
});
