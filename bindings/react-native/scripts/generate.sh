#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
package_dir="$PWD"
ubrn_bin="${UBRN_BIN:-$(command -v ubrn)}"
# UniFFI library discovery runs cargo metadata in the current directory.
cd ../..
cargo build -p cashu-ffi
metadata=$(cargo metadata --format-version 1 --no-deps)
target_dir=$(node -e 'process.stdout.write(JSON.parse(require("node:fs").readFileSync(0, "utf8")).target_directory)' <<< "$metadata")
case "$(uname -s)" in
  Darwin) library="$target_dir/debug/libcashu_ffi.dylib" ;;
  Linux) library="$target_dir/debug/libcashu_ffi.so" ;;
  *) echo 'Generate bindings on Linux or macOS' >&2; exit 1 ;;
esac
"$ubrn_bin" generate jsi bindings --library "$library" --ts-dir "$package_dir/src/generated" --cpp-dir "$package_dir/cpp/generated" --no-format
cd "$package_dir"
"$ubrn_bin" generate jsi turbo-module --config ubrn.config.yaml cashu_ffi
node scripts/fix-generated.cjs
