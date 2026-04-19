// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared LP constraint builders for SCED and SCUC formulations.
//!
//! Each builder produces an [`LpBlock`] containing `triplets` with **absolute**
//! row/column indices and `row_lower`/`row_upper` arrays indexed **locally**
//! from `0` to `n_rows - 1`.
//!
//! Callers merge a block into the master arrays via:
//! ```text
//! let block = build_X(..., row_base);
//! all_triplets.extend(block.triplets);   // already have absolute rows
//! all_lo.extend(block.row_lower);        // local, appended in order
//! all_hi.extend(block.row_upper);
//! row_cursor += block.n_rows();
//! ```
//!
//! # Unit conventions
//!
//! SCED stores storage `ch`/`dis` variables in **per-unit** (MW/base_mva).
//! SCUC stores them in **MW**.  Pass `storage_in_pu = true` for SCED.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::market::ReserveDirection;
use surge_network::network::INACTIVE_FLOWGATE_LIMIT_MW;
use surge_sparse::Triplet;

use crate::common::layout::LpBlock;
use crate::common::reserves::ReserveLpLayout;
use crate::common::setup::{DispatchBranchLookup, DispatchSetup, ResolvedMonitoredElement};
use crate::common::spec::DispatchProblemSpec;

// ---------------------------------------------------------------------------
// PAR phase-shift injection
// ---------------------------------------------------------------------------

/// Compute per-bus phase-angle injection vector from PST branches and PAR
/// scheduled-interchange setpoints.
///
/// Returns `pbusinj[i]` in per-unit for `i` in `0..n_bus`.
pub(crate) fn compute_par_injection(
    network: &Network,
    spec: &DispatchProblemSpec<'_>,
    bus_map: &HashMap<u32, usize>,
    n_bus: usize,
    par_branch_set: &HashSet<usize>,
    branch_lookup: &DispatchBranchLookup,
    base: f64,
) -> Vec<f64> {
    let mut pbusinj = vec![0.0_f64; n_bus];

    for (br_idx, branch) in network.branches.iter().enumerate() {
        if par_branch_set.contains(&br_idx) {
            continue;
        }
        if !branch.in_service || branch.x.abs() < 1e-20 || branch.phase_shift_rad.abs() < 1e-12 {
            continue;
        }
        let phi_rad = branch.phase_shift_rad;
        let pf = branch.b_dc() * phi_rad;
        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];
        pbusinj[from_idx] += pf;
        pbusinj[to_idx] -= pf;
    }

    for ps in spec.par_setpoints {
        if let Some(br) =
            branch_lookup.in_service_branch(network, ps.from_bus, ps.to_bus, ps.circuit.as_str())
        {
            if br.x.abs() < 1e-20 {
                continue;
            }
            let from_idx = bus_map[&ps.from_bus];
            let to_idx = bus_map[&ps.to_bus];
            pbusinj[from_idx] += ps.target_mw / base;
            pbusinj[to_idx] -= ps.target_mw / base;
        }
    }

    pbusinj
}

/// Compute per-bus phase-shift injection from in-service PST branches.
///
/// Unlike [`compute_par_injection`], this does not exclude PAR-controlled
/// branches or add scheduled-interchange setpoints.
pub(crate) fn compute_phase_shift_injection(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    n_bus: usize,
) -> Vec<f64> {
    let mut pbusinj = vec![0.0_f64; n_bus];

    for branch in &network.branches {
        if !branch.in_service || branch.x.abs() < 1e-20 || branch.phase_shift_rad.abs() < 1e-12 {
            continue;
        }
        let phi_rad = branch.phase_shift_rad;
        let pf = branch.b_dc() * phi_rad;
        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];
        pbusinj[from_idx] += pf;
        pbusinj[to_idx] -= pf;
    }

    pbusinj
}

/// Compute constant HVDC loss-a injections by bus in per-unit.
pub(crate) fn compute_hvdc_loss_injection(
    spec: &DispatchProblemSpec<'_>,
    hvdc_from_idx: &[Option<usize>],
    n_bus: usize,
    base: f64,
) -> Vec<f64> {
    let mut hvdc_loss_a_bus = vec![0.0_f64; n_bus];

    for (k, hvdc) in spec.hvdc_links.iter().enumerate() {
        if let Some(fi) = hvdc_from_idx.get(k).copied().flatten() {
            hvdc_loss_a_bus[fi] += hvdc.loss_a_mw / base;
        }
    }

    hvdc_loss_a_bus
}

// ---------------------------------------------------------------------------
// Branch thermal flow rows
// ---------------------------------------------------------------------------

/// Build branch thermal flow constraint rows.
///
/// `n_rows = constrained_branches.len()`.  Local row `i` corresponds to
/// `constrained_branches[i]`.
///
/// When `switching_pf_l_cols` is `Some`, the caller is running the
/// switchable-branch formulation. In that mode the thermal envelope is
/// enforced by the Big-M flow definition rows (`pf_l ≤ fmax · u^on`)
/// built separately in `scuc::rows::build_branch_flow_definition_rows`,
/// so this function emits trivially-unbounded rows that preserve the
/// row count and downstream offset arithmetic without adding any
/// constraint work.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_thermal_rows(
    network: &Network,
    constrained_branches: &[usize],
    bus_map: &HashMap<u32, usize>,
    col_base: usize,
    row_base: usize,
    theta_off: usize,
    slack_layout: Option<SoftLimitSlackLayout>,
    base: f64,
    switching_pf_l_cols: Option<&[usize]>,
) -> LpBlock {
    let n = constrained_branches.len();
    let mut block = LpBlock {
        triplets: Vec::with_capacity(2 * n),
        row_lower: vec![0.0; n],
        row_upper: vec![0.0; n],
    };

    if switching_pf_l_cols.is_some() {
        // Switchable branch mode: the Big-M flow definition row family
        // handles the thermal envelope against `u^on`. Emit trivial
        // `[-∞, +∞]` rows so downstream row offset arithmetic is
        // unchanged. The slack columns remain unbound and unused.
        for ci in 0..n {
            block.row_lower[ci] = -1e30;
            block.row_upper[ci] = 1e30;
        }
        return block;
    }

    for (ci, &l) in constrained_branches.iter().enumerate() {
        let br = &network.branches[l];
        if br.x.abs() < 1e-20 {
            block.row_lower[ci] = -1e30;
            block.row_upper[ci] = 1e30;
            continue;
        }
        let b_val = br.b_dc();
        let from = bus_map[&br.from_bus];
        let to = bus_map[&br.to_bus];
        let row = row_base + ci;

        block.triplets.push(Triplet {
            row,
            col: col_base + theta_off + from,
            val: b_val,
        });
        block.triplets.push(Triplet {
            row,
            col: col_base + theta_off + to,
            val: -b_val,
        });
        if let Some(slack) = slack_layout {
            block.triplets.push(Triplet {
                row,
                col: col_base + slack.lower_off + ci,
                val: 1.0,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + slack.upper_off + ci,
                val: -1.0,
            });
        }

        let fmax = br.rating_a_mva / base;
        let pfinj = if br.phase_shift_rad.abs() < 1e-12 {
            0.0
        } else {
            b_val * br.phase_shift_rad
        };
        block.row_lower[ci] = -fmax - pfinj;
        block.row_upper[ci] = fmax - pfinj;
    }

    block
}

