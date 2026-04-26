# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""FastAPI app for the RTO day-ahead market dashboard."""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Any

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import FileResponse, JSONResponse, StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field

from dashboards.rto import api as rto_api

logger = logging.getLogger("dashboards.rto.server")

STATIC_DIR = Path(__file__).parent / "static"


class SolveRequest(BaseModel):
    """Scenario body the frontend POSTs to /api/solve (native save/load format)."""

    # We don't enforce an exact schema here — the adapter in ``api.py``
    # validates sub-sections and fills in defaults. Keeping this loose
    # lets the save/load round-trip carry forward fields the server may
    # not yet understand (forwards-compatible).
    source: dict[str, Any] = Field(default_factory=dict)
    time_axis: dict[str, Any] = Field(default_factory=dict)
    load_config: dict[str, Any] = Field(default_factory=dict)
    offers_config: dict[str, Any] = Field(default_factory=dict)
    renewables_config: dict[str, Any] = Field(default_factory=dict)
    reserves_config: dict[str, Any] = Field(default_factory=dict)
    policy: dict[str, Any] = Field(default_factory=dict)
    generators: list[dict[str, Any]] = Field(default_factory=list)
    loads: list[dict[str, Any]] = Field(default_factory=list)
    network_summary: dict[str, Any] = Field(default_factory=dict)
    network_payload: str | None = None  # raw Surge-JSON for custom uploads


class NetworkUpload(BaseModel):
    """Plain JSON payload representing a Surge network (produced by surge.dump)."""

    title: str | None = None
    payload: str  # serialized Surge-JSON text


def create_app() -> FastAPI:
    app = FastAPI(title="Surge RTO Dashboard", version="0.1")

    @app.get("/api/meta")
    def meta() -> dict[str, Any]:
        return {
            "dashboard": "rto",
            "cases": rto_api.available_cases(),
            "profile_shapes": list(rto_api.LOAD_PROFILE_SHAPES.keys()),
            "renewable_shapes": list(rto_api.RENEWABLE_PROFILE_SHAPES.keys()),
            "commitment_modes": ["optimize", "all_committed", "fixed_initial"],
            "lp_solvers": ["highs", "gurobi"],
            "reserve_products": ["reg_up", "reg_down", "syn", "nsyn"],
        }

    @app.get("/api/cases/{case_id}/scaffold")
    def scaffold(case_id: str) -> dict[str, Any]:
        try:
            return rto_api.build_scaffold(case_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        except Exception as exc:  # noqa: BLE001
            logger.exception("scaffold failed for case %r", case_id)
            raise HTTPException(status_code=500, detail=f"scaffold failed: {exc}")

    @app.post("/api/upload-network")
    def upload_network(body: NetworkUpload) -> dict[str, Any]:
        """Accept a serialized Surge-JSON payload and return a scaffold scenario.

        The payload ships back verbatim in the scaffold's ``network_payload``
        field so the subsequent ``/api/solve`` call can rebuild the network
        without the client holding the string.
        """
        import tempfile, surge  # type: ignore
        with tempfile.NamedTemporaryFile(mode="w", suffix=".surge.json", delete=False) as fh:
            fh.write(body.payload)
            tmp = Path(fh.name)
        try:
            network = surge.load(str(tmp))
        except Exception as exc:  # noqa: BLE001
            raise HTTPException(status_code=400, detail=f"invalid network: {exc}")
        finally:
            tmp.unlink(missing_ok=True)
        scaffold_dict = rto_api.build_scaffold(
            case_id=None, network=network, case_title=body.title or "uploaded network"
        )
        scaffold_dict["network_payload"] = body.payload
        return scaffold_dict

    @app.post("/api/solve")
    def solve(body: SolveRequest) -> JSONResponse:
        try:
            result = rto_api.run_solve(body.model_dump())
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc))
        except Exception as exc:  # noqa: BLE001
            logger.exception("RTO solve failed")
            raise HTTPException(status_code=500, detail=f"solve failed: {exc}")
        return JSONResponse(result)

    @app.post("/api/solve/stream")
    def solve_stream(body: SolveRequest) -> StreamingResponse:
        """Stream the solve as Server-Sent Events.

        The dashboard's modal overlay subscribes to this endpoint
        so the user can watch log lines arrive in real time
        instead of staring at a spinner. The wire format is plain
        SSE: ``event: log`` per Python log line, one ``event:
        result`` carrying the final flattened-result JSON, or
        ``event: error`` on failure. Heartbeat ``: ping`` comments
        keep idle connections alive across reverse proxies.
        """
        scenario = body.model_dump()
        return StreamingResponse(
            rto_api.run_solve_stream(scenario),
            media_type="text/event-stream",
            headers={
                "Cache-Control": "no-cache",
                "X-Accel-Buffering": "no",  # disable nginx buffering when behind one
            },
        )

    @app.get("/")
    def index() -> FileResponse:
        return FileResponse(STATIC_DIR / "index.html")

    if STATIC_DIR.exists():
        app.mount("/static", StaticFiles(directory=STATIC_DIR), name="rto_static")

    return app


app = create_app()


__all__ = ["app", "create_app"]
