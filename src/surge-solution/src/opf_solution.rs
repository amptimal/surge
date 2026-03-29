// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Optimal Power Flow solution types.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use surge_network::market::VirtualBidResult;

use crate::{ParResult, PfSolution};

fn serialize_branch_loading_pct<S>(values: &[f64], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use serde::ser::SerializeSeq;

    let mut seq = serializer.serialize_seq(Some(values.len()))?;
    for value in values {
        if value.is_finite() {
            seq.serialize_element(value)?;
        } else {
            seq.serialize_element(&Option::<f64>::None)?;
        }
    }
    seq.end()
}

fn deserialize_branch_loading_pct<'de, D>(deserializer: D) -> Result<Vec<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<Option<f64>>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .map(|value| value.unwrap_or(f64::NAN))
        .collect())
}

/// OPF formulation type — identifies which solver produced this solution.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpfType {
    /// DC-OPF via sparse B-θ formulation (lossless, linear).
    #[default]
    DcOpf,
    /// AC-OPF via Ipopt NLP (nonlinear, exact losses).
    AcOpf,
    /// DC Security-Constrained OPF (preventive or corrective N-1).
    DcScopf,
    /// AC Security-Constrained OPF (Benders N-1, NLP master).
    AcScopf,
    /// DC-OPF with embedded HVDC links.
    HvdcOpf,
}

// ---------------------------------------------------------------------------
// Sub-structs
// ---------------------------------------------------------------------------

/// Generator dispatch results and identity mapping.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfGeneratorResults {
    /// Optimal real power dispatch per generator (MW).
    ///
    /// Indexed by **in-service** generators in `network.generators` order.
    pub gen_p_mw: Vec<f64>,
    /// Optimal reactive power dispatch per generator (MVAr).
    ///
    /// Same ordering as `gen_p_mw`. Empty for DC-OPF; populated for AC-OPF.
    pub gen_q_mvar: Vec<f64>,
    /// External bus number for each entry in `gen_p_mw` / `gen_q_mvar`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gen_bus_numbers: Vec<u32>,
    /// Canonical generator identifier for each entry in `gen_p_mw` / `gen_q_mvar`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gen_ids: Vec<String>,
    /// Machine identifier for each entry in `gen_p_mw` / `gen_q_mvar`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gen_machine_ids: Vec<String>,
    /// Lower active-power bound duals ($/MWh), one per in-service generator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_pg_min: Vec<f64>,
    /// Upper active-power bound duals ($/MWh), one per in-service generator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_pg_max: Vec<f64>,
    /// Reactive power lower-bound duals ($/MWh per pu), one per in-service generator.
    /// AC-OPF only; empty for DC.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_qg_min: Vec<f64>,
    /// Reactive power upper-bound duals ($/MWh per pu), one per in-service generator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_qg_max: Vec<f64>,
}

/// LMP decomposition per bus.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfPricing {
    /// Locational marginal prices per bus ($/MWh).
    pub lmp: Vec<f64>,
    /// Energy component of LMP per bus ($/MWh).
    pub lmp_energy: Vec<f64>,
    /// Congestion component of LMP per bus ($/MWh).
    pub lmp_congestion: Vec<f64>,
    /// Loss component of LMP per bus ($/MWh). Zero for DC-OPF.
    pub lmp_loss: Vec<f64>,
    /// Reactive LMP per bus ($/MVAr-h). AC-OPF only; empty for DC.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lmp_reactive: Vec<f64>,
}

/// Branch-level results: loading, shadow prices, and constraint duals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfBranchResults {
    /// Branch loading percentage: `max(|Sf|, |St|) / rate_a * 100`.
    ///
    /// `NaN` for branches with no positive Rate A limit; serialized as `null`
    /// in JSON.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "serialize_branch_loading_pct",
        deserialize_with = "deserialize_branch_loading_pct"
    )]
    pub branch_loading_pct: Vec<f64>,
    /// Shadow prices on branch thermal flow limits ($/MWh per MW).
    pub branch_shadow_prices: Vec<f64>,
    /// Shadow prices on branch angmin constraints ($/MWh per rad).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_angmin: Vec<f64>,
    /// Shadow prices on branch angmax constraints ($/MWh per rad).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_angmax: Vec<f64>,
    /// Shadow prices for flowgate constraints ($/MWh).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flowgate_shadow_prices: Vec<f64>,
    /// Shadow prices for interface constraints ($/MWh).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interface_shadow_prices: Vec<f64>,
    /// Voltage magnitude lower-bound duals ($/MWh per pu), one per bus.
    /// AC-OPF only; empty for DC.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_vm_min: Vec<f64>,
    /// Voltage magnitude upper-bound duals ($/MWh per pu), one per bus.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_price_vm_max: Vec<f64>,
}