// ---------------------------------------------------------------------------
// Flowgate rows
// ---------------------------------------------------------------------------

/// Build flowgate constraint rows.
///
/// `n_rows = fg_rows.len()`.  Local row `ri` corresponds to `fg_rows[ri]`.
///
/// `hour` selects the per-hour limit (pass `0` for SCED).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_flowgate_rows(
    network: &Network,
    fg_rows: &[usize],
    resolved_flowgates: &[ResolvedMonitoredElement],
    col_base: usize,
    row_base: usize,
    theta_off: usize,
    hvdc_off: usize,
    hvdc_band_offsets: &[usize],
    n_hvdc_links: usize,
    spec: &DispatchProblemSpec<'_>,
    slack_layout: Option<SoftLimitSlackLayout>,
    base: f64,
    hour: usize,
) -> LpBlock {
    let n = fg_rows.len();
    let mut block = LpBlock {
        triplets: Vec::with_capacity(4 * n),
        row_lower: vec![0.0; n],
        row_upper: vec![0.0; n],
    };

    // Sentinel row bounds (pu) used when a flowgate's single-period
    // marker says it's inactive at `hour`. Wide enough to be free.
    let sentinel_pu = INACTIVE_FLOWGATE_LIMIT_MW / base;

    for (ri, &fgi) in fg_rows.iter().enumerate() {
        let fg = &network.flowgates[fgi];
        let row = row_base + ri;

        // Fast path: single-active-period flowgates (security N-1 cuts
        // populated by `build_branch_security_flowgate`) that aren't
        // active at this `hour` become free rows — no coefficient or
        // slack triplets, bounds at `±INACTIVE_FLOWGATE_LIMIT_MW`.
        // Gurobi presolves free rows in O(1). On explicit-N-1 SCUC
        // this skips ~17/18 of the flowgate triplets (617-bus D1 = 8.6M
        // flowgates × 17 inactive hours × ~6 triplets = ~900M triplets
        // saved on the Rust COO matrix). The slack columns still exist
        // in the layout — they simply don't participate in this row's
        // sum, which leaves them at their LB of 0 under the "soft
        // slack with nonnegative domain" convention; objective-side
        // coefficients on those unused slacks never multiply a nonzero
        // primal, so the objective and dual are unchanged.
        if let Some(active) = fg.limit_mw_active_period {
            if hour != active as usize {
                block.row_lower[ri] = -sentinel_pu;
                block.row_upper[ri] = sentinel_pu;
                continue;
            }
        }

        let resolved = &resolved_flowgates[fgi];

        for term in &resolved.terms {
            block.triplets.push(Triplet {
                row,
                col: col_base + theta_off + term.from_bus_idx,
                val: term.theta_coeff,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + theta_off + term.to_bus_idx,
                val: -term.theta_coeff,
            });
        }

        // HVDC coefficients
        for &(hvdc_k, coeff) in &fg.hvdc_coefficients {
            if hvdc_k < n_hvdc_links {
                let hvdc = &spec.hvdc_links[hvdc_k];
                let n_bands = hvdc.n_vars();
                let band_base = hvdc_band_offsets[hvdc_k];
                for b in 0..n_bands {
                    block.triplets.push(Triplet {
                        row,
                        col: col_base + hvdc_off + band_base + b,
                        val: coeff,
                    });
                }
            }
        }
        for &(hvdc_k, band_idx, coeff) in &fg.hvdc_band_coefficients {
            if hvdc_k < n_hvdc_links {
                let hvdc = &spec.hvdc_links[hvdc_k];
                if band_idx < hvdc.n_vars() {
                    let band_base = hvdc_band_offsets[hvdc_k];
                    block.triplets.push(Triplet {
                        row,
                        col: col_base + hvdc_off + band_base + band_idx,
                        val: coeff,
                    });
                }
            }
        }
        if let Some(slack) = slack_layout {
            block.triplets.push(Triplet {
                row,
                col: col_base + slack.lower_off + ri,
                val: 1.0,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + slack.upper_off + ri,
                val: -1.0,
            });
        }

        let fg_limit = fg.effective_limit_mw(hour);
        let fg_rev = fg.effective_reverse_or_forward(hour);
        block.row_lower[ri] = -fg_rev / base - resolved.shift_offset;
        block.row_upper[ri] = fg_limit / base - resolved.shift_offset;
    }

    block
}

/// Initialize flowgate nomogram metadata used by tightening loops.
pub(crate) fn init_flowgate_nomogram_data(
    network: &Network,
    fg_rows: &[usize],
    resolved_flowgates: &[ResolvedMonitoredElement],
) -> (Vec<f64>, Vec<f64>) {
    let mut fg_limits = Vec::with_capacity(fg_rows.len());
    let mut fg_shift_offsets = Vec::with_capacity(fg_rows.len());

    for &fgi in fg_rows {
        let fg = &network.flowgates[fgi];
        fg_limits.push(fg.effective_limit_mw(0));
        fg_shift_offsets.push(resolved_flowgates[fgi].shift_offset);
    }

    (fg_limits, fg_shift_offsets)
}

// ---------------------------------------------------------------------------
// Interface rows
// ---------------------------------------------------------------------------

/// Build interface constraint rows.
///
/// `n_rows = iface_rows.len()`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_interface_rows(
    network: &Network,
    iface_rows: &[usize],
    resolved_interfaces: &[ResolvedMonitoredElement],
    col_base: usize,
    row_base: usize,
    theta_off: usize,
    slack_layout: Option<SoftLimitSlackLayout>,
    base: f64,
    hour: usize,
) -> LpBlock {
    let n = iface_rows.len();
    let mut block = LpBlock {
        triplets: Vec::with_capacity(4 * n),
        row_lower: vec![0.0; n],
        row_upper: vec![0.0; n],
    };

    for (ri, &ii) in iface_rows.iter().enumerate() {
        let iface = &network.interfaces[ii];
        let row = row_base + ri;
        let resolved = &resolved_interfaces[ii];

        for term in &resolved.terms {
            block.triplets.push(Triplet {
                row,
                col: col_base + theta_off + term.from_bus_idx,
                val: term.theta_coeff,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + theta_off + term.to_bus_idx,
                val: -term.theta_coeff,
            });
        }
        if let Some(slack) = slack_layout {
            block.triplets.push(Triplet {
                row,
                col: col_base + slack.lower_off + ri,
                val: 1.0,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + slack.upper_off + ri,
                val: -1.0,
            });
        }

        block.row_lower[ri] =
            -iface.effective_limit_reverse_mw(hour) / base - resolved.shift_offset;
        block.row_upper[ri] = iface.effective_limit_forward_mw(hour) / base - resolved.shift_offset;
    }

    block
}

// ---------------------------------------------------------------------------
// Power balance rows
// ---------------------------------------------------------------------------

/// Extra power-balance term injected directly into a bus row.
///
/// `col` is an absolute LP column index because some terms, such as SCUC
/// rebound variables, live outside the per-hour column block.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PowerBalanceExtraTerm {
    pub bus_idx: usize,
    pub col: usize,
    pub coeff: f64,
}

