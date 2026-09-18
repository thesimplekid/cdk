#!/usr/bin/env bash
# Run in nix develop .#react-native on macOS; Xcode supplies the iOS SDKs.
set -euo pipefail
cd "$(dirname "$0")/../../.."
if [ "$(uname -s)" != Darwin ]; then
  echo 'The iOS release build requires macOS and Xcode.' >&2
  exit 1
fi
output_dir="${1:?Pass the artifact output directory}"
mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
metadata=$(cargo metadata --locked --format-version 1 --no-deps)
target_dir=$(node -e 'process.stdout.write(JSON.parse(require("node:fs").readFileSync(0, "utf8")).target_directory)' <<< "$metadata")
for target in aarch64-apple-ios aarch64-apple-ios-sim; do
  case "$target" in
    aarch64-apple-ios) sdk=iphoneos ;;
    aarch64-apple-ios-sim) sdk=iphonesimulator ;;
  esac
  sdk_root=$(xcrun --sdk "$sdk" --show-sdk-path)
  clang=$(xcrun --sdk "$sdk" --find clang)
  target_env=${target//-/_}
  linker_env="CARGO_TARGET_$(printf '%s' "$target_env" | tr '[:lower:]' '[:upper:]')_LINKER"
  env "SDKROOT=$sdk_root" "CC_${target_env}=$clang" "$linker_env=$clang" \
    IPHONEOS_DEPLOYMENT_TARGET=15.1 \
    cargo build --release --locked -p cashu-ffi --target "$target"
done
xcodebuild -create-xcframework \
  -library "$target_dir/aarch64-apple-ios/release/libcashu_ffi.a" \
  -library "$target_dir/aarch64-apple-ios-sim/release/libcashu_ffi.a" \
  -output "$output_dir/CashuCdkReactNativeFramework.xcframework"
