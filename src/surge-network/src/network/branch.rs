// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Branch (transmission line / transformer) representation.

use num_complex::Complex64;
use serde::{Deserialize, Serialize};

use crate::dynamics::{CoreLossModel, CoreType, TransformerSaturation};
use crate::market::AmbientConditions;

/// Zero-sequence winding connection type for transformers.
///
/// Determines how zero-sequence current propagates (or is blocked) through a
/// transformer. Standard ANSI/IEEE notation: G = grounded neutral.
///
/// # Zero-sequence rules (IEC 60909 / IEEE Std 1110)
///
/// | Connection  | Zero-sequence path |
/// |-------------|--------------------|
/// | WyeGWyeG    | Passes freely through transformer (both sides grounded) |
/// | WyeGDelta   | Blocked on delta (secondary) side; grounded-wye primary sees zero-seq |
/// | DeltaWyeG   | Blocked on delta (primary) side; grounded-wye secondary sees zero-seq |
/// | DeltaDelta  | Blocked completely — no zero-sequence path through the transformer |
/// | WyeGWye     | Blocked — ungrounded wye presents no zero-seq return path |
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum TransformerConnection {
    /// Both sides grounded wye — passes zero-sequence freely (default).
    ///
    /// Use for transformers where both neutrals are solidly grounded.
    /// Zero-sequence admittance is modeled identically to positive-sequence.
    #[default]
    WyeGWyeG,
    /// Primary grounded wye, secondary delta — blocks zero-sequence on secondary.
    ///
    /// The grounded-wye primary can carry zero-sequence current, but the delta
    /// secondary circulates it internally (no path to the secondary bus). In
    /// the zero-sequence Y-bus, only the primary-side shunt admittance appears
    /// as a self-admittance at the primary bus; the off-diagonal entries are zero.
    WyeGDelta,
    /// Primary delta, secondary grounded wye — blocks zero-sequence on primary.
    ///
    /// Symmetric to `WyeGDelta`: the grounded-wye secondary bus sees a shunt
    /// to ground but no coupling to the primary bus in the zero-sequence network.
    DeltaWyeG,
    /// Both sides delta — blocks zero-sequence completely.
    ///
    /// No zero-sequence current can pass through or terminate at either winding.
    /// The transformer is omitted entirely from the zero-sequence Y-bus.
    DeltaDelta,
    /// Grounded wye, ungrounded wye — blocks zero-sequence.
    ///
    /// The ungrounded wye presents no return path for zero-sequence current.
    /// Treated the same as `DeltaDelta` in the zero-sequence network.
    WyeGWye,
}

/// Tap-ratio control mode for AC-OPF.
///
/// When `Continuous`, the AC-OPF treats the off-nominal turns ratio as a
/// continuous NLP variable bounded by `[BranchOpfControl::tap_min, BranchOpfControl::tap_max]`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TapMode {
    /// Tap ratio is fixed at `Branch::tap` (default).
    #[default]
    Fixed,
    /// Tap ratio is a continuous NLP variable in AC-OPF.
    Continuous,
}

/// Phase-shifting transformer control mode for AC-OPF.
///
/// When `Continuous`, the AC-OPF treats the phase shift angle as a
/// continuous NLP variable bounded by
/// `[BranchOpfControl::phase_min_rad, BranchOpfControl::phase_max_rad]`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseMode {
    /// Phase shift is fixed at `Branch::phase_shift_rad` (default).
    #[default]
    Fixed,
    /// Phase shift is a continuous NLP variable in AC-OPF.
    Continuous,
}

/// Branch type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BranchType {
    /// Transmission line (overhead or cable).
    #[default]
    Line,
    /// Two-winding transformer.
    Transformer,
    /// Three-winding transformer (star-bus expanded into 3 two-winding branches).
    Transformer3W,
    /// Series capacitor or series reactor (negative reactance).
    SeriesCapacitor,
    /// Zero-impedance tie line (bus coupler or closed switch).
    ZeroImpedanceTie,
}

/// Physical line construction type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineType {
    /// Overhead transmission line on towers/poles.
    Overhead,
    /// Underground cable (typically higher capacitance, lower inductance).
    UndergroundCable,
    /// Submarine cable (subsea crossing).
    SubmarineCable,
}