/// Linear lower/upper violation slack columns for a row family.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SoftLimitSlackLayout {
    pub lower_off: usize,
    pub upper_off: usize,
}

/// Build power balance equality rows (`n_bus` rows).
///
/// Covers B-theta, gen injection (non-storage), storage ch/dis, HVDC,
/// dispatchable loads, virtual bids, and any caller-provided extra bus terms.
/// Row bounds are set to the per-bus RHS:
/// `-(Pd + Gs)/base - pbusinj - hvdc_loss_a`.
///
/// ## Switchable-branch mode
///
/// When `switching_pf_l_cols` is `Some(cols)` (one column per entry in
/// `network.branches`), the y-bus `b·Δθ` branch contribution is
/// replaced by a `pf_l` flow injection: `-pf_l` at the from-bus and
/// `+pf_l` at the to-bus. This is the KCL rewrite that makes the LP
/// respect the `u^on_lt` branch commitment variables — when a branch
/// is off, `pf_l = 0` (enforced by the Big-M flow definition rows)
/// and the branch correctly drops out of both bus injection rows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_power_balance_rows(
    network: &Network,
    setup: &DispatchSetup,
    gen_indices: &[usize],
    gen_bus_idx: &[usize],
    spec: &DispatchProblemSpec<'_>,
    bus_map: &HashMap<u32, usize>,
    col_base: usize,
    row_base: usize,
    pbusinj: &[f64],
    hvdc_loss_a_bus: &[f64],
    hvdc_from_idx: &[Option<usize>],
    hvdc_to_idx: &[Option<usize>],
    hvdc_band_offsets: &[usize],
    dl_list: &[&surge_network::market::DispatchableLoad],
    active_vbids: &[usize],
    par_branch_set: Option<&HashSet<usize>>,
    extra_terms: &[PowerBalanceExtraTerm],
    theta_off: usize,
    pg_off: usize,
    sto_ch_off: usize,
    sto_dis_off: usize,
    hvdc_off: usize,
    dl_off: usize,
    vbid_off: usize,
    storage_in_pu: bool,
    base: f64,
    switching_pf_l_cols: Option<&[usize]>,
) -> LpBlock {
    let n_bus = network.n_buses();
    let n_storage = setup.n_storage;
    let has_hvdc = !spec.hvdc_links.is_empty();
    let has_dl = !dl_list.is_empty();
    let n_hvdc_terms: usize = spec.hvdc_links.iter().map(|hvdc| 2 * hvdc.n_vars()).sum();

    let mut block = LpBlock {
        triplets: Vec::with_capacity(
            6 * n_bus
                + gen_indices.len()
                + 2 * n_storage
                + n_hvdc_terms
                + dl_list.len()
                + active_vbids.len()
                + extra_terms.len(),
        ),
        row_lower: vec![0.0; n_bus],
        row_upper: vec![0.0; n_bus],
    };

    if let Some(pf_l_cols) = switching_pf_l_cols {
        // Switchable-branch KCL rewrite: replace the y-bus `b·Δθ`
        // contribution with `-pf_l` at from-bus and `+pf_l` at
        // to-bus. When a branch is off, pf_l = 0 (enforced by the
        // Big-M flow definition rows) and the branch cleanly drops
        // out of the bus balance. When on, the Big-M rows tie pf_l
        // to `b·(θ_from − θ_to)` so this formulation is algebraically
        // identical to the y-bus case at every feasible commitment.
        debug_assert_eq!(
            pf_l_cols.len(),
            network.branches.len(),
            "switching_pf_l_cols must have one entry per network.branches"
        );
        for (br_idx, branch) in network.branches.iter().enumerate() {
            if branch.x.abs() < 1e-20
                || par_branch_set.is_some_and(|par_set| par_set.contains(&br_idx))
            {
                continue;
            }
            let from = bus_map[&branch.from_bus];
            let to = bus_map[&branch.to_bus];
            let eq_from = row_base + from;
            let eq_to = row_base + to;
            let pf_col = pf_l_cols[br_idx];
            // -pf_l injection at from-bus (power leaves), +pf_l at to-bus.
            block.triplets.push(Triplet {
                row: eq_from,
                col: pf_col,
                val: -1.0,
            });
            block.triplets.push(Triplet {
                row: eq_to,
                col: pf_col,
                val: 1.0,
            });
        }
    } else {
        // B-theta terms from branch admittances (the pre-B5 y-bus
        // formulation, still the default when `allow_branch_switching
        // = false` and the only mode SCED ever uses).
        for (br_idx, branch) in network.branches.iter().enumerate() {
            if !branch.in_service
                || branch.x.abs() < 1e-20
                || par_branch_set.is_some_and(|par_set| par_set.contains(&br_idx))
            {
                continue;
            }
            let from = bus_map[&branch.from_bus];
            let to = bus_map[&branch.to_bus];
            let b = branch.b_dc();
            let eq_from = row_base + from;
            let eq_to = row_base + to;

            block.triplets.push(Triplet {
                row: eq_from,
                col: col_base + theta_off + to,
                val: -b,
            });
            block.triplets.push(Triplet {
                row: eq_to,
                col: col_base + theta_off + from,
                val: -b,
            });
            block.triplets.push(Triplet {
                row: eq_from,
                col: col_base + theta_off + from,
                val: b,
            });
            block.triplets.push(Triplet {
                row: eq_to,
                col: col_base + theta_off + to,
                val: b,
            });
        }
    }

    // -A_gen: skip storage generators (their injection via dis/ch)
    for (j, &bus_idx) in gen_bus_idx.iter().enumerate() {
        if network.generators[gen_indices[j]].is_storage() {
            continue;
        }
        block.triplets.push(Triplet {
            row: row_base + bus_idx,
            col: col_base + pg_off + j,
            val: -1.0,
        });
    }

    // Storage: dis injects (-scale), ch absorbs (+scale)
    if n_storage > 0 {
        let sto_scale = if storage_in_pu { 1.0 } else { 1.0 / base };
        for &(s, _, gi) in &setup.storage_gen_local {
            let g = &network.generators[gi];
            let bus_idx = bus_map[&g.bus];
            block.triplets.push(Triplet {
                row: row_base + bus_idx,
                col: col_base + sto_dis_off + s,
                val: -sto_scale,
            });
            block.triplets.push(Triplet {
                row: row_base + bus_idx,
                col: col_base + sto_ch_off + s,
                val: sto_scale,
            });
        }
    }

    // HVDC injection
    if has_hvdc {
        for (k, hvdc) in spec.hvdc_links.iter().enumerate() {
            if hvdc.is_banded() {
                for (b, band) in hvdc.bands.iter().enumerate() {
                    let col = col_base + hvdc_off + hvdc_band_offsets[k] + b;
                    if let Some(fi) = hvdc_from_idx[k] {
                        block.triplets.push(Triplet {
                            row: row_base + fi,
                            col,
                            val: 1.0,
                        });
                    }
                    if let Some(ti) = hvdc_to_idx[k] {
                        block.triplets.push(Triplet {
                            row: row_base + ti,
                            col,
                            val: -(1.0 - band.loss_b_frac),
                        });
                    }
                }
            } else {
                let col = col_base + hvdc_off + hvdc_band_offsets[k];
                if let Some(fi) = hvdc_from_idx[k] {
                    block.triplets.push(Triplet {
                        row: row_base + fi,
                        col,
                        val: 1.0,
                    });
                }
                if let Some(ti) = hvdc_to_idx[k] {
                    block.triplets.push(Triplet {
                        row: row_base + ti,
                        col,
                        val: -(1.0 - hvdc.loss_b_frac),
                    });
                }
            }
        }
    }

    // DR dispatchable load: +1 (consumes power)
    if has_dl {
        for (k, dl) in dl_list.iter().enumerate() {
            if let Some(&bus_idx) = bus_map.get(&dl.bus) {
                block.triplets.push(Triplet {
                    row: row_base + bus_idx,
                    col: col_base + dl_off + k,
                    val: 1.0,
                });
            }
        }
    }

    // Virtual bids
    for (k, &bi) in active_vbids.iter().enumerate() {
        let vb = &spec.virtual_bids[bi];
        if let Some(&bus_idx) = bus_map.get(&vb.bus) {
            let coeff = match vb.direction {
                surge_network::market::VirtualBidDirection::Inc => -1.0,
                surge_network::market::VirtualBidDirection::Dec => 1.0,
            };
            block.triplets.push(Triplet {
                row: row_base + bus_idx,
                col: col_base + vbid_off + k,
                val: coeff,
            });
        }
    }

    // Any additional bus injections or withdrawals not covered by the base
    // formulation, such as power-balance penalty slacks or DR rebound terms.
    for term in extra_terms {
        block.triplets.push(Triplet {
            row: row_base + term.bus_idx,
            col: term.col,
            val: term.coeff,
        });
    }

    // Row bounds: equality at -(Pd + Gs)/base - pbusinj - hvdc_loss_a
    let bus_pd_mw = network.bus_load_p_mw_with_map(bus_map);
    for i in 0..n_bus {
        let pd_pu = bus_pd_mw[i] / base;
        let gs_pu = network.buses[i].shunt_conductance_mw / base;
        let rhs = -pd_pu - gs_pu - pbusinj[i] - hvdc_loss_a_bus[i];
        block.row_lower[i] = rhs;
        block.row_upper[i] = rhs;
    }

    block
}

