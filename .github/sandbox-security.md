# Sandbox security baseline

Secret scanning, dependency review, Dependabot, and CodeQL for this repository's security workflow. This baseline does not install or scan `circus-sandbox/qa`. It does not edit `AGENTS.md`, `paseo-ci.yml`, or the quality workflow.

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

There is no local CodeQL or dependency-review command in this change. Those jobs run only in `.github/workflows/sandbox-security.yml`.

## Scan scope

| Check             | When                                                                 | Scope                                                                                                                                                                                                                                                                                               |
| ----------------- | -------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Gitleaks          | `pull_request`, `push` to `main`, `merge_group`, `workflow_dispatch` | Every ref in the checkout. No path filter, so the job can be required. Default rules only.                                                                                                                                                                                                          |
| Dependency review | `pull_request` only                                                  | Manifest diff for that pull request. `fail-on-severity: low`, scopes `runtime` and `development`, `warn-only: false`, no pull-request comment. GitHub treats a skipped `merge_group` job as successful even though it performed no review; do not require this PR-only job as merge-queue coverage. |
| CodeQL            | Same triggers as Gitleaks                                            | Language `actions`, `build-mode: none`, query suite `security-extended`. Path is `.github/workflows/sandbox-security.yml` only.                                                                                                                                                                     |
| Dependabot        | Weekly                                                               | `package-ecosystem: github-actions` at `/` only. It can open update PRs for actions used by any workflow, including `paseo-ci.yml`. It does not edit that workflow and does not approve or merge.                                                                                                   |

CodeQL does not use `javascript-typescript`. This baseline has no `circus-sandbox/qa` source, and a JavaScript scan would either see nothing useful or analyze `circus-paseo`. `paths-ignore` also excludes `circus-paseo`, `circus-sandbox`, `.omp`, and `paseo-ci.yml`.

Rust CodeQL is not configured here. It belongs in a separate change, after a crate with `Cargo.toml` exists.

## Fixture exception

`.gitleaksignore` is two fingerprints, not a path or test-directory allowlist. Both are public X25519 low-order points in `circus-paseo/packages/relay/src/crypto.test.ts` at `df74dad5e9e3922c7a71ac238dd2f1306e4916d5`, lines 91 and 92, rule `generic-api-key`. The test rejects them as invalid peer public keys. They are not credentials. A different commit, path, line, rule, or value still fails.

## Review trust boundary

PR workflows and scanner configuration are reviewable code, not an immutable trust root. Green checks alone do not authorize merging changes to the gates themselves. `.github/CODEOWNERS` assigns the workflows, scanner, configuration, ownership file, dependency policy, and manifests to the authorized repository owner. Branch protection must require code-owner approval, dismiss stale approvals, enforce the rule for administrators, and disallow force pushes.

GitHub uses CODEOWNERS from the base branch. This bootstrap PR therefore still needs an independent authorized maintainer review before merge; the new ownership policy cannot protect its own initial introduction. Authors cannot approve their own PR. Add another authorized security reviewer to CODEOWNERS when granting reviewer access rather than bypassing approval.

For public fork PRs, GitHub downgrades requested write permissions to read-only; this workflow uses `pull_request`, not `pull_request_target`, and never opts into write tokens for forks. Contributors with write access to the same repository are trusted workflow publishers, not part of the untrusted-fork boundary; their gate changes still require protected, independent review. The CodeQL job alone requests `security-events: write` for its standard SARIF integration. It has no repository-content write permission, and checkout does not persist credentials. Keep the repository's default workflow permission read-only and workflow self-approval disabled. See [GitHub's fork permission calculation](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#how-permissions-are-calculated-for-a-workflow-job).
