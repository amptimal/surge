#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV_PYTHON="$ROOT_DIR/.venv/bin/python3"

if [[ ! -x "$VENV_PYTHON" ]]; then
  cat >&2 <<'EOF'
Repo Python environment is missing.

Expected interpreter:
  .venv/bin/python3

Bootstrap:
  python3 -m venv .venv
  source .venv/bin/activate
  pip install maturin numpy
  cd src/surge-py
  maturin develop --release
  cd ../..
EOF
  exit 1
fi

export PYTHONPATH="$ROOT_DIR${PYTHONPATH:+:$PYTHONPATH}"

if ! "$VENV_PYTHON" -c 'import surge' >/dev/null 2>&1; then
  cat >&2 <<'EOF'
Repo venv is present, but the surge package is not importable from it.

Rebuild the editable package in the repo venv:
  source .venv/bin/activate
  cd src/surge-py
  maturin develop --release
  cd ../..
EOF
  exit 1
fi

exec "$VENV_PYTHON" "$@"
