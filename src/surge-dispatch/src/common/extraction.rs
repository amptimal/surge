// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared result-extraction helpers for SCED and SCUC solvers.
//!
//! Each function takes a **solution vector** `x` and a **variable-index function**
//! `col(local_off) -> usize` that maps a local (within-hour) column offset to an
//! absolute index in `x`.  For SCED use `|off| off`; for SCUC use `|off| var_idx(t, off)`.

use std::collections::HashMap;

use surge_network::market::{
    DemandResponseResults, DispatchableLoad, LoadDispatchResult, VirtualBidResult,
};
use surge_opf::advanced::IslandRefs;

use crate::common::spec::DispatchProblemSpec;

// ---------------------------------------------------------------------------
// HVDC dispatch extraction
// ---------------------------------------------------------------------------

/// Extract per-link HVDC dispatch vectors from the LP solution.
///
/// Returns `(hvdc_dispatch_mw, hvdc_band_dispatch_mw)`:
/// - `hvdc_dispatch_mw[k]` = total MW for link `k`
/// - `hvdc_band_dispatch_mw[k]` = per-band MW for banded links (empty for legacy)
///
/// `col` maps a local-hour column offset to an absolute `x` index.
pub(crate) fn extract_hvdc_dispatch(
    x: &[f64],
    spec: &DispatchProblemSpec<'_>,
    hvdc_band_offsets: &[usize],
    base: f64,
    col: impl Fn(usize) -> usize,
) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n_hvdc = spec.hvdc_links.len();
    let mut hvdc_dispatch_mw = Vec::with_capacity(n_hvdc);
    let mut hvdc_band_dispatch_mw = Vec::with_capacity(n_hvdc);

    for (k, hvdc) in spec.hvdc_links.iter().enumerate() {
        if hvdc.is_banded() {
            let band_mw: Vec<f64> = (0..hvdc.bands.len())
                .map(|b| x[col(hvdc_band_offsets[k] + b)] * base)
                .collect();
            let total: f64 = band_mw.iter().sum();
            hvdc_dispatch_mw.push(total);
            hvdc_band_dispatch_mw.push(band_mw);
        } else {
            hvdc_dispatch_mw.push(x[col(hvdc_band_offsets[k])] * base);
            hvdc_band_dispatch_mw.push(vec![]);
        }
    }

    (hvdc_dispatch_mw, hvdc_band_dispatch_mw)
}

// ---------------------------------------------------------------------------
// Virtual bid result extraction
// ---------------------------------------------------------------------------

