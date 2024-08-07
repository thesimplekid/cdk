#!/usr/bin/env bash

set -euo pipefail

buildargs=(
    "-p cdk"
    "-p cdk-axum"
    "-p cdk-cln"
    "-p cdk-fake-wallet"
    "-p cdk-phoenixd"
    "-p cdk-redb"
    "-p cdk-sqlite"
)

for arg in "${buildargs[@]}"; do
    echo  "Checking '$arg' docs"
    cargo doc $arg --all-features
    echo
done