/// Shared DC network row family: thermal, flowgate, interface, and power balance.
#[allow(clippy::too_many_arguments)]
pub(crate) struct DcNetworkRowsInput<'a> {
    pub flow_network: &'a Network,
    pub dispatch_network: &'a Network,
    pub constrained_branches: &'a [usize],
    pub fg_rows: &'a [usize],
    pub resolved_flowgates: &'a [ResolvedMonitoredElement],
    pub iface_rows: &'a [usize],
    pub resolved_interfaces: &'a [ResolvedMonitoredElement],
    pub setup: &'a DispatchSetup,
    pub gen_indices: &'a [usize],
    pub gen_bus_idx: &'a [usize],
    pub spec: &'a DispatchProblemSpec<'a>,
    pub bus_map: &'a HashMap<u32, usize>,
    pub pbusinj: &'a [f64],
    pub hvdc_loss_a_bus: &'a [f64],
    pub hvdc_from_idx: &'a [Option<usize>],
    pub hvdc_to_idx: &'a [Option<usize>],
    pub hvdc_band_offsets: &'a [usize],
    pub dl_list: &'a [&'a surge_network::market::DispatchableLoad],
    pub active_vbids: &'a [usize],
    pub par_branch_set: Option<&'a HashSet<usize>>,
    pub extra_terms: &'a [PowerBalanceExtraTerm],
    pub col_base: usize,
    pub row_base: usize,
    pub theta_off: usize,
    pub pg_off: usize,
    pub sto_ch_off: usize,
    pub sto_dis_off: usize,
    pub hvdc_off: usize,
    pub branch_slack: Option<SoftLimitSlackLayout>,
    pub flowgate_slack: Option<SoftLimitSlackLayout>,
    pub interface_slack: Option<SoftLimitSlackLayout>,
    pub dl_off: usize,
    pub vbid_off: usize,
    pub n_hvdc_links: usize,
    pub storage_in_pu: bool,
    pub base: f64,
    pub hour: usize,
    /// Switchable-branch mode — one absolute LP column per entry in
    /// `network.branches` holding `pf_l_t` for this period. When
    /// `Some`, `build_power_balance_rows` and `build_thermal_rows`
    /// switch to the `pf_l` formulation described on those functions;
    /// when `None`, they use the y-bus formulation.
    pub switching_pf_l_cols: Option<&'a [usize]>,
}

pub(crate) fn build_dc_network_rows(input: DcNetworkRowsInput<'_>) -> LpBlock {
    fn append_block(dst: &mut LpBlock, src: LpBlock) {
        dst.triplets.extend(src.triplets);
        dst.row_lower.extend(src.row_lower);
        dst.row_upper.extend(src.row_upper);
    }

    let n_branch_flow = input.constrained_branches.len();
    let n_fg_rows = input.fg_rows.len();
    let n_flow = n_branch_flow + n_fg_rows + input.iface_rows.len();
    let mut block = LpBlock::empty();

    append_block(
        &mut block,
        build_thermal_rows(
            input.dispatch_network,
            input.constrained_branches,
            input.bus_map,
            input.col_base,
            input.row_base,
            input.theta_off,
            input.branch_slack,
            input.base,
            input.switching_pf_l_cols,
        ),
    );

    append_block(
        &mut block,
        build_flowgate_rows(
            input.flow_network,
            input.fg_rows,
            input.resolved_flowgates,
            input.col_base,
            input.row_base + n_branch_flow,
            input.theta_off,
            input.hvdc_off,
            input.hvdc_band_offsets,
            input.n_hvdc_links,
            input.spec,
            input.flowgate_slack,
            input.base,
            input.hour,
        ),
    );

    append_block(
        &mut block,
        build_interface_rows(
            input.flow_network,
            input.iface_rows,
            input.resolved_interfaces,
            input.col_base,
            input.row_base + n_branch_flow + n_fg_rows,
            input.theta_off,
            input.interface_slack,
            input.base,
            input.hour,
        ),
    );

    append_block(
        &mut block,
        build_power_balance_rows(
            input.dispatch_network,
            input.setup,
            input.gen_indices,
            input.gen_bus_idx,
            input.spec,
            input.bus_map,
            input.col_base,
            input.row_base + n_flow,
            input.pbusinj,
            input.hvdc_loss_a_bus,
            input.hvdc_from_idx,
            input.hvdc_to_idx,
            input.hvdc_band_offsets,
            input.dl_list,
            input.active_vbids,
            input.par_branch_set,
            input.extra_terms,
            input.theta_off,
            input.pg_off,
            input.sto_ch_off,
            input.sto_dis_off,
            input.hvdc_off,
            input.dl_off,
            input.vbid_off,
            input.storage_in_pu,
            input.base,
            input.switching_pf_l_cols,
        ),
    );

    block
}

