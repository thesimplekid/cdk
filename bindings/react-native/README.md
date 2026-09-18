# Rust crypto for cashu-ts on React Native

`@cashu/cdk-react-native` supplies an `OutputDataCreator` for **cashu-ts 4.10.2**.
The synchronous UniFFI/JSI module calls `crates/cashu-ffi`, which uses CDK's
`cashu` protocol primitives without loading the CDK wallet, databases or network
clients. No mint or wallet state is held in the native module.

## Use in an app

The source package lives in the CDK monorepo. The release workflow publishes
`@cashu/cdk-react-native` to npm after building both mobile platforms. For local
development, build the artifacts below and install the package tarball. Use
React Native's **New Architecture with Hermes**. The development dependency and
codegen checks target React Native 0.81; on-device builds still need validation.
Expo apps need a development/native build; Expo Go cannot load this module.

```ts
import { Wallet } from '@cashu/cashu-ts';
import { createOutputDataCreator } from '@cashu/cdk-react-native';

const wallet = new Wallet('https://your-mint.example', {
  outputDataCreator: createOutputDataCreator(),
});
await wallet.loadMint();
```

Load the text-encoding, randomness and other polyfills required by your cashu-ts
app **before importing these packages**. The adapter uses `TextDecoder` and
`Uint8Array`; amounts and blinding factors use `bigint`, not JavaScript numbers.
The native module installs itself when imported. Its synchronous methods must
not be wrapped in promises because cashu-ts consumes their results immediately.
Large batches run on the JS calling thread; benchmark them on your target phones.

Random, deterministic and locked output creation each send the denominations to
Rust in one output-creation call. Locking keys are normalized once per batch;
SIG_ALL shares an ephemeral key, while SIG_INPUTS generates independent keys.

The creator also exposes `createRestoreData(seed, keysetId, startCounter, count)`
for explicit batched restore scans. It returns blank outputs in counter order and
accepts at most 10,000 counters per call. This additional method is not invoked
automatically by cashu-ts's wallet restore API.

## Supported operations

- Random output creation with native OS randomness and message blinding.
- NUT-13 deterministic secrets/blinding, including 16–64 byte seeds, BIP32
  counters below 2^31 and HMAC counters through `Number.MAX_SAFE_INTEGER`.
- P2PK and HTLC locks, additional tags, thresholds and refund paths.
- NUT-28 P2BK key blinding, including shared ephemeral keys for SIG_ALL splits.
- Native signature unblinding and verification of DLEQ proofs when supplied.
  Wrong keyset/amount, corrupted output material and invalid DLEQ are rejected.
  Zero-amount change/restore outputs accept the returned denomination.

Only full **00/01 hex keyset IDs** and secp256k1 are supported. Legacy base64 IDs
and newer curve versions are rejected. Amounts are limited to Rust's `u64` range.
The peer dependency is exact because these custom hooks need conformance checks
before updating cashu-ts.

Other wallet crypto, such as input signing and verification outside `toProof`,
still uses cashu-ts. `OutputData.serialize()` accepts these outputs, but
`OutputData.deserialize()` reconstructs cashu-ts's default implementation, so
that restoration path uses its JavaScript crypto. This package does not provide
an opaque keystore: cashu-ts receives secrets and blinding factors in JS memory.

## Build

Prerequisites: the repository Rust toolchain, Node 22.11 or newer, npm, and the
usual React Native native build tools. Use an I/O scratch/build directory on the
dedicated disk when working on this workstation:

```sh
export CARGO_TARGET_DIR=/data/rust/targets/cashu-ffi
export npm_config_cache=/data/rust/tmp/cashu-ffi-npm
cd bindings/react-native
npm ci
npm run build
npm run typecheck
```

`uniffi-bindgen-react-native` is pinned to **0.30.0-1**, matching this workspace's
UniFFI 0.30. Its npm package includes the native runtime and builds the generator
with Cargo on first use. `npm run build` regenerates the TS/C++ bindings and
Android/iOS module integration sources before compiling TypeScript. These
outputs are ignored by version control; commit the Rust API, handwritten
TypeScript, generator configuration, lockfiles and build scripts instead.
`npm run generate` is also available when only regenerated sources are needed.

`npm pack` runs the build through `prepack`, so generated sources and compiled
JavaScript are included even from a clean checkout. Native binaries still need
the platform build commands below. Installing the packed npm package does not
run the Rust generator or require Rust on the consumer's machine.

Android requires an Android SDK/NDK, Java, and `cargo-ndk` (`cargo install
cargo-ndk --locked`). Configure `ANDROID_HOME` and `ANDROID_NDK_HOME`, then:

