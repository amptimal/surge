# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Default :class:`MarketConfig` + reserve products for the RTO day-ahead market."""

from __future__ import annotations

from dataclasses import replace

from surge.market import (
    LossFactorRules,
    MarketConfig,
    NON_SPINNING,
    REG_DOWN,
    REG_UP,
    ReserveProductDef,
    SPINNING,
)

from .policy import RtoPolicy


#: Four-product day-ahead AS set: regulation up/down, spinning, non-spinning.
#: Ramp and reactive products are not part of the phase-1 scope.
RTO_DEFAULT_RESERVE_PRODUCTS: tuple[ReserveProductDef, ...] = (
    REG_UP,
    REG_DOWN,
    SPINNING,
    NON_SPINNING,
)


def default_config(
    policy: RtoPolicy | None = None,
    *,
    base_mva: float = 100.0,
) -> MarketConfig:
    """Return the canonical RTO day-ahead :class:`MarketConfig`.

    Starts from :meth:`MarketConfig.default` and overrides:

    * **Power-balance penalty** → VOLL-scaled (default $9 000/MWh).
    * **Thermal penalty** → $5 000/MWh default (overloads rarely preferred
      to scarcity).

    AC reconcile is not invoked by the day-ahead workflow (no AC stage),
    so the :attr:`MarketConfig.ac_reconcile` defaults are carried along
    unused — they matter only if a caller rebuilds the workflow with an
    explicit AC redispatch stage.

    The reserve-product set is not part of :class:`MarketConfig`; build
    it once and pass it into the dispatch request via
    :func:`default_reserve_products`.
    """
    policy = policy or RtoPolicy()
    config = MarketConfig.default(
        base_mva,
        p_bus_vio_cost=policy.voll_per_mwh,
        q_bus_vio_cost=policy.voll_per_mwh,
        s_vio_cost=policy.thermal_overload_cost_per_mwh,
    )
    # Disable DC loss factors by default — the ISO day-ahead settlement
    # assumes a lossless DC clearing, which keeps ``gen_revenue ==
    # load_payment + congestion_rent`` cleanly. Callers who want the
    # losses allowance can pass a :class:`MarketConfig` with loss
    # factors re-enabled.
    network_rules = replace(
        config.network_rules,
        loss_factors=LossFactorRules(enabled=False),
    )
    return replace(config, network_rules=network_rules)


def default_reserve_products(
    policy: RtoPolicy | None = None,
) -> tuple[ReserveProductDef, ...]:
    """Four-product AS set with the policy's shortfall cost applied."""
    policy = policy or RtoPolicy()
    return tuple(
        ReserveProductDef(
            id=p.id,
            name=p.name,
            direction=p.direction,
            qualification=p.qualification,
            energy_coupling=p.energy_coupling,
            shared_limit_products=p.shared_limit_products,
            balance_products=p.balance_products,
            deploy_secs=p.deploy_secs,
            kind=p.kind,
            shortfall_cost_per_mw=policy.reserve_shortfall_cost_per_mwh,
        )
        for p in RTO_DEFAULT_RESERVE_PRODUCTS
    )


__all__ = [
    "RTO_DEFAULT_RESERVE_PRODUCTS",
    "default_config",
    "default_reserve_products",
]
