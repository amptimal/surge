// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-OPF variable index mapping.
//!
//! Maps NLP variables to their physical meaning: voltage angles, magnitudes,
//! generator dispatch, transformer taps, phase shifters, shunts, FACTS, HVDC, storage.

use surge_network::Network;

use crate::common::context::OpfNetworkContext;

use super::types::{AcOpfError, SvcOpfData, TcscOpfData};

/// Maps variables in the NLP vector to their physical meaning.
///
/// Variable layout: `[Va(n-1) | Vm(n) | Pg(ng) | Qg(ng) | τ(n_tap) | θ_s(n_ps) | b_sw(n_sw) | b_svc(n_svc) | x_comp(n_tcsc) | P_conv(n_conv) | Q_conv(n_conv) | V_dc(n_dc_bus) | I_conv(n_conv)]`
pub(crate) struct AcOpfMapping {
    pub(crate) n_bus: usize,
    pub(crate) slack_idx: usize,
    /// In-service generator indices into network.generators[].
    pub(crate) gen_indices: Vec<usize>,
    pub(crate) n_gen: usize,
    /// Map from bus internal index to list of local gen indices at that bus.
    pub(crate) bus_gen_map: Vec<Vec<usize>>,
    /// Constrained branch indices (thermal flow limits).
    pub(crate) constrained_branches: Vec<usize>,
    /// Angle-difference constrained branch indices and their (angmin, angmax) in radians.
    ///
    /// Only includes branches where at least one limit is tighter than ±π.
    pub(crate) angle_constrained_branches: Vec<(usize, f64, f64)>,
    /// Tap-control branch indices with (branch_idx, tap_min, tap_max).
    pub(crate) tap_ctrl_branches: Vec<(usize, f64, f64)>,
    /// Phase-shift-control branch indices with (branch_idx, phase_min_rad, phase_max_rad).
    pub(crate) ps_ctrl_branches: Vec<(usize, f64, f64)>,
    /// Total number of variables.
    pub(crate) n_var: usize,
    /// Total number of constraints (including D-curve, excluding Benders cuts).
    pub(crate) n_con: usize,
    /// Constraint row offset where D-curve (P-Q capability) constraints start.
    pub(crate) pq_con_offset: usize,
    // Variable offsets
    pub(crate) va_offset: usize, // 0
    pub(crate) vm_offset: usize, // n_bus - 1
    pub(crate) pg_offset: usize, // n_bus - 1 + n_bus
    pub(crate) qg_offset: usize, // n_bus - 1 + n_bus + n_gen
    /// Offset of τ[0] (after Qg).
    pub(crate) tap_offset: usize,
    /// Offset of θ_s[0] (after τ).
    pub(crate) ps_offset: usize,
    /// Offset of b_sw[0] (after θ_s).
    pub(crate) sw_offset: usize,
    /// Number of switched shunt OPF variables.
    pub(crate) n_sw: usize,
    /// Internal bus index for each switched-shunt OPF variable.
    pub(crate) switched_shunt_bus_idx: Vec<usize>,
    /// Offset of b_svc[0] (after b_sw).
    pub(crate) svc_offset: usize,
    /// Number of SVC NLP variables.
    pub(crate) n_svc: usize,
    /// SVC device data.
    pub(crate) svc_devices: Vec<SvcOpfData>,
    /// Offset of x_comp[0] (after b_svc).
    pub(crate) tcsc_offset: usize,
    /// Number of TCSC NLP variables.
    pub(crate) n_tcsc: usize,
    /// TCSC device data.
    pub(crate) tcsc_devices: Vec<TcscOpfData>,
    // --- HVDC NLP augmentation ---
    /// Offset of P_conv[0] (after x_comp).
    pub(crate) pconv_offset: usize,
    /// Offset of Q_conv[0] (after P_conv).
    pub(crate) qconv_offset: usize,
    /// Offset of V_dc[0] (after Q_conv).
    pub(crate) vdc_offset: usize,
    /// Offset of I_conv[0] (after V_dc).
    pub(crate) iconv_offset: usize,
    /// Number of DC converters (HVDC NLP variables).
    pub(crate) n_conv: usize,
    /// Number of DC buses (HVDC NLP variables).
    pub(crate) n_dc_bus: usize,
    /// AC bus internal index for each converter k.
    pub(crate) conv_ac_bus: Vec<usize>,
    /// Constraint row offset where HVDC DC KCL constraints begin.
    pub(crate) dc_kcl_row_offset: usize,
    /// Constraint row offset where HVDC current-definition constraints begin.
    pub(crate) iconv_eq_row_offset: usize,
    /// Constraint row offset where HVDC DC-control constraints begin.
    pub(crate) dc_control_row_offset: usize,
    /// Constraint row offset where HVDC AC-control constraints begin.
    pub(crate) ac_control_row_offset: usize,
    // --- Storage NLP variables ---
    /// Number of storage units co-optimized as NLP variables (CostMinimization mode).
    pub(crate) n_sto: usize,
    /// Offset of dis[0] in variable vector (after I_conv).
    pub(crate) discharge_offset: usize,
    /// Offset of ch[0] in variable vector (after dis).
    pub(crate) charge_offset: usize,
    /// Internal bus index for each storage unit s.
    pub(crate) storage_bus_idx: Vec<usize>,
    /// Active flowgate indices into `network.flowgates` (in_service only).
    pub(crate) flowgate_indices: Vec<usize>,
    /// Active interface indices into `network.interfaces` (in_service, limit > 0).
    pub(crate) interface_indices: Vec<usize>,
    /// Constraint row offset where flowgate constraints begin.
    pub(crate) fg_con_offset: usize,
    /// Constraint row offset where interface constraints begin.
    pub(crate) iface_con_offset: usize,
}

