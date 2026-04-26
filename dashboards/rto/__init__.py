# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Interactive RTO dashboard wrapping the canonical go_c3 native pipeline.

Every solve runs through :func:`surge.market.go_c3.solve_workflow`,
the validator-aligned two-stage SCUC → AC SCED that the GO-C3
benchmark uses. The dashboard adds:

* a configurable policy form (workflow shape, solver / commitment
  knobs, loss + N-1 security tuning, AC SCED tuning including
  reactive-pin retry),
* per-bus / per-resource visualizations (LMP heatmap, generator
  trace, AS pricing) of the native pipeline's solved output.

Cases are restricted to the bundled GO-C3 problem archives —
arbitrary ``surge.Network`` cases can't be solved by the goc3
native pipeline, so they don't appear in the registry.
"""

from .server.app import create_app

__all__ = ["create_app"]