/// Transformer winding connection type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindingConnection {
    /// Wye (star) connection, ungrounded neutral.
    Wye,
    /// Wye (star) connection, solidly grounded neutral.
    WyeGrounded,
    /// Delta (mesh) connection.
    Delta,
    /// Zigzag (interconnected star) connection.
    Zigzag,
    /// Autotransformer connection (shared winding).
    Auto,
}

// ---------------------------------------------------------------------------
// Sub-structs — optional groups of related fields factored out of Branch.
// ---------------------------------------------------------------------------

/// OPF tap/phase optimization parameters for a branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchOpfControl {
    /// Tap-ratio control mode for AC-OPF (default: `Fixed`).
    #[serde(default)]
    pub tap_mode: TapMode,
    /// Minimum tap ratio (pu) when `tap_mode = Continuous`. Typical: 0.9.
    #[serde(default = "BranchOpfControl::default_tap_min")]
    pub tap_min: f64,
    /// Maximum tap ratio (pu) when `tap_mode = Continuous`. Typical: 1.1.
    #[serde(default = "BranchOpfControl::default_tap_max")]
    pub tap_max: f64,
    /// Discrete tap step size (pu). Used for post-solve rounding in discrete AC-OPF.
    ///
    /// When `> 0`, the continuous NLP tap solution is rounded to the nearest
    /// `tap_min + n * tap_step` value. `0.0` = continuous (no rounding).
    /// Typical OLTC: `0.00625` (1/160, 16 steps over +/-10%).
    #[serde(default)]
    pub tap_step: f64,
    /// Phase-shifter control mode for AC-OPF (default: `Fixed`).
    #[serde(default)]
    pub phase_mode: PhaseMode,
    /// Minimum phase shift (radians) when `phase_mode = Continuous`. Typical: -30 deg.
    #[serde(
        default = "BranchOpfControl::default_phase_min_rad",
        alias = "phase_min_deg"
    )]
    pub phase_min_rad: f64,
    /// Maximum phase shift (radians) when `phase_mode = Continuous`. Typical: 30 deg.
    #[serde(
        default = "BranchOpfControl::default_phase_max_rad",
        alias = "phase_max_deg"
    )]
    pub phase_max_rad: f64,
    /// Discrete phase-shift step size (radians). Used for post-solve rounding.
    ///
    /// When `> 0`, the continuous NLP phase solution is rounded to the nearest
    /// discrete step. `0.0` = continuous (no rounding).
    #[serde(default, alias = "phase_step_deg")]
    pub phase_step_rad: f64,
}

impl BranchOpfControl {
    fn default_tap_min() -> f64 {
        0.9
    }
    fn default_tap_max() -> f64 {
        1.1
    }
    fn default_phase_min_rad() -> f64 {
        (-30.0_f64).to_radians()
    }
    fn default_phase_max_rad() -> f64 {
        30.0_f64.to_radians()
    }
}

impl Default for BranchOpfControl {
    fn default() -> Self {
        Self {
            tap_mode: TapMode::Fixed,
            tap_min: 0.9,
            tap_max: 1.1,
            tap_step: 0.0,
            phase_mode: PhaseMode::Fixed,
            phase_min_rad: (-30.0_f64).to_radians(),
            phase_max_rad: 30.0_f64.to_radians(),
            phase_step_rad: 0.0,
        }
    }
}

/// Physical line properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineData {
    /// Line length in km.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_km: Option<f64>,
    /// Physical line construction type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_type: Option<LineType>,
    /// Conductor designation (e.g. "Drake", "Falcon").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conductor: Option<String>,
    /// Number of sub-conductors per bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_bundles: Option<u32>,
    /// Resistance-temperature coefficient (1/deg-C). Default 0.
    #[serde(default)]
    pub r_temp_coeff: f64,
    /// Reference temperature for rated R (deg-C). Default 20.
    #[serde(default = "LineData::default_r_ref_temp_c")]
    pub r_ref_temp_c: f64,
}

impl LineData {
    fn default_r_ref_temp_c() -> f64 {
        20.0
    }
}

impl Default for LineData {
    fn default() -> Self {
        Self {
            length_km: None,
            line_type: None,
            conductor: None,
            n_bundles: None,
            r_temp_coeff: 0.0,
            r_ref_temp_c: 20.0,
        }
    }
}

