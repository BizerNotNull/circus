#!/usr/bin/env bash
# Lint this checkout's sandbox quality workflow with a checksum-pinned actionlint binary.
# CI and a local shell use the same entrypoint:
#   bash .github/scripts/sandbox-actionlint.sh
# Does not lint workflows or scripts that are not in this checkout.
set -euo pipefail

version="1.7.12"
# SHA-256 lines copied from the official release asset
# actionlint_1.7.12_checksums.txt on 2026-10-24:
# https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_checksums.txt

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${root}"

os="$(uname -s)"
arch="$(uname -m)"
binary="actionlint"
case "${os}:${arch}" in
  Linux:x86_64)
    archive="actionlint_${version}_linux_amd64.tar.gz"
    sum="8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"
    ;;
  Linux:aarch64)
    archive="actionlint_${version}_linux_arm64.tar.gz"
    sum="325e971b6ba9bfa504672e29be93c24981eeb1c07576d730e9f7c8805afff0c6"
    ;;
  Darwin:arm64)
    archive="actionlint_${version}_darwin_arm64.tar.gz"
    sum="aba9ced2dee8d27fecca3dc7feb1a7f9a52caefa1eb46f3271ea66b6e0e6953f"
    ;;
  Darwin:x86_64)
    archive="actionlint_${version}_darwin_amd64.tar.gz"
    sum="5b44c3bc2255115c9b69e30efc0fecdf498fdb63c5d58e17084fd5f16324c644"
    ;;
  MINGW*:x86_64 | MSYS*:x86_64 | CYGWIN*:x86_64)
    archive="actionlint_${version}_windows_amd64.zip"
    sum="6e7241b51e6817ea6a047693d8e6fed13b31819c9a0dd6c5a726e1592d22f6e9"
    binary="actionlint.exe"
    ;;
  *)
    echo "sandbox-actionlint: unsupported platform ${os}:${arch}" >&2
    exit 1
    ;;
esac

if [[ ! "${sum}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "sandbox-actionlint: pinned checksum is not 64 hex characters" >&2
  exit 1
fi

if ! command -v sha256sum >/dev/null 2>&1; then
  echo "sandbox-actionlint: sha256sum is required" >&2
  exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "sandbox-actionlint: curl is required" >&2
  exit 1
fi

workdir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sandbox-actionlint.XXXXXXXX")"
cleanup() {
  local status=$?
  local dir="${workdir:-}"
  workdir=""
  if [[ -z "${dir}" ]]; then
    exit "${status}"
  fi
  case "${dir}" in
    */sandbox-actionlint.*) ;;
    *)
      echo "sandbox-actionlint: refusing to remove unexpected directory ${dir}" >&2
      if [[ "${status}" -eq 0 ]]; then
        status=1
      fi
      exit "${status}"
      ;;
  esac
  if ! rm -rf -- "${dir}"; then
    echo "sandbox-actionlint: failed to remove ${dir}" >&2
    if [[ "${status}" -eq 0 ]]; then
      status=1
    fi
  fi
  exit "${status}"
}
trap cleanup EXIT
dest="${workdir}/${archive}"
url="https://github.com/rhysd/actionlint/releases/download/v${version}/${archive}"

curl --fail --silent --show-error --location \
  --proto '=https' --tlsv1.2 \
  --retry 3 --retry-connrefused \
  --output "${dest}" \
  "${url}"

if [[ ! -s "${dest}" ]]; then
  echo "sandbox-actionlint: download is empty: ${url}" >&2
  exit 1
fi

actual="$(sha256sum "${dest}")"
actual="${actual%% *}"
if [[ "${actual}" != "${sum}" ]]; then
  echo "sandbox-actionlint: checksum mismatch for ${archive}" >&2
  echo "sandbox-actionlint: expected ${sum}" >&2
  echo "sandbox-actionlint: actual   ${actual}" >&2
  exit 1
fi

printf '%s  %s\n' "${sum}" "${archive}" >"${workdir}/SHA256SUMS"
(
  cd "${workdir}"
  sha256sum --check --strict SHA256SUMS
)

if [[ "${archive}" == *.zip ]]; then
  if ! command -v unzip >/dev/null 2>&1; then
    echo "sandbox-actionlint: unzip is required" >&2
    exit 1
  fi
  unzip -o -q "${dest}" -d "${workdir}"
else
  tar --extract --gzip --file "${dest}" --directory "${workdir}" "${binary}"
fi

tool="${workdir}/${binary}"
if [[ ! -f "${tool}" ]]; then
  echo "sandbox-actionlint: archive did not contain ${binary}" >&2
  exit 1
fi
chmod +x "${tool}"

raw="$("${tool}" -version)"
got="${raw%%$'\n'*}"
got="${got%$'\r'}"
case "${got}" in
  1.7.12 | v1.7.12) ;;
  *)
    echo "sandbox-actionlint: binary version '${got}' != ${version}" >&2
    exit 1
    ;;
esac

if ! command -v shellcheck >/dev/null 2>&1; then
  echo "sandbox-actionlint: shellcheck is required; refusing to skip script lint" >&2
  exit 1
fi

shellcheck --severity=warning \
  .github/scripts/sandbox-actionlint.sh

"${tool}" -no-color \
  .github/workflows/sandbox-quality.yml
