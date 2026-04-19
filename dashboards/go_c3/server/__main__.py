# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Launch the dashboard server: ``python -m dashboards.go_c3.server``."""
from __future__ import annotations

import argparse
import logging

import uvicorn


def main() -> None:
    parser = argparse.ArgumentParser(description="GO C3 dashboard server")
    parser.add_argument("--host", default="127.0.0.1", help="bind address (local only by default)")
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument(
        "--reload",
        action="store_true",
        help="watch Python + .so files; restart on change (dev only)",
    )
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    uvicorn.run(
        "dashboards.go_c3.server.app:app",
        host=args.host,
        port=args.port,
        reload=args.reload,
        reload_dirs=[
            "dashboards/go_c3",
            "benchmarks/go_c3",
            "markets/go_c3",
            "src/surge-py/python/surge",
        ]
        if args.reload
        else None,
        log_level="info",
    )


if __name__ == "__main__":
    main()
