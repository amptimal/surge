# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Live FastAPI server for the GO C3 dashboard."""
from .app import create_app

__all__ = ["create_app"]
