#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
#
# Build the COPT NLP shim as a standalone shared library.
#
# Requires COPT 8.x headers at $COPT_HOME/include/coptcpp_inc/.
# Produces libsurge_copt_nlp.{so,dylib} and writes it to
# ${SURGE_COPT_NLP_SHIM_OUT:-$COPT_HOME/lib/...}.
# On Windows, use scripts/build-copt-nlp-shim.ps1 instead.
#
# Usage:
#   scripts/build-copt-nlp-shim.sh          # uses COPT_HOME=/opt/copt80
#   COPT_HOME=/path/to/copt scripts/build-copt-nlp-shim.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="${ROOT}/src/surge-opf/copt_nlp_shim.cpp"

COPT_HOME="${COPT_HOME:-/opt/copt80}"
HEADERS="${COPT_HOME}/include/coptcpp_inc"
LIB_DIR="${COPT_HOME}/lib"
OUT_OVERRIDE="${SURGE_COPT_NLP_SHIM_OUT:-}"

if [[ ! -f "${SRC}" ]]; then
    echo "error: shim source not found at ${SRC}" >&2
    exit 1
fi

if [[ ! -d "${HEADERS}" ]]; then
    echo "error: COPT C++ headers not found at ${HEADERS}" >&2
    echo "       Set COPT_HOME to your COPT 8.x installation directory." >&2
    exit 1
fi

if [[ ! -d "${LIB_DIR}" ]]; then
    echo "error: COPT lib directory not found at ${LIB_DIR}" >&2
    exit 1
fi

OS="$(uname -s)"
case "${OS}" in
    Linux)
        OUT_NAME="libsurge_copt_nlp.so"
        OUT_PATH="${OUT_OVERRIDE:-${LIB_DIR}/${OUT_NAME}}"
        CXX="${CXX:-g++}"
        mkdir -p "$(dirname "${OUT_PATH}")"
        echo "Compiling ${OUT_NAME} (Linux)..."
        "${CXX}" -shared -fPIC -fvisibility=hidden \
            -std=c++17 -O2 \
            -I"${COPT_HOME}/include" -I"${HEADERS}" \
            -L"${LIB_DIR}" -Wl,--no-as-needed -lcopt_cpp -lcopt -lstdc++ \
            -Wl,-rpath,'$ORIGIN' \
            -o "${OUT_PATH}" \
            "${SRC}"
        ;;
    Darwin)
        OUT_NAME="libsurge_copt_nlp.dylib"
        OUT_PATH="${OUT_OVERRIDE:-${LIB_DIR}/${OUT_NAME}}"
        CXX="${CXX:-c++}"
        mkdir -p "$(dirname "${OUT_PATH}")"
        echo "Compiling ${OUT_NAME} (macOS)..."
        "${CXX}" -shared -fPIC -fvisibility=hidden \
            -std=c++17 -O2 \
            -I"${COPT_HOME}/include" -I"${HEADERS}" \
            -L"${LIB_DIR}" -lcopt_cpp -lc++ \
            -Wl,-rpath,@loader_path \
            -o "${OUT_PATH}" \
            "${SRC}"
        ;;
    MINGW*|MSYS*|CYGWIN*)
        echo "error: this script does not support Windows." >&2
        echo "       Use scripts/build-copt-nlp-shim.ps1 instead." >&2
        exit 1
        ;;
    *)
        echo "error: unsupported platform ${OS}" >&2
        exit 1
        ;;
esac

echo "Installed ${OUT_PATH}"

# Verify the symbol is exported.
case "${OS}" in
    Linux)  nm -D "${OUT_PATH}" | grep -q copt_nlp_solve && echo "Symbol copt_nlp_solve: OK" ;;
    Darwin) nm "${OUT_PATH}" | grep -q copt_nlp_solve && echo "Symbol copt_nlp_solve: OK" ;;
esac
