# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
# ============================================================================
# surge:latest — builds the surge-py wheel from source and runs the
# Surge dashboards hub (battery + GO C3 + RTO) on port 8787.
#
# Stage 1 (wheel-builder): Ubuntu 26.04 + Rust + Python 3.14 (native) + C
# build deps. Produces a single manylinux-style wheel at /build/wheels/.
#
# Stage 2 (runtime):       Ubuntu 26.04 + Python 3.14 (native) + shared-
# library runtime deps (Ipopt, HiGHS, BLAS, SuiteSparse). Installs the wheel
# plus dashboard deps, copies markets/, benchmarks/, dashboards/, and the
# bundled examples/ used by the RTO dashboard. Runs the uvicorn hub bound
# on 0.0.0.0:8787 — the hub mounts each discovered dashboard at its own
# path prefix and serves the landing page at ``/``.
#
# Solvers (Ipopt, HiGHS) are dlopen'd via libloading at runtime, so the
# wheel does NOT link them statically — we just need the .so files and
# the IPOPT_LIB_DIR / HIGHS_LIB_DIR env vars pointing at them. The image
# ships HiGHS v1.14.0 (built from source) as the LP solver and Ipopt
# (from Ubuntu apt) as the NLP solver; commercial solvers (Gurobi, COPT,
# CPLEX) are intentionally not bundled — supply them externally if needed.
#
# Build:  docker build -f docker/Dockerfile -t surge:latest .
# Run:    docker run --rm -p 127.0.0.1:8787:8787 \
#           -v /abs/path/to/target/benchmarks/go-c3:/data \
#           surge:latest
# ============================================================================

# ─── Stage 1 · wheel-builder ───────────────────────────────────────────────
FROM ubuntu:26.04 AS wheel-builder

ENV DEBIAN_FRONTEND=noninteractive

