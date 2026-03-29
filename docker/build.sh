#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
# Build the surge-builder base image (Rust toolchain + C solver libraries).
#
# Usage:
#   ./docker/build.sh              # Build base image
#   ./docker/build.sh --no-cache   # Full rebuild from scratch
set -euo pipefail

cd "$(dirname "$0")/.."

NO_CACHE=""

for arg in "$@"; do
    case "$arg" in
        --no-cache) NO_CACHE="--no-cache" ;;
        *) echo "Unknown arg: $arg"; exit 1 ;;
    esac
done

echo "==> Building surge-builder:latest..."
docker build $NO_CACHE -f docker/Dockerfile.base -t surge-builder:latest .

echo "==> Done. Image size: $(docker image inspect surge-builder:latest --format '{{.Size}}' | numfmt --to=iec-i --suffix=B 2>/dev/null || docker image inspect surge-builder:latest --format '{{.Size}}')"
