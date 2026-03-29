#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
#
# Build the COPT NLP shim shared library, then build Rust artifacts.
#
# Usage:
#   scripts/build-internal-copt-nlp.sh [wheel|py-dev|cli|all]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-all}"

export COPT_HOME="${COPT_HOME:-/opt/copt80}"
export LD_LIBRARY_PATH="${COPT_HOME}/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"

# Step 1: Build the standalone COPT NLP shim shared library.
"${ROOT}/scripts/build-copt-nlp-shim.sh"

# Step 2: Build Rust artifacts (no special env vars needed).
case "${MODE}" in
    wheel)
        cd "${ROOT}/src/surge-py"
        maturin build --release --out dist
        ;;
    py-dev)
        cd "${ROOT}/src/surge-py"
        maturin develop --release
        ;;
    cli)
        cd "${ROOT}"
        cargo build --release --bin surge-solve
        ;;
    all)
        cd "${ROOT}"
        cargo build --release --bin surge-solve
        cd "${ROOT}/src/surge-py"
        maturin build --release --out dist
        ;;
    *)
        echo "Usage: $0 [wheel|py-dev|cli|all]" >&2
        exit 2
        ;;
esac
