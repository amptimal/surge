# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""FastAPI application factory for the GO C3 dashboard server."""
from __future__ import annotations

import asyncio
import logging
import os
import threading
from contextlib import asynccontextmanager
from pathlib import Path
from typing import AsyncIterator

from fastapi import FastAPI
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles

from benchmarks.go_c3.paths import default_cache_root

from .case_data import CaseRegistry
from .jobs import JobBus
from .routes import make_api_router

logger = logging.getLogger("go_c3.server")

STATIC_DIR = Path(__file__).parent / "static"


def create_app(cache_root: Path | None = None) -> FastAPI:
    # Precedence: explicit kwarg → SURGE_GO_C3_CACHE_ROOT env → repo-relative default.
    # The env var is the knob the Docker image uses to point /data at the mounted
    # dataset + runs dir, since the repo-tree fallback doesn't exist inside the
    # container.
    if cache_root is None:
        env_root = os.environ.get("SURGE_GO_C3_CACHE_ROOT")
        if env_root:
            cache_root = Path(env_root).expanduser().resolve()
    resolved_root = cache_root or default_cache_root()

    @asynccontextmanager
    async def lifespan(app: FastAPI) -> AsyncIterator[None]:
        # Import surge once at startup so every request hits the same dylib.
        # We don't assign it to app.state — callers reach for it via
        # _import_surge() which handles sys.path, making Phase 3's solve
        # workers reuse the same module.
        from benchmarks.go_c3.runner import _import_surge

        _import_surge()
        app.state.cache_root = resolved_root
        app.state.registry = CaseRegistry(resolved_root)
        app.state.jobs = JobBus()
        app.state.jobs.bind_loop(asyncio.get_running_loop())

        # Pre-warm the leaderboard workbook cache in the background. Cold parse
        # of the 30 MB competition xlsx dominates first-case load time (~4.7s),
        # and its result is process-wide (lru_cache on path + mtime). Running
        # this off the main thread lets the server start accepting requests
        # immediately — if the first case request races the warm-up, it just
        # waits on the same lru_cache entry.
        def _prewarm() -> None:
            from benchmarks.go_c3 import references
            try:
                references.preload_leaderboard_workbooks(resolved_root)
            except Exception:  # noqa: BLE001
                logger.exception("leaderboard workbook pre-warm failed (non-fatal)")

        threading.Thread(target=_prewarm, name="go_c3_prewarm", daemon=True).start()

        logger.info("go_c3 server ready · cache_root=%s", resolved_root)
        try:
            yield
        finally:
            app.state.jobs.shutdown()
            logger.info("go_c3 server shutdown")

    app = FastAPI(title="GO C3 Dashboard", version="0.1", lifespan=lifespan)
    app.include_router(make_api_router())

    @app.get("/")
    def index() -> FileResponse:
        return FileResponse(STATIC_DIR / "index.html")

    if STATIC_DIR.exists():
        app.mount("/static", StaticFiles(directory=STATIC_DIR), name="static")
    return app


app = create_app()
