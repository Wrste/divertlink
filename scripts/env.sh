#!/usr/bin/env sh
# Source this to get the workspace-local Rust toolchain:  . ./scripts/env.sh
ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")/.." && pwd)
export DIVERTLINK_ROOT="$ROOT"
export CARGO_HOME="$ROOT/.toolchain/cargo"
export RUSTUP_HOME="$ROOT/.toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