impl AcOpfMapping {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        context: &OpfNetworkContext<'_>,
        constrained_branches: Vec<usize>,
        enforce_angle_limits: bool,
        optimize_taps: bool,
        optimize_phase_shifters: bool,
        optimize_switched_shunts: bool,
        optimize_svc: bool,
        optimize_tcsc: bool,
        hvdc: Option<&super::hvdc::HvdcNlpData>,
        storage_bus_idx: Vec<usize>,
        enforce_flowgates: bool,
        enforce_capability_curves: bool,
    ) -> Result<Self, AcOpfError> {
        use surge_network::network::{PhaseMode, TapMode};

        let network: &Network = context.network;
        let n_bus = network.n_buses();
        let slack_idx = context.slack_idx;
        let gen_indices = context.gen_indices.clone();
        let n_gen = gen_indices.len();

        let bus_map = &context.bus_map;
        let bus_gen_map = context.bus_gen_map.clone();

        let n_va = n_bus - 1; // skip slack
        let n_vm = n_bus;

        // Collect tap-controllable branches.
        let mut tap_ctrl_branches: Vec<(usize, f64, f64)> = Vec::new();
        if optimize_taps {
            for (br_idx, br) in network.branches.iter().enumerate() {
                if let Some(ctrl) = &br.opf_control {
                    if br.in_service && ctrl.tap_mode == TapMode::Continuous {
                        tap_ctrl_branches.push((br_idx, ctrl.tap_min, ctrl.tap_max));
                    }
                }
            }
        }
        let n_tap = tap_ctrl_branches.len();

        // Collect phase-shift-controllable branches.
        let mut ps_ctrl_branches: Vec<(usize, f64, f64)> = Vec::new();
        if optimize_phase_shifters {
            for (br_idx, br) in network.branches.iter().enumerate() {
                if let Some(ctrl) = &br.opf_control {
                    if br.in_service && ctrl.phase_mode == PhaseMode::Continuous {
                        let lo_rad = ctrl.phase_min_rad;
                        let hi_rad = ctrl.phase_max_rad;
                        ps_ctrl_branches.push((br_idx, lo_rad, hi_rad));
                    }
                }
            }
        }
        let n_ps = ps_ctrl_branches.len();

        let switched_shunt_bus_idx: Vec<usize> = if optimize_switched_shunts {
            let mut resolved = Vec::with_capacity(network.controls.switched_shunts_opf.len());
            for shunt in &network.controls.switched_shunts_opf {
                let bus_idx = *bus_map.get(&shunt.bus).ok_or_else(|| {
                    AcOpfError::InvalidNetwork(format!(
                        "switched shunt OPF `{}` references unknown host bus {}",
                        shunt.id, shunt.bus
                    ))
                })?;
                resolved.push(bus_idx);
            }
            resolved
        } else {
            vec![]
        };
        let n_sw = switched_shunt_bus_idx.len();

        // Collect SVC/STATCOM devices for optimization.
        let base_mva = network.base_mva;
        let svc_devices: Vec<SvcOpfData> = if optimize_svc {
            network
                .facts_devices
                .iter()
                .filter(|f| f.mode.in_service() && f.mode.has_shunt())
                .filter_map(|f| {
                    let bus_idx = *bus_map.get(&f.bus_from)?;
                    let b_max = f.q_max / base_mva; // capacitive limit
                    let b_min = if f.q_min != 0.0 {
                        f.q_min / base_mva
                    } else {
                        -f.q_max / base_mva
                    };
                    let b_init = (f.q_setpoint_mvar / base_mva).clamp(b_min, b_max);
                    Some(SvcOpfData {
                        bus_idx,
                        b_min,
                        b_max,
                        b_init,
                    })
                })
                .collect()
        } else {
            vec![]
        };
        let n_svc = svc_devices.len();

        // Collect TCSC devices for optimization.
        let tcsc_devices: Vec<TcscOpfData> = if optimize_tcsc {
            network
                .facts_devices
                .iter()
                .filter(|f| f.mode.in_service() && f.mode.has_series() && f.bus_to != 0)
                .filter_map(|f| {
                    let from_idx = *bus_map.get(&f.bus_from)?;
                    let to_idx = *bus_map.get(&f.bus_to)?;
                    // Find matching branch
                    let (_branch_idx, br) =
                        network.branches.iter().enumerate().find(|(_, br)| {
                            br.in_service
                                && ((br.from_bus == f.bus_from && br.to_bus == f.bus_to)
                                    || (br.from_bus == f.bus_to && br.to_bus == f.bus_from))
                        })?;
                    let x_orig = br.x;
                    let r = br.r;
                    let tap = if br.tap.abs() > 1e-10 { br.tap } else { 1.0 };
                    let shift_rad = br.phase_shift_rad;
                    // Compensation limits: default to [-0.5*|x|, 0.8*|x|] if not specified
                    let x_comp_min = f.x_min.unwrap_or(-0.5 * x_orig.abs());
                    let x_comp_max_raw = f.x_max.unwrap_or(0.8 * x_orig.abs());
                    // Safety: prevent near-zero impedance
                    let x_comp_max = x_comp_max_raw.min(x_orig.abs() - r.abs().max(0.001));
                    // Skip device if bounds are degenerate (|x_orig| too close to |r|)
                    if x_comp_max <= x_comp_min + 1e-6 {
                        return None;
                    }
                    let x_comp_init = f.series_reactance_pu.clamp(x_comp_min, x_comp_max);
                    Some(TcscOpfData {
                        from_idx,
                        to_idx,
                        x_orig,
                        r,
                        tap,
                        shift_rad,
                        x_comp_min,
                        x_comp_max,
                        x_comp_init,
                    })
                })
                .collect()
        } else {
            vec![]
        };
        let n_tcsc = tcsc_devices.len();

        // HVDC dimensions
        let (n_conv, n_dc_bus, conv_ac_bus) = if let Some(h) = hvdc {
            let ac: Vec<usize> = h.converters.iter().map(|c| c.ac_bus_idx).collect();
            (h.n_conv, h.n_dc_bus, ac)
        } else {
            (0, 0, vec![])
        };

        // Variable layout: [Va(n_va) | Vm(n_vm) | Pg(n_gen) | Qg(n_gen) | τ(n_tap) | θ_s(n_ps) | b_sw(n_sw) | b_svc(n_svc) | x_comp(n_tcsc) | P_conv(n_conv) | Q_conv(n_conv) | V_dc(n_dc_bus) | I_conv(n_conv) | dis(n_sto) | ch(n_sto)]
        let qg_offset = n_va + n_vm + n_gen;
        let tap_offset = qg_offset + n_gen;
        let ps_offset = tap_offset + n_tap;
        let sw_offset = ps_offset + n_ps;
        let svc_offset = sw_offset + n_sw;
        let tcsc_offset = svc_offset + n_svc;
        let pconv_offset = tcsc_offset + n_tcsc;
        let qconv_offset = pconv_offset + n_conv;
        let vdc_offset = qconv_offset + n_conv;
        let iconv_offset = vdc_offset + n_dc_bus;
        let n_sto = storage_bus_idx.len();
        let discharge_offset = iconv_offset + n_conv;
        let charge_offset = discharge_offset + n_sto;
        let n_var = charge_offset + n_sto;

        // Collect angle-constrained branches (only when explicitly enabled).
        // Default off — matches MATPOWER's opf.ignore_angle_lim = 1 default.
        // Many case files (e.g., ACTIVSg exported from PowerWorld) store
        // angmin = angmax = 0 as the current operating angle, not a binding limit;
        // enforcing those values makes the NLP infeasible.
        const ANG_LO: f64 = -std::f64::consts::PI;
        const ANG_HI: f64 = std::f64::consts::PI;
        let mut angle_constrained_branches: Vec<(usize, f64, f64)> = Vec::new();
        if enforce_angle_limits {
            for (br_idx, br) in network.branches.iter().enumerate() {
                if !br.in_service {
                    continue;
                }
                let lo = br.angle_diff_min_rad.unwrap_or(f64::NEG_INFINITY);
                let hi = br.angle_diff_max_rad.unwrap_or(f64::INFINITY);
                if lo > ANG_LO || hi < ANG_HI {
                    angle_constrained_branches.push((br_idx, lo, hi));
                }
            }
        }
        let n_ang = angle_constrained_branches.len();

        // Count D-curve constraints: 2 per consecutive segment pair per in-service generator.
        // Gated on enforce_capability_curves — when false, all generators use flat Q bounds.
        let n_pq_cons: usize = if enforce_capability_curves {
            gen_indices
                .iter()
                .map(|&gi| {
                    let len = network.generators[gi]
                        .reactive_capability
                        .as_ref()
                        .map_or(0, |r| r.pq_curve.len());
                    if len >= 2 { 2 * (len - 1) } else { 0 }
                })
                .sum()
        } else {
            0
        };

        // Constraint layout:
        //   2*n_bus (P+Q balance)
        //   + 2*n_constrained_branches (from+to flow limits)
        //   + n_ang (angle difference constraints)
        //   + n_dc_bus (DC KCL equality constraints, when HVDC augmented)
        //   + n_conv (converter current-definition: P²+Q²-Vm²·I²=0)
        //   + n_conv (DC control equations)
        //   + n_conv (AC control equations)
        //   + n_pq_cons (D-curve piecewise-linear capability constraints)
        let dc_kcl_row_offset = 2 * n_bus + 2 * constrained_branches.len() + n_ang;
        let iconv_eq_row_offset = dc_kcl_row_offset + n_dc_bus;
        let dc_control_row_offset = iconv_eq_row_offset + n_conv;
        let ac_control_row_offset = dc_control_row_offset + n_conv;
        let pq_con_offset = ac_control_row_offset + n_conv;

        // Collect active flowgate and interface indices.
        let flowgate_indices: Vec<usize> = if enforce_flowgates {
            network
                .flowgates
                .iter()
                .enumerate()
                .filter(|(_, fg)| fg.in_service)
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![]
        };
        let interface_indices: Vec<usize> = if enforce_flowgates {
            network
                .interfaces
                .iter()
                .enumerate()
                .filter(|(_, iface)| iface.in_service && iface.limit_forward_mw > 0.0)
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![]
        };
        let n_fg = flowgate_indices.len();
        let n_iface = interface_indices.len();
        let fg_con_offset = pq_con_offset + n_pq_cons;
        let iface_con_offset = fg_con_offset + n_fg;
        let n_con = iface_con_offset + n_iface;

        Ok(Self {
            n_bus,
            slack_idx,
            gen_indices,
            n_gen,
            bus_gen_map,
            constrained_branches,
            angle_constrained_branches,
            tap_ctrl_branches,
            ps_ctrl_branches,
            n_var,
            n_con,
            pq_con_offset,
            va_offset: 0,
            vm_offset: n_va,
            pg_offset: n_va + n_vm,
            qg_offset,
            tap_offset,
            ps_offset,
            sw_offset,
            n_sw,
            switched_shunt_bus_idx,
            svc_offset,
            n_svc,
            svc_devices,
            tcsc_offset,
            n_tcsc,
            tcsc_devices,
            pconv_offset,
            qconv_offset,
            vdc_offset,
            iconv_offset,
            n_conv,
            n_dc_bus,
            conv_ac_bus,
            dc_kcl_row_offset,
            iconv_eq_row_offset,
            dc_control_row_offset,
            ac_control_row_offset,
            n_sto,
            discharge_offset,
            charge_offset,
            storage_bus_idx,
            flowgate_indices,
            interface_indices,
            fg_con_offset,
            iface_con_offset,
        })
    }

    /// Get tap ratio variable index for tap-controllable branch k.
    #[inline]
    pub(crate) fn tap_var(&self, k: usize) -> usize {
        self.tap_offset + k
    }

    /// Get phase-shift variable index for phase-controllable branch k.
    #[inline]
    pub(crate) fn ps_var(&self, k: usize) -> usize {
        self.ps_offset + k
    }

    /// Get switched shunt susceptance variable index for shunt i.
    #[inline]
    pub(crate) fn sw_var(&self, i: usize) -> usize {
        self.sw_offset + i
    }

    /// Get SVC susceptance variable index for device i.
    #[inline]
    pub(crate) fn svc_var(&self, i: usize) -> usize {
        self.svc_offset + i
    }

    /// Get TCSC compensation reactance variable index for device i.
    #[inline]
    pub(crate) fn tcsc_var(&self, i: usize) -> usize {
        self.tcsc_offset + i
    }

    /// Get P_conv variable index for converter k.
    #[inline]
    pub(crate) fn pconv_var(&self, k: usize) -> usize {
        self.pconv_offset + k
    }

    /// Get Q_conv variable index for converter k.
    #[inline]
    pub(crate) fn qconv_var(&self, k: usize) -> usize {
        self.qconv_offset + k
    }

    /// Get V_dc variable index for DC bus d.
    #[inline]
    pub(crate) fn vdc_var(&self, d: usize) -> usize {
        self.vdc_offset + d
    }

    /// Get I_conv variable index for converter k.
    #[inline]
    pub(crate) fn iconv_var(&self, k: usize) -> usize {
        self.iconv_offset + k
    }

    pub(crate) fn dc_control_row(&self, k: usize) -> usize {
        self.dc_control_row_offset + k
    }

    pub(crate) fn ac_control_row(&self, k: usize) -> usize {
        self.ac_control_row_offset + k
    }

    /// Get discharge variable index for storage unit s.
    #[inline]
    pub(crate) fn discharge_var(&self, s: usize) -> usize {
        self.discharge_offset + s
    }

    /// Get charge variable index for storage unit s.
    #[inline]
    pub(crate) fn charge_var(&self, s: usize) -> usize {
        self.charge_offset + s
    }

    /// Get Va index for bus i (returns None for slack).
    #[inline]
    pub(crate) fn va_var(&self, bus: usize) -> Option<usize> {
        if bus == self.slack_idx {
            None
        } else if bus < self.slack_idx {
            Some(self.va_offset + bus)
        } else {
            Some(self.va_offset + bus - 1)
        }
    }

    /// Get Vm index for bus i.
    #[inline]
    pub(crate) fn vm_var(&self, bus: usize) -> usize {
        self.vm_offset + bus
    }

    /// Get Pg index for local gen j.
    #[inline]
    pub(crate) fn pg_var(&self, j: usize) -> usize {
        self.pg_offset + j
    }

    /// Get Qg index for local gen j.
    #[inline]
    pub(crate) fn qg_var(&self, j: usize) -> usize {
        self.qg_offset + j
    }

    /// Unpack x into (va, vm, pg, qg) vectors.
    pub(crate) fn extract_voltages_and_dispatch<'a>(
        &self,
        x: &'a [f64],
    ) -> (Vec<f64>, &'a [f64], &'a [f64], &'a [f64]) {
        // Reconstruct full va including slack = 0
        let mut va = vec![0.0; self.n_bus];
        for (i, va_i) in va.iter_mut().enumerate() {
            if let Some(idx) = self.va_var(i) {
                *va_i = x[idx];
            }
            // slack stays 0
        }
        let vm = &x[self.vm_offset..self.vm_offset + self.n_bus];
        let pg = &x[self.pg_offset..self.pg_offset + self.n_gen];
        let qg = &x[self.qg_offset..self.qg_offset + self.n_gen];
        (va, vm, pg, qg)
    }
}
