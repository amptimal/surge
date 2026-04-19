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
from pathlib import Path
from typing import Any

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
    app = FastAPI(title="Surge Dashboards", version="0.1")
    manifests = discover_dashboards(dashboards_root)
    if include is not None:
        wanted = set(include)
        manifests = [m for m in manifests if m.name in wanted]

    mounted: list[DashboardManifest] = []
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
            continue
        try:
            sub_app = factory()
        except Exception as exc:  # noqa: BLE001
            logger.warning(
                "dashboard %r: factory raised %s — skipping",
                manifest.name,
                exc,
            )
            continue
        mount_path = "/" + manifest.route.strip("/")
        app.mount(mount_path, sub_app, name=manifest.name)
        mounted.append(manifest)
        logger.info("mounted dashboard %r at %s", manifest.name, mount_path)

    # Stash for the API + landing.
    app.state.mounted_dashboards = mounted

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