/// Transformer winding identity and nameplate data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformerData {
    /// Transformer zero-sequence winding connection (FPQ-01).
    ///
    /// Controls how zero-sequence current propagates through this transformer
    /// in the fault analysis zero-sequence Y-bus:
    ///
    /// - `WyeGWyeG` (default): passes zero-sequence freely — same admittance as positive-seq.
    /// - `WyeGDelta`: primary (from) side sees zero-seq shunt to ground; secondary blocked.
    /// - `DeltaWyeG`: secondary (to) side sees zero-seq shunt to ground; primary blocked.
    /// - `DeltaDelta`: transformer is skipped entirely in the zero-sequence Y-bus.
    /// - `WyeGWye`: same as `DeltaDelta` — no zero-sequence path.
    ///
    /// For transmission lines (non-transformers), this field is ignored.
    #[serde(default)]
    pub transformer_connection: TransformerConnection,
    /// Winding rated kV (individual winding, not line-to-line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winding_rated_kv: Option<f64>,
    /// Winding rated MVA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winding_rated_mva: Option<f64>,
    /// Parent 3-winding transformer ID (star-bus expansion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_transformer_id: Option<String>,
    /// Winding number within parent transformer (1, 2, or 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winding_number: Option<u8>,
    /// Winding connection type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winding_connection: Option<WindingConnection>,
    /// Neutral impedance of this winding (pu, system base).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zn_winding: Option<Complex64>,
    /// Oil temperature limit in deg-C (from CGMES OilTemperatureLimit, informational).
    ///
    /// This is the transformer insulating oil temperature threshold at which the equipment
    /// is rated (PATL or TATL depending on OperationalLimitType). Stored per the CIM spec
    /// (OilTemperatureLimit attaches to ConductingEquipment via OperationalLimitSet).
    /// Not converted to MVA — requires equipment-specific thermal derating curves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oil_temp_limit_c: Option<f64>,
    /// Winding temperature limit in deg-C (from CGMES WindingTemperatureLimit, informational).
    ///
    /// Temperature threshold for the transformer winding insulation. Same structure as
    /// OilTemperatureLimit — stored per CIM spec, not converted to MVA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winding_temp_limit_c: Option<f64>,
    /// Impedance limit in Ohms (from CGMES ImpedanceLimit, informational).
    ///
    /// Represents a protection or operational limit on the series impedance magnitude.
    /// Stored per CIM spec (ImpedanceLimit attaches to ConductingEquipment via
    /// OperationalLimitSet). Not applied to the admittance model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impedance_limit_ohm: Option<f64>,
}

impl Default for TransformerData {
    fn default() -> Self {
        Self {
            transformer_connection: TransformerConnection::WyeGWyeG,
            winding_rated_kv: None,
            winding_rated_mva: None,
            parent_transformer_id: None,
            winding_number: None,
            winding_connection: None,
            zn_winding: None,
            oil_temp_limit_c: None,
            winding_temp_limit_c: None,
            impedance_limit_ohm: None,
        }
    }
}

/// Series capacitor/reactor protection data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeriesCompData {
    /// Bypass current threshold (kA) for series capacitor protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass_current_ka: Option<f64>,
    /// Rated reactive power of series element (MVAr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rated_mvar_series: Option<f64>,
    /// Series capacitor is currently bypassed.
    #[serde(default)]
    pub bypassed: bool,
}