// ---------------------------------------------------------------------------
// Generator PWL epiograph rows
// ---------------------------------------------------------------------------

/// Build generator PWL epiograph constraint rows.
///
/// For each PWL generator `k` with local gen index `j`, for each segment:
///   `e_g[k] - slope_pu * Pg[j] >= intercept`
pub(crate) fn build_gen_epiograph_rows(
    setup: &DispatchSetup,
    col_base: usize,
    row_base: usize,
    pg_off: usize,
    e_g_off: usize,
) -> LpBlock {
    let n = setup.n_pwl_rows;
    let mut block = LpBlock {
        triplets: Vec::with_capacity(2 * n),
        row_lower: vec![0.0; n],
        row_upper: vec![0.0; n],
    };

    let mut row_idx = 0usize;
    for (k, (j, segments)) in setup.pwl_gen_info.iter().enumerate() {
        for &(slope_pu, intercept) in segments {
            let row = row_base + row_idx;
            block.triplets.push(Triplet {
                row,
                col: col_base + e_g_off + k,
                val: 1.0,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + pg_off + j,
                val: -slope_pu,
            });
            block.row_lower[row_idx] = intercept;
            block.row_upper[row_idx] = 1e30;
            row_idx += 1;
        }
    }

    block
}

// ---------------------------------------------------------------------------
// System policy rows
// ---------------------------------------------------------------------------

/// Count CO2-cap and tie-line policy rows across a DC horizon.
pub(crate) fn system_policy_rows(
    tie_line_pairs: &[((usize, usize), f64)],
    spec: &DispatchProblemSpec<'_>,
    n_hours: usize,
) -> usize {
    tie_line_pairs.len() * n_hours + usize::from(spec.co2_cap_t.is_some())
}

#[allow(clippy::too_many_arguments)]
pub(crate) struct DcSystemPolicyRowsInput<'a> {
    pub spec: &'a DispatchProblemSpec<'a>,
    pub hourly_networks: &'a [Network],
    pub effective_co2_rate: &'a [f64],
    pub tie_line_pairs: &'a [((usize, usize), f64)],
    pub hour_col_bases: &'a [usize],
    pub theta_off: usize,
    pub pg_off: usize,
    pub hvdc_off: usize,
    pub hvdc_band_offsets: &'a [usize],
    pub row_base: usize,
    pub base: f64,
    pub step_h: f64,
}

pub(crate) fn build_system_policy_rows(input: DcSystemPolicyRowsInput<'_>) -> LpBlock {
    let n_hours = input.hourly_networks.len();
    let n_rows = system_policy_rows(input.tie_line_pairs, input.spec, n_hours);
    if n_rows == 0 {
        return LpBlock::empty();
    }

    debug_assert_eq!(input.hour_col_bases.len(), n_hours);

    let mut block = LpBlock {
        triplets: Vec::new(),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };
    let mut local_row = 0usize;

    if let Some(cap_t) = input.spec.co2_cap_t {
        let row = input.row_base + local_row;
        for hour in 0..n_hours {
            let pg_base = input.hour_col_bases[hour] + input.pg_off;
            for (gen_idx, &rate) in input.effective_co2_rate.iter().enumerate() {
                if rate > 0.0 {
                    block.triplets.push(Triplet {
                        row,
                        col: pg_base + gen_idx,
                        val: rate * input.base * input.step_h,
                    });
                }
            }
        }
        block.row_lower[local_row] = -1e30;
        block.row_upper[local_row] = cap_t;
        local_row += 1;
    }

    for (hour, hourly_network) in input.hourly_networks.iter().enumerate() {
        let col_base = input.hour_col_bases[hour];
        let bus_map = hourly_network.bus_index_map();
        for &((from_area, _to_area), limit_mw) in input.tie_line_pairs {
            let row = input.row_base + local_row;
            let mut shift_offset = 0.0;
            let (from_area, to_area) = (from_area, _to_area);

            for branch in &hourly_network.branches {
                if !branch.in_service || branch.x.abs() < 1e-20 {
                    continue;
                }
                let Some(&from_idx) = bus_map.get(&branch.from_bus) else {
                    continue;
                };
                let Some(&to_idx) = bus_map.get(&branch.to_bus) else {
                    continue;
                };
                let branch_from_area = input.spec.load_area.get(from_idx).copied();
                let branch_to_area = input.spec.load_area.get(to_idx).copied();
                let coeff = match (branch_from_area, branch_to_area) {
                    (Some(a), Some(b)) if a == from_area && b == to_area => 1.0,
                    (Some(a), Some(b)) if a == to_area && b == from_area => -1.0,
                    _ => continue,
                };
                let theta_coeff = coeff * branch.b_dc();
                block.triplets.push(Triplet {
                    row,
                    col: col_base + input.theta_off + from_idx,
                    val: theta_coeff,
                });
                block.triplets.push(Triplet {
                    row,
                    col: col_base + input.theta_off + to_idx,
                    val: -theta_coeff,
                });
                if branch.phase_shift_rad.abs() > 1e-12 {
                    shift_offset += theta_coeff * branch.phase_shift_rad;
                }
            }

            for (hvdc_k, hvdc) in input.spec.hvdc_links.iter().enumerate() {
                let Some(&from_idx) = bus_map.get(&hvdc.from_bus) else {
                    continue;
                };
                let Some(&to_idx) = bus_map.get(&hvdc.to_bus) else {
                    continue;
                };
                let hvdc_from_area = input.spec.load_area.get(from_idx).copied();
                let hvdc_to_area = input.spec.load_area.get(to_idx).copied();
                let coeff = match (hvdc_from_area, hvdc_to_area) {
                    (Some(a), Some(b)) if a == from_area && b == to_area => 1.0,
                    (Some(a), Some(b)) if a == to_area && b == from_area => -1.0,
                    _ => continue,
                };
                let band_base = input.hvdc_band_offsets[hvdc_k];
                for band_idx in 0..hvdc.n_vars() {
                    block.triplets.push(Triplet {
                        row,
                        col: col_base + input.hvdc_off + band_base + band_idx,
                        val: coeff,
                    });
                }
            }
            block.row_lower[local_row] = -1e30;
            block.row_upper[local_row] = limit_mw / input.base - shift_offset;
            local_row += 1;
        }
    }

    debug_assert_eq!(local_row, n_rows);
    block
}

// ---------------------------------------------------------------------------
// Storage rows
// ---------------------------------------------------------------------------

