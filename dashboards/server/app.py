# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Hub FastAPI app that discovers dashboards and mounts each one.

Each dashboard at ``dashboards/<name>/`` declares itself via
``manifest.json`` and exposes a FastAPI factory (by convention,
``dashboards.<name>.server.app:create_app``). The hub:

1. Scans ``dashboards/*/manifest.json`` at startup.
2. Calls each dashboard's factory to get its FastAPI app.
3. Mounts the app at ``/<route>`` (from the manifest).
4. Serves the landing page at ``/`` listing all dashboards.
5. Serves ``/api/dashboards`` returning the manifest list.

Individual dashboards can still be run standalone — e.g.
``python -m dashboards.go_c3.server`` binds the GO C3 app directly
at ``/`` without the hub.

Dashboard frontends must use **relative** URLs for their own API
and static routes (``fetch("api/cases")``, ``href="static/x.css"``)
so they resolve correctly both when mounted under a prefix and when
running standalone.
"""

from __future__ import annotations

import logging
from contextlib import AsyncExitStack, asynccontextmanager
from pathlib import Path
from typing import Any, AsyncIterator

from fastapi import FastAPI
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles

from dashboards.manifest import DashboardManifest, discover_dashboards

logger = logging.getLogger("dashboards.hub")

STATIC_DIR = Path(__file__).parent / "static"


def create_hub_app(
    dashboards_root: Path | None = None,
    *,
    include: list[str] | None = None,
) -> FastAPI:
    """Build the hub FastAPI app.

    ``include``: optional whitelist of dashboard ``name`` values to
    mount. ``None`` mounts every discovered dashboard.
    """
    manifests = discover_dashboards(dashboards_root)
    if include is not None:
        wanted = set(include)
        manifests = [m for m in manifests if m.name in wanted]

    mounted: list[tuple[DashboardManifest, FastAPI]] = []
    skipped: list[DashboardManifest] = []
    for manifest in manifests:
        try:
            factory = manifest.load_app_factory()
        except Exception as exc:  # noqa: BLE001
            logger.warning(
                "dashboard %r at %s: failed to load app factory (%s) — skipping",
                manifest.name,
                manifest.source_dir,
                exc,
            )
            skipped.append(manifest)
            continue
        try:
            sub_app = factory()
        except Exception as exc:  # noqa: BLE001
            logger.warning(
                "dashboard %r: factory raised %s — skipping",
                manifest.name,
                exc,
            )
            skipped.append(manifest)
            continue
        mounted.append((manifest, sub_app))

    # Chain each mounted sub-app's lifespan into the hub's lifespan.
    # Starlette / FastAPI does not propagate lifespans to apps attached
    # via ``.mount()``, so without this every sub-app's ``@asynccontextmanager``
    # lifespan handler is a no-op — leaving startup state (e.g. the
    # GO C3 dashboard's ``app.state.registry``) unset and every API
    # request 500s on first state access.
    @asynccontextmanager
    async def lifespan(_app: FastAPI) -> AsyncIterator[None]:
        async with AsyncExitStack() as stack:
            for manifest, sub_app in mounted:
                router = sub_app.router
                if router.lifespan_context is None:
                    continue
                try:
                    await stack.enter_async_context(router.lifespan_context(sub_app))
                except Exception:  # noqa: BLE001
                    logger.exception(
                        "dashboard %r: lifespan startup failed",
                        manifest.name,
                    )
                    raise
            logger.info(
                "hub ready · mounted=%s",
                ", ".join(m.name for m, _ in mounted) or "(none)",
            )
            yield

    app = FastAPI(title="Surge Dashboards", version="0.1", lifespan=lifespan)

    for manifest, sub_app in mounted:
        mount_path = "/" + manifest.route.strip("/")
        app.mount(mount_path, sub_app, name=manifest.name)
        logger.info("mounted dashboard %r at %s", manifest.name, mount_path)

    # Stash for the API + landing.
    app.state.mounted_dashboards = [m for m, _ in mounted]

    @app.get("/api/dashboards")
    def list_dashboards() -> JSONResponse:
        return JSONResponse(
            {
                "dashboards": [m.to_summary() for m in app.state.mounted_dashboards],
            }
        )

    @app.get("/")
    def landing() -> FileResponse:
        return FileResponse(STATIC_DIR / "landing.html")

    if STATIC_DIR.exists():
        app.mount("/static", StaticFiles(directory=STATIC_DIR), name="hub_static")

    return app


app = create_hub_app()


__all__ = ["app", "create_hub_app"]