/// Zero-sequence impedance data for fault analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZeroSeqData {
    /// Zero-sequence series resistance in per-unit (system base).
    ///
    /// From PSS/E `.seq` RLINZ field or CGMES per-length-impedance data.
    pub r0: f64,
    /// Zero-sequence series reactance in per-unit (system base).
    ///
    /// From PSS/E `.seq` XLINZ field.
    pub x0: f64,
    /// Zero-sequence total charging susceptance in per-unit (system base).
    ///
    /// From PSS/E `.seq` BCHZ field.
    pub b0: f64,
    /// Transformer neutral grounding impedance Zn (per-unit on system base).
    ///
    /// For grounded-wye transformer windings, `3*Zn` is added in series with the
    /// zero-sequence impedance. This increases zero-sequence impedance and reduces
    /// SLG fault currents, while 3LG faults are unaffected.
    ///
    /// From PSS/E `.seq` RG1/XG1 (primary) or RG2/XG2 (secondary) fields.
    /// `None` = solidly grounded (Zn = 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zn: Option<Complex64>,
    /// Zero-sequence terminal shunt conductance at the from-bus end (pu, system base).
    ///
    /// From PSS/E `.seq` GI field (token 6). Non-zero for underground cables with
    /// significant dielectric losses. Appears in the zero-sequence Y-bus as a shunt
    /// admittance at the from-bus diagonal. Defaults to 0 (zero) when absent.
    #[serde(default)]
    pub gi0: f64,
    /// Zero-sequence terminal shunt susceptance at the from-bus end (pu, system base).
    ///
    /// From PSS/E `.seq` BI field (token 7). See `gi0` for context.
    #[serde(default)]
    pub bi0: f64,
    /// Zero-sequence terminal shunt conductance at the to-bus end (pu, system base).
    ///
    /// From PSS/E `.seq` GJ field (token 8).
    #[serde(default)]
    pub gj0: f64,
    /// Zero-sequence terminal shunt susceptance at the to-bus end (pu, system base).
    ///
    /// From PSS/E `.seq` BJ field (token 9).
    #[serde(default)]
    pub bj0: f64,
    /// Whether this transformer winding is delta-connected.
    ///
    /// A delta-wound transformer blocks zero-sequence currents and triplen harmonics
    /// (3rd, 9th, 15th, ...). When `true` and the harmonic order `h` satisfies `h % 3 == 0`,
    /// the harmonic Y-bus builder zeros out this branch's admittance contribution,
    /// preventing triplen harmonic current from propagating through the delta winding.
    ///
    /// Default: `false` (wye-connected or transmission line — no blocking).
    #[serde(default)]
    pub delta_connected: bool,
}

impl Default for ZeroSeqData {
    fn default() -> Self {
        Self {
            r0: 0.0,
            x0: 0.0,
            b0: 0.0,
            zn: None,
            gi0: 0.0,
            bi0: 0.0,
            gj0: 0.0,
            bj0: 0.0,
            delta_connected: false,
        }
    }
}

/// Harmonic analysis data for a branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarmonicData {
    /// Skin-effect resistance correction coefficient for harmonic analysis (FPQ-22).
    ///
    /// At harmonic order h, the effective AC resistance is scaled by:
    ///   `R_h = R_1 * (1.0 + skin_effect_alpha * (h - 1))`
    ///
    /// This IEC 60287-simplified model accounts for current crowding toward the
    /// conductor surface at higher frequencies. Typical values:
    /// - `0.0` — no skin effect (default; appropriate for conductors < 100 mm^2)
    /// - `0.01-0.05` — medium conductors (100-300 mm^2)
    /// - `0.05-0.10` — large conductors (> 300 mm^2, ACSR bundled)
    ///
    /// The reactance X_h is unaffected (it scales linearly with h as omega*L).
    #[serde(default)]
    pub skin_effect_alpha: f64,
    /// Transformer core saturation characteristic for nonlinear harmonic analysis.
    ///
    /// When `Some`, the iterative harmonic solver computes voltage-dependent
    /// magnetizing harmonic currents from this curve. When `None`, the
    /// magnetizing branch uses the linear shunt admittance (g_mag + j*b_mag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<TransformerSaturation>,
    /// Transformer core construction type (affects GIC K-factor and saturation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_type: Option<CoreType>,
    /// Frequency-dependent core loss decomposition for harmonic analysis.
    ///
    /// When `Some`, the harmonic Y-bus uses frequency-scaled g_core(h) instead
    /// of constant g_mag. When `None`, uses `CoreLossModel::default()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_loss_model: Option<CoreLossModel>,
}