```sh
npm run build:android
```

The default ABIs are arm64-v8a (devices) and x86_64 (emulators). Add other targets
to `ubrn.config.yaml` and rebuild if needed. The host app's SDK/NDK/Kotlin settings
override `android/gradle.properties`.

iOS requires macOS, Xcode and CocoaPods:

```sh
npm run build:ios
```

This creates a device + Apple Silicon simulator XCFramework. Add
`x86_64-apple-ios` to the config for Intel simulators. Build both platforms before
packing a package intended for both:

```sh
npm run build
npm pack
# In the consuming app:
npm install /path/to/cashu-cdk-react-native-0.18.0.tgz
cd ios && pod install
```

The package must contain `android/src/main/jniLibs/<abi>/libcashu_ffi.so` for
Android and `CashuCdkReactNativeFramework.xcframework` for iOS. These binary
artifacts are ignored by version control but included in npm packages. Rebuild
the app after installation; a Metro reload alone cannot install native code.

## Validation

From the repository root:

```sh
cargo test -p cashu-ffi
cargo clippy -p cashu-ffi --all-targets -- -D warnings
cd bindings/react-native
npm ci
npm run build
npm run typecheck
npm test
```

The TypeScript tests call the actual Rust library through a host-only example
process and compare it with cashu-ts's default implementation. They cover both
NUT-13 derivations, splits, boundary counters, large amounts, P2PK/HTLC/P2BK,
SIG_ALL key sharing, DLEQ, altered responses and blank change outputs. They do
not substitute for running the generated JSI bridge on Android/iOS.

For device verification, inject the creator into your app, create deterministic
outputs and compare `OutputData.serialize()` with the default creator; then run
a mint/send/receive/melt cycle against your regtest mint. Verify both platforms
before distributing native binaries.

## Nix builds and releases

The flake exposes these packages:

| Output | Contents |
| --- | --- |
| `cashu-ffi` | Host library and the Rust conformance test executable |
| `uniffi-bindgen-react-native` | Generator 0.30.0-1, with pinned source hash and Cargo lockfile |
| `react-native-bindings` | Generated TS/C++ and native integration, compiled JS/types, under `package/` |
| `react-native-android` | arm64-v8a and x86_64 native libraries, built with NDK 27.1 and API 24 (x86_64 Linux host) |

```sh
nix build .#react-native-bindings   # also runs Rust and TS conformance checks
nix build .#react-native-android
# Equivalent Just entry points:
just binding-react-native
just binding-react-native-android
```

The host bindings output is an intermediate artifact; it is not a complete
mobile npm release until the Android libraries and iOS XCFramework are added.
Generated code stays out of Git and is recreated inside the Nix build. The
npm dependency hash in `nix/react-native.nix` must be updated after changes to
`package-lock.json` (use `prefetch-npm-deps`). The generator's separate Cargo
lockfile is `nix/uniffi-bindgen-react-native.Cargo.lock`.

For development tools, use `nix develop .#react-native`. On macOS, the release
script uses that pinned Rust toolchain plus the installed Xcode iOS SDKs:

```sh
nix develop .#react-native -c bash bindings/react-native/scripts/build-ios.sh /path/to/artifacts
```

The iOS build runs outside the Nix sandbox because it needs Xcode's SDKs. Those
SDKs are supplied by the macOS runner; the iOS binary is not a pure Nix build.
The script builds arm64 device and simulator slices matching `ubrn.config.yaml`.

`.github/workflows/react-native-publish.yml` can be dispatched independently or
through `ffi-publish-all.yml`:

```sh
just ffi-release-react-native 0.18.0
```

Before dispatching, push the release tag containing these workflows and pass the
repository's full release CI on that commit. The tag must match the Cargo
workspace version, which sets the built npm package version. Configure the
repository's `NPM_TOKEN` secret with publish access to `@cashu/cdk-react-native`.
The workflow checks out the exact tag commit for every platform, assembles the
Nix package and mobile binaries, and verifies required files, library formats,
iOS slices, and package version before publishing the tarball with npm
provenance. Stable versions use `latest`; prereleases use `next`. Nightly
publishing is not wired into this package.

To inspect a prepared release tarball locally:

```sh
python3 bindings/react-native/scripts/verify-package.py /path/to/package.tgz --version 0.18.0
```

The final pack/publish steps use `--ignore-scripts` to preserve the already built
artifacts rather than rebuilding them. Ordinary local `npm pack` still invokes
`prepack` and generates the host bindings. No release is triggered by a PR build.
