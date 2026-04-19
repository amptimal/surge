# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""HTTP routes for the GO C3 dashboard server."""
from __future__ import annotations

import asyncio
from typing import Any

from fastapi import APIRouter, HTTPException, Request, Response
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .health import health_snapshot
from .jobs import make_solve_target, make_validate_target


class SolveRequest(BaseModel):
    policy: dict[str, Any] | None = None


class ValidateRequest(BaseModel):
    pass


def make_api_router() -> APIRouter:
    router = APIRouter(prefix="/api")

    @router.get("/health")
    def health() -> dict[str, Any]:
        return health_snapshot()

    @router.get("/cases")
    def list_cases(request: Request) -> dict[str, Any]:
        return {"cases": request.app.state.registry.index()}

    @router.get("/cases/{dataset}/{division}/{sw}/{scenario_id}")
    def get_case(dataset: str, division: str, sw: str, scenario_id: int, request: Request) -> dict[str, Any]:
        key = f"{dataset}/{division}/{sw}/{scenario_id}"
        try:
            data = request.app.state.registry.load_case(key)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        if data is None:
            raise HTTPException(status_code=404, detail=f"case not available: {key}")
        return data

    @router.get("/cases/{dataset}/{division}/{sw}/{scenario_id}/log")
    def get_case_log(dataset: str, division: str, sw: str, scenario_id: int, request: Request) -> Response:
        key = f"{dataset}/{division}/{sw}/{scenario_id}"
        try:
            body = request.app.state.registry.solve_log(key)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        if body is None:
            raise HTTPException(status_code=404, detail="solve log not found")
        return Response(content=body, media_type="text/plain; charset=utf-8")

    @router.get("/cases/{dataset}/{division}/{sw}/{scenario_id}/archives")
    def list_archives(dataset: str, division: str, sw: str, scenario_id: int, request: Request) -> dict[str, Any]:
        key = f"{dataset}/{division}/{sw}/{scenario_id}"
        try:
            archives = request.app.state.registry.list_archives(key)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        return {"key": key, "archives": archives}

    @router.get("/cases/{dataset}/{division}/{sw}/{scenario_id}/archives/{timestamp}")
    def get_archive(
        dataset: str,
        division: str,
        sw: str,
        scenario_id: int,
        timestamp: str,
        request: Request,
    ) -> dict[str, Any]:
        key = f"{dataset}/{division}/{sw}/{scenario_id}"
        try:
            report = request.app.state.registry.archive_run_report(key, timestamp)
        except FileNotFoundError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        return {"key": key, "timestamp": timestamp, "run_report": report}

    @router.get("/cases/{dataset}/{division}/{sw}/{scenario_id}/archives/{timestamp}/log")
    def get_archive_log(
        dataset: str,
        division: str,
        sw: str,
        scenario_id: int,
        timestamp: str,
        request: Request,
    ) -> Response:
        key = f"{dataset}/{division}/{sw}/{scenario_id}"
        try:
            body = request.app.state.registry.archive_solve_log(key, timestamp)
        except FileNotFoundError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        if body is None:
            raise HTTPException(status_code=404, detail="archive has no solve.log")
        return Response(content=body, media_type="text/plain; charset=utf-8")

    # ─── jobs: solve + validate ──────────────────────────────────────────

    @router.post("/cases/{dataset}/{division}/{sw}/{scenario_id}/solve")
    def post_solve(
        dataset: str,
        division: str,
        sw: str,
        scenario_id: int,
        body: SolveRequest,
        request: Request,
    ) -> dict[str, Any]:
        key = f"{dataset}/{division}/{sw}/{scenario_id}"
        registry = request.app.state.registry
        try:
            registry._scenario_for(key)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        bus = request.app.state.jobs
        target = make_solve_target(registry, key, body.policy, bus)
        job = bus.submit(kind="solve", case_key=key, target=target, policy=body.policy)
        return job.to_dict()

    @router.post("/cases/{dataset}/{division}/{sw}/{scenario_id}/validate")
    def post_validate(
        dataset: str,
        division: str,
        sw: str,
        scenario_id: int,
        body: ValidateRequest,
        request: Request,
    ) -> dict[str, Any]:
        key = f"{dataset}/{division}/{sw}/{scenario_id}"
        registry = request.app.state.registry
        try:
            registry._scenario_for(key)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        bus = request.app.state.jobs
        target = make_validate_target(registry, key, bus)
        job = bus.submit(kind="validate", case_key=key, target=target)
        return job.to_dict()

    @router.get("/jobs")
    def list_jobs(request: Request) -> dict[str, Any]:
        jobs = request.app.state.jobs.list_recent()
        return {"jobs": [j.to_dict() for j in jobs]}

    @router.get("/jobs/{job_id}")
    def get_job(job_id: str, request: Request) -> dict[str, Any]:
        job = request.app.state.jobs.get(job_id)
        if job is None:
            raise HTTPException(status_code=404, detail=f"job not found: {job_id}")
        payload = job.to_dict()
        payload["log"] = request.app.state.jobs.log_text(job)
        return payload

    @router.get("/jobs/{job_id}/stream")
    async def stream_job(job_id: str, request: Request) -> StreamingResponse:
        job = request.app.state.jobs.get(job_id)
        if job is None:
            raise HTTPException(status_code=404, detail=f"job not found: {job_id}")
        bus = request.app.state.jobs
        queue = await bus.subscribe(job, replay_log=True)

        async def event_gen():
            try:
                while True:
                    if await request.is_disconnected():
                        break
                    try:
                        payload = await asyncio.wait_for(queue.get(), timeout=1.0)
                    except asyncio.TimeoutError:
                        if job.status in ("succeeded", "failed"):
                            while not queue.empty():
                                yield queue.get_nowait()
                            break
                        yield ": keepalive\n\n"
                        continue
                    yield payload
            finally:
                bus.unsubscribe(job, queue)

        return StreamingResponse(
            event_gen(),
            media_type="text/event-stream",
            headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"},
        )

    return router
