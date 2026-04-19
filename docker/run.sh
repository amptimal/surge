#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
# Run the surge:latest dashboards-hub image with sensible defaults:
#
# - Binds to 127.0.0.1:8787 on the host (local-only). The hub mounts
#   each discovered dashboard (battery, GO C3, RTO) at its own path
#   prefix and serves the landing page at ``/``.
# - Solves use HiGHS (LP) and Ipopt (NLP) — both bundled in the image.
#   No commercial-solver license is forwarded; pass --env GUROBI_HOME
#   yourself if you want to layer one in.
# - Mounts the repo's target/benchmarks/go-c3 dir into the container's
#   /data so the GO C3 dashboard reads existing datasets/runs from disk.
# - Mounts ~/Documents/go-competition-results into /results for the
#   leaderboard xlsx + reference submissions (skipped if missing).
#
# Pass additional --flag args through to the server CLI by appending
# them after a `--` separator:
#     ./docker/run.sh -- --port 9090
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

DATA_DIR="${REPO_ROOT}/target/benchmarks/go-c3"
RESULTS_DIR="${HOME}/Documents/go-competition-results"

MOUNTS=("-v" "${DATA_DIR}:/data")
if [ -d "${RESULTS_DIR}" ]; then
  MOUNTS+=("-v" "${RESULTS_DIR}:/results:ro")
fi

EXTRA_ARGS=()
if [ "${1:-}" = "--" ]; then
  shift
  EXTRA_ARGS=("$@")
fi

exec docker run --rm -it \
  --name surge-dashboard \
  -p 127.0.0.1:8787:8787 \
  "${MOUNTS[@]}" \
  surge:latest \
  "${EXTRA_ARGS[@]}"
