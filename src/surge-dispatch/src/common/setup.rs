// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Pre-solve setup for SCED and SCUC formulations.
//!
//! [`DispatchSetup`] computes all derived data from the network and immutable
//! dispatch-problem data
//! that is needed before building the LP/MILP.  Both SCED and SCUC call
//! [`DispatchSetup::build`] once; the result is passed to layout and
//! constraint-builder helpers.

use std::collections::{HashMap, HashSet};

use surge_network::Network;
use surge_network::market::{ReserveProduct, SystemReserveRequirement, ZonalReserveRequirement};
use surge_network::network::{Branch, Generator};

use crate::common::blocks::{DispatchBlock, build_dispatch_blocks};
use crate::common::catalog::DispatchCatalog;
use crate::common::costs::{
    effective_co2_price, effective_co2_rates, resolve_pwl_gen_segments_for_period,
    storage_charge_epi_segments, storage_discharge_epi_segments,
};
use crate::common::runtime::effective_storage_soc_mwh;
use crate::common::spec::DispatchProblemSpec;
use crate::error::ScedError;

#[derive(Default)]
pub(crate) struct DispatchBranchLookup {
    in_service: HashMap<(u32, u32), HashMap<String, usize>>,
    dc_active: HashMap<(u32, u32), HashMap<String, usize>>,
}

impl DispatchBranchLookup {
    fn insert(
        lookup: &mut HashMap<(u32, u32), HashMap<String, usize>>,
        branch: &Branch,
        branch_idx: usize,
    ) {
        lookup
            .entry((branch.from_bus, branch.to_bus))
            .or_default()
            .insert(branch.circuit.clone(), branch_idx);
    }

    fn build(network: &Network) -> Self {
        let mut lookup = Self::default();
        for (branch_idx, branch) in network.branches.iter().enumerate() {
            if !branch.in_service {
                continue;
            }
            Self::insert(&mut lookup.in_service, branch, branch_idx);
            if branch.x.abs() > 1e-20 {
                Self::insert(&mut lookup.dc_active, branch, branch_idx);
            }
        }
        lookup
    }

    pub fn in_service_index(&self, from_bus: u32, to_bus: u32, circuit: &str) -> Option<usize> {
        self.in_service
            .get(&(from_bus, to_bus))
            .and_then(|circuits| circuits.get(circuit))
            .copied()
    }

    pub fn in_service_branch<'a>(
        &self,
        network: &'a Network,
        from_bus: u32,
        to_bus: u32,
        circuit: &str,
    ) -> Option<&'a Branch> {
        self.in_service_index(from_bus, to_bus, circuit)
            .map(|branch_idx| &network.branches[branch_idx])
    }

    pub fn dc_branch<'a>(
        &self,
        network: &'a Network,
        from_bus: u32,
        to_bus: u32,
        circuit: &str,
    ) -> Option<&'a Branch> {
        self.dc_active
            .get(&(from_bus, to_bus))
            .and_then(|circuits| circuits.get(circuit))
            .map(|&branch_idx| &network.branches[branch_idx])
    }
}

/// One resolved monitored-branch contribution in a flowgate/interface row.
pub(crate) struct MonitoredBranchTerm {
    pub from_bus_idx: usize,
    pub to_bus_idx: usize,
    /// Includes both the monitored-element coefficient and branch susceptance.
    pub theta_coeff: f64,
}

/// Resolved monitored-element representation for flowgates and interfaces.
pub(crate) struct ResolvedMonitoredElement {
    pub terms: Vec<MonitoredBranchTerm>,
    pub shift_offset: f64,
}

fn resolve_monitored_element(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    branch_lookup: &DispatchBranchLookup,
    members: &[surge_network::network::WeightedBranchRef],
) -> ResolvedMonitoredElement {
    let mut terms = Vec::with_capacity(members.len());
    let mut shift_offset = 0.0;

    for wbr in members {
        let coeff = wbr.coefficient;
        let Some(branch) = branch_lookup.dc_branch(
            network,
            wbr.branch.from_bus,
            wbr.branch.to_bus,
            wbr.branch.circuit.as_str(),
        ) else {
            continue;
        };
        let theta_coeff = coeff * branch.b_dc();
        terms.push(MonitoredBranchTerm {
            from_bus_idx: bus_map[&branch.from_bus],
            to_bus_idx: bus_map[&branch.to_bus],
            theta_coeff,
        });
        if branch.phase_shift_rad.abs() > 1e-12 {
            shift_offset += theta_coeff * branch.phase_shift_rad;
        }
    }

    ResolvedMonitoredElement {
        terms,
        shift_offset,
    }
}

