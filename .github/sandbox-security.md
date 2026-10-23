# Sandbox security baseline

Secret scanning, dependency review, Dependabot, and CodeQL for this repository's security workflow. This baseline does not install or scan `circus-sandbox/qa`. It does not edit `AGENTS.md`, `paseo-ci.yml`, or the quality workflow.

## Commands

Full-history secret scan. CI and a local shell use the same entrypoint. The checkout must not be shallow.

```bash
bash .github/scripts/sandbox-gitleaks.sh
```

The script downloads gitleaks 8.30.1, checks the archive SHA-256 against pins in the script, and does not fetch `checksums.txt`. It does not use gitleaks-action. The scan command is:

```text
gitleaks git --redact=100 --no-banner --exit-code=1 --log-opts="--all --full-history --diff-merges=separate" --gitleaks-ignore-path .gitleaksignore
```

`--all` alone omits merge diffs, so `--diff-merges=separate` stays. Temporary files use `mktemp -d` under `RUNNER_TEMP`, `TMPDIR`, or `/tmp`, and cleanup removes only a `sandbox-gitleaks.*` directory.

There is no local CodeQL or dependency-review command in this change. Those jobs run only in `.github/workflows/sandbox-security.yml`.

## Scan scope

| Check | When | Scope |
| --- | --- | --- |
| Gitleaks | `pull_request`, `push` to `main`, `merge_group`, `workflow_dispatch` | Every ref in the checkout. No path filter, so the job can be required. Default rules only. |
| Dependency review | `pull_request` only | Manifest diff for that pull request. `fail-on-severity: low`, scopes `runtime` and `development`, `warn-only: false`, no pull-request comment. A skipped run on `merge_group` does not pass, so do not require this job there. |
| CodeQL | Same triggers as Gitleaks | Language `actions`, `build-mode: none`, query suite `security-extended`. Path is `.github/workflows/sandbox-security.yml` only. |
| Dependabot | Weekly | `package-ecosystem: github-actions` at `/` only. It can open update PRs for actions used by any workflow, including `paseo-ci.yml`. It does not edit that workflow and does not approve or merge. |

CodeQL does not use `javascript-typescript`. This baseline has no `circus-sandbox/qa` source, and a JavaScript scan would either see nothing useful or analyze `circus-paseo`. `paths-ignore` also excludes `circus-paseo`, `circus-sandbox`, `.omp`, and `paseo-ci.yml`.

Rust CodeQL is not configured here. It belongs in a separate change, after a crate with `Cargo.toml` exists.

## Fixture exception

`.gitleaksignore` is two fingerprints, not a path or test-directory allowlist. Both are public X25519 low-order points in `circus-paseo/packages/relay/src/crypto.test.ts` at `df74dad5e9e3922c7a71ac238dd2f1306e4916d5`, lines 91 and 92, rule `generic-api-key`. The test rejects them as invalid peer public keys. They are not credentials. A different commit, path, line, rule, or value still fails.