# Build-time dependencies: compilers + linked C libraries.
# `libsuitesparse-dev` is a compile-time need (surge-sparse links KLU).
# `coinor-libipopt-dev` / `libhighs-dev` are NOT needed at compile time —
# surge uses libloading to dlopen them at runtime — so we skip them here.
# `cmake` + `git` are needed to build HiGHS from source (below).
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git pkg-config \
      build-essential cmake libclang-dev \
      libsuitesparse-dev \
      libopenblas-dev libgfortran-13-dev \
      patchelf \
      python3.14 python3.14-dev python3.14-venv \
      python3-pip \
    && rm -rf /var/lib/apt/lists/*

# Build HiGHS v1.14.0 from source as a shared library, installed to /opt/highs.
# Ubuntu 26.04's ``libhighs-dev`` apt package ships an older version — this
# step lets us ship the latest HiGHS LP/MIP solver without relying on the
# distro. The runtime stage copies /opt/highs/ out of this stage.
# To upgrade: bump the ARG below (and mirror in Dockerfile.base / CI).
ARG HIGHS_VERSION=v1.14.0
RUN git clone --depth 1 --branch ${HIGHS_VERSION} \
        https://github.com/ERGO-Code/HiGHS.git /opt/highs-src \
    && cmake -S /opt/highs-src -B /opt/highs-build \
        -DFAST_BUILD=ON \
        -DBUILD_SHARED_LIBS=ON \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX=/opt/highs \
        -DZLIB=OFF \
    && cmake --build /opt/highs-build -j$(nproc) \
    && cmake --install /opt/highs-build \
    && rm -rf /opt/highs-src /opt/highs-build

# Install Rust stable. Keeps the toolchain minimal — we don't need clippy
# or rustfmt in a release build.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

# Create an isolated venv with Python 3.14 so maturin's bin interpreter
# points at the right site-packages and doesn't collide with system pip.
RUN python3.14 -m venv /opt/build-venv
ENV PATH="/opt/build-venv/bin:${PATH}"
RUN pip install --no-cache-dir --upgrade pip maturin

WORKDIR /build

# Copy only what's needed to build the wheel. Cargo.toml + Cargo.lock must
# sit at the workspace root; the surge-py crate is under src/surge-py;
# examples/cases is embedded into the wheel via ``include_bytes!`` for
# the bundled IEEE / market starter networks.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples

# Build the wheel against Python 3.14. Pinned interpreter so the wheel
# ABI tag matches the runtime stage's interpreter.
RUN cd src/surge-py \
    && maturin build --release --interpreter python3.14 --out /build/wheels

# ─── Stage 2 · runtime ─────────────────────────────────────────────────────
FROM ubuntu:26.04 AS runtime

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

# Runtime shared libraries.
#
# We pull the -dev package for Ipopt (rather than the bare runtime
# libipopt3) because surge's libloading code looks for the unversioned
# symlink ``libipopt.so`` which only ships in the -dev package. The
# headers are unused at runtime (~5 MB extra) but skipping symlink
# curation keeps this Dockerfile arch-agnostic across amd64
# (x86_64-linux-gnu) and arm64 (aarch64-linux-gnu) without hand-rolled
# ln steps.
#
# HiGHS is NOT pulled from apt — we copy the v1.14.0 build from the
# wheel-builder stage into /opt/highs (see below). Ubuntu 26.04 ships
# an older HiGHS than we want to run against.
#
# libsuitesparse-dev is pulled in (vs. enumerating libklu5, libamd3, …
# individually) for simplicity — ~25 MB extra is acceptable.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl \
      python3.14 python3.14-venv python3-pip \
      coinor-libipopt-dev \
      libsuitesparse-dev \
      libopenblas0 libgfortran5 \
    && rm -rf /var/lib/apt/lists/*

# Copy the HiGHS v1.14.0 shared-library install from the wheel-builder
# stage. libloading picks it up via HIGHS_LIB_DIR (set below).
COPY --from=wheel-builder /opt/highs /opt/highs

# Dedicated venv so `pip install` doesn't hit PEP 668's externally-managed
# wall that Ubuntu's system python enforces.
RUN python3.14 -m venv /opt/surge-venv
ENV PATH="/opt/surge-venv/bin:${PATH}"
RUN pip install --no-cache-dir --upgrade pip

# Install the wheel + dashboard extras (fastapi, uvicorn, sse-starlette,
# networkx, scipy, httpx) in one resolve so pip shares the dep solve.
# scipy is required by NetworkX ≥3.x for the layout algorithms the RTO
# dashboard's grid tab uses (kamada_kawai_layout + spring_layout both
# call into scipy.sparse for the all-pairs distance / FR matrices).
COPY --from=wheel-builder /build/wheels/*.whl /tmp/
RUN pip install --no-cache-dir \
      /tmp/surge_py-*.whl \
      'fastapi>=0.100' \
      'uvicorn[standard]>=0.30' \
      'sse-starlette>=2.0' \
      'networkx>=3.0' \
      'scipy>=1.11' \
      'httpx>=0.27' \
      'zstandard>=0.22' \
    && rm /tmp/*.whl

# Dashboard code + market specs + benchmark harness live alongside the
# installed wheel so the server can import them. PYTHONPATH=/opt/surge
# makes these top-level packages visible. ``examples/`` ships the IEEE
# + GO C3 starter cases the RTO dashboard exposes as presets.
COPY markets /opt/surge/markets
COPY benchmarks /opt/surge/benchmarks
COPY dashboards /opt/surge/dashboards
COPY examples /opt/surge/examples

# Register /opt/highs/lib with the dynamic linker so bare-name lookups
# (e.g. via libloading's fallback path) resolve libhighs.so without us
# relying on LD_LIBRARY_PATH. Done as root before the USER switch below.
RUN echo "/opt/highs/lib" > /etc/ld.so.conf.d/highs.conf && ldconfig

# Non-root runtime user. UID 1000 matches typical host users so any files
# written into /data are owned sensibly on the host side. Ubuntu 26.04
# ships a default ``ubuntu`` user at 1000:1000 — drop it first so we own
# the slot cleanly.
RUN (id -u ubuntu >/dev/null 2>&1 && userdel -r ubuntu 2>/dev/null) || true \
    && (getent group ubuntu >/dev/null 2>&1 && groupdel ubuntu) || true \
    && groupadd -g 1000 surge \
    && useradd -m -u 1000 -g 1000 -s /bin/bash surge \
    && mkdir -p /data /results \
    && chown -R surge:surge /opt/surge /data /results
USER surge
WORKDIR /opt/surge

# Solver shared libs are dlopen'd at runtime.
#   - Ipopt: ``libipopt.so`` from the -dev package lands in the multiarch
#     dir (/usr/lib/{x86_64,aarch64}-linux-gnu) and is resolved by
#     libloading's bare-name fallback via ld.so.cache.
#   - HiGHS: ``libhighs.so`` lives in /opt/highs/lib (copied from the
#     wheel-builder stage). HIGHS_LIB_DIR is set explicitly; ldconfig
#     (above) also registers it globally.
ENV HIGHS_LIB_DIR=/opt/highs/lib \
    SURGE_GO_C3_CACHE_ROOT=/data \
    SURGE_GO_C3_RESULTS_ROOT=/results \
    PYTHONPATH=/opt/surge

# Mountable state — dataset + solve runs in /data, leaderboard xlsx + reference
# submissions in /results. Both optional; falling back to an in-container
# /data gives a working (ephemeral) dashboard with no scenarios discovered
# until one is downloaded.
VOLUME ["/data", "/results"]

EXPOSE 8787

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -sf http://127.0.0.1:8787/api/dashboards || exit 1

ENTRYPOINT ["python3.14", "-m", "dashboards.server"]
CMD ["--host", "0.0.0.0", "--port", "8787"]