/// Build all storage constraint rows for one period.
///
/// Row layout (`n_rows` total, returned by [`LpBlock::n_rows`]):
///
/// | Range | Description |
/// |-------|-------------|
/// | `[0..n_sto)` | SoC feasibility (SCED) or SoC dynamics equality (SCUC) |
/// | `[n_sto..2*n_sto)` | Discharge AS coupling: `dis/scale + Σup_R ≤ dis_max/base` |
/// | `[2*n_sto..3*n_sto)` | Charge AS coupling: `ch/scale + Σdn_R ≤ ch_max/base` |
/// | `[3*n_sto..+n_dis_epi)` | Discharge offer epiograph |
/// | `[..+n_ch_epi)` | Charge bid epiograph |
/// | `[..+n_sto)` | Pg linking: `Pg_j = dis/scale - ch/scale` |
///
/// ## `storage_in_pu = true` (SCED)
///
/// ch/dis variables are in p.u. (bounds = MW/base).  SoC row is double-bounded:
/// `soc_min - soc_init ≤ ch·base·η·dt - dis·base/η·dt ≤ soc_max - soc_init`
///
/// ## `storage_in_pu = false` (SCUC)
///
/// ch/dis variables are in MW (bounds in MW).  SoC dynamics row is equality:
/// `soc[t] - ch·η·dt + dis/η·dt = soc_prev`
/// where `soc_prev` comes from `soc_prev_col` (t > 0) or `soc_prev_mwh[s]` (t = 0).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_storage_rows(
    network: &Network,
    setup: &DispatchSetup,
    sto_ch_off: usize,
    sto_dis_off: usize,
    sto_soc_off: usize,
    sto_epi_dis_off: usize,
    sto_epi_ch_off: usize,
    pg_off: usize,
    col_base: usize,
    row_base: usize,
    soc_prev_mwh: &[f64],
    soc_prev_col: Option<usize>,
    dt_hours: f64,
    reserve_layout: &ReserveLpLayout,
    storage_in_pu: bool,
    base: f64,
) -> LpBlock {
    let n_sto = setup.n_storage;
    if n_sto == 0 {
        return LpBlock::empty();
    }
    let use_explicit_soc = storage_in_pu && sto_soc_off != sto_ch_off;

    let n_dis_epi = setup.n_sto_dis_offer_rows;
    let n_ch_epi = setup.n_sto_ch_bid_rows;
    // Foldback cuts — one extra row per storage unit per direction where
    // the threshold is set. Cuts are applied to the (discharge + up-AS)
    // and (charge + down-AS) sums, with the start-of-period SoC driving
    // the RHS: at soc_min the cap is 0 MW, at the threshold it is full.
    let n_fb_dis = setup
        .storage_foldback_discharge_mwh
        .iter()
        .filter(|o| o.is_some())
        .count();
    let n_fb_ch = setup
        .storage_foldback_charge_mwh
        .iter()
        .filter(|o| o.is_some())
        .count();
    let n_rows = n_sto + n_sto + n_sto + n_dis_epi + n_ch_epi + n_sto + n_fb_dis + n_fb_ch;

    let mut block = LpBlock {
        triplets: Vec::with_capacity(
            6 * n_sto + 2 * n_dis_epi + 2 * n_ch_epi + 4 * (n_fb_dis + n_fb_ch),
        ),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };

    // ---- 1. SoC rows ----
    for &(s, _, gi) in &setup.storage_gen_local {
        let g = &network.generators[gi];
        let sto = g
            .storage
            .as_ref()
            .expect("storage_gen_local only contains generators with storage");
        // Asymmetric efficiencies: η_ch applied to the charge leg (SoC gains
        // less than the metered charge MW); 1/η_dis on the discharge leg (SoC
        // loses more than the metered discharge MW).
        let eta_ch = setup.storage_eta_charge[s];
        let eta_dis = setup.storage_eta_discharge[s];
        let row = row_base + s;

        if storage_in_pu && !use_explicit_soc {
            // SCED: ch/dis in p.u., so multiply by base to get MW·h
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_ch_off + s,
                val: base * eta_ch * dt_hours,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_dis_off + s,
                val: -base / eta_dis * dt_hours,
            });
            let soc_init = soc_prev_mwh[s];
            block.row_lower[s] = sto.soc_min_mwh - soc_init;
            block.row_upper[s] = sto.soc_max_mwh - soc_init;
        } else if storage_in_pu {
            // SCED explicit-SoC variant: ch/dis remain in p.u., soc is in MWh.
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_soc_off + s,
                val: 1.0,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_ch_off + s,
                val: -base * eta_ch * dt_hours,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_dis_off + s,
                val: base * dt_hours / eta_dis,
            });
            if let Some(prev_cb) = soc_prev_col {
                block.triplets.push(Triplet {
                    row,
                    col: prev_cb + sto_soc_off + s,
                    val: -1.0,
                });
                block.row_lower[s] = 0.0;
                block.row_upper[s] = 0.0;
            } else {
                let soc_init = soc_prev_mwh[s];
                block.row_lower[s] = soc_init;
                block.row_upper[s] = soc_init;
            }
        } else {
            // SCUC: ch/dis in MW, soc in MWh
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_soc_off + s,
                val: 1.0,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_ch_off + s,
                val: -eta_ch * dt_hours,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_dis_off + s,
                val: dt_hours / eta_dis,
            });
            if let Some(prev_cb) = soc_prev_col {
                block.triplets.push(Triplet {
                    row,
                    col: prev_cb + sto_soc_off + s,
                    val: -1.0,
                });
                block.row_lower[s] = 0.0;
                block.row_upper[s] = 0.0;
            } else {
                let soc_init = soc_prev_mwh[s];
                block.row_lower[s] = soc_init;
                block.row_upper[s] = soc_init;
            }
        }
    }

    // ---- 2. Discharge AS coupling ----
    // dis + Σ(up reserve R[j]) ≤ dis_max/base   (all terms in p.u.)
    let dis_scale = if storage_in_pu { 1.0 } else { 1.0 / base };
    for &(s, j, gi) in &setup.storage_gen_local {
        let g = &network.generators[gi];
        let local_row = n_sto + s;
        let row = row_base + local_row;
        block.triplets.push(Triplet {
            row,
            col: col_base + sto_dis_off + s,
            val: dis_scale,
        });
        for ap in &reserve_layout.products {
            if ap.product.direction == ReserveDirection::Up {
                block.triplets.push(Triplet {
                    row,
                    col: col_base + ap.gen_var_offset + j,
                    val: 1.0,
                });
            }
        }
        block.row_lower[local_row] = -1e30;
        block.row_upper[local_row] = g.discharge_mw_max() / base;
    }

    // ---- 3. Charge AS coupling ----
    let ch_scale = if storage_in_pu { 1.0 } else { 1.0 / base };
    for &(s, j, gi) in &setup.storage_gen_local {
        let g = &network.generators[gi];
        let local_row = 2 * n_sto + s;
        let row = row_base + local_row;
        block.triplets.push(Triplet {
            row,
            col: col_base + sto_ch_off + s,
            val: ch_scale,
        });
        for ap in &reserve_layout.products {
            if ap.product.direction == ReserveDirection::Down {
                block.triplets.push(Triplet {
                    row,
                    col: col_base + ap.gen_var_offset + j,
                    val: 1.0,
                });
            }
        }
        block.row_lower[local_row] = -1e30;
        block.row_upper[local_row] = g.charge_mw_max() / base;
    }

    // ---- 4. Discharge offer epiograph ----
    // e_dis[k] - slope × dis[s] ≥ intercept
    let mut epi_idx = 3 * n_sto;
    for (k, (s, segments)) in setup.sto_dis_offer_info.iter().enumerate() {
        for &(slope_pu, intercept) in segments {
            let slope_coeff = if storage_in_pu {
                slope_pu
            } else {
                slope_pu / base
            };
            let row = row_base + epi_idx;
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_epi_dis_off + k,
                val: 1.0,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_dis_off + s,
                val: -slope_coeff,
            });
            block.row_lower[epi_idx] = intercept;
            block.row_upper[epi_idx] = 1e30;
            epi_idx += 1;
        }
    }

    // ---- 5. Charge bid epiograph ----
    let mut ch_epi_idx = epi_idx;
    for (k, (s, segments)) in setup.sto_ch_bid_info.iter().enumerate() {
        for &(slope_pu, intercept) in segments {
            let slope_coeff = if storage_in_pu {
                slope_pu
            } else {
                slope_pu / base
            };
            let row = row_base + ch_epi_idx;
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_epi_ch_off + k,
                val: 1.0,
            });
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_ch_off + s,
                val: -slope_coeff,
            });
            block.row_lower[ch_epi_idx] = intercept;
            block.row_upper[ch_epi_idx] = 1e30;
            ch_epi_idx += 1;
        }
    }

    // ---- 6. Pg linking ----
    // Pg_j = dis/scale - ch/scale  (scale = 1 for p.u. SCED, 1/base for MW SCUC)
    let link_base = ch_epi_idx;
    let link_scale = if storage_in_pu { 1.0 } else { 1.0 / base };
    for &(s, j, _gi) in &setup.storage_gen_local {
        let local_row = link_base + s;
        let row = row_base + local_row;
        block.triplets.push(Triplet {
            row,
            col: col_base + pg_off + j,
            val: 1.0,
        });
        block.triplets.push(Triplet {
            row,
            col: col_base + sto_dis_off + s,
            val: -link_scale,
        });
        block.triplets.push(Triplet {
            row,
            col: col_base + sto_ch_off + s,
            val: link_scale,
        });
        block.row_lower[local_row] = 0.0;
        block.row_upper[local_row] = 0.0;
    }

    // ---- 7. SoC-dependent power foldback cuts ----
    // At low SoC, the physical discharge cap (plus any up-direction AS
    // reservations) derates linearly to 0 at soc_min. Symmetrically at
    // high SoC for charge + down-AS. Uses the START-of-period SoC (the
    // known constant for t=0, or the previous period's SoC column
    // otherwise) so the cut is a single linear constraint.
    //
    //   discharge cut : (s1 − s0)·(p_dis + Σ R_up) − P_dis_max·soc_start ≤ −P_dis_max·s0
    //   charge    cut : (s3 − s2)·(p_ch  + Σ R_dn) + P_ch_max ·soc_start ≤  P_ch_max ·s3
    //
    // Coefficient scaling mirrors the existing AS-coupling rows: in
    // p.u. mode the p_dis / p_ch / reserve variables are in p.u. and
    // must be multiplied by ``base`` to line up with the MWh SoC term.
    let fb_base_local = link_base + n_sto;
    let mut fb_row_local = fb_base_local;
    let flow_scale = if storage_in_pu { base } else { 1.0 };
    for &(s, j, gi) in &setup.storage_gen_local {
        if let Some(s1_mwh) = setup.storage_foldback_discharge_mwh[s] {
            let g = &network.generators[gi];
            let sto = g.storage.as_ref().expect("storage");
            let s0 = sto.soc_min_mwh;
            let p_max = g.discharge_mw_max();
            let slope = s1_mwh - s0;
            let row = row_base + fb_row_local;
            // Power term
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_dis_off + s,
                val: slope * flow_scale,
            });
            for ap in &reserve_layout.products {
                if ap.product.direction == ReserveDirection::Up {
                    block.triplets.push(Triplet {
                        row,
                        col: col_base + ap.gen_var_offset + j,
                        val: slope * flow_scale,
                    });
                }
            }
            // SoC reference: previous-period column when present, otherwise a constant RHS.
            if let Some(prev_cb) = soc_prev_col {
                block.triplets.push(Triplet {
                    row,
                    col: prev_cb + sto_soc_off + s,
                    val: -p_max,
                });
                block.row_lower[fb_row_local] = -1e30;
                block.row_upper[fb_row_local] = -p_max * s0;
            } else {
                let soc_init = soc_prev_mwh[s];
                block.row_lower[fb_row_local] = -1e30;
                block.row_upper[fb_row_local] = p_max * (soc_init - s0);
            }
            fb_row_local += 1;
        }
    }
    for &(s, j, gi) in &setup.storage_gen_local {
        if let Some(s2_mwh) = setup.storage_foldback_charge_mwh[s] {
            let g = &network.generators[gi];
            let sto = g.storage.as_ref().expect("storage");
            let s3 = sto.soc_max_mwh;
            let p_max = g.charge_mw_max();
            let slope = s3 - s2_mwh;
            let row = row_base + fb_row_local;
            block.triplets.push(Triplet {
                row,
                col: col_base + sto_ch_off + s,
                val: slope * flow_scale,
            });
            for ap in &reserve_layout.products {
                if ap.product.direction == ReserveDirection::Down {
                    block.triplets.push(Triplet {
                        row,
                        col: col_base + ap.gen_var_offset + j,
                        val: slope * flow_scale,
                    });
                }
            }
            if let Some(prev_cb) = soc_prev_col {
                block.triplets.push(Triplet {
                    row,
                    col: prev_cb + sto_soc_off + s,
                    val: p_max,
                });
                block.row_lower[fb_row_local] = -1e30;
                block.row_upper[fb_row_local] = p_max * s3;
            } else {
                let soc_init = soc_prev_mwh[s];
                block.row_lower[fb_row_local] = -1e30;
                block.row_upper[fb_row_local] = p_max * (s3 - soc_init);
            }
            fb_row_local += 1;
        }
    }

    block
}