impl Default for HarmonicData {
    fn default() -> Self {
        Self {
            skin_effect_alpha: 0.0,
            saturation: None,
            core_type: None,
            core_loss_model: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Branch — core struct with optional sub-struct groups.
// ---------------------------------------------------------------------------

/// A branch connecting two buses in the power system network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    /// From bus number.
    pub from_bus: u32,
    /// To bus number.
    pub to_bus: u32,
    /// Circuit identifier (for parallel lines).
    pub circuit: String,
    /// Series resistance in per-unit.
    pub r: f64,
    /// Series reactance in per-unit.
    pub x: f64,
    /// Total line charging susceptance in per-unit.
    pub b: f64,
    /// Line charging conductance (total, pu on system base).
    ///
    /// Half is applied at each end of the pi-circuit model, matching the `b/2`
    /// convention for line charging susceptance.  Zero for most overhead
    /// transmission lines; non-zero for underground cables with significant
    /// dielectric losses, as found in some CGMES/CIM datasets (ACLineSegment
    /// `gch` field).
    #[serde(default)]
    pub g_pi: f64,
    /// Transformer off-nominal turns ratio (1.0 for lines).
    pub tap: f64,
    /// Transformer phase shift angle in **radians** (0.0 for lines).
    ///
    /// IO parsers convert from degrees at the boundary.
    #[serde(alias = "phase_shift_deg")]
    pub phase_shift_rad: f64,
    /// Transformer magnetizing conductance (pu on system base).
    ///
    /// Represents the real (loss) component of the transformer core admittance.
    /// Modeled as a shunt at the winding-1 (from-bus) terminal in the Y-bus.
    /// PSS/E MAG1 field. Zero for transmission lines and transformers without
    /// explicit magnetizing data.
    #[serde(default)]
    pub g_mag: f64,
    /// Transformer magnetizing susceptance (pu on system base).
    ///
    /// Represents the reactive (magnetizing) component of the transformer core
    /// admittance. Modeled as a shunt at the winding-1 (from-bus) terminal in
    /// the Y-bus. PSS/E MAG2 field. Zero for transmission lines and
    /// transformers without explicit magnetizing data.
    #[serde(default)]
    pub b_mag: f64,
    /// Long-term rating (MVA).
    pub rating_a_mva: f64,
    /// Short-term rating (MVA).
    pub rating_b_mva: f64,
    /// Emergency rating (MVA).
    pub rating_c_mva: f64,
    /// Branch status (true = in service).
    pub in_service: bool,
    /// Minimum phase angle difference across branch (from - to) in **radians**.
    ///
    /// Convention: all internal angle quantities are in radians.  IO parsers
    /// (MATPOWER, PSS/E, etc.) convert from degrees at the boundary.
    /// `None` = unconstrained (equivalent to -2pi).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_diff_min_rad: Option<f64>,
    /// Maximum phase angle difference across branch (from - to) in **radians**.
    ///
    /// Convention: all internal angle quantities are in radians.  IO parsers
    /// (MATPOWER, PSS/E, etc.) convert from degrees at the boundary.
    /// `None` = unconstrained (equivalent to +2pi).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_diff_max_rad: Option<f64>,
    /// Branch type classification.
    #[serde(default)]
    pub branch_type: BranchType,
    /// Impedance correction table number (PSS/E TAB1 field).
    ///
    /// When set, the branch's R and X are scaled by the interpolated factor
    /// from `Network::impedance_corrections` at the current tap position
    /// before Y-bus construction. `None` means no correction (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<u32>,
    /// Per-branch ambient conditions for dynamic line rating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambient: Option<AmbientConditions>,
    /// Ownership entries (PSS/E O1,F1..O4,F4). Up to 4 co-owners.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<super::owner::OwnershipEntry>,

    // --- optional sub-structs ---
    /// OPF tap/phase optimization parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opf_control: Option<BranchOpfControl>,
    /// Physical line properties.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_data: Option<LineData>,
    /// Transformer winding identity and nameplate data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transformer_data: Option<TransformerData>,
    /// Series capacitor/reactor protection data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_comp: Option<SeriesCompData>,
    /// Zero-sequence impedance data for fault analysis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero_seq: Option<ZeroSeqData>,
    /// Harmonic analysis data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harmonic: Option<HarmonicData>,
}

impl Default for Branch {
    fn default() -> Self {
        Self {
            from_bus: 0,
            to_bus: 0,
            circuit: "1".to_string(),
            r: 0.0,
            x: 0.0,
            b: 0.0,
            g_pi: 0.0,
            tap: 1.0,
            phase_shift_rad: 0.0,
            g_mag: 0.0,
            b_mag: 0.0,
            rating_a_mva: 0.0,
            rating_b_mva: 0.0,
            rating_c_mva: 0.0,
            in_service: true,
            angle_diff_min_rad: None,
            angle_diff_max_rad: None,
            branch_type: BranchType::Line,
            tab: None,
            ambient: None,
            owners: Vec::new(),
            opf_control: None,
            line_data: None,
            transformer_data: None,
            series_comp: None,
            zero_seq: None,
            harmonic: None,
        }
    }
}

/// Canonical pi-model admittance parameters for a branch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BranchPiAdmittance {
    pub g_ff: f64,
    pub b_ff: f64,
    pub g_ft: f64,
    pub b_ft: f64,
    pub g_tf: f64,
    pub b_tf: f64,
    pub g_tt: f64,
    pub b_tt: f64,
}

/// Canonical branch power flows in per-unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BranchPowerFlowsPu {
    pub p_from_pu: f64,
    pub q_from_pu: f64,
    pub p_to_pu: f64,
    pub q_to_pu: f64,
}

