import { access, constants } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { SandboxSetupSnapshot } from "@getpaseo/protocol/sandbox-setup";
import type { RunCommand } from "./command.js";

export interface SandboxEnvironment {
  source: string | null;
  bridge: string | null;
  python: string | null;
  docker: string | null;
  endpoint: string | null;
  image: string | null;
  modelCaFile: string | null;
  trustedTls: boolean;
  checks: SandboxSetupSnapshot["checks"];
}

// Fully qualify built-in image tags because some local Engines fail short-name inspection.
export const CODEX_IMAGE = "docker.io/library/circus-agent-codex:0.164.0";
export const CLAUDE_CODE_IMAGE = "docker.io/library/circus-agent-claude-code:2.1.310";
export async function exists(path: string, mode = constants.R_OK): Promise<boolean> {
  try {
    await access(path, mode);
    return true;
  } catch {
    return false;
  }
}

async function firstExecutable(candidates: string[]): Promise<string | null> {
  for (const candidate of candidates) {
    if (await exists(candidate, constants.X_OK)) return candidate;
  }
  return null;
}

async function firstSupportedPython(candidates: string[], run: RunCommand): Promise<string | null> {
  for (const candidate of candidates) {
    const checked = await run({
      executable: candidate,
      args: ["-I", "-c", "import sys; raise SystemExit(sys.version_info < (3, 11))"],
    });
    if (checked.ok) return candidate;
  }
  return null;
}

async function findSource(
  candidates: string[],
  agent: "codex" | "claude-code",
): Promise<string | null> {
  for (let candidate of candidates) {
    for (let depth = 0; depth < 6; depth += 1) {
      if (
        await exists(
          join(
            candidate,
            `circus-sandbox/images/${agent === "claude-code" ? "claude-code" : "codex"}/Dockerfile`,
          ),
        )
      )
        return candidate;
      const parent = dirname(candidate);
      if (parent === candidate) break;
      candidate = parent;
    }
  }
  return null;
}

interface InspectEnvironmentInput {
  platform: string;
  userHome: string;
  sourceCandidates: string[];
  run: RunCommand;
  configured?: {
    python: string;
    backend:
      | { kind: "docker"; endpoint: string; image: string }
      | { kind: "containerd"; image: string };
    model?: { caFile?: string };
    agent?: "codex" | "claude-code";
  };
  configuredBridge?: string;
  binaryDirectories: string[];
  agent?: "codex" | "claude-code";
  agentImage?: string;
}
function defaultImageFor(agent: "codex" | "claude-code"): string {
  return agent === "claude-code" ? CLAUDE_CODE_IMAGE : CODEX_IMAGE;
}

function configuredImage(
  input: InspectEnvironmentInput,
  backend: { kind: "docker"; endpoint: string; image: string } | undefined,
  agent: "codex" | "claude-code",
): string {
  const sameAgent = input.agent === undefined || input.agent === input.configured?.agent;
  if (sameAgent) return backend?.image ?? input.agentImage ?? CODEX_IMAGE;
  return input.agentImage ?? defaultImageFor(agent);
}

function environmentChecks(
  platform: string,
  source: string | null,
  bridge: string | null,
  python: string | null,
  tlsReady: boolean,
  endpoint: string | null,
  image: string | null,
  agent: "codex" | "claude-code",
): SandboxSetupSnapshot["checks"] {
  const agentName = agent === "claude-code" ? "Claude Code" : "Codex";
  const checks: SandboxSetupSnapshot["checks"] = [
    {
      id: "platform",
      label: "Credential storage",
      ready: platform === "darwin",
      detail:
        platform === "darwin"
          ? "macOS Keychain keeps credentials on this computer."
          : "This setup currently requires macOS Keychain. Use the documented host configuration on other systems.",
    },
    {
      id: "source",
      label: "Circus runtime files",
      ready: source !== null,
      detail: source
        ? "Circus source folder detected."
        : "Select the Circus source folder to prepare the runtime.",
    },
    {
      id: "bridge",
      label: "Task runner",
      ready: bridge !== null,
      detail: bridge
        ? "Task runner found."
        : "Prepare the runtime. Rust 1.99.1 must be installed for this source build.",
    },
    {
      id: "python",
      label: "Model connector",
      ready: python !== null,
      detail: python
        ? "Python 3.11 or later found."
        : "Install Python 3.11 or later, then check again.",
    },
    {
      id: "tls",
      label: "Secure model connection",
      ready: tlsReady,
      detail: tlsReady
        ? "Trusted certificates loaded. Model connections require HTTPS verification."
        : "The model connector could not load trusted certificates. Repair Python's certificate store or the configured certificate bundle before saving.",
    },
    {
      id: "docker",
      label: "Sandbox engine",
      ready: endpoint !== null,
      detail: endpoint
        ? "Local Linux Docker Engine is responding."
        : "Install and open Docker Desktop, or start your local Linux Docker Engine, then check again.",
    },
    {
      id: "image",
      label: `${agentName} environment`,
      ready: image !== null,
      detail: image
        ? `Pinned ${agentName} ${agent === "claude-code" ? "image" : "0.164.0 image"} found.`
        : `Prepare the runtime to build the ${agentName} environment. The first build needs internet access.`,
    },
  ];
  return checks;
}