/// Extract virtual bid results for one period.
///
/// `col` maps a local-hour column offset to an absolute `x` index.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_virtual_bid_results(
    x: &[f64],
    spec: &DispatchProblemSpec<'_>,
    active_vbids: &[usize],
    vbid_off: usize,
    lmp: &[f64],
    bus_map: &HashMap<u32, usize>,
    base: f64,
    col: impl Fn(usize) -> usize,
) -> Vec<VirtualBidResult> {
    active_vbids
        .iter()
        .enumerate()
        .map(|(k, &bi)| {
            let vb = &spec.virtual_bids[bi];
            let cleared_mw = x[col(vbid_off + k)] * base;
            let bus_lmp = bus_map
                .get(&vb.bus)
                .and_then(|&idx| lmp.get(idx).copied())
                .unwrap_or(0.0);
            VirtualBidResult {
                position_id: vb.position_id.clone(),
                bus: vb.bus,
                direction: vb.direction,
                cleared_mw,
                price_per_mwh: vb.price_per_mwh,
                lmp: bus_lmp,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// DR / Dispatchable-load result extraction
// ---------------------------------------------------------------------------

/// Extract dispatchable-load (DR) results for one period.
///
/// `col` maps a local-hour column offset to an absolute `x` index.
/// The `x` values at `dl_off + k` are in p.u. (the LP variable is p.u.);
/// `LoadDispatchResult::from_solution` handles the `base` scaling.
pub(crate) fn extract_dr_results(
    x: &[f64],
    dl_list: &[&DispatchableLoad],
    dl_off: usize,
    lmp: &[f64],
    bus_map: &HashMap<u32, usize>,
    base: f64,
    col: impl Fn(usize) -> usize,
) -> DemandResponseResults {
    if dl_list.is_empty() {
        return DemandResponseResults::default();
    }
    let loads: Vec<LoadDispatchResult> = dl_list
        .iter()
        .enumerate()
        .map(|(k, dl)| {
            let p_served = x[col(dl_off + k)];
            let lmp_at_bus = bus_map
                .get(&dl.bus)
                .and_then(|&idx| lmp.get(idx).copied())
                .unwrap_or(0.0);
            LoadDispatchResult::from_solution(dl, p_served, 0.0, lmp_at_bus, base)
        })
        .collect();
    DemandResponseResults::from_load_results(loads, base)
}

/// Correct DC nodal prices so congestion impacts move away from, not toward,
/// the island reference bus price.
///
/// The sparse DC formulation anchors the reference-bus energy price correctly,
/// but the relative congestion component arrives mirrored in the raw balance-row
/// duals. Reflecting each island around its anchored reference price restores
/// the economically correct direction while keeping the reference price fixed.
pub(crate) fn correct_dc_lmp_orientation(lmp: &mut [f64], island_refs: &IslandRefs) {
    if island_refs.n_islands == 0 || island_refs.island_ref_bus.is_empty() {
        return;
    }

    for (island_id, &ref_bus) in island_refs.island_ref_bus.iter().enumerate() {
        let Some(&ref_lmp) = lmp.get(ref_bus) else {
            continue;
        };
        for (bus_idx, &bus_island) in island_refs.bus_island.iter().enumerate() {
            if bus_island == island_id
                && let Some(bus_lmp) = lmp.get_mut(bus_idx)
            {
                *bus_lmp = 2.0 * ref_lmp - *bus_lmp;
            }
        }
        if let Some(bus_lmp) = lmp.get_mut(ref_bus) {
            *bus_lmp = ref_lmp;
        }
    }
}

// ---------------------------------------------------------------------------
// Branch / flowgate / interface shadow price extraction
// ---------------------------------------------------------------------------

/// Extract single-period branch thermal shadow prices.
///
/// Returns a `Vec<f64>` with one entry per constrained branch in
/// `network_plan.constrained_branches`, preserving that order.
pub(crate) fn extract_branch_shadow_prices_single(
    row_dual: &[f64],
    n_branch_flow: usize,
    base: f64,
) -> Vec<f64> {
    (0..n_branch_flow).map(|row| row_dual[row] / base).collect()
}

/// Extract single-period flowgate shadow prices.
///
/// Returns a `Vec<f64>` with one entry per `network.flowgates` entry (0 for unconstrained).
/// Callers pass `row_dual` from the LP solution, `n_branch_flow` as the base offset.
pub(crate) fn extract_flowgate_shadow_prices_single(
    row_dual: &[f64],
    fg_rows: &[usize],
    n_fg: usize,
    n_branch_flow: usize,
    base: f64,
) -> Vec<f64> {
    let mut v = vec![0.0; n_fg];
    for (ri, &fgi) in fg_rows.iter().enumerate() {
        v[fgi] = row_dual[n_branch_flow + ri] / base;
    }
    v
}

/// Extract single-period interface shadow prices.
///
/// Returns a `Vec<f64>` with one entry per `network.interfaces` entry (0 for unconstrained).
pub(crate) fn extract_interface_shadow_prices_single(
    row_dual: &[f64],
    iface_rows: &[usize],
    n_iface: usize,
    n_branch_flow: usize,
    n_fg_rows: usize,
    base: f64,
) -> Vec<f64> {
    let mut v = vec![0.0; n_iface];
    for (ri, &ii) in iface_rows.iter().enumerate() {
        v[ii] = row_dual[n_branch_flow + n_fg_rows + ri] / base;
    }
    v
}

/// Extract multi-period flowgate shadow prices (worst-case hour = max absolute value).
///
/// Returns a `Vec<f64>` with one entry per `network.flowgates` entry.
pub(crate) fn extract_branch_shadow_prices_multi(
    row_dual: &[f64],
    n_branch_flow: usize,
    hour_row_bases: &[usize],
    base: f64,
) -> Vec<Vec<f64>> {
    hour_row_bases
        .iter()
        .copied()
        .map(|t| {
            (0..n_branch_flow)
                .map(|row_idx| row_dual[t + row_idx] / base)
                .collect()
        })
        .collect()
}

/// Extract multi-period flowgate shadow prices (worst-case hour = max absolute value).
///
/// Returns a `Vec<f64>` with one entry per `network.flowgates` entry.
pub(crate) fn extract_flowgate_shadow_prices_multi(
    row_dual: &[f64],
    fg_rows: &[usize],
    n_fg: usize,
    n_branch_flow: usize,
    hour_row_bases: &[usize],
    base: f64,
) -> Vec<Vec<f64>> {
    hour_row_bases
        .iter()
        .copied()
        .map(|t| {
            let mut v = vec![0.0; n_fg];
            for (ri, &fgi) in fg_rows.iter().enumerate() {
                let row = t + n_branch_flow + ri;
                v[fgi] = row_dual[row] / base;
            }
            v
        })
        .collect()
}

/// Extract multi-period interface shadow prices (worst-case hour = max absolute value).
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_interface_shadow_prices_multi(
    row_dual: &[f64],
    iface_rows: &[usize],
    n_iface: usize,
    n_branch_flow: usize,
    n_fg_rows: usize,
    hour_row_bases: &[usize],
    base: f64,
) -> Vec<Vec<f64>> {
    hour_row_bases
        .iter()
        .copied()
        .map(|t| {
            let mut v = vec![0.0; n_iface];
            for (ri, &ii) in iface_rows.iter().enumerate() {
                let row = t + n_branch_flow + n_fg_rows + ri;
                v[ii] = row_dual[row] / base;
            }
            v
        })
        .collect()
}