impl BranchPowerFlowsPu {
    #[inline]
    pub fn s_from_pu(self) -> f64 {
        (self.p_from_pu * self.p_from_pu + self.q_from_pu * self.q_from_pu).sqrt()
    }

    #[inline]
    pub fn s_to_pu(self) -> f64 {
        (self.p_to_pu * self.p_to_pu + self.q_to_pu * self.q_to_pu).sqrt()
    }

    #[inline]
    pub fn max_s_pu(self) -> f64 {
        self.s_from_pu().max(self.s_to_pu())
    }
}

impl Branch {
    /// Effective tap ratio, normalizing MATPOWER's tap=0 convention to 1.0.
    ///
    /// MATPOWER uses `tap = 0` in the data file to mean "no transformer" (i.e.,
    /// unity tap ratio). This method returns 1.0 when `|tap| < 1e-10` and the
    /// stored tap value otherwise.
    #[inline]
    pub fn effective_tap(&self) -> f64 {
        if self.tap.abs() < 1e-10 {
            1.0
        } else {
            self.tap
        }
    }

    /// True if this branch is a transformer (non-unity tap or non-zero phase shift).
    #[inline]
    pub fn is_transformer(&self) -> bool {
        (self.effective_tap() - 1.0).abs() > 1e-6 || self.phase_shift_rad.abs() > 1e-8
    }

    /// Pi-model admittance parameters for this branch.
    ///
    /// Returns the 8 admittance components `(g_ff, b_ff, g_ft, b_ft, g_tf, b_tf, g_tt, b_tt)`
    /// used to assemble the Y-bus and compute branch power flows.
    ///
    /// `z_sq_tol` is the caller-supplied zero-impedance guard threshold. When `r² + x²`
    /// is below this value, the branch is treated as a low-impedance tie (gs = 1e6, bs = 0).
    /// Each call site passes its existing threshold to preserve exact current behavior.
    ///
    /// The from-side self-admittance includes `g_pi/2` and `g_mag`/`b_mag` (transformer
    /// magnetizing branch at winding-1). The to-side self-admittance includes `g_pi/2`
    /// but not the magnetizing terms.
    #[inline]
    pub fn pi_model_admittances(&self, z_sq_tol: f64) -> (f64, f64, f64, f64, f64, f64, f64, f64) {
        let z_sq = self.r * self.r + self.x * self.x;
        let (gs, bs) = if z_sq > z_sq_tol {
            (self.r / z_sq, -self.x / z_sq)
        } else {
            (1e6, 0.0)
        };

        let tap = self.effective_tap();
        let tap_sq = tap * tap;
        let (cos_s, sin_s) = (self.phase_shift_rad.cos(), self.phase_shift_rad.sin());

        let g_ff = (gs + self.g_pi / 2.0) / tap_sq + self.g_mag;
        let b_ff = (bs + self.b / 2.0) / tap_sq + self.b_mag;
        let g_ft = -(gs * cos_s - bs * sin_s) / tap;
        let b_ft = -(gs * sin_s + bs * cos_s) / tap;
        let g_tf = -(gs * cos_s + bs * sin_s) / tap;
        let b_tf = (gs * sin_s - bs * cos_s) / tap;
        let g_tt = gs + self.g_pi / 2.0;
        let b_tt = bs + self.b / 2.0;

        (g_ff, b_ff, g_ft, b_ft, g_tf, b_tf, g_tt, b_tt)
    }

