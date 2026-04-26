# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""FastAPI app for the battery operator dashboard.

Stateless — each request supplies a full scenario; the server runs
one solve in a temp dir and returns the results. Designed for
sub-second iteration loops.
"""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field

from dashboards.battery import api as battery_api

logger = logging.getLogger("dashboards.battery.server")

STATIC_DIR = Path(__file__).parent / "static"


class SolveRequest(BaseModel):
    """Canonical scenario body the frontend POSTs to /api/solve."""

    time_axis: dict[str, Any] = Field(default_factory=dict)
    site: dict[str, Any] = Field(default_factory=dict)
    lmp_forecast_per_mwh: list[float] = Field(default_factory=list)
    as_products: list[dict[str, Any]] = Field(default_factory=list)
    policy: dict[str, Any] = Field(default_factory=dict)
    pwl_strategy: dict[str, Any] | None = None
    # BA ACE / CR distributions used by the post-clearing AS-deployment
    # overlay; opaque to the LP but consumed by api._compute_as_implied.
    distributions: dict[str, Any] = Field(default_factory=dict)


def create_app() -> FastAPI:
    app = FastAPI(title="Surge Battery Dashboard", version="0.1")

    @app.get("/api/meta")
    def meta() -> dict[str, Any]:
        return {
            "dashboard": "battery",
            "available_as_products": battery_api.available_as_products(),
            "dispatch_modes": ["optimal_foresight", "pwl_offers"],
            "period_couplings": ["coupled", "sequential"],
            "limits": {
                "min_resolution_minutes": 1,
                "max_resolution_minutes": 60,
                "max_periods": 8760,
            },
        }

    @app.get("/api/default-scenario")
    def default_scenario() -> dict[str, Any]:
        return battery_api.default_scenario()

    @app.post("/api/solve")
    def solve(body: SolveRequest) -> JSONResponse:
        try:
            result = battery_api.run_solve(body.model_dump())
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc))
        except Exception as exc:  # noqa: BLE001
            logger.exception("battery solve failed")
            raise HTTPException(status_code=500, detail=f"solve failed: {exc}")
        return JSONResponse(result)

    @app.get("/")
    def index() -> FileResponse:
        return FileResponse(STATIC_DIR / "index.html")

    if STATIC_DIR.exists():
        app.mount("/static", StaticFiles(directory=STATIC_DIR), name="battery_static")

    return app


app = create_app()


__all__ = ["app", "create_app"]