impl OpfBranchResults {
    /// Indices of binding branches (|branch_shadow_prices\[i\]| > 1e-6).
    pub fn binding_branch_indices(&self) -> Vec<usize> {
        self.branch_shadow_prices
            .iter()
            .enumerate()
            .filter(|(_, price)| price.abs() > 1e-6)
            .map(|(i, _)| i)
            .collect()
    }
}

/// FACTS device, storage, and transformer dispatch results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfDeviceDispatch {
    /// Switched shunt dispatch: `(bus_idx, continuous_b_pu, rounded_b_pu)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switched_shunt_dispatch: Vec<(usize, f64, f64)>,
    /// Transformer tap dispatch: `(branch_idx, continuous_tap, rounded_tap)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tap_dispatch: Vec<(usize, f64, f64)>,
    /// Phase-shifter dispatch: `(branch_idx, continuous_rad, rounded_rad)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_dispatch: Vec<(usize, f64, f64)>,
    /// SVC dispatch: `(device_index, optimal_b_svc_pu, mu_lower, mu_upper)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub svc_dispatch: Vec<(usize, f64, f64, f64)>,
    /// TCSC dispatch: `(device_index, optimal_x_comp_pu, mu_lower, mu_upper)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tcsc_dispatch: Vec<(usize, f64, f64, f64)>,
    /// Net storage dispatch (MW). Positive = discharging, negative = charging.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_net_mw: Vec<f64>,
    /// Whether the discrete round-and-check verification passed.
    ///
    /// `None` = continuous mode. `Some(true)` = passed. `Some(false)` = violations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discrete_feasible: Option<bool>,
    /// Human-readable descriptions of violations introduced by discrete rounding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discrete_violations: Vec<String>,
}

// ---------------------------------------------------------------------------
// OpfSolution
// ---------------------------------------------------------------------------

