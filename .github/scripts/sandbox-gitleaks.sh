#!/usr/bin/env bash
# Scan every ref in this Git checkout with the open-source gitleaks binary.
# CI and a local shell use the same entrypoint:
#   bash .github/scripts/sandbox-gitleaks.sh
# Does not use gitleaks-action (organization license). Default rules only.
# .gitleaksignore allows two exact fingerprints; it does not exclude a path.
set -euo pipefail

version="8.30.1"
# SHA-256 lines copied from the official release asset
# gitleaks_8.30.1_checksums.txt on 2026-10-24:
# https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_checksums.txt
# Runtime compares the downloaded archive to these pins. It does not fetch
# checksums.txt (that file is published beside the binary and is not a second trust root).

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if ! root="$(git -C "${script_dir}" rev-parse --show-toplevel)"; then
  echo "sandbox-gitleaks: ${script_dir} is not inside a git checkout" >&2
  exit 1
fi
cd "${root}"

if [[ "$(git rev-parse --is-shallow-repository)" == "true" ]]; then
  echo "sandbox-gitleaks: shallow clone cannot satisfy a full-history scan" >&2
  exit 1
fi

os="$(uname -s)"
arch="$(uname -m)"
binary="gitleaks"
case "${os}:${arch}" in
  Linux:x86_64)
    archive="gitleaks_${version}_linux_x64.tar.gz"
    sum="551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb"
    ;;
  Linux:aarch64)
    archive="gitleaks_${version}_linux_arm64.tar.gz"
    sum="e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080"
    ;;
  Darwin:arm64)
    archive="gitleaks_${version}_darwin_arm64.tar.gz"
    sum="b40ab0ae55c505963e365f271a8d3846efbc170aa17f2607f13df610a9aeb6a5"
    ;;
  Darwin:x86_64)
    archive="gitleaks_${version}_darwin_x64.tar.gz"
    sum="dfe101a4db2255fc85120ac7f3d25e4342c3c20cf749f2c20a18081af1952709"
    ;;
  MINGW*:x86_64 | MSYS*:x86_64 | CYGWIN*:x86_64)
    archive="gitleaks_${version}_windows_x64.zip"
    sum="d29144deff3a68aa93ced33dddf84b7fdc26070add4aa0f4513094c8332afc4e"
    binary="gitleaks.exe"
    ;;
  *)
    echo "sandbox-gitleaks: unsupported platform ${os}:${arch}" >&2
    exit 1
    ;;
esac

if [[ ! "${sum}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "sandbox-gitleaks: pinned checksum is not 64 hex characters" >&2
  exit 1
fi

if ! command -v sha256sum >/dev/null 2>&1; then
  echo "sandbox-gitleaks: sha256sum is required" >&2
  exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "sandbox-gitleaks: curl is required" >&2
  exit 1
fi

workdir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sandbox-gitleaks.XXXXXXXX")"
cleanup() {
  local status=$?
  local dir="${workdir:-}"
  workdir=""
  if [[ -z "${dir}" ]]; then
    exit "${status}"
  fi
  case "${dir}" in
    */sandbox-gitleaks.*) ;;
    *)
      echo "sandbox-gitleaks: refusing to remove unexpected directory ${dir}" >&2
      if [[ "${status}" -eq 0 ]]; then
        status=1
      fi
      exit "${status}"
      ;;
  esac
  if ! rm -rf -- "${dir}"; then
    echo "sandbox-gitleaks: failed to remove ${dir}" >&2
    if [[ "${status}" -eq 0 ]]; then
      status=1
    fi
  fi
  exit "${status}"
}
trap cleanup EXIT
dest="${workdir}/${archive}"
url="https://github.com/gitleaks/gitleaks/releases/download/v${version}/${archive}"

curl --fail --silent --show-error --location \
  --proto '=https' --tlsv1.2 \
  --retry 3 --retry-connrefused \
  --output "${dest}" \
  "${url}"

if [[ ! -s "${dest}" ]]; then
  echo "sandbox-gitleaks: download is empty: ${url}" >&2
  exit 1
fi

actual="$(sha256sum "${dest}")"
actual="${actual%% *}"
if [[ "${actual}" != "${sum}" ]]; then
  echo "sandbox-gitleaks: checksum mismatch for ${archive}" >&2
  echo "sandbox-gitleaks: expected ${sum}" >&2
  echo "sandbox-gitleaks: actual   ${actual}" >&2
  exit 1
fi

printf '%s  %s\n' "${sum}" "${archive}" >"${workdir}/SHA256SUMS"
(
  cd "${workdir}"
  sha256sum --check --strict SHA256SUMS
)

if [[ "${archive}" == *.zip ]]; then
  if ! command -v unzip >/dev/null 2>&1; then
    echo "sandbox-gitleaks: unzip is required" >&2
    exit 1
  fi
  unzip -o -q "${dest}" -d "${workdir}"
else
  tar --extract --gzip --file "${dest}" --directory "${workdir}" "${binary}"
fi

tool="${workdir}/${binary}"
if [[ ! -f "${tool}" ]]; then
  echo "sandbox-gitleaks: archive did not contain ${binary}" >&2
  exit 1
fi
chmod +x "${tool}"
raw="$("${tool}" version)"
got="${raw%%$'\n'*}"
got="${got%$'\r'}"
case "${got}" in
  8.30.1 | v8.30.1) ;;
  *)
    echo "sandbox-gitleaks: binary version '${got}' != ${version}" >&2
    exit 1
    ;;
esac

# Include every ref and merge-only patches. --all alone omits merge diffs.
# Findings and scan errors both exit non-zero (gitleaks v8.30.1 findingSummaryAndExit).
"${tool}" git \
  --redact=100 \
  --no-banner \
  --exit-code=1 \
  --log-opts="--all --full-history --diff-merges=separate" \
  --gitleaks-ignore-path "${root}/.gitleaksignore" \
  "${root}"
