# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""FastAPI app for the datacenter operator dashboard.

Stateless — each request supplies a full scenario; the server runs
one SCUC in a temp dir and returns the results.
"""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field

from dashboards.datacenter import api as datacenter_api

logger = logging.getLogger("dashboards.datacenter.server")

STATIC_DIR = Path(__file__).parent / "static"


class SolveRequest(BaseModel):
    """Canonical scenario body the frontend POSTs to /api/solve."""

    time_axis: dict[str, Any] = Field(default_factory=dict)
    site: dict[str, Any] = Field(default_factory=dict)
    lmp_forecast_per_mwh: list[float] = Field(default_factory=list)
    natural_gas_price_per_mmbtu: list[float] = Field(default_factory=list)
    as_products: list[dict[str, Any]] = Field(default_factory=list)
    policy: dict[str, Any] = Field(default_factory=dict)
    four_cp: dict[str, Any] | None = None


def create_app() -> FastAPI:
    app = FastAPI(title="Surge Datacenter Dashboard", version="0.1")

    @app.get("/api/meta")
    def meta() -> dict[str, Any]:
        return {
            "dashboard": "datacenter",
            "available_as_products": datacenter_api.available_as_products(),
            "commitment_modes": list(datacenter_api.COMMITMENT_MODES),
            "period_couplings": list(datacenter_api.PERIOD_COUPLINGS),
            "lp_solvers": ["highs", "gurobi"],
            "limits": {
                "min_resolution_minutes": 5,
                "max_resolution_minutes": 60,
                "max_periods": 744,
            },
        }

    @app.get("/api/default-scenario")
    def default_scenario() -> dict[str, Any]:
        return datacenter_api.default_scenario()

    @app.post("/api/solve")
    def solve(body: SolveRequest) -> JSONResponse:
        try:
            result = datacenter_api.run_solve(body.model_dump())
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc))
        except Exception as exc:  # noqa: BLE001
            logger.exception("datacenter solve failed")
            raise HTTPException(status_code=500, detail=f"solve failed: {exc}")
        return JSONResponse(result)

    @app.get("/")
    def index() -> FileResponse:
        return FileResponse(STATIC_DIR / "index.html")

    if STATIC_DIR.exists():
        app.mount("/static", StaticFiles(directory=STATIC_DIR), name="datacenter_static")

    return app


app = create_app()


__all__ = ["app", "create_app"]