export async function inspectEnvironment(
  input: InspectEnvironmentInput,
): Promise<SandboxEnvironment> {
  const backend = input.configured?.backend;
  if (backend && backend.kind !== "docker")
    throw new Error("Desktop setup only inspects Docker backends.");
  const agent = input.agent ?? input.configured?.agent ?? "codex";
  const source = await findSource(input.sourceCandidates, agent);
  const bridgeCandidates = input.binaryDirectories.map((directory) =>
    join(directory, "circus-stdio-bridge"),
  );
  if (source)
    bridgeCandidates.unshift(
      join(source, "circus-sandbox/target/debug/circus-stdio-bridge"),
      join(source, "circus-sandbox/target/release/circus-stdio-bridge"),
    );
  if (input.configuredBridge) bridgeCandidates.unshift(input.configuredBridge);
  const bridge = await firstExecutable(bridgeCandidates);
  const pythonCandidates = input.binaryDirectories.map((directory) => join(directory, "python3"));
  if (input.configured) pythonCandidates.unshift(input.configured.python);
  const python = await firstSupportedPython(pythonCandidates, input.run);
  const tls = await inspectPythonTrust({
    python,
    platform: input.platform,
    run: input.run,
    caFile: input.configured?.model?.caFile,
  });
  const docker = await firstExecutable(
    input.binaryDirectories.map((directory) => join(directory, "docker")),
  );
  const endpoints = backend
    ? [backend.endpoint]
    : [
        "unix:///var/run/docker.sock",
        `unix://${input.userHome}/.docker/run/docker.sock`,
        `unix://${input.userHome}/.colima/default/docker.sock`,
      ];
  const imageRef = configuredImage(input, backend, agent);
  const { endpoint, image } = await inspectDocker({
    docker,
    endpoints,
    imageRef,
    run: input.run,
  });
  const checks = environmentChecks(
    input.platform,
    source,
    bridge,
    python,
    tls.ready,
    endpoint,
    image,
    agent,
  );
  return {
    source,
    bridge,
    python,
    docker,
    endpoint,
    image,
    modelCaFile: tls.caFile,
    trustedTls: tls.ready,
    checks,
  };
}

interface InspectDockerInput {
  docker: string | null;
  endpoints: string[];
  imageRef: string;
  run: RunCommand;
}
async function inspectDocker({ docker, endpoints, imageRef, run }: InspectDockerInput) {
  let endpoint: string | null = null;
  let image: string | null = null;
  if (!docker) return { endpoint, image };
  for (const candidate of endpoints) {
    if (!candidate.startsWith("unix:///")) continue;
    const inspected = await run({
      executable: docker,
      args: ["--host", candidate, "info", "--format", "{{.OSType}}"],
      timeoutMs: 3000,
    });
    if (inspected.ok && inspected.stdout.trim() === "linux") {
      endpoint = candidate;
      break;
    }
  }
  if (!endpoint) return { endpoint, image };
  const inspected = await run({
    executable: docker,
    args: ["--host", endpoint, "image", "inspect", "--format", "{{.Id}}", imageRef],
  });
  if (inspected.ok && /^sha256:[a-f0-9]{64}$/.test(inspected.stdout.trim()))
    image = inspected.stdout.trim();
  return { endpoint, image };
}

interface PythonTrustInput {
  python: string | null;
  platform: string;
  run: RunCommand;
  caFile?: string;
}

export async function inspectPythonTrust(input: PythonTrustInput) {
  const { python, caFile: configuredCa } = input;
  if (!python) return { ready: false, caFile: configuredCa ?? null };
  const verify = async (caFile: string | null) =>
    (
      await input.run({
        executable: python,
        args: [
          "-I",
          "-c",
          "import ssl,sys; context=ssl.create_default_context(cafile=sys.argv[1] or None); raise SystemExit(not context.get_ca_certs())",
          caFile ?? "",
        ],
      })
    ).ok;
  if (configuredCa) return { ready: await verify(configuredCa), caFile: configuredCa };
  if (await verify(null)) return { ready: true, caFile: null };
  const systemCa = "/etc/ssl/cert.pem";
  if (input.platform === "darwin" && (await verify(systemCa)))
    return { ready: true, caFile: systemCa };
  return { ready: false, caFile: null };
}
