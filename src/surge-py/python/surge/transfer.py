# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Transfer-capability workflows exposed under an explicit namespace."""

from __future__ import annotations

from . import _native

BldfResult = _native.BldfResult
GsfResult = _native.GsfResult
InjectionCapabilityResult = _native.InjectionCapabilityResult
TransferStudy = _native.TransferStudy
NercAtcResult = _native.NercAtcResult
AcAtcResult = _native.AcAtcResult
AfcResult = _native.AfcResult
MultiTransferResult = _native.MultiTransferResult
TransferPath = _native.TransferPath
Flowgate = _native.Flowgate
AtcOptions = _native.AtcOptions


def compute_bldf(network):
    """Compute Bus Load Distribution Factor matrix.

    BLDF[b, l] gives the change in per-unit flow on branch l per 1 p.u.
    load increase at bus b (slack absorbs the difference).

    Args:
        network: Power system network.

    Returns:
        BldfResult with matrix (n_buses x n_branches), bus_numbers,
        branch_from, branch_to.
    """
    return _native.compute_bldf(network)


def compute_gsf(network):
    """Compute Generation Shift Factor matrix.

    GSF[l, g] gives the change in flow on branch l per unit increase
    in generation at generator g.

    Args:
        network: Power system network.

    Returns:
        GsfResult with gsf matrix (n_branches x n_generators), gen_buses,
        branch_from, branch_to.
    """
    return _native.compute_gsf(network)


def compute_injection_capability(
    network,
    post_contingency_rating_fraction=1.0,
    exact=False,
    monitored_branches=None,
    contingency_branches=None,
    slack_weights=None,
):
    """Compute per-bus injection capability considering N-1 constraints.

    Args:
        network: Power system network.
        post_contingency_rating_fraction: Fraction of branch rating used post-contingency.
        exact: When True, re-solve each outage exactly instead of LODF screening.
        monitored_branches: Optional list of monitored branch indices.
        contingency_branches: Optional list of outage branch indices.
        slack_weights: Optional per-bus slack participation weights.

    Returns:
        InjectionCapabilityResult with per-bus capability data.
    """
    return _native.compute_injection_capability(
        network,
        post_contingency_rating_fraction=post_contingency_rating_fraction,
        exact=exact,
        monitored_branches=monitored_branches,
        contingency_branches=contingency_branches,
        slack_weights=slack_weights,
    )


def compute_nerc_atc(network, path, options=None):
    """Compute NERC Available Transfer Capability (MOD-029/MOD-030).

    ATC = TTC - TRM - CBM - ETC.

    Args:
        network: Power system network.
        path: TransferPath defining source and sink buses.
        options: Optional AtcOptions with monitored branches, contingencies, and margins.

    Returns:
        NercAtcResult with atc_mw, ttc_mw, trm_mw, cbm_mw, etc_mw, limit_cause,
        and derived binding info.
    """
    return _native.compute_nerc_atc(network, path, options)


def compute_ac_atc(network, path, v_min=0.95, v_max=1.05):
    """Compute AC-aware Available Transfer Capability with voltage constraints.

    Args:
        network: Power system network.
        path: TransferPath defining source and sink buses.
        v_min: Minimum acceptable voltage magnitude (p.u.).
        v_max: Maximum acceptable voltage magnitude (p.u.).

    Returns:
        AcAtcResult with atc_mw, thermal and voltage limits, and binding info.
    """
    return _native.compute_ac_atc(network, path, v_min, v_max)


def compute_afc(network, path, flowgates):
    """Compute Available Flowgate Capability for a list of flowgates.

    Args:
        network: Power system network.
        path: TransferPath defining the transfer direction.
        flowgates: List of Flowgate definitions to evaluate.

    Returns:
        List of AfcResult, one per flowgate, with afc_mw and binding info.
    """
    return _native.compute_afc(network, path, flowgates)


def compute_multi_transfer(network, paths, weights=None, max_transfer_mw=None):
    """Compute simultaneous transfer capability across multiple paths.

    Args:
        network: Power system network.
        paths: List of TransferPath objects to optimize jointly.
        weights: Optional per-path weights for the objective function.
        max_transfer_mw: Optional per-path upper bounds on transfer (MW).

    Returns:
        MultiTransferResult with per-path transfer_mw and binding info.
    """
    return _native.compute_multi_transfer(network, paths, weights, max_transfer_mw)


def prepare_transfer_study(network):
    """Prepare reusable transfer-study state for repeated ATC, AFC, and interface runs.

    Args:
        network: Power system network.

    Returns:
        TransferStudy that can be reused for multiple transfer computations.
    """
    return _native.prepare_transfer_study(network)

__all__ = [
    "AcAtcResult",
    "AfcResult",
    "AtcOptions",
    "BldfResult",
    "Flowgate",
    "GsfResult",
    "InjectionCapabilityResult",
    "MultiTransferResult",
    "NercAtcResult",
    "TransferPath",
    "TransferStudy",
    "compute_ac_atc",
    "compute_afc",
    "compute_bldf",
    "compute_gsf",
    "compute_injection_capability",
    "compute_multi_transfer",
    "compute_nerc_atc",
    "prepare_transfer_study",
]