/// Result of an Optimal Power Flow computation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpfSolution {
    /// OPF formulation type; required for correct interpretation of all other fields.
    pub opf_type: OpfType,
    /// System MVA base used to convert MW/MVAr values to per-unit.
    pub base_mva: f64,
    /// Underlying power flow solution (voltages, injections, branch flows).
    pub power_flow: PfSolution,

    // System totals
    /// Total generation cost ($/hr).
    pub total_cost: f64,
    /// Total system load (MW).
    pub total_load_mw: f64,
    /// Total generation (MW).
    pub total_generation_mw: f64,
    /// Total system losses (MW). Zero for DC-OPF.
    pub total_losses_mw: f64,

    // Grouped results
    /// Generator dispatch and dual values.
    #[serde(flatten)]
    pub generators: OpfGeneratorResults,
    /// LMP decomposition.
    #[serde(flatten)]
    pub pricing: OpfPricing,
    /// Branch-level results and constraint duals.
    #[serde(flatten)]
    pub branches: OpfBranchResults,
    /// FACTS device, storage, and transformer dispatch.
    #[serde(flatten)]
    pub devices: OpfDeviceDispatch,

    // Supplemental results
    /// PAR implied shift angles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub par_results: Vec<ParResult>,
    /// Virtual bid clearing results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub virtual_bid_results: Vec<VirtualBidResult>,
    /// Benders cut dual values from AC-SCOPF.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benders_cut_duals: Vec<f64>,

    // Solver metadata
    /// Total solve time in seconds.
    pub solve_time_secs: f64,
    /// Number of solver iterations, when the backend exposes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u32>,
    /// Name of the LP/NLP solver (e.g. `"HiGHS"`, `"Gurobi"`, `"Ipopt"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_name: Option<String>,
    /// Version string of the solver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_opf(
        opf_type: OpfType,
        gen_p_mw: Vec<f64>,
        gen_q_mvar: Vec<f64>,
        lmp: Vec<f64>,
    ) -> OpfSolution {
        let n_buses = lmp.len();
        let pf = PfSolution {
            voltage_magnitude_pu: vec![1.0; n_buses],
            voltage_angle_rad: vec![0.0; n_buses],
            ..Default::default()
        };
        OpfSolution {
            opf_type,
            base_mva: 100.0,
            power_flow: pf,
            generators: OpfGeneratorResults {
                gen_p_mw,
                gen_q_mvar,
                ..Default::default()
            },
            pricing: OpfPricing {
                lmp,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_opf_solution_dc_construction_and_field_access() {
        let sol = make_opf(
            OpfType::DcOpf,
            vec![50.0, 100.0],
            vec![],
            vec![25.0, 30.0, 28.0],
        );
        assert_eq!(sol.opf_type, OpfType::DcOpf);
        assert_eq!(sol.base_mva, 100.0);
        assert_eq!(sol.generators.gen_p_mw, vec![50.0, 100.0]);
        assert!(
            sol.generators.gen_q_mvar.is_empty(),
            "DC-OPF should have empty gen_q_mvar"
        );
        assert_eq!(sol.pricing.lmp, vec![25.0, 30.0, 28.0]);
        assert_eq!(sol.power_flow.voltage_magnitude_pu.len(), 3);
    }

    #[test]
    fn test_opf_solution_ac_construction_with_reactive() {
        let sol = make_opf(OpfType::AcOpf, vec![75.0], vec![20.0], vec![35.0, 40.0]);
        assert_eq!(sol.opf_type, OpfType::AcOpf);
        assert_eq!(sol.generators.gen_p_mw, vec![75.0]);
        assert_eq!(sol.generators.gen_q_mvar, vec![20.0]);
    }

    #[test]
    fn test_opf_type_variants() {
        assert_eq!(OpfType::DcOpf, OpfType::DcOpf);
        assert_eq!(OpfType::AcOpf, OpfType::AcOpf);
        assert_eq!(OpfType::DcScopf, OpfType::DcScopf);
        assert_eq!(OpfType::AcScopf, OpfType::AcScopf);
        assert_eq!(OpfType::HvdcOpf, OpfType::HvdcOpf);
        assert_ne!(OpfType::DcOpf, OpfType::AcOpf);
    }

    #[test]
    fn test_opf_solution_cost_and_load_fields() {
        let mut sol = make_opf(OpfType::DcOpf, vec![150.0, 200.0], vec![], vec![30.0]);
        sol.total_cost = 5000.0;
        sol.total_load_mw = 340.0;
        sol.total_generation_mw = 350.0;
        sol.total_losses_mw = 10.0;
        assert_eq!(sol.total_cost, 5000.0);
        assert_eq!(sol.total_load_mw, 340.0);
        assert_eq!(sol.total_generation_mw, 350.0);
        assert_eq!(sol.total_losses_mw, 10.0);
    }

    #[test]
    fn test_opf_solution_solver_metadata() {
        let mut sol = make_opf(OpfType::DcOpf, vec![], vec![], vec![]);
        sol.solver_name = Some("HiGHS".to_string());
        sol.solver_version = Some("1.7.0".to_string());
        sol.iterations = Some(42);
        sol.solve_time_secs = 1.23;
        assert_eq!(sol.solver_name.as_deref(), Some("HiGHS"));
        assert_eq!(sol.solver_version.as_deref(), Some("1.7.0"));
        assert_eq!(sol.iterations, Some(42));
        assert_eq!(sol.solve_time_secs, 1.23);
    }

    #[test]
    fn test_opf_solution_binding_branches() {
        let mut sol = make_opf(OpfType::DcOpf, vec![], vec![], vec![]);
        sol.branches.branch_shadow_prices = vec![0.0, 5.2, 0.0, -3.1];
        let binding = sol.branches.binding_branch_indices();
        assert_eq!(binding, vec![1, 3]);
        assert_eq!(sol.branches.branch_shadow_prices[1], 5.2);
        assert_eq!(sol.branches.branch_shadow_prices[3], -3.1);
    }
}
