# Sandbox security baseline

Secret scanning, dependency review, Dependabot, and Actions CodeQL for the sandbox workflows. QA JavaScript and Rust have separate CodeQL configurations and workflows; the security workflow does not install QA dependencies or compile Rust. Paseo source is outside these CodeQL scopes.

## Commands

Full-history secret scan. CI and a local shell use the same entrypoint. The checkout must not be shallow.

```bash
bash .github/scripts/sandbox-gitleaks.sh
```

Local prerequisites: Bash, Git, curl, tar (or unzip for Windows archives), and GNU coreutils (`sha256sum`). macOS users must install GNU coreutils before running this script; stock macOS does not provide `sha256sum`. Linux CI supplies these utilities. Download and scan errors fail closed.

The script downloads gitleaks 8.30.1, checks the archive SHA-256 against pins in the script, and does not fetch `checksums.txt`. It does not use gitleaks-action. The scan command is:

```text
gitleaks git --redact=100 --no-banner --exit-code=1 --log-opts="--all --full-history --diff-merges=separate" --gitleaks-ignore-path .gitleaksignore
```

`--all` alone omits merge diffs, so `--diff-merges=separate` stays. Temporary files use `mktemp -d` under `RUNNER_TEMP`, `TMPDIR`, or `/tmp`, and cleanup removes only a `sandbox-gitleaks.*` directory.

There is no local CodeQL or dependency-review command in this change. Actions CodeQL and dependency review run in `.github/workflows/sandbox-security.yml`; QA and Rust CodeQL run in their separate workflows.

## Scan scope

| Check             | When                                                                 | Scope                                                                                                                                                                                                                                                                                                                                                                  |
| ----------------- | -------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Gitleaks          | `pull_request`, `push` to `main`, `merge_group`, `workflow_dispatch` | Every ref in the checkout. No path filter, so the job can be required. Default rules only.                                                                                                                                                                                                                                                                             |
| Dependency review | `pull_request` only                                                  | Manifest diff for that pull request. `fail-on-severity: low`, scopes `runtime` and `development`, `warn-only: false`, no pull-request comment. GitHub treats a skipped `merge_group` job as successful even though it performed no review; do not require this PR-only job as merge-queue coverage.                                                                    |
| CodeQL (actions)  | Same triggers as Gitleaks                                            | Language `actions`, `build-mode: none`, query suite `security-extended`. Quoted path `.github/workflows/sandbox-*.yml` includes quality, security, metadata, and Rust workflows, not their source trees.                                                                                                                                                               |
| Dependabot        | Weekly                                                               | `github-actions` at `/`, npm at `/circus-sandbox/qa`, and Cargo at `/circus-sandbox`. Actions updates can involve any workflow, including `paseo-ci.yml`; this configuration does not edit it or approve or merge PRs. Commit-message prefixes are `ci` for Actions and `chore` for npm/Cargo, all with `include: scope` for conventional `deps` or `deps-dev` titles. |

The Actions CodeQL configuration does not select `javascript-typescript` or Rust source. Its `paths-ignore` still excludes `circus-paseo`, `circus-sandbox`, `.omp`, and `paseo-ci.yml`. [QA CodeQL](workflows/sandbox-quality.yml) separately uses `.github/codeql/sandbox-qa.yml` for `circus-sandbox/qa`. The real crate and its `Cargo.toml` are present; [Rust CodeQL](workflows/sandbox-rust.yml) is configured in the independent Rust workflow with `.github/codeql/sandbox-rust.yml`, `build-mode: none`, and a sandbox-only Rust source scope.

The quality workflow runs `bash .github/scripts/sandbox-actionlint.sh`: checksum-pinned actionlint checks every `.github/workflows/sandbox-*.yml`, and ShellCheck checks every `.github/scripts/sandbox-*.sh`. Missing file sets fail rather than being skipped. The existing private temporary directory, EXIT cleanup, and failure propagation remain.

The separate [metadata workflow](workflows/sandbox-metadata.yml) runs `PR title` only for `pull_request` events (`opened`, `reopened`, `synchronize`, `edited`), without installing dependencies. It passes the title through `PR_TITLE`, never shell-source interpolation. The accepted grammar is `type: description` or `type(scope): description`: a lowercase ASCII letter starts the type, followed only by lowercase ASCII letters, digits, or hyphens; an optional nonempty scope has no parentheses and may contain Chinese text; the colon has exactly one following space; the description is nonblank and the whole title is single-line. This is a metadata check, not a security scan.

## Fixture exception

`.gitleaksignore` is two fingerprints, not a path or test-directory allowlist. Both are public X25519 low-order points in `circus-paseo/packages/relay/src/crypto.test.ts` at `df74dad5e9e3922c7a71ac238dd2f1306e4916d5`, lines 91 and 92, rule `generic-api-key`. The test rejects them as invalid peer public keys. They are not credentials. A different commit, path, line, rule, or value still fails.

## Review trust boundary

PR workflows and scanner configuration are reviewable code, not an immutable trust root. Green checks alone do not authorize merging changes to the gates themselves. `.github/CODEOWNERS` identifies responsibility for the workflows, scanner, configuration, ownership file, dependency policy, and manifests; it does not by itself enforce approval.

The current repository policy permits authorized automatic merges without a non-author or code-owner approval. The merger must still inspect security-sensitive changes, complete applicable review discussions, and verify the latest CI before using the PR merge path, without administrator bypass or force pushes. Protection settings are mutable repository policy, not properties guaranteed by this file. Required status checks are enabled only after their workflows enter the base branch, avoiding bootstrap deadlocks; their absence must not be represented as successful coverage.

For public fork PRs, GitHub downgrades requested write permissions to read-only; these workflows use `pull_request`, not `pull_request_target`, and never opt into write tokens for forks. This does not prevent the standard CodeQL SARIF integration: GitHub specifically allows code-scanning result uploads when the workflow run is triggered by `pull_request`, including read-only untrusted runs. See [GitHub's code-scanning upload exception](https://docs.github.com/en/code-security/reference/code-scanning/troubleshoot-analysis-errors/resource-not-accessible) and [fork permission calculation](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#how-permissions-are-calculated-for-a-workflow-job). This exception is not general write access or a guarantee that a particular upload succeeded. Contributors with write access to the same repository are trusted workflow publishers, not part of the untrusted-fork boundary; their gate changes still require the merger's substantive review, not an assumed independent approval. Each CodeQL job requests `security-events: write` for its standard SARIF integration, not repository-content write permission, and checkout does not persist credentials. Keep the repository's default workflow permission read-only and workflow self-approval disabled.
