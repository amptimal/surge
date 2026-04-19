# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""FastAPI server for the RTO day-ahead market dashboard."""

from .app import create_app

__all__ = ["create_app"]
