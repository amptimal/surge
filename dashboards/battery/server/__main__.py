# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Launch the battery dashboard standalone: ``python -m dashboards.battery.server``."""

from __future__ import annotations

import argparse
import logging

import uvicorn


def main() -> None:
    parser = argparse.ArgumentParser(description="Surge battery operator dashboard")
    parser.add_argument(
        "--host",
        default="127.0.0.1",
        help="bind address (local only by default)",
    )
    parser.add_argument("--port", type=int, default=8788)
    parser.add_argument(
        "--reload",
        action="store_true",
        help="watch dashboards/ + markets/ + surge extension; restart on change",
    )
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    uvicorn.run(
        "dashboards.battery.server.app:app",
        host=args.host,
        port=args.port,
        reload=args.reload,
        reload_dirs=[
            "dashboards/battery",
            "markets/battery",
            "src/surge-py/python/surge",
        ]
        if args.reload
        else None,
        log_level="info",
    )


if __name__ == "__main__":
    main()
