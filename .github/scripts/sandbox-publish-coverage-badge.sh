#!/usr/bin/env bash
# Publish a shields.io endpoint JSON file to the repository's `badges` branch.
#
# Usage (from repository root, with GITHUB_TOKEN and GITHUB_REPOSITORY set):
#   bash .github/scripts/sandbox-publish-coverage-badge.sh \
#     <lcov.info> <badge-file.json> <label>
#
# Computes line coverage from lcov LF/LH records and upserts the endpoint JSON
# on `badges` via the GitHub Contents API (no interactive git credentials).
# Display only: not a coverage gate and not isolation evidence.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <lcov.info> <badge-file.json> <label>" >&2
  exit 2
fi

lcov_path=$1
badge_file=$2
label=$3

case "${badge_file}" in
  *.json) ;;
  *)
    echo "badge file must end in .json: ${badge_file}" >&2
    exit 2
    ;;
esac

if [[ ! -f "${lcov_path}" ]]; then
  echo "lcov file not found: ${lcov_path}" >&2
  exit 1
fi

if [[ -z "${GITHUB_TOKEN:-}" || -z "${GITHUB_REPOSITORY:-}" ]]; then
  echo "GITHUB_TOKEN and GITHUB_REPOSITORY are required" >&2
  exit 1
fi

read -r lines_hit lines_found < <(
  awk -F: '
    /^LF:/ { lf += $2 }
    /^LH:/ { lh += $2 }
    END { printf "%d %d\n", lh, lf }
  ' "${lcov_path}"
)

if [[ "${lines_found}" -le 0 ]]; then
  echo "lcov contained no line records" >&2
  exit 1
fi

# One decimal place, matching common llvm-cov summaries.
percent=$(
  awk -v lh="${lines_hit}" -v lf="${lines_found}" \
    'BEGIN { printf "%.1f", 100.0 * lh / lf }'
)
message="${percent}%"

# Brand green at/above 70%; amber then red below so low coverage is visible.
color=$(
  awk -v p="${percent}" 'BEGIN {
    if (p + 0 < 50) print "e05d44"
    else if (p + 0 < 70) print "dfb317"
    else print "218D76"
  }'
)

payload_file=$(mktemp)
trap 'rm -f "${payload_file}"' EXIT

python3 - "${payload_file}" "${label}" "${message}" "${color}" <<'PY'
import json
import sys

path, label, message, color = sys.argv[1:5]
payload = {
    "schemaVersion": 1,
    "label": label,
    "message": message,
    "color": color,
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(payload, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY

api="https://api.github.com/repos/${GITHUB_REPOSITORY}/contents/${badge_file}"
auth=(
  -H "Authorization: Bearer ${GITHUB_TOKEN}"
  -H "Accept: application/vnd.github+json"
  -H "X-GitHub-Api-Version: 2022-11-28"
)

# Ensure the badges branch exists (orphan with no other files is fine).
branch_code=$(
  curl -sS -o /dev/null -w "%{http_code}" "${auth[@]}" \
    "https://api.github.com/repos/${GITHUB_REPOSITORY}/branches/badges"
)
if [[ "${branch_code}" == "404" ]]; then
  empty_tree=$(
    curl -sS "${auth[@]}" \
      -X POST "https://api.github.com/repos/${GITHUB_REPOSITORY}/git/trees" \
      -d '{"tree":[]}' \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["sha"])'
  )
  commit_sha=$(
    curl -sS "${auth[@]}" \
      -X POST "https://api.github.com/repos/${GITHUB_REPOSITORY}/git/commits" \
      -d "$(python3 -c "import json; print(json.dumps({
        'message': 'chore(badges): created empty badges branch',
        'tree': '${empty_tree}',
        'parents': []
      }))")" \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["sha"])'
  )
  curl -sS "${auth[@]}" \
    -X POST "https://api.github.com/repos/${GITHUB_REPOSITORY}/git/refs" \
    -d "$(python3 -c "import json; print(json.dumps({
      'ref': 'refs/heads/badges',
      'sha': '${commit_sha}'
    }))")" >/dev/null
  echo "created badges branch"
elif [[ "${branch_code}" != "200" ]]; then
  echo "failed to query badges branch (HTTP ${branch_code})" >&2
  exit 1
fi

existing_sha=""
existing_code=$(
  curl -sS -o "${payload_file}.meta" -w "%{http_code}" "${auth[@]}" \
    "${api}?ref=badges"
)
if [[ "${existing_code}" == "200" ]]; then
  existing_sha=$(
    python3 -c 'import json,sys; print(json.load(open(sys.argv[1],encoding="utf-8"))["sha"])' \
      "${payload_file}.meta"
  )
  existing_message=$(
    python3 -c '
import base64, json, sys
meta = json.load(open(sys.argv[1], encoding="utf-8"))
print(json.loads(base64.b64decode(meta["content"]).decode("utf-8")).get("message", ""))
' "${payload_file}.meta"
  )
  if [[ "${existing_message}" == "${message}" ]]; then
    echo "badge unchanged: ${badge_file} (${message})"
    exit 0
  fi
elif [[ "${existing_code}" != "404" ]]; then
  echo "failed to query ${badge_file} on badges (HTTP ${existing_code})" >&2
  cat "${payload_file}.meta" >&2 || true
  exit 1
fi

content_b64=$(python3 -c 'import base64, pathlib, sys; print(base64.b64encode(pathlib.Path(sys.argv[1]).read_bytes()).decode("ascii"))' "${payload_file}")
body=$(
  python3 - "${badge_file}" "${message}" "${content_b64}" "${existing_sha}" <<'PY'
import json
import sys

path, message, content_b64, existing_sha = sys.argv[1:5]
payload = {
    "message": f"chore(badges): updated {path} to {message}",
    "content": content_b64,
    "branch": "badges",
}
if existing_sha:
    payload["sha"] = existing_sha
print(json.dumps(payload))
PY
)

put_code=$(
  curl -sS -o "${payload_file}.put" -w "%{http_code}" "${auth[@]}" \
    -X PUT "${api}" \
    -d "${body}"
)
if [[ "${put_code}" != "200" && "${put_code}" != "201" ]]; then
  echo "failed to publish ${badge_file} (HTTP ${put_code})" >&2
  cat "${payload_file}.put" >&2 || true
  exit 1
fi

echo "published ${badge_file}: ${label} ${message}"