// ---------------------------------------------------------------------------
// Frequency security rows
// ---------------------------------------------------------------------------

/// Build frequency security constraint rows (0, 1, or 2 rows).
///
/// When `use_binary_inertia = true` (SCUC), the inertia constraint uses the
/// commitment binary `u[j]` instead of `Pg[j]`:
///   `Σ H_i·Sg_i·u_i ≥ h_min_mws`  (units: MW·s)
///
/// When `use_binary_inertia = false` (SCED):
///   `Σ (H_i·Sg_i/Pmax_i) · Pg_i_pu ≥ h_min_mws/base`
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_frequency_rows(
    network: &Network,
    gen_indices: &[usize],
    spec: &DispatchProblemSpec<'_>,
    col_base: usize,
    row_base: usize,
    pg_off: usize,
    base: f64,
    use_binary_inertia: bool,
    u_off: usize,
) -> LpBlock {
    let has_inertia = spec.frequency_security.effective_min_inertia_mws() > 0.0;
    let has_pfr = spec.frequency_security.min_pfr_mw.is_some_and(|v| v > 0.0);
    let n_rows = usize::from(has_inertia) + usize::from(has_pfr);

    if n_rows == 0 {
        return LpBlock::empty();
    }

    let mut block = LpBlock {
        triplets: Vec::with_capacity(gen_indices.len() * n_rows),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };

    let mut local_row = 0usize;

    if has_inertia {
        let h_min_mws = spec.frequency_security.effective_min_inertia_mws();
        let row = row_base + local_row;
        for (j, &gi) in gen_indices.iter().enumerate() {
            let g = &network.generators[gi];
            // Use per-generator H from network data; fall back to the caller-supplied
            // `generator_h_values` vector (indexed by dispatch position j) when the
            // network model does not carry inertia constants.
            let h = g.h_inertia_s.unwrap_or_else(|| {
                spec.frequency_security
                    .generator_h_values
                    .get(j)
                    .copied()
                    .unwrap_or(0.0)
            });
            let sg_mw = g.machine_base_mva;
            if use_binary_inertia {
                let coeff = h * sg_mw;
                if coeff.abs() > 1e-12 {
                    block.triplets.push(Triplet {
                        row,
                        col: col_base + u_off + j,
                        val: coeff,
                    });
                }
            } else {
                let h_coeff = if g.pmax > 1e-6 {
                    h * sg_mw / g.pmax
                } else {
                    0.0
                };
                if h_coeff.abs() > 1e-12 {
                    block.triplets.push(Triplet {
                        row,
                        col: col_base + pg_off + j,
                        val: h_coeff,
                    });
                }
            }
        }
        block.row_lower[local_row] = if use_binary_inertia {
            h_min_mws
        } else {
            h_min_mws / base
        };
        block.row_upper[local_row] = 1e30;
        local_row += 1;
    }

    if has_pfr {
        let pfr_req_pu = spec
            .frequency_security
            .min_pfr_mw
            .expect("has_pfr guards min_pfr_mw.is_some()")
            / base;
        let sum_pmax_pu: f64 = gen_indices
            .iter()
            .map(|&gi| {
                let g = &network.generators[gi];
                if g.pfr_eligible { g.pmax / base } else { 0.0 }
            })
            .sum();
        let row = row_base + local_row;
        for (j, &gi) in gen_indices.iter().enumerate() {
            if network.generators[gi].pfr_eligible {
                block.triplets.push(Triplet {
                    row,
                    col: col_base + pg_off + j,
                    val: -1.0,
                });
            }
        }
        block.row_lower[local_row] = pfr_req_pu - sum_pmax_pu;
        block.row_upper[local_row] = 1e30;
    }

    block
}

