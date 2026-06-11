#!/usr/bin/env bash
# Runtime UI smoke: boots the BUILT frontend in headless Chromium via
# CDP and exercises what compile-time gates can't see — tab panels
# actually rendering (the d41i effect-loop freeze was invisible to
# svelte-check/vitest/build) and the tool-table sort/filter/pagination.
#
# Requirements: a chromium binary on $PATH and node >= 20 (the CDP
# client uses node's experimental WebSocket). The pre-release script
# skips this gate when chromium is missing.
#
# Usage: scripts/ui-smoke.sh   (expects frontend/dist to be built)
set -euo pipefail
cd "$(dirname "$0")/.."

CHROME="${CHROME:-$(command -v chromium-browser || command -v chromium || command -v google-chrome || true)}"
if [[ -z "$CHROME" ]]; then
  echo "ui-smoke: no chromium binary found — skipping" >&2
  exit 0
fi
if [[ ! -f frontend/dist/index.html ]]; then
  echo "ui-smoke: frontend/dist missing — run 'pnpm build' first" >&2
  exit 1
fi

PORT="${UI_SMOKE_PORT:-4179}"
CDP_PORT="${UI_SMOKE_CDP_PORT:-9233}"
PROFILE="$(mktemp -d)"
cleanup() {
  [[ -n "${PREVIEW_PID:-}" ]] && kill "$PREVIEW_PID" 2>/dev/null || true
  [[ -n "${CHROME_PID:-}" ]] && kill "$CHROME_PID" 2>/dev/null || true
  rm -rf "$PROFILE"
}
trap cleanup EXIT

(cd frontend && pnpm exec vite preview --port "$PORT" >/dev/null 2>&1) &
PREVIEW_PID=$!
"$CHROME" --headless=new --remote-debugging-port="$CDP_PORT" --no-first-run \
  --no-sandbox --user-data-dir="$PROFILE" about:blank >/dev/null 2>&1 &
CHROME_PID=$!

for _ in $(seq 1 30); do
  if curl -fsS -o /dev/null "http://localhost:$PORT/" 2>/dev/null \
    && curl -fsS -o /dev/null "http://127.0.0.1:$CDP_PORT/json/version" 2>/dev/null; then
    break
  fi
  sleep 0.5
done

export APP_URL="http://localhost:$PORT/"
export CDP_PORT
node --experimental-websocket scripts/ui-smoke-tabs.mjs 2>&1 | grep -v ExperimentalWarning
node --experimental-websocket scripts/ui-smoke-table.mjs 2>&1 | grep -v ExperimentalWarning
echo "ui-smoke: all passed"
