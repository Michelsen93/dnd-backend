#!/bin/bash
# Fetches crates and pre-builds the test binaries so `cargo test` is fast in Claude Code cloud sessions.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "$CLAUDE_PROJECT_DIR"
cargo fetch
cargo test --no-run
