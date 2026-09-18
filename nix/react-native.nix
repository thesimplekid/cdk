{ pkgs, lib, craneLib, stableToolchain, src, cargoVendorDir, version, nixpkgs, system }:
let
  libraryExtension = if pkgs.stdenv.isDarwin then "dylib" else "so";
  toolchainVersion = (builtins.fromTOML (builtins.readFile ../rust-toolchain.toml)).toolchain.channel;
  generatorVersion = "0.30.0-1";
  generator = (pkgs.makeRustPlatform {
    cargo = stableToolchain;
    rustc = stableToolchain;
  }).buildRustPackage {
    pname = "uniffi-bindgen-react-native";
    version = generatorVersion;
    src = pkgs.fetchzip {
      url = "https://registry.npmjs.org/uniffi-bindgen-react-native/-/uniffi-bindgen-react-native-${generatorVersion}.tgz";
      hash = "sha256-AqvJFdsjusgaj3CZbASbQVFWOm42sNqnkHJvyCwjgFY=";
    };
    cargoLock.lockFile = ./uniffi-bindgen-react-native.Cargo.lock;
    postPatch = ''
      cp ${./uniffi-bindgen-react-native.Cargo.lock} Cargo.lock
    '';
    cargoBuildFlags = [ "-p" "uniffi-bindgen-react-native" ];
    doCheck = false;
    postInstall = ''
      ln -s uniffi-bindgen-react-native $out/bin/ubrn
    '';
  };

  ffiArgs = {
    inherit src version cargoVendorDir;
    pname = "cashu-ffi";
    cargoExtraArgs = "-p cashu-ffi";
    strictDeps = true;
  };
  ffiDeps = craneLib.buildDepsOnly ffiArgs;
  ffi = craneLib.mkCargoDerivation (ffiArgs // {
    cargoArtifacts = ffiDeps;
    doInstallCargoArtifacts = false;
    buildPhaseCargoCommand = "cargo build --release --locked -p cashu-ffi --lib --example conformance";
    doCheck = true;
    checkPhaseCargoCommand = "cargo test --release --locked -p cashu-ffi";
    installPhaseCommand = ''
      mkdir -p $out/lib $out/bin
      cp target/release/libcashu_ffi.${libraryExtension} $out/lib/
      cp target/release/examples/conformance $out/bin/
    '';
  });

  generated = craneLib.mkCargoDerivation (ffiArgs // {
    pname = "cashu-react-native-generated";
    cargoArtifacts = null;
    doInstallCargoArtifacts = false;
    nativeBuildInputs = [ generator pkgs.nodejs_22 ];
    buildPhaseCargoCommand = ''
      uniffi-bindgen-react-native generate jsi bindings --library \
        ${ffi}/lib/libcashu_ffi.${libraryExtension} \
        --ts-dir bindings/react-native/src/generated \
        --cpp-dir bindings/react-native/cpp/generated --no-format
      cd bindings/react-native
      uniffi-bindgen-react-native generate jsi turbo-module --config ubrn.config.yaml cashu_ffi
      node scripts/fix-generated.cjs
      cd ../..
    '';
    doCheck = false;
    installPhaseCommand = ''
      mkdir -p $out
      cp -r bindings/react-native/. $out/
    '';
  });

  bindings = pkgs.buildNpmPackage {
    pname = "cashu-react-native-bindings";
    inherit version;
    src = generated;
    nodejs = pkgs.nodejs_22;
    npmDepsHash = "sha256-VA6lwDMAGx80IJY6PaOWKLJZ42VicuGVxQZ6EG3NQfc=";
    npmFlags = [ "--ignore-scripts" ];
    npmBuildScript = "compile";
    dontNpmPrune = true;
    doCheck = true;
    checkPhase = ''
      runHook preCheck
      npm run format:check
      npm run typecheck
      CDK_CRYPTO_TEST_BIN=${ffi}/bin/conformance npm test
      runHook postCheck
    '';
    installPhase = ''
      runHook preInstall
      # Cargo workspace metadata is the release version source of truth.
      npm pkg set version=${lib.escapeShellArg version}
      mkdir -p $out/package
      tar --exclude=./node_modules -cf - . | tar -xf - -C $out/package
      runHook postInstall
    '';
  };

  # SDK-backed Android artifacts are available on the supported NDK host.
  androidPkgs = import nixpkgs {
    inherit system;
    config = { android_sdk.accept_license = true; allowUnfree = true; };
  };
  androidSdk = (androidPkgs.androidenv.composeAndroidPackages {
    platformVersions = [ "36" ];
    buildToolsVersions = [ "36.0.0" ];
    includeNDK = true;
    ndkVersions = [ "27.1.12297006" ];
    includeEmulator = false;
    includeSystemImages = false;
  }).androidsdk;
  ndk = "${androidSdk}/libexec/android-sdk/ndk/27.1.12297006/toolchains/llvm/prebuilt/linux-x86_64/bin";
  androidToolchain = pkgs.rust-bin.stable.${toolchainVersion}.default.override {
    targets = [ "aarch64-linux-android" "x86_64-linux-android" ];
  };
  androidCrane = craneLib.overrideToolchain androidToolchain;
  androidArgs = ffiArgs // {
    pname = "cashu-ffi-android";
    cargoExtraArgs = "-p cashu-ffi --target aarch64-linux-android --target x86_64-linux-android";
    CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = "${ndk}/aarch64-linux-android24-clang";
    CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER = "${ndk}/x86_64-linux-android24-clang";
    CC_aarch64_linux_android = "${ndk}/aarch64-linux-android24-clang";
    CC_x86_64_linux_android = "${ndk}/x86_64-linux-android24-clang";
    AR_aarch64_linux_android = "${ndk}/llvm-ar";
    AR_x86_64_linux_android = "${ndk}/llvm-ar";
    CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS = "-C link-arg=-Wl,-z,max-page-size=16384";
    CARGO_TARGET_X86_64_LINUX_ANDROID_RUSTFLAGS = "-C link-arg=-Wl,-z,max-page-size=16384";
  };
  android = androidCrane.mkCargoDerivation (androidArgs // {
    cargoArtifacts = androidCrane.buildDepsOnly androidArgs;
    doInstallCargoArtifacts = false;
    # These libraries are for Android, not the Nix build host.
    dontFixup = true;
    buildPhaseCargoCommand = "cargo build --release --locked ${androidArgs.cargoExtraArgs}";
    doCheck = false;
    installPhaseCommand = ''
      mkdir -p $out/android/src/main/jniLibs/{arm64-v8a,x86_64}
      cp target/aarch64-linux-android/release/libcashu_ffi.so $out/android/src/main/jniLibs/arm64-v8a/
      cp target/x86_64-linux-android/release/libcashu_ffi.so $out/android/src/main/jniLibs/x86_64/
    '';
  });
in
{
  inherit generator ffi bindings android;
  shell = pkgs.mkShell {
    packages = [ stableToolchain generator pkgs.nodejs_22 pkgs.pkg-config ];
    # Prefer the pinned native generator over npm's cargo-run launcher.
    UBRN_BIN = "${generator}/bin/ubrn";
  };
}
