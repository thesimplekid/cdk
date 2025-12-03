# Implementing Nix + Cachix + Crane CI Caching for Rust

This guide explains how to set up efficient CI caching for Rust projects using Nix, Cachix, and Crane. This approach can reduce CI build times from 10-15 minutes to 1-3 minutes for most builds.

## Overview

The system has 4 main components:

1. **Cachix** - Binary cache service that stores/retrieves pre-built Nix artifacts
2. **Crane** - Nix library for building Rust projects with incremental caching
3. **Flake** - Nix configuration defining builds and caching
4. **GitHub Actions** - CI that uses Cachix for caching

## How It Works

The key insight is splitting the Rust build into two phases:

1. **Dependencies Phase** (`workspaceDeps`): Builds only external dependencies using dummy source files. This only rebuilds when `Cargo.toml` or `Cargo.lock` changes.

2. **Workspace Phase** (`workspaceBuild`): Builds your actual code, reusing the cached dependencies from phase 1.

Since dependencies rarely change, they can be downloaded from Cachix instead of compiled on most CI runs.

```
First CI Run (cold cache):
┌─────────────────────────────────────────────────────────────┐
│  1. nix build .#deps                                        │
│     └─ Compiles ALL dependencies (~5-10 min)                │
│     └─ Stores compressed ./target in Nix store              │
│     └─ Cachix automatically pushes to remote cache          │
│                                                             │
│  2. nix build .#default                                     │
│     └─ cargoArtifacts = workspaceDeps (uses cached deps)    │
│     └─ Only compiles YOUR code (~1-2 min)                   │
└─────────────────────────────────────────────────────────────┘

Subsequent CI Runs (warm cache):
┌─────────────────────────────────────────────────────────────┐
│  1. nix build .#deps                                        │
│     └─ Cachix downloads pre-built deps (~30 sec)            │
│     └─ NO compilation needed!                               │
│                                                             │
│  2. nix build .#default                                     │
│     └─ Uses downloaded deps                                 │
│     └─ Only compiles YOUR changed code (~1-2 min)           │
└─────────────────────────────────────────────────────────────┘
```

## Step 1: Set Up Cachix

### Create a Cachix Cache

```bash
# Install cachix
nix-env -iA cachix -f https://cachix.org/api/v1/install

# Create an account and cache at https://app.cachix.org
# Or via CLI:
cachix authtoken <your-token>
```

You'll get:

- Cache name (e.g., `myproject`)
- Public key (e.g., `myproject.cachix.org-1:AbCdEf123...`)
- Auth token (for pushing to cache)

## Step 2: Create the Flake

Create `flake.nix` in your project root:

```nix
{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";
    flake-utils.url = "github:numtide/flake-utils";

    # Rust toolchains
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Crane for Rust builds
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, fenix, crane, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # Get Rust toolchain
        rustToolchain = fenix.packages.${system}.stable.toolchain;

        # Create crane lib with our toolchain
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Filter source files (CRITICAL for caching)
        # Only include files needed for cargo
        cargoTomlFilter = path: type:
          (builtins.match ".*Cargo\\.toml$" path != null) ||
          (builtins.match ".*Cargo\\.lock$" path != null);

        rustFilter = path: type:
          (cargoTomlFilter path type) ||
          (builtins.match ".*\\.rs$" path != null);

        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (rustFilter path type) ||
            (craneLib.filterCargoSources path type);
        };

        # Common arguments for all builds
        commonArgs = {
          inherit src;
          pname = "myproject";
          version = "0.1.0";

          # Add native dependencies here
          nativeBuildInputs = with pkgs; [
            pkg-config
          ];

          buildInputs = with pkgs; [
            openssl
          ];
        };

        #######################################
        # THE KEY CACHING MECHANISM
        #######################################

        # Phase 1: Build ONLY dependencies
        # This uses dummy source files, so it only rebuilds
        # when Cargo.toml or Cargo.lock changes
        workspaceDeps = craneLib.buildDepsOnly (commonArgs // {
          pname = "myproject-deps";
        });

        # Phase 2: Build the actual workspace
        # Reuses cached dependencies from Phase 1
        workspaceBuild = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = workspaceDeps;  # <-- This is the magic
        });

        # Tests reuse the build artifacts
        workspaceTest = craneLib.cargoTest (commonArgs // {
          cargoArtifacts = workspaceBuild;
        });

        # Clippy reuses dependency artifacts
        workspaceClippy = craneLib.cargoClippy (commonArgs // {
          cargoArtifacts = workspaceDeps;
          cargoClippyExtraArgs = "--all-targets -- -D warnings";
        });

      in {
        packages = {
          default = workspaceBuild;
          deps = workspaceDeps;  # Expose deps for explicit caching
        };

        checks = {
          build = workspaceBuild;
          test = workspaceTest;
          clippy = workspaceClippy;
        };

        devShells.default = craneLib.devShell {
          packages = with pkgs; [
            rust-analyzer
            cargo-watch
          ];
        };
      }
    );

  # IMPORTANT: Configure Cachix as a substituter
  nixConfig = {
    extra-substituters = [ "https://myproject.cachix.org" ];
    extra-trusted-public-keys = [
      "myproject.cachix.org-1:YourPublicKeyHere="
    ];
  };
}
```