// ---------------------------------------------------------------------------
// Block linking rows (DISP-PWR)
// ---------------------------------------------------------------------------

/// Build DISP-PWR block-linking constraint rows (`n_gen` rows).
///
/// **SCED** (`u_off = None`):
///   `Pg[j] - Σᵢ Δᵢ[j] = Pmin[j]/base`  (committed) or `= 0` (uncommitted)
///
/// **SCUC** (`u_off = Some(u)`):
///   `Pg[j] - Σᵢ Δᵢ[j] - (Pmin[j]/base)·u[j] = 0`
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_block_linking_rows(
    setup: &DispatchSetup,
    spec: &DispatchProblemSpec<'_>,
    gen_indices: &[usize],
    network: &Network,
    period: usize,
    col_base: usize,
    row_base: usize,
    pg_off: usize,
    block_off: usize,
    u_off: Option<usize>,
    base: f64,
) -> LpBlock {
    let n_gen = gen_indices.len();
    let mut block = LpBlock {
        triplets: Vec::with_capacity(3 * n_gen + setup.n_block_vars),
        row_lower: vec![0.0; n_gen],
        row_upper: vec![0.0; n_gen],
    };

    for (j, blocks) in setup.gen_blocks.iter().enumerate() {
        let gi = gen_indices[j];
        let g = &network.generators[gi];
        let row = row_base + j;

        block.triplets.push(Triplet {
            row,
            col: col_base + pg_off + j,
            val: 1.0,
        });
        for i in 0..blocks.len() {
            block.triplets.push(Triplet {
                row,
                col: col_base + block_off + setup.gen_block_start[j] + i,
                val: -1.0,
            });
        }

        if let Some(u) = u_off {
            let pmin_pu = g.pmin / base;
            block.triplets.push(Triplet {
                row,
                col: col_base + u + j,
                val: -pmin_pu,
            });
            block.row_lower[j] = 0.0;
            block.row_upper[j] = 0.0;
        } else {
            let is_committed = spec.period(period).is_committed(j);
            let link_rhs = if is_committed { g.pmin / base } else { 0.0 };
            block.row_lower[j] = link_rhs;
            block.row_upper[j] = link_rhs;
        }
    }

    block
}

// ---------------------------------------------------------------------------
// Per-block reserve rows
// ---------------------------------------------------------------------------

/// Build per-block reserve constraint rows.
///
/// For each active product `p`:
///   Linking:  `R[j] - Σᵢ ρᵢ[j] = 0`  (`n_gen` rows per product)
///   Headroom: `Δᵢ[j] + ρᵢ[j] ≤ widthᵢ/base`  (`n_block_vars` rows per product)
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_per_block_reserve_rows(
    setup: &DispatchSetup,
    reserve_layout: &ReserveLpLayout,
    col_base: usize,
    row_base: usize,
    block_off: usize,
    blk_res_off: usize,
    base: f64,
) -> LpBlock {
    let n_active = reserve_layout.products.len();
    let n_gen_blocks = setup.gen_blocks.len();
    let n_block_vars = setup.n_block_vars;
    let n_link = n_gen_blocks * n_active;
    let n_headroom: usize = n_block_vars * n_active;
    let n_rows = n_link + n_headroom;

    let mut block = LpBlock {
        triplets: Vec::with_capacity(3 * n_rows),
        row_lower: vec![0.0; n_rows],
        row_upper: vec![0.0; n_rows],
    };

    let mut cur_local = 0usize;

    for (pi, ap) in reserve_layout.products.iter().enumerate() {
        // Linking rows: R[j] = Σᵢ ρᵢ[j]
        for (j, gen_blocks) in setup.gen_blocks.iter().enumerate() {
            let row = row_base + cur_local;
            block.triplets.push(Triplet {
                row,
                col: col_base + ap.gen_var_offset + j,
                val: 1.0,
            });
            for i in 0..gen_blocks.len() {
                block.triplets.push(Triplet {
                    row,
                    col: col_base + blk_res_off + pi * n_block_vars + setup.gen_block_start[j] + i,
                    val: -1.0,
                });
            }
            block.row_lower[cur_local] = 0.0;
            block.row_upper[cur_local] = 0.0;
            cur_local += 1;
        }

        // Headroom rows: Δᵢ + ρᵢ ≤ widthᵢ/base
        for (j, gen_blocks) in setup.gen_blocks.iter().enumerate() {
            for (i, blk) in gen_blocks.iter().enumerate() {
                let row = row_base + cur_local;
                block.triplets.push(Triplet {
                    row,
                    col: col_base + block_off + setup.gen_block_start[j] + i,
                    val: 1.0,
                });
                block.triplets.push(Triplet {
                    row,
                    col: col_base + blk_res_off + pi * n_block_vars + setup.gen_block_start[j] + i,
                    val: 1.0,
                });
                block.row_lower[cur_local] = f64::NEG_INFINITY;
                block.row_upper[cur_local] = blk.width_mw() / base;
                cur_local += 1;
            }
        }
    }

    block
}
