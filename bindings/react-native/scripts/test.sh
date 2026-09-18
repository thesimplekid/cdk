#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -z "${CDK_CRYPTO_TEST_BIN:-}" ]; then
  cargo build --manifest-path ../../Cargo.toml -p cashu-ffi --example conformance
  metadata=$(cargo metadata --manifest-path ../../Cargo.toml --format-version 1 --no-deps)
  target_dir=$(node -e 'process.stdout.write(JSON.parse(require("node:fs").readFileSync(0, "utf8")).target_directory)' <<< "$metadata")
  export CDK_CRYPTO_TEST_BIN="$target_dir/debug/examples/conformance"
fi
exec tsx --test test/*.test.ts
