# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Shared problem/policy contracts for markets.

A market module under ``markets/<name>/`` declares a ``Problem``
dataclass whose shape conforms to :class:`MarketProblem`. The
contract is:

* ``build_network(policy)`` returns a :class:`surge.Network`
  (the topology the dispatch solves on).
* ``build_request(policy)`` returns the canonical
  :class:`DispatchRequest` dict the solver consumes.

Everything else — forecasts, reserve requirements, site parameters,
etc. — is concrete to the market and lives as typed fields on the
dataclass. The two methods above are the only entry shape the
framework and downstream tooling rely on.

Markets with multi-stage workflows (SCUC → AC-SCED) can still
conform: ``build_request`` returns the request for the first (SCUC)
stage, and later stages are driven by a :class:`MarketWorkflow`.
"""

from __future__ import annotations

from typing import Any, Protocol, runtime_checkable


@runtime_checkable
class MarketProblem(Protocol):
    """Protocol every ``markets/<name>/problem.py`` should satisfy.

    Runtime-checkable so tests can assert conformance with
    ``isinstance(problem, MarketProblem)``.
    """

    def build_network(self, policy: Any) -> Any:
        """Build or return the :class:`surge.Network` for this problem."""
        ...

    def build_request(self, policy: Any) -> dict[str, Any]:
        """Assemble the canonical :class:`DispatchRequest` dict."""
        ...


__all__ = ["MarketProblem"]
