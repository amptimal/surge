# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""FastAPI server for the datacenter dashboard."""

from .app import create_app

__all__ = ["create_app"]
