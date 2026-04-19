# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Discover dashboards under :mod:`dashboards` by their ``manifest.json``.

Each dashboard lives at ``dashboards/<name>/`` and declares itself
with a ``manifest.json`` file. The hub server at
:mod:`dashboards.server` uses :func:`discover_dashboards` to build
its landing page and mount each dashboard's FastAPI app at
``/<route>``.

Manifest shape::

    {
      "name": "go_c3",                        # python sub-package name
      "title": "GO Challenge 3",              # human-readable
      "description": "ARPA-E GO Challenge 3 baseline dashboard.",
      "route": "go_c3",                       # url path prefix (typically == name)
      "icon": "⚡",                            # optional emoji or symbol
      "tag": "competition",                   # optional category label
      "app_import": "dashboards.go_c3.server.app:create_app"   # optional; defaults derived
    }

Add a new dashboard by creating the folder + manifest.json — no hub
code changes required.
"""

from __future__ import annotations

import importlib
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable


@dataclass(frozen=True)
class DashboardManifest:
    """Declarative metadata for one dashboard."""

    name: str
    title: str
    description: str
    route: str
    icon: str = "●"
    tag: str = ""
    #: ``"module:callable"`` of a factory that returns a FastAPI app.
    #: Defaults to ``dashboards.<name>.server.app:create_app``.
    app_import: str = ""
    #: Filesystem root of the dashboard (for static file mounts etc.)
    source_dir: Path = field(default_factory=Path)

    @property
    def default_app_import(self) -> str:
        return self.app_import or f"dashboards.{self.name}.server.app:create_app"

    def load_app_factory(self) -> Callable[[], Any]:
        """Import the ``create_app`` factory for this dashboard."""
        module_path, _, attr = self.default_app_import.partition(":")
        module = importlib.import_module(module_path)
        factory = getattr(module, attr or "create_app")
        if not callable(factory):
            raise TypeError(
                f"dashboard {self.name!r}: {self.default_app_import!r} "
                f"is not callable"
            )
        return factory

    def to_summary(self) -> dict[str, Any]:
        """Public JSON shape for the landing page + /api/dashboards."""
        return {
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "route": self.route,
            "icon": self.icon,
            "tag": self.tag,
        }


def discover_dashboards(root: Path | None = None) -> list[DashboardManifest]:
    """Scan ``dashboards/*/manifest.json`` and return one manifest per dashboard.

    Skips any folder without a manifest (e.g. ``_shared/``, the hub
    ``server/``, package-level ``__pycache__``, or WIP dashboards).
    Results are sorted by title for a stable landing-page order.
    """
    root = Path(root) if root else Path(__file__).resolve().parent
    manifests: list[DashboardManifest] = []
    for entry in sorted(root.iterdir()):
        if not entry.is_dir() or entry.name.startswith(("_", ".")):
            continue
        manifest_path = entry / "manifest.json"
        if not manifest_path.is_file():
            continue
        try:
            data = json.loads(manifest_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            raise ValueError(
                f"invalid JSON in {manifest_path}: {exc}"
            ) from exc
        manifests.append(
            DashboardManifest(
                name=str(data.get("name") or entry.name),
                title=str(data.get("title") or entry.name.replace("_", " ").title()),
                description=str(data.get("description") or ""),
                route=str(data.get("route") or entry.name),
                icon=str(data.get("icon") or "●"),
                tag=str(data.get("tag") or ""),
                app_import=str(data.get("app_import") or ""),
                source_dir=entry,
            )
        )
    manifests.sort(key=lambda m: m.title.lower())
    return manifests


__all__ = ["DashboardManifest", "discover_dashboards"]
