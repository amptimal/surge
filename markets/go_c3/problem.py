# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Re-export of the canonical :class:`surge.market.go_c3.GoC3Problem`.

The market layer used to define its own :class:`GoC3Problem` dataclass
that wrapped a path + the parsed JSON. The canonical
:class:`surge.market.go_c3.GoC3Problem` carries the Rust-side handle
needed by the native workflow builder *and* the same convenience
properties (``periods``, ``buses``, ``summary()``, ...). The two
versions are now one — this module re-exports the canonical one.
"""

from __future__ import annotations

from surge.market.go_c3 import GoC3Problem

__all__ = ["GoC3Problem"]