/// All derived pre-solve setup data, computed once from network + problem spec.
///
/// Callers apply network transformations (FACTS expansion, MTDC injection,
/// NSPIN filtering) *before* calling [`DispatchSetup::build`].
pub(crate) struct DispatchSetup {
    // --- Lookup cache ---
    /// Precomputed branch lookup tables for monitored-element resolution.
    pub branch_lookup: DispatchBranchLookup,
    /// In-service PAR-controlled branches to exclude from B-theta flow rows.
    pub par_branch_set: HashSet<usize>,
    /// Flowgate monitored branches resolved onto bus indices and theta coefficients.
    pub resolved_flowgates: Vec<ResolvedMonitoredElement>,
    /// Interface monitored branches resolved onto bus indices and theta coefficients.
    pub resolved_interfaces: Vec<ResolvedMonitoredElement>,

    // --- Generator ---
    /// Global indices of in-service generators, in network order.
    pub gen_indices: Vec<usize>,
    /// Per in-service generator: internal bus index (in bus_map).
    pub gen_bus_idx: Vec<usize>,
    pub n_gen: usize,

    // --- Block mode (DISP-PWR) ---
    pub is_block_mode: bool,
    pub has_per_block_reserves: bool,
    /// Per in-service generator: dispatch blocks (empty if not block mode).
    pub gen_blocks: Vec<Vec<DispatchBlock>>,
    /// Per in-service generator: start index within the flat block variable array.
    pub gen_block_start: Vec<usize>,
    /// Total block variables across all generators (= sum of block counts).
    pub n_block_vars: usize,

    // --- Storage ---
    /// `(s, j, gi)` triples: s=local storage index, j=local gen index, gi=global gen index.
    pub storage_gen_local: Vec<(usize, usize, usize)>,
    /// Per storage unit: charge-leg efficiency (fraction of metered charge
    /// MW that lands in the SoC reservoir).
    pub storage_eta_charge: Vec<f64>,
    /// Per storage unit: discharge-leg efficiency (fraction of SoC draw that
    /// reaches the grid as metered discharge MW).
    pub storage_eta_discharge: Vec<f64>,
    /// Per storage unit: discharge-side foldback threshold (MWh). ``None``
    /// disables the foldback cut for that unit.
    pub storage_foldback_discharge_mwh: Vec<Option<f64>>,
    /// Per storage unit: charge-side foldback threshold (MWh). ``None``
    /// disables the foldback cut for that unit.
    pub storage_foldback_charge_mwh: Vec<Option<f64>>,
    /// Per storage unit: initial state-of-charge in MWh after request overrides.
    pub storage_initial_soc_mwh: Vec<f64>,
    pub n_storage: usize,

    // --- Generator PWL epiograph (DISP-PLC) ---
    /// `(local_gen_j, Vec<(slope_pu, intercept)>)` per PWL generator.
    pub pwl_gen_info: Vec<(usize, Vec<(f64, f64)>)>,
    pub n_pwl_gen: usize,
    pub n_pwl_rows: usize,

    // --- Storage offer-curve epiograph ---
    pub sto_dis_offer_info: Vec<(usize, Vec<(f64, f64)>)>,
    pub sto_ch_bid_info: Vec<(usize, Vec<(f64, f64)>)>,
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub n_sto_dis_offer_rows: usize,
    pub n_sto_ch_bid_rows: usize,

    // --- HVDC ---
    pub n_hvdc_vars: usize,
    pub n_hvdc_links: usize,
    /// Per HVDC link: offset of its first variable *relative to the start of the HVDC block*.
    pub hvdc_band_offsets_rel: Vec<usize>,

    // --- Dispatchable loads + virtual bids ---
    #[allow(dead_code)] // reserved for future builder extraction
    pub n_dl: usize,
    #[allow(dead_code)] // reserved for future builder extraction
    pub n_vbid: usize,

    // --- CO2 ---
    pub effective_co2_price: f64,
    /// Per in-service generator: effective CO2 emission rate (tCO2/MWh).
    pub effective_co2_rate: Vec<f64>,

    // --- Tie-lines ---
    /// `((area_a, area_b), limit_mw)` pairs.
    pub tie_line_pairs: Vec<((usize, usize), f64)>,

    // --- Reserve products (resolved from options) ---
    pub r_products: Vec<ReserveProduct>,
    pub r_sys_reqs: Vec<SystemReserveRequirement>,
    pub r_zonal_reqs: Vec<ZonalReserveRequirement>,

