// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 HVDC link construction.
//!
//! GO C3 models each DC line with a single scalar dispatch variable
//! `p_dc_fr` (MW at the rectifier side). The bidirectional envelope
//! defaults to `[-pdc_ub, +pdc_ub]` unless an explicit `pdc_lb` is
//! provided. Losses are ignored in the public scenarios we've
//! encountered; the validator treats the from-side MW as equal to the
//! to-side MW modulo the explicit loss coefficients the LP carries.
//!
//! The AC-reconcile path also needs *synthetic* generators at each
//! converter terminal to carry the reactive power injection the AC
//! OPF is allowed to choose. Those synthetic resources live on the
//! adapter context (`dc_line_reactive_support_resource_ids`) and this
//! module emits their offer/bound schedules when the formulation is
//! AC.

use std::collections::HashMap;

use surge_dispatch::request::{GeneratorDispatchBoundsProfile, ResourceCommitmentSchedule};
use surge_dispatch::{HvdcDispatchLink, HvdcDispatchPoint, ResourceDispatchPoint};
use surge_io::go_c3::types::GoC3DcLine;
use surge_io::go_c3::{
    DcLineReactiveBounds, GoC3CommitmentMode, GoC3Context, GoC3Formulation, GoC3Policy, GoC3Problem,
};
use surge_network::market::{OfferCurve, OfferSchedule};

use crate::offers::build_offer_curve;

/// Scratch output of [`build_hvdc_request_pieces`]; the caller folds
/// these into the top-level request fields.
pub(super) struct HvdcPieces {
    pub links: Vec<HvdcDispatchLink>,
    pub previous_dispatch: Vec<HvdcDispatchPoint>,
    /// Synthetic generators (one per DC line terminal) introduced
    /// only when the formulation is AC.
    pub synthetic_offer_schedules: Vec<surge_dispatch::GeneratorOfferSchedule>,
    pub synthetic_dispatch_bounds: Vec<GeneratorDispatchBoundsProfile>,
    pub synthetic_previous_dispatch: Vec<ResourceDispatchPoint>,
    pub synthetic_commitment_schedules: Vec<ResourceCommitmentSchedule>,
}

/// Convert one GO C3 DC line into a `HvdcDispatchLink`.
fn build_link(
    dc: &GoC3DcLine,
    bus_uid_to_number: &HashMap<String, u32>,
    base_mva: f64,
) -> Option<HvdcDispatchLink> {
    let from_bus = *bus_uid_to_number.get(&dc.fr_bus)?;
    let to_bus = *bus_uid_to_number.get(&dc.to_bus)?;

    let pdc_ub_pu = if dc.pdc_ub != 0.0 {
        dc.pdc_ub
    } else {
        dc.initial_status.pdc_fr.abs()
    };
    // GO C3 `pdc_lb` is not a defined field in the current schema;
    // the Python adapter derives `-pdc_ub` when absent. For symmetry
    // we do the same.
    let pdc_lb_pu = -pdc_ub_pu;
    Some(HvdcDispatchLink {
        id: dc.uid.clone(),
        name: dc.uid.clone(),
        from_bus,
        to_bus,
        p_dc_min_mw: pdc_lb_pu * base_mva,
        p_dc_max_mw: (pdc_ub_pu * base_mva).max(0.0),
        loss_a_mw: 0.0,
        loss_b_frac: 0.0,
        ramp_mw_per_min: 0.0,
        cost_per_mwh: 0.0,
        bands: Vec::new(),
    })
}

/// Convert GO C3 DC-line reactive bounds into a terminal-generator
/// `(q_min_mvar, q_max_mvar)` pair.
///
/// GO C3 signs q at the DC terminal as "MVAr absorbed from the AC
/// grid at that terminal"; Surge's synthetic generator signs q as
/// "MVAr injected into the AC grid." The flip is `q_gen = -q_dc`.
fn dc_line_terminal_generator_q_bounds_mvar(
    q_lb_pu: f64,
    q_ub_pu: f64,
    base_mva: f64,
) -> (f64, f64) {
    let mut qmin = -q_ub_pu * base_mva;
    let mut qmax = -q_lb_pu * base_mva;
    if qmin > qmax {
        std::mem::swap(&mut qmin, &mut qmax);
    }
    (qmin, qmax)
}

