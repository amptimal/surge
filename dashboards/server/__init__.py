# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Surge dashboard hub.

The hub is a FastAPI application that auto-discovers dashboards under
``dashboards/<name>/`` via their ``manifest.json`` and mounts each
sub-app at its declared ``route``. The landing page at ``/`` lists
every discovered dashboard.

Launch via ``python -m dashboards.server``.
"""

from .app import create_hub_app

__all__ = ["create_hub_app"]