    #[allow(dead_code)] // reserved for future builder extraction
    pub base: f64,
}

impl DispatchSetup {
    /// Build setup from a (possibly transformed) network and immutable problem data.
    ///
    /// The caller is responsible for applying any network transformations
    /// (FACTS expansion, MTDC injection, per-hour profiles) before calling
    /// this function.  `gen_bus_idx` is computed using `bus_map`.
    pub fn build(network: &Network, spec: &DispatchProblemSpec<'_>) -> Result<Self, ScedError> {
        Self::build_for_period(network, spec, 0)
    }

    /// Build setup for a specific dispatch period.
    pub fn build_for_period(
        network: &Network,
        spec: &DispatchProblemSpec<'_>,
        period: usize,
    ) -> Result<Self, ScedError> {
        let base = network.base_mva;
        let bus_map = network.bus_index_map();
        let catalog = DispatchCatalog::from_network(network, spec.dispatchable_loads);
        let branch_lookup = DispatchBranchLookup::build(network);
        let par_branch_set: HashSet<usize> = spec
            .par_setpoints
            .iter()
            .filter_map(|ps| {
                branch_lookup.in_service_index(ps.from_bus, ps.to_bus, ps.circuit.as_str())
            })
            .collect();
        let resolved_flowgates = network
            .flowgates
            .iter()
            .map(|fg| resolve_monitored_element(network, &bus_map, &branch_lookup, &fg.monitored))
            .collect();
        let resolved_interfaces = network
            .interfaces
            .iter()
            .map(|iface| {
                resolve_monitored_element(network, &bus_map, &branch_lookup, &iface.members)
            })
            .collect();

        // --- In-service generators ---
        let gen_indices = catalog.in_service_gen_indices.clone();
        let n_gen = gen_indices.len();
        if n_gen == 0 {
            return Err(ScedError::NoGenerators);
        }

        let gen_bus_idx: Vec<usize> = gen_indices
            .iter()
            .map(|&gi| bus_map[&network.generators[gi].bus])
            .collect();

        // --- Block mode ---
        let is_block_mode = spec.is_block_mode();
        let has_per_block_reserves = spec.has_per_block_reserves();
        let gen_blocks: Vec<Vec<DispatchBlock>> = if is_block_mode {
            gen_indices
                .iter()
                .map(|&gi| build_dispatch_blocks(&network.generators[gi]))
                .collect()
        } else {
            vec![]
        };
        let n_block_vars: usize = gen_blocks.iter().map(|b| b.len()).sum();
        let gen_block_start: Vec<usize> = {
            let mut starts = Vec::with_capacity(gen_blocks.len());
            let mut acc = 0usize;
            for b in &gen_blocks {
                starts.push(acc);
                acc += b.len();
            }
            starts
        };

        // --- Storage ---
        let storage_gen_local: Vec<(usize, usize, usize)> = catalog
            .storage_gen_indices
            .iter()
            .enumerate()
            .map(|(storage_idx, &global_gen_idx)| {
                (
                    storage_idx,
                    catalog
                        .local_gen_index(global_gen_idx)
                        .expect("storage generator must be in-service"),
                    global_gen_idx,
                )
            })
            .collect();
        let n_storage = storage_gen_local.len();

        for &(_, _, gi) in &storage_gen_local {
            let sto = network.generators[gi]
                .storage
                .as_ref()
                .expect("storage_gen_local only contains generators with storage");
            sto.validate()
                .map_err(|err| ScedError::InvalidInput(format!("storage[{gi}]: {err}")))?;
        }

        let storage_eta_charge: Vec<f64> = storage_gen_local
            .iter()
            .map(|&(_, _, gi)| {
                network.generators[gi]
                    .storage
                    .as_ref()
                    .expect("storage_gen_local only contains generators with storage")
                    .charge_efficiency
                    .max(1e-9)
            })
            .collect();
        let storage_eta_discharge: Vec<f64> = storage_gen_local
            .iter()
            .map(|&(_, _, gi)| {
                network.generators[gi]
                    .storage
                    .as_ref()
                    .expect("storage_gen_local only contains generators with storage")
                    .discharge_efficiency
                    .max(1e-9)
            })
            .collect();
        let storage_foldback_discharge_mwh: Vec<Option<f64>> = storage_gen_local
            .iter()
            .map(|&(_, _, gi)| {
                network.generators[gi]
                    .storage
                    .as_ref()
                    .expect("storage_gen_local only contains generators with storage")
                    .discharge_foldback_soc_mwh
            })
            .collect();
        let storage_foldback_charge_mwh: Vec<Option<f64>> = storage_gen_local
            .iter()
            .map(|&(_, _, gi)| {
                network.generators[gi]
                    .storage
                    .as_ref()
                    .expect("storage_gen_local only contains generators with storage")
                    .charge_foldback_soc_mwh
            })
            .collect();
        let storage_initial_soc_mwh: Vec<f64> = storage_gen_local
            .iter()
            .map(|&(_, _, gi)| {
                effective_storage_soc_mwh(
                    spec.initial_state.storage_soc_override.as_ref(),
                    gi,
                    &network.generators[gi],
                )
            })
            .collect();

        // --- PWL epiograph ---
        let pwl_gen_info = if is_block_mode {
            vec![]
        } else {
            resolve_pwl_gen_segments_for_period(
                network,
                &gen_indices,
                spec.offer_schedules,
                period,
                base,
                spec.generator_pwl_cost_breakpoints(),
            )
        };
        let n_pwl_gen = pwl_gen_info.len();
        let n_pwl_rows: usize = pwl_gen_info.iter().map(|(_, segs)| segs.len()).sum();

        // --- Storage epiograph ---
        let storage_gen_refs: Vec<(usize, &Generator)> = storage_gen_local
            .iter()
            .map(|&(_, _, gi)| (gi, &network.generators[gi]))
            .collect();
        let sto_dis_offer_info = storage_discharge_epi_segments(&storage_gen_refs, base)?;
        let sto_ch_bid_info = storage_charge_epi_segments(&storage_gen_refs, base)?;
        let n_sto_dis_offer_rows: usize =
            sto_dis_offer_info.iter().map(|(_, segs)| segs.len()).sum();
        let n_sto_ch_bid_rows: usize = sto_ch_bid_info.iter().map(|(_, segs)| segs.len()).sum();
        let n_sto_dis_epi = sto_dis_offer_info.len();
        let n_sto_ch_epi = sto_ch_bid_info.len();

        // --- HVDC ---
        let n_hvdc_links = spec.hvdc_links.len();
        let n_hvdc_vars: usize = spec.hvdc_links.iter().map(|h| h.n_vars()).sum();
        let hvdc_band_offsets_rel: Vec<usize> = {
            let mut offsets = Vec::with_capacity(n_hvdc_links);
            let mut cursor = 0usize;
            for h in spec.hvdc_links {
                offsets.push(cursor);
                cursor += h.n_vars();
            }
            offsets
        };

        // --- Dispatchable loads + virtual bids (counts only) ---
        let n_dl = spec
            .dispatchable_loads
            .iter()
            .filter(|dl| dl.in_service)
            .count();
        let n_vbid = spec.virtual_bids.iter().filter(|vb| vb.in_service).count();

        // --- CO2 ---
        let eff_co2_price = effective_co2_price(spec);
        let eff_co2_rate = effective_co2_rates(network, &gen_indices, spec);

        // --- Tie-lines ---
        let tie_line_pairs: Vec<((usize, usize), f64)> = spec
            .tie_line_limits
            .map(|tl| {
                tl.limits_mw
                    .iter()
                    .map(|(&pair, &limit)| (pair, limit))
                    .collect()
            })
            .unwrap_or_default();

        // --- Reserve products ---
        let (r_products, r_sys_reqs, r_zonal_reqs) =
            crate::common::reserves::resolve_reserve_config(spec);

        Ok(Self {
            branch_lookup,
            par_branch_set,
            resolved_flowgates,
            resolved_interfaces,
            gen_indices,
            gen_bus_idx,
            n_gen,
            is_block_mode,
            has_per_block_reserves,
            gen_blocks,
            gen_block_start,
            n_block_vars,
            storage_gen_local,
            storage_eta_charge,
            storage_eta_discharge,
            storage_foldback_discharge_mwh,
            storage_foldback_charge_mwh,
            storage_initial_soc_mwh,
            n_storage,
            pwl_gen_info,
            n_pwl_gen,
            n_pwl_rows,
            sto_dis_offer_info,
            sto_ch_bid_info,
            n_sto_dis_epi,
            n_sto_ch_epi,
            n_sto_dis_offer_rows,
            n_sto_ch_bid_rows,
            n_hvdc_vars,
            n_hvdc_links,
            hvdc_band_offsets_rel,
            n_dl,
            n_vbid,
            effective_co2_price: eff_co2_price,
            effective_co2_rate: eff_co2_rate,
            tie_line_pairs,
            r_products,
            r_sys_reqs,
            r_zonal_reqs,
            base,
        })
    }
}