## Step 3: GitHub Actions Workflow

Create `.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main, master]
  pull_request:
    branches: [main, master]

# Cancel previous runs on same PR
concurrency:
  group: ${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: true

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      # Install Nix
      - uses: DeterminateSystems/nix-installer-action@main

      # Set up Cachix - THIS IS THE KEY STEP
      - uses: cachix/cachix-action@v16
        with:
          name: myproject                              # Your cache name
          authToken: '${{ secrets.CACHIX_AUTH_TOKEN }}' # For pushing
        continue-on-error: true  # Don't fail if Cachix is down

      # Build - will use cached deps from Cachix
      - name: Build
        run: nix build -L .#default

      # Run tests
      - name: Test
        run: nix build -L .#checks.x86_64-linux.test

      # Run clippy
      - name: Clippy
        run: nix build -L .#checks.x86_64-linux.clippy
```

## Step 4: Add Cachix Auth Token to GitHub

1. Go to your Cachix dashboard
2. Generate an auth token
3. Add to GitHub: Settings → Secrets → Actions → `CACHIX_AUTH_TOKEN`

## Key Concepts

### 1. Source Filtering (Critical!)

Only include files that affect the build. Changes to `README.md` won't trigger rebuilds:

```nix
src = pkgs.lib.cleanSourceWith {
  src = ./.;
  filter = path: type:
    (builtins.match ".*\\.rs$" path != null) ||
    (builtins.match ".*Cargo\\.(toml|lock)$" path != null);
};
```

### 2. Dependency Isolation

`buildDepsOnly` uses dummy `.rs` files, so only `Cargo.toml`/`Cargo.lock` matter:

```nix
workspaceDeps = craneLib.buildDepsOnly { ... };
```

This means:

- Changing `src/main.rs` → deps NOT rebuilt
- Changing `Cargo.toml` → deps rebuilt

### 3. Artifact Chaining

Each build phase can reuse artifacts from previous phases:

```nix
workspaceBuild = craneLib.buildPackage {
  cargoArtifacts = workspaceDeps;  # Extract cached ./target
  ...
};

workspaceTest = craneLib.cargoTest {
  cargoArtifacts = workspaceBuild;  # Reuse built artifacts
  ...
};
```

## Pushing Release Binaries to Cachix

For releases, explicitly push the built artifacts:

```yaml
- name: Build release
  run: nix build -L .#default

- name: Push to Cachix
  run: |
    cachix push myproject \
      $(nix-store --query --references $(readlink -f result)) \
      $(readlink -f result)
```

You can also pin releases for long-term availability:

```yaml
- name: Pin release
  run: |
    cachix pin myproject "release-v1.0.0" $(readlink -f result)
```

## Expected Results

| Scenario | Without Cachix | With Cachix |
|----------|---------------|-------------|
| Fresh build (deps change) | 10-15 min | 10-15 min |
| Code-only change | 10-15 min | 1-3 min |
| No changes | 10-15 min | ~30 sec |

## Advanced: Multiple Build Profiles

You can define different profiles for CI vs release builds:

```nix
# In flake.nix
let
  mkBuild = profile: craneLib.buildPackage (commonArgs // {
    cargoArtifacts = workspaceDeps;
    CARGO_PROFILE = profile;
  });
in {
  packages = {
    default = mkBuild "release";
    ci = mkBuild "dev";
  };
}
```

## Advanced: Cross-Compilation Caching

For cross-compilation, create separate dependency caches per target:

```nix
workspaceDepsWasm = craneLib.buildDepsOnly (commonArgs // {
  pname = "myproject-deps-wasm";
  CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
});

workspaceBuildWasm = craneLib.buildPackage (commonArgs // {
  cargoArtifacts = workspaceDepsWasm;
  CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
});
```

## Troubleshooting

### Cache Not Being Used

1. Verify `nixConfig` is set in `flake.nix`
2. Check that the Cachix action runs before build steps
3. Ensure auth token has read permissions

### Rebuilding Dependencies Unexpectedly

1. Check source filtering - unrelated files may be included
2. Verify `Cargo.lock` is committed to git
3. Check for environment variable differences

### Slow First Build

The first build after changing dependencies will always be slow. Consider:

- Running a scheduled nightly build to warm the cache
- Using self-hosted runners with persistent Nix stores

## References

- [Crane Documentation](https://crane.dev/)
- [Cachix Documentation](https://docs.cachix.org/)
- [Fenix (Rust toolchains for Nix)](https://github.com/nix-community/fenix)
- [Nix Flakes](https://nixos.wiki/wiki/Flakes)