    /// Canonical pi-model admittance parameters as a named struct.
    #[inline]
    pub fn pi_model(&self, z_sq_tol: f64) -> BranchPiAdmittance {
        let (g_ff, b_ff, g_ft, b_ft, g_tf, b_tf, g_tt, b_tt) = self.pi_model_admittances(z_sq_tol);
        BranchPiAdmittance {
            g_ff,
            b_ff,
            g_ft,
            b_ft,
            g_tf,
            b_tf,
            g_tt,
            b_tt,
        }
    }

    /// Canonical from-end and to-end branch power flows in per-unit.
    ///
    /// `theta_ft_rad` is the bus-angle difference `va_from - va_to` in radians.
    #[inline]
    pub fn power_flows_pu(
        &self,
        vf_pu: f64,
        vt_pu: f64,
        theta_ft_rad: f64,
        z_sq_tol: f64,
    ) -> BranchPowerFlowsPu {
        let adm = self.pi_model(z_sq_tol);
        let (sin_ft, cos_ft) = theta_ft_rad.sin_cos();
        let theta_tf_rad = -theta_ft_rad;
        let (sin_tf, cos_tf) = theta_tf_rad.sin_cos();

        BranchPowerFlowsPu {
            p_from_pu: vf_pu * vf_pu * adm.g_ff
                + vf_pu * vt_pu * (adm.g_ft * cos_ft + adm.b_ft * sin_ft),
            q_from_pu: -vf_pu * vf_pu * adm.b_ff
                + vf_pu * vt_pu * (adm.g_ft * sin_ft - adm.b_ft * cos_ft),
            p_to_pu: vt_pu * vt_pu * adm.g_tt
                + vt_pu * vf_pu * (adm.g_tf * cos_tf + adm.b_tf * sin_tf),
            q_to_pu: -vt_pu * vt_pu * adm.b_tt
                + vt_pu * vf_pu * (adm.g_tf * sin_tf - adm.b_tf * cos_tf),
        }
    }

    /// DC series susceptance, corrected for off-nominal tap ratio.
    ///
    /// MATPOWER convention: TAP = 0 in the data file means "no transformer" and
    /// is treated as tap = 1.0.  For transformers with non-zero tap, the effective
    /// DC susceptance is 1 / (x * tap), matching MATPOWER's `makeBdc`.
    ///
    /// Returns the **signed** susceptance: b = 1 / (x * tap).
    /// Negative reactance (series compensation) correctly produces negative b,
    /// which is the physically accurate value for B-theta DC power flow and
    /// DC-OPF B-matrix assembly (matches MATPOWER `makeBdc` exactly — no abs).
    ///
    /// Branches with |x*tap| < 1e-20 (true zero-impedance ties) return 0.0
    /// to avoid division-by-zero; callers that need tie-line treatment should
    /// handle this case explicitly.
    ///
    /// The threshold is intentionally very small to match MATPOWER's `makeBdc`
    /// which computes `b = 1/x` with no clipping.  Real branches may have very
    /// small per-unit reactances (e.g. 6e-10 after ohm-to-pu conversion) and
    /// must not be zeroed out.
    #[inline]
    pub fn b_dc(&self) -> f64 {
        let tap = self.effective_tap();
        let denom = self.x * tap;
        if denom.abs() < 1e-20 {
            0.0
        } else {
            1.0 / denom
        }
    }

    /// Check whether a given angle difference (in **radians**) violates this
    /// branch's angle limits.
    ///
    /// `angle_diff_rad` should be `va_from - va_to` in radians (matching the
    /// Newton-Raphson voltage angle convention).
    ///
    /// Returns `true` if the angle difference is outside `[angmin, angmax]`.
    /// If either limit is `None`, that side is unconstrained.
    #[inline]
    pub fn angle_diff_violates(&self, angle_diff_rad: f64) -> bool {
        if let Some(lo) = self.angle_diff_min_rad {
            debug_assert!(
                lo.abs() <= 2.0 * std::f64::consts::PI + 0.01,
                "angle_diff_min_rad appears to be in degrees ({lo}), expected radians"
            );
            if angle_diff_rad < lo {
                return true;
            }
        }
        if let Some(hi) = self.angle_diff_max_rad {
            debug_assert!(
                hi.abs() <= 2.0 * std::f64::consts::PI + 0.01,
                "angle_diff_max_rad appears to be in degrees ({hi}), expected radians"
            );
            if angle_diff_rad > hi {
                return true;
            }
        }
        false
    }