/// Build all HVDC-related request pieces.
pub(super) fn build_hvdc_request_pieces(
    problem: &GoC3Problem,
    context: &GoC3Context,
    policy: &GoC3Policy,
) -> HvdcPieces {
    let base_mva = problem.network.general.base_norm_mva;
    let periods = problem.time_series_input.general.time_periods;

    let mut links = Vec::new();
    let mut previous = Vec::new();
    for dc in &problem.network.dc_line {
        if let Some(link) = build_link(dc, &context.bus_uid_to_number, base_mva) {
            links.push(link);
            previous.push(HvdcDispatchPoint {
                link_id: dc.uid.clone(),
                mw: dc.initial_status.pdc_fr * base_mva,
            });
        }
    }

    let mut synthetic_offer_schedules = Vec::new();
    let mut synthetic_dispatch_bounds = Vec::new();
    let mut synthetic_previous = Vec::new();
    let mut synthetic_commit = Vec::new();

    if matches!(policy.formulation, GoC3Formulation::Ac) {
        let mut schedules: Vec<(&String, &Vec<bool>)> = context
            .internal_support_commitment_schedule
            .iter()
            .collect();
        schedules.sort_by(|a, b| a.0.cmp(b.0));
        for (resource_id, schedule) in schedules {
            if !schedule.iter().any(|b| *b) {
                continue;
            }
            let (q_min_mvar, q_max_mvar) = if let Some((dc_line_uid, output_key)) = context
                .dc_line_reactive_support_resource_to_output
                .get(resource_id)
            {
                let DcLineReactiveBounds {
                    qdc_fr_lb,
                    qdc_fr_ub,
                    qdc_to_lb,
                    qdc_to_ub,
                } = context
                    .dc_line_q_bounds
                    .get(dc_line_uid)
                    .cloned()
                    .unwrap_or_default();
                let (q_lb, q_ub) = if output_key == "qdc_fr" {
                    (qdc_fr_lb, qdc_fr_ub)
                } else {
                    (qdc_to_lb, qdc_to_ub)
                };
                let (q_min, q_max) = dc_line_terminal_generator_q_bounds_mvar(q_lb, q_ub, base_mva);
                (vec![q_min; periods], vec![q_max; periods])
            } else {
                (vec![0.0; periods], vec![0.0; periods])
            };

            synthetic_offer_schedules.push(surge_dispatch::GeneratorOfferSchedule {
                resource_id: resource_id.clone(),
                schedule: OfferSchedule {
                    periods: (0..periods)
                        .map(|_| {
                            Some(build_offer_curve(&[], 0.0, Vec::new(), base_mva)).map(|mut c| {
                                c.segments = vec![(0.0, 0.0)];
                                c
                            })
                        })
                        .collect(),
                },
            });
            synthetic_dispatch_bounds.push(GeneratorDispatchBoundsProfile {
                resource_id: resource_id.clone(),
                p_min_mw: vec![0.0; periods],
                p_max_mw: vec![0.0; periods],
                q_min_mvar: Some(q_min_mvar),
                q_max_mvar: Some(q_max_mvar),
            });
            synthetic_previous.push(ResourceDispatchPoint {
                resource_id: resource_id.clone(),
                mw: 0.0,
            });
            if matches!(policy.commitment_mode, GoC3CommitmentMode::FixedInitial) {
                let committed_initial = schedule.first().copied().unwrap_or(false);
                let mut commit_periods: Vec<bool> =
                    schedule.iter().take(periods).copied().collect();
                if commit_periods.len() < periods {
                    commit_periods.resize(periods, false);
                }
                synthetic_commit.push(ResourceCommitmentSchedule {
                    resource_id: resource_id.clone(),
                    initial: committed_initial,
                    periods: Some(commit_periods),
                });
            }
        }
    }

    HvdcPieces {
        links,
        previous_dispatch: previous,
        synthetic_offer_schedules,
        synthetic_dispatch_bounds,
        synthetic_previous_dispatch: synthetic_previous,
        synthetic_commitment_schedules: synthetic_commit,
    }
}

/// Build the `OfferCurve` with a trivial `(0, 0)` segment, suitable
/// for synthetic resources that carry no energy value.
#[allow(dead_code)]
fn trivial_offer_curve(_base_mva: f64) -> OfferCurve {
    OfferCurve {
        segments: vec![(0.0, 0.0)],
        no_load_cost: 0.0,
        startup_tiers: Vec::new(),
    }
}