    pub fn new_line(from_bus: u32, to_bus: u32, r: f64, x: f64, b: f64) -> Self {
        Self {
            from_bus,
            to_bus,
            r,
            x,
            b,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a branch with the given x and tap, everything else defaulted.
    fn branch_with(x: f64, tap: f64) -> Branch {
        Branch {
            from_bus: 1,
            to_bus: 2,
            r: 0.01,
            x,
            rating_a_mva: 100.0,
            rating_b_mva: 100.0,
            rating_c_mva: 100.0,
            tap,
            ..Default::default()
        }
    }

    #[test]
    fn test_b_dc_simple_line() {
        // Line with x=0.1, tap=1.0 => b_dc = 1 / (0.1 * 1.0) = 10.0
        let br = branch_with(0.1, 1.0);
        assert!((br.b_dc() - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_b_dc_with_tap() {
        // Transformer with tap=1.05, x=0.1 => b_dc = 1 / (0.1 * 1.05)
        let br = branch_with(0.1, 1.05);
        let expected = 1.0 / (0.1 * 1.05);
        assert!(
            (br.b_dc() - expected).abs() < 1e-10,
            "b_dc with tap=1.05: got {}, expected {}",
            br.b_dc(),
            expected
        );
    }

    #[test]
    fn test_b_dc_tap_zero_treated_as_one() {
        // MATPOWER convention: tap=0 in the data file means "no transformer",
        // treated as tap=1.0.  b_dc should equal 1/x = 1/0.1 = 10.0.
        let br = branch_with(0.1, 0.0);
        assert!(
            (br.b_dc() - 10.0).abs() < 1e-10,
            "tap=0 should be treated as tap=1.0; got b_dc={}",
            br.b_dc()
        );
    }

    #[test]
    fn test_b_dc_zero_x() {
        // Zero-impedance tie line: x=0, tap=1.0
        // The implementation returns 0.0 when |x*tap| < 1e-20.
        let br = branch_with(0.0, 1.0);
        assert!(
            br.b_dc().abs() < 1e-10,
            "zero-impedance branch should return b_dc=0.0; got {}",
            br.b_dc()
        );
    }

    #[test]
    fn test_b_dc_negative_x() {
        // Series capacitor: negative reactance x = -0.05, tap=1.0
        // b_dc = 1 / (-0.05 * 1.0) = -20.0  (signed, physically correct)
        let br = branch_with(-0.05, 1.0);
        let expected = 1.0 / (-0.05);
        assert!(
            (br.b_dc() - expected).abs() < 1e-10,
            "series capacitor b_dc: got {}, expected {}",
            br.b_dc(),
            expected
        );
    }

    #[test]
    fn test_new_line_defaults() {
        let br = Branch::new_line(5, 10, 0.01, 0.1, 0.02);
        assert_eq!(br.from_bus, 5);
        assert_eq!(br.to_bus, 10);
        assert_eq!(br.circuit, "1");
        assert!((br.r - 0.01).abs() < 1e-15);
        assert!((br.x - 0.1).abs() < 1e-15);
        assert!((br.b - 0.02).abs() < 1e-15);
        assert!(
            (br.tap - 1.0).abs() < 1e-15,
            "new_line tap should default to 1.0"
        );
        assert!(
            (br.phase_shift_rad).abs() < 1e-15,
            "new_line phase_shift_rad should default to 0.0"
        );
        assert!(br.in_service, "new_line should be in service by default");
        assert!(
            (br.rating_a_mva).abs() < 1e-15,
            "new_line rating_a_mva should default to 0.0"
        );
        assert!(
            br.angle_diff_min_rad.is_none(),
            "new_line angle_diff_min_rad should be None"
        );
        assert!(
            br.angle_diff_max_rad.is_none(),
            "new_line angle_diff_max_rad should be None"
        );
        // Moved fields: sub-structs default to None for new lines.
        assert!(br.zero_seq.is_none(), "new_line zero_seq should be None");
        assert!(br.harmonic.is_none(), "new_line harmonic should be None");
        assert!(
            br.transformer_data.is_none(),
            "new_line transformer_data should be None"
        );
    }
}
