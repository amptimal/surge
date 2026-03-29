// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DSS → three-phase distribution network converter.
//!
//! Converts a parsed DSS catalog to a `surge_dist::ThreePhaseNetwork`
//! suitable for the three-phase BFS solver.
//!
//! # Phase specification parsing
//!
//! DSS bus strings encode phase connections after a dot: `"650.1.2.3"`
//! means bus 650, phases 1 (A), 2 (B), 3 (C) → all three phases.
//! Single-phase: `"node.1"` → phase A only.
//!
//! # Unit conventions
//!
//! The output ThreePhaseNetwork uses actual units throughout:
//! - Impedance: Ω (total, not per-km)
//! - Load: kW and kVAr per phase
//! - Voltage: kV (line-to-neutral at source)

use std::collections::HashMap;
use std::path::Path;

use super::objects::{DssCatalog, DssObject, WdgConn};
use super::resolve::{resolve_linecodes, resolve_xfmrcodes, strip_phases};
use super::to_network::DssParseError;
use surge_dist::{
    LoadConnection, PhaseImpedanceMatrix, RegulatorControl, ThreePhaseBranch, ThreePhaseLoad,
    ThreePhaseLoadModel, ThreePhaseNetwork, ThreePhaseTransformer,
};

// ─────────────────────────────────────────────────────────────────────────────
// Catalog builder (replicates the private logic from to_network.rs)
// ─────────────────────────────────────────────────────────────────────────────

/// Build a resolved DssCatalog from a .dss file path.
///
/// Handles redirects, resolves linecodes and xfmrcodes.
fn build_dss_catalog(path: &Path) -> Result<DssCatalog, DssParseError> {
    let content = std::fs::read_to_string(path).map_err(|e| DssParseError::Io {
        path: path.to_string_lossy().to_string(),
        source: e,
    })?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    build_catalog_from_str(&content, Some(base_dir))
}

fn build_catalog_from_str(
    content: &str,
    base_dir: Option<&Path>,
) -> Result<DssCatalog, DssParseError> {
    let mut catalog = DssCatalog::new();
    let mut last_obj_idx: Option<usize> = None;
    let mut last_was_circuit = false;
    let mut frequency_hz = 60.0_f64;
    process_dss_stream(
        content,
        base_dir,
        0,
        &mut catalog,
        &mut last_obj_idx,
        &mut last_was_circuit,
        &mut frequency_hz,
    )?;

    resolve_linecodes(&mut catalog);
    resolve_xfmrcodes(&mut catalog);

    Ok(catalog)
}

fn process_dss_stream(
    content: &str,
    base_dir: Option<&Path>,
    depth: usize,
    catalog: &mut DssCatalog,
    last_obj_idx: &mut Option<usize>,
    last_was_circuit: &mut bool,
    frequency_hz: &mut f64,
) -> Result<(), DssParseError> {
    use super::command::parse_commands;
    use super::lexer::tokenize;

    if depth > 16 {
        return Err(DssParseError::UnresolvedRef(
            "redirect/compile nesting depth exceeded".to_string(),
        ));
    }

    let tokens = tokenize(content);
    let commands = parse_commands(&tokens);
    for cmd in &commands {
        process_dss_command(
            cmd,
            catalog,
            last_obj_idx,
            last_was_circuit,
            frequency_hz,
            base_dir,
            depth,
        )?;
    }
    Ok(())
}

fn process_dss_command(
    cmd: &super::command::DssCommand,
    catalog: &mut DssCatalog,
    last_obj_idx: &mut Option<usize>,
    last_was_circuit: &mut bool,
    _frequency_hz: &mut f64,
    base_dir: Option<&Path>,
    depth: usize,
) -> Result<(), DssParseError> {
    use super::command::DssCommand;
    use super::objects::DssObject;

    match cmd {
        DssCommand::Clear => {
            *catalog = DssCatalog::new();
            *last_obj_idx = None;
        }

        DssCommand::New {
            obj_type,
            obj_name,
            properties,
        }
        | DssCommand::Edit {
            obj_type,
            obj_name,
            properties,
        } => {
            let is_circuit =
                obj_type.to_lowercase() == "circuit" || obj_type.to_lowercase() == "vsource";

            if is_circuit {
                let mut circ = super::objects::CircuitData {
                    name: obj_name.clone(),
                    ..Default::default()
                };
                for (k, v) in properties {
                    circ.apply_property(k, v);
                }
                catalog.circuit = Some(circ);
                *last_obj_idx = None;
                *last_was_circuit = true;
            } else {
                match DssObject::new_for_type(obj_type) {
                    Some(mut obj) => {
                        *obj.name_mut() = obj_name.clone();
                        // Handle 'like=<name>' property by cloning the referenced object first
                        let like_name: Option<String> = properties
                            .iter()
                            .find(|(k, _)| k.to_lowercase() == "like")
                            .map(|(_, v)| v.clone());
                        if let Some(ref base_name) = like_name {
                            // Clone base object BEFORE other properties are applied
                            if let Some(base_obj) = catalog.find(obj_type, base_name).cloned() {
                                obj = base_obj;
                                *obj.name_mut() = obj_name.clone();
                            }
                        }
                        // Apply explicit properties (after like= resolution, skip 'like' itself)
                        for (k, v) in properties {
                            if k.to_lowercase() != "like" {
                                obj.apply_property(k, v);
                            }
                        }
                        let idx = catalog.upsert(obj_type, obj_name, obj);
                        *last_obj_idx = Some(idx);
                        *last_was_circuit = false;
                    }
                    None => {
                        *last_obj_idx = None;
                    }
                }
            }
        }

        DssCommand::More { properties } => {
            if *last_was_circuit {
                if let Some(ref mut circ) = catalog.circuit {
                    for (k, v) in properties {
                        circ.apply_property(k, v);
                    }
                }
            } else if let Some(i) = *last_obj_idx
                && let Some(obj) = catalog.get_mut(i)
            {
                for (k, v) in properties {
                    obj.apply_property(k, v);
                }
            }
        }

        DssCommand::Redirect {
            path: redirect_path,
        }
        | DssCommand::Compile {
            path: redirect_path,
        } => {
            let full_path = if let Some(bd) = base_dir {
                bd.join(redirect_path)
            } else {
                std::path::PathBuf::from(redirect_path)
            };
            let redirect_content =
                std::fs::read_to_string(&full_path).map_err(|source| DssParseError::Io {
                    path: full_path.to_string_lossy().to_string(),
                    source,
                })?;
            let child_base = full_path
                .parent()
                .unwrap_or_else(|| base_dir.unwrap_or(Path::new(".")));
            process_dss_stream(
                &redirect_content,
                Some(child_base),
                depth + 1,
                catalog,
                last_obj_idx,
                last_was_circuit,
                _frequency_hz,
            )?;
        }

        DssCommand::Set { .. } | DssCommand::Solve | DssCommand::Unknown { .. } => {
            // No-op for network building
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase spec parser
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a DSS bus specification into (bus_name, phase_bitmask).
///
/// Phase bitmask: bit 0 = phase A (DSS phase 1),
///               bit 1 = phase B (DSS phase 2),
///               bit 2 = phase C (DSS phase 3).
///
/// Examples:
///   "650.1.2.3" → ("650", 0b111)   three phases
///   "node.1"    → ("node", 0b001)  phase A only
///   "node.2"    → ("node", 0b010)  phase B only
///   "node.3"    → ("node", 0b100)  phase C only
///   "node.1.2"  → ("node", 0b011)  A and B
///   "node"      → ("node", 0b111)  default: all phases
fn parse_phase_spec(bus: &str) -> (&str, u8) {
    let dot_pos = bus.find('.');
    let (name, rest) = if let Some(pos) = dot_pos {
        (&bus[..pos], &bus[pos + 1..])
    } else {
        (bus, "")
    };
    if rest.is_empty() {
        return (name, 0b111);
    }
    let mut mask = 0u8;
    for part in rest.split('.') {
        match part.trim() {
            "1" => mask |= 0b001,
            "2" => mask |= 0b010,
            "3" => mask |= 0b100,
            _ => {}
        }
    }
    if mask == 0 {
        mask = 0b111;
    }
    (name, mask)
}

/// Parse a bus spec like `799.1.2` and return the delta pair as
/// `(regulated_phase, reference_phase)` in 0-indexed form.
///
/// For a single-phase delta transformer, the first listed node is the
/// regulated terminal and the second is the return/reference terminal.
/// Returns `None` if fewer than two phase nodes are specified.
fn parse_delta_pair(bus: &str) -> Option<(usize, usize)> {
    let dot_pos = bus.find('.')?;
    let rest = &bus[dot_pos + 1..];
    let mut phases = Vec::new();
    for part in rest.split('.') {
        match part.trim() {
            "1" => phases.push(0usize),
            "2" => phases.push(1),
            "3" => phases.push(2),
            _ => {}
        }
    }
    if phases.len() >= 2 {
        Some((phases[0], phases[1]))
    } else {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Lower-triangular matrix expansion
// ─────────────────────────────────────────────────────────────────────────────

/// Expand a lower-triangular flat vector to a full N×N matrix (symmetrized).
///
/// DSS lower-triangular order for an N-phase line:
///   tri = [R(0,0), R(1,0), R(1,1), R(2,0), R(2,1), R(2,2), ...]  (0-indexed)
///
/// Returns a 3×3 matrix where the N×N sub-matrix is placed at the positions
/// given by `active_phases` (a sorted list of 3-phase indices 0=A, 1=B, 2=C).
///
/// Example: A 2-phase line on phases B+C (active_phases=[1,2]) gets:
///   m[1][1] = tri[0]  (B self-impedance)
///   m[2][1] = m[1][2] = tri[1]  (B-C mutual)
///   m[2][2] = tri[2]  (C self-impedance)
/// All other elements remain 0.
fn expand_lower_tri_to_3x3(tri: &[f64], active_phases: &[usize]) -> [[f64; 3]; 3] {
    let mut m = [[0.0f64; 3]; 3];
    let n = active_phases.len();

    // Decode lower-triangular into n×n sub-matrix and place at correct positions
    let mut idx = 0usize;
    for row in 0..n {
        for col in 0..=row {
            if idx >= tri.len() {
                break;
            }
            let r3 = active_phases[row];
            let c3 = active_phases[col];
            m[r3][c3] = tri[idx];
            m[c3][r3] = tri[idx]; // symmetrize
            idx += 1;
        }
    }
    m
}

/// Expand a lower-triangular flat vector to a full 3×3 matrix (symmetrized).
/// Assumes all three phases A, B, C are active (standard slot assignment).
/// Use `expand_lower_tri_to_3x3` with explicit active_phases for non-standard phase connections.
#[allow(dead_code)]
fn expand_lower_tri_3x3(tri: &[f64]) -> [[f64; 3]; 3] {
    expand_lower_tri_to_3x3(tri, &[0, 1, 2])
}

/// Decode active phases from a bitmask into a sorted list of phase indices.
///
/// phase_mask bits: bit0=A(0), bit1=B(1), bit2=C(2).
/// Returns indices of active phases in ascending order (0=A, 1=B, 2=C).
fn active_phase_indices(phase_mask: u8) -> Vec<usize> {
    let mut phases = Vec::new();
    for ph in 0..3usize {
        if (phase_mask >> ph) & 1 == 1 {
            phases.push(ph);
        }
    }
    phases
}

// ─────────────────────────────────────────────────────────────────────────────
// Bus enumeration
// ─────────────────────────────────────────────────────────────────────────────

/// Build a 0-indexed bus map from a resolved DssCatalog.
///
/// The circuit source bus gets index 0.
/// All other buses are sorted alphabetically and get indices 1, 2, ...
///
/// Returns (bus_map: HashMap<lowercase_name, index>, bus_names: Vec<String>)
fn build_3ph_bus_map(catalog: &DssCatalog) -> (HashMap<String, usize>, Vec<String>) {
    let mut names: std::collections::BTreeSet<String> = Default::default();
    let mut source_bus = String::new();

    if let Some(ref circ) = catalog.circuit {
        source_bus = circ.bus.to_lowercase();
        // Source bus is NOT added to the sorted set — it gets index 0
    }

    for obj in &catalog.objects {
        match obj {
            DssObject::Line(l) => {
                let b1 = strip_phases(&l.bus1.to_lowercase()).to_string();
                let b2 = strip_phases(&l.bus2.to_lowercase()).to_string();
                if !b1.is_empty() && b1 != source_bus {
                    names.insert(b1);
                }
                if !b2.is_empty() && b2 != source_bus {
                    names.insert(b2);
                }
            }
            DssObject::Transformer(t) => {
                for b in &t.buses {
                    let bn = strip_phases(&b.to_lowercase()).to_string();
                    if !bn.is_empty() && bn != source_bus {
                        names.insert(bn);
                    }
                }
            }
            DssObject::Load(l) => {
                let bus1_lc = l.bus1.to_lowercase();
                let (name, _) = parse_phase_spec(&bus1_lc);
                let bn = name.to_string();
                if !bn.is_empty() && bn != source_bus {
                    names.insert(bn);
                }
            }
            DssObject::Reactor(r) => {
                // 2-bus reactors are series elements (like lines)
                if !r.bus1.is_empty() {
                    let b1 = strip_phases(&r.bus1.to_lowercase()).to_string();
                    if !b1.is_empty() && b1 != source_bus {
                        names.insert(b1);
                    }
                }
                if !r.bus2.is_empty() {
                    let b2 = strip_phases(&r.bus2.to_lowercase()).to_string();
                    if !b2.is_empty() && b2 != source_bus {
                        names.insert(b2);
                    }
                }
            }
            DssObject::Capacitor(c) => {
                if !c.bus1.is_empty() {
                    let b1 = strip_phases(&c.bus1.to_lowercase()).to_string();
                    if !b1.is_empty() && b1 != source_bus {
                        names.insert(b1);
                    }
                }
            }
            DssObject::Generator(g) => {
                if !g.bus1.is_empty() {
                    let b = strip_phases(&g.bus1.to_lowercase()).to_string();
                    if !b.is_empty() && b != source_bus {
                        names.insert(b);
                    }
                }
            }
            DssObject::PvSystem(pv) => {
                if !pv.bus1.is_empty() {
                    let b = strip_phases(&pv.bus1.to_lowercase()).to_string();
                    if !b.is_empty() && b != source_bus {
                        names.insert(b);
                    }
                }
            }
            DssObject::Storage(st) => {
                if !st.bus1.is_empty() {
                    let b = strip_phases(&st.bus1.to_lowercase()).to_string();
                    if !b.is_empty() && b != source_bus {
                        names.insert(b);
                    }
                }
            }
            DssObject::Fault(f) => {
                for bus_str in [&f.bus1, &f.bus2] {
                    if !bus_str.is_empty() {
                        let b = strip_phases(&bus_str.to_lowercase()).to_string();
                        if !b.is_empty() && b != source_bus {
                            names.insert(b);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Source bus at index 0, rest sorted alphabetically
    let mut bus_names = vec![source_bus.clone()];
    bus_names.extend(names.iter().cloned());

    let bus_map: HashMap<String, usize> = bus_names
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), i))
        .collect();

    (bus_map, bus_names)
}

// ─────────────────────────────────────────────────────────────────────────────
// Line → ThreePhaseBranch
// ─────────────────────────────────────────────────────────────────────────────

/// Mask inactive phases in a 3x3 impedance matrix.
///
/// `phase_mask` bits: bit0=A, bit1=B, bit2=C.
/// Inactive phases: zero out their rows/columns; set diagonal to 1e6 Ω.
fn apply_phase_mask_to_z(z: &mut [[f64; 2]; 9], phase_mask: u8) {
    for ph in 0..3usize {
        let active = (phase_mask >> ph) & 1 == 1;
        if !active {
            // Zero out the entire row and column for this phase
            for j in 0..3 {
                z[ph * 3 + j] = [0.0, 0.0];
                z[j * 3 + ph] = [0.0, 0.0];
            }
            // Set diagonal to large impedance to block current
            z[ph * 3 + ph] = [1e6, 0.0];
        }
    }
}

fn line_to_3ph_branch(
    line: &super::objects::LineData,
    bus_map: &HashMap<String, usize>,
    system_freq_hz: f64,
) -> Option<ThreePhaseBranch> {
    if line.bus1.is_empty() || line.bus2.is_empty() {
        return None;
    }

    let bus1_lc = line.bus1.to_lowercase();
    let bus2_lc = line.bus2.to_lowercase();
    let (from_name_str, phase_mask) = parse_phase_spec(&bus1_lc);
    let (to_name_str, _) = parse_phase_spec(&bus2_lc);
    let from_name = from_name_str;
    let to_name = to_name_str;
    let from_bus = *bus_map.get(from_name)?;
    let to_bus = *bus_map.get(to_name)?;

    if from_bus == to_bus {
        return None; // degenerate
    }

    let len_km = line.length * line.units.to_km_factor();
    // Guard against zero-length lines (treat as negligible impedance)
    let len_km = if len_km <= 0.0 { 1.0 } else { len_km };

    let z_matrix = if !line.rmatrix.is_empty() && !line.xmatrix.is_empty() {
        // Full matrix — lower triangular in Ω/km, multiply by length.
        // Use active_phase_indices to correctly place the n×n sub-matrix at
        // the right 3×3 positions for non-standard phase combinations (e.g. A+C, C-only).
        let active = active_phase_indices(phase_mask);
        let r3 = expand_lower_tri_to_3x3(&line.rmatrix, &active);
        let x3 = expand_lower_tri_to_3x3(&line.xmatrix, &active);

        let mut z = [[0.0f64; 2]; 9];
        for i in 0..3 {
            for j in 0..3 {
                z[i * 3 + j] = [r3[i][j] * len_km, x3[i][j] * len_km];
            }
        }

        // Apply phase mask to zero out inactive phases and set diagonals to 1e6
        apply_phase_mask_to_z(&mut z, phase_mask);
        PhaseImpedanceMatrix { z }
    } else {
        // Sequence impedance → 3×3 phase impedance via Kron reduction
        let r1 = line.r1 * len_km;
        let x1 = line.x1 * len_km;
        let r0 = line.r0 * len_km;
        let x0 = line.x0 * len_km;

        let r_self = (r0 + 2.0 * r1) / 3.0;
        let r_mut = (r0 - r1) / 3.0;
        let x_self = (x0 + 2.0 * x1) / 3.0;
        let x_mut = (x0 - x1) / 3.0;

        let mut z = [[0.0f64; 2]; 9];
        for i in 0..3 {
            for j in 0..3 {
                z[i * 3 + j] = if i == j {
                    [r_self, x_self]
                } else {
                    [r_mut, x_mut]
                };
            }
        }

        // Apply phase mask based on actual phase connections
        apply_phase_mask_to_z(&mut z, phase_mask);
        PhaseImpedanceMatrix { z }
    };

    // Compute shunt susceptance from capacitance matrix (π-model).
    //
    // DSS Cmatrix is in nF/length-unit (lower-triangular, same layout as rmatrix).
    // Total capacitance: C_total = Cmatrix × length [nF]
    // Shunt susceptance: B = 2π × f × C_total / 1e3 [µS]
    //   (dividing nF by 1e3 gives µF, then µF × ω = µS)
    //
    // For the BFS π-model, the total B is split equally at both ends.
    let freq = system_freq_hz;
    let omega = 2.0 * std::f64::consts::PI * freq;
    let b_shunt_us = if !line.cmatrix.is_empty() {
        let active = active_phase_indices(phase_mask);
        let c3 = expand_lower_tri_to_3x3(&line.cmatrix, &active);
        // Effective per-phase susceptance accounting for off-diagonal coupling.
        //
        // For balanced voltages V_b = V_a·e^{-j120°}, V_c = V_a·e^{+j120°}:
        //   I_shunt_a = j·(B[0][0]·V_a + B[0][1]·V_b + B[0][2]·V_c)
        //             = j·V_a·(B[0][0] + B[0][1]·e^{-j120°} + B[0][2]·e^{+j120°})
        //
        // The real part of the coefficient (the effective B_aa) is:
        //   B_eff[a] = B[0][0] - 0.5·(B[0][1] + B[0][2])
        //
        // This is exact for balanced voltages and includes inter-phase coupling.
        // Typically B_eff > B_self because off-diagonal terms are negative.
        let mut b = [0.0f64; 3];
        let other_phases: [(usize, usize); 3] = [(1, 2), (0, 2), (0, 1)];
        for ph in 0..3 {
            let (j, k) = other_phases[ph];
            let c_nf_self = c3[ph][ph] * len_km;
            let c_nf_j = c3[ph][j] * len_km;
            let c_nf_k = c3[ph][k] * len_km;
            let c_eff = c_nf_self - 0.5 * (c_nf_j + c_nf_k);
            b[ph] = omega * c_eff / 1e3; // µS
        }
        b
    } else if line.c1 > 0.0 || line.c0 > 0.0 {
        // Sequence capacitance → positive-sequence phase capacitance.
        // C_eff = C1 (positive-sequence) for balanced voltages.
        let c1_nf = line.c1 * len_km;
        let b_pos_seq = omega * c1_nf / 1e3; // µS
        [b_pos_seq, b_pos_seq, b_pos_seq]
    } else {
        [0.0; 3]
    };

    Some(ThreePhaseBranch {
        from_bus,
        to_bus,
        z_matrix,
        b_shunt_us,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Load → ThreePhaseLoad
// ─────────────────────────────────────────────────────────────────────────────

fn load_to_3ph_load(
    load: &super::objects::LoadData,
    bus_map: &HashMap<String, usize>,
) -> Option<ThreePhaseLoad> {
    if load.bus1.is_empty() {
        return None;
    }

    let bus1_lc = load.bus1.to_lowercase();
    let (bus_name, phase_mask) = parse_phase_spec(&bus1_lc);
    let bus_idx = *bus_map.get(bus_name)?;

    let is_delta = matches!(load.conn, WdgConn::Delta);
    let conn = if is_delta {
        LoadConnection::Delta
    } else {
        LoadConnection::Wye
    };

    // Map DSS load model to our ThreePhaseLoadModel enum
    let model = match load.model {
        super::objects::LoadModel::ConstantZ | super::objects::LoadModel::ConstantZ2 => {
            ThreePhaseLoadModel::ConstantZ
        }
        // DSS Model 4: P = constant, Q ∝ V² (constant P, quadratic Q)
        super::objects::LoadModel::ConstantPFixedQ => ThreePhaseLoadModel::ConstantPConstantXQ,
        // DSS Model 5: Constant current magnitude. |I| is constant so P ∝ |V|, Q ∝ |V|.
        super::objects::LoadModel::ConstantPConstantXQ => ThreePhaseLoadModel::ConstantI,
        _ => ThreePhaseLoadModel::ConstantPQ,
    };

    if is_delta {
        // Delta load: map bus phase spec to line-to-line pairs (AB/BC/CA).
        //   .1.2 → AB pair (pa/qa fields)
        //   .2.3 → BC pair (pb/qb fields)
        //   .3.1 → CA pair (pc/qc fields)
        //   .1.2.3 or no spec → balanced 3-phase delta (total/3 per pair)
        let (mut pa, mut qa) = (0.0, 0.0); // AB pair
        let (mut pb, mut qb) = (0.0, 0.0); // BC pair
        let (mut pc, mut qc) = (0.0, 0.0); // CA pair

        match phase_mask {
            0b011 => {
                // phases 1,2 → AB
                pa = load.kw;
                qa = load.kvar;
            }
            0b110 => {
                // phases 2,3 → BC
                pb = load.kw;
                qb = load.kvar;
            }
            0b101 => {
                // phases 3,1 → CA
                pc = load.kw;
                qc = load.kvar;
            }
            _ => {
                // 3-phase balanced delta (0b111 or any other)
                let kw3 = load.kw / 3.0;
                let kvar3 = load.kvar / 3.0;
                pa = kw3;
                qa = kvar3;
                pb = kw3;
                qb = kvar3;
                pc = kw3;
                qc = kvar3;
            }
        }

        Some(ThreePhaseLoad {
            bus_idx,
            pa_kw: pa,
            qa_kvar: qa,
            pb_kw: pb,
            qb_kvar: qb,
            pc_kw: pc,
            qc_kvar: qc,
            model,
            conn,
        })
    } else {
        // Wye load: distribute total power equally across active phases
        let n_active = phase_mask.count_ones() as f64;
        let kw_per_phase = if n_active > 0.0 {
            load.kw / n_active
        } else {
            0.0
        };
        let kvar_per_phase = if n_active > 0.0 {
            load.kvar / n_active
        } else {
            0.0
        };

        Some(ThreePhaseLoad {
            bus_idx,
            pa_kw: if phase_mask & 0b001 != 0 {
                kw_per_phase
            } else {
                0.0
            },
            qa_kvar: if phase_mask & 0b001 != 0 {
                kvar_per_phase
            } else {
                0.0
            },
            pb_kw: if phase_mask & 0b010 != 0 {
                kw_per_phase
            } else {
                0.0
            },
            qb_kvar: if phase_mask & 0b010 != 0 {
                kvar_per_phase
            } else {
                0.0
            },
            pc_kw: if phase_mask & 0b100 != 0 {
                kw_per_phase
            } else {
                0.0
            },
            qc_kvar: if phase_mask & 0b100 != 0 {
                kvar_per_phase
            } else {
                0.0
            },
            model,
            conn,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Capacitor → ThreePhaseLoad (negative Q injection)
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a DSS Capacitor to a ThreePhaseLoad with negative kVAr (reactive injection).
///
/// Capacitors inject reactive power (Q < 0 in load convention), reducing the reactive
/// current drawn from the source and improving voltage profile.
fn capacitor_to_3ph_load(
    cap: &super::objects::CapacitorData,
    bus_map: &HashMap<String, usize>,
) -> Option<ThreePhaseLoad> {
    if cap.bus1.is_empty() {
        return None;
    }

    let bus1_lc = cap.bus1.to_lowercase();
    let (bus_name, phase_mask_raw) = parse_phase_spec(&bus1_lc);
    let bus_idx = *bus_map.get(bus_name)?;

    // If bus has no phase spec, use the capacitor's phases count to determine mask
    let phase_mask = if phase_mask_raw == 0b111 && cap.phases < 3 {
        // Single-phase cap with no explicit phase: default to all-phases
        0b111u8
    } else {
        phase_mask_raw
    };

    let n_active = phase_mask.count_ones().max(1) as f64;
    // Total kVAr is sum of all steps (all steps assumed energized)
    let total_kvar = cap.total_kvar();
    // Negative kVAr = reactive power injection (capacitor reduces inductive current)
    let kvar_per_phase = -total_kvar / n_active;

    let qa_kvar = if phase_mask & 0b001 != 0 {
        kvar_per_phase
    } else {
        0.0
    };
    let qb_kvar = if phase_mask & 0b010 != 0 {
        kvar_per_phase
    } else {
        0.0
    };
    let qc_kvar = if phase_mask & 0b100 != 0 {
        kvar_per_phase
    } else {
        0.0
    };

    Some(ThreePhaseLoad {
        bus_idx,
        pa_kw: 0.0,
        qa_kvar,
        pb_kw: 0.0,
        qb_kvar,
        pc_kw: 0.0,
        qc_kvar,
        // Capacitors are impedance elements: Q ∝ V² (constant-Z behavior).
        // The kVAr values above are at rated voltage; actual Q scales with V².
        model: ThreePhaseLoadModel::ConstantZ,
        conn: LoadConnection::Wye,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Generator / PV / Storage → ThreePhaseLoad (negative-P injection = DER)
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a DSS Generator to a ThreePhaseLoad with negative kW (active power injection).
fn generator_to_3ph_load(
    gendata: &super::objects::GeneratorData,
    bus_map: &HashMap<String, usize>,
) -> Option<ThreePhaseLoad> {
    if gendata.bus1.is_empty() {
        return None;
    }

    let bus1_lc = gendata.bus1.to_lowercase();
    let (bus_name, phase_mask) = parse_phase_spec(&bus1_lc);
    let bus_idx = *bus_map.get(bus_name)?;

    let n_active = phase_mask.count_ones().max(1) as f64;
    let kw_per_phase = -gendata.kw / n_active; // negative = injection
    let kvar_per_phase = -gendata.kvar / n_active;

    Some(ThreePhaseLoad {
        bus_idx,
        pa_kw: if phase_mask & 0b001 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qa_kvar: if phase_mask & 0b001 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        pb_kw: if phase_mask & 0b010 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qb_kvar: if phase_mask & 0b010 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        pc_kw: if phase_mask & 0b100 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qc_kvar: if phase_mask & 0b100 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        model: ThreePhaseLoadModel::ConstantPQ,
        conn: LoadConnection::Wye,
    })
}

/// Convert a DSS PvSystem to a ThreePhaseLoad with negative kW injection.
///
/// Power output = pmpp × irradiance × pf (default pf=1.0, irradiance=1.0).
fn pvsystem_to_3ph_load(
    pv: &super::objects::PvSystemData,
    bus_map: &HashMap<String, usize>,
) -> Option<ThreePhaseLoad> {
    if pv.bus1.is_empty() {
        return None;
    }

    let bus1_lc = pv.bus1.to_lowercase();
    let (bus_name, phase_mask) = parse_phase_spec(&bus1_lc);
    let bus_idx = *bus_map.get(bus_name)?;

    // PV output: Pmpp × irradiance (clipped to kVA rating)
    let p_out = (pv.pmpp * pv.irradiance).min(pv.kva);
    let q_out = if pv.pf.abs() < 1.0 - 1e-6 {
        p_out * (1.0 - pv.pf * pv.pf).sqrt() / pv.pf.abs() * pv.pf.signum()
    } else {
        0.0
    };

    let n_active = phase_mask.count_ones().max(1) as f64;
    let kw_per_phase = -p_out / n_active;
    let kvar_per_phase = -q_out / n_active;

    Some(ThreePhaseLoad {
        bus_idx,
        pa_kw: if phase_mask & 0b001 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qa_kvar: if phase_mask & 0b001 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        pb_kw: if phase_mask & 0b010 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qb_kvar: if phase_mask & 0b010 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        pc_kw: if phase_mask & 0b100 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qc_kvar: if phase_mask & 0b100 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        model: ThreePhaseLoadModel::ConstantPQ,
        conn: LoadConnection::Wye,
    })
}

/// Convert a DSS Storage to a ThreePhaseLoad with negative kW injection (discharging).
///
/// Storage is modeled as a constant-power DER at its rated dispatch.
fn storage_to_3ph_load(
    storage: &super::objects::StorageData,
    bus_map: &HashMap<String, usize>,
) -> Option<ThreePhaseLoad> {
    if storage.bus1.is_empty() {
        return None;
    }

    let bus1_lc = storage.bus1.to_lowercase();
    let (bus_name, phase_mask) = parse_phase_spec(&bus1_lc);
    let bus_idx = *bus_map.get(bus_name)?;

    // Default: discharging at rated kW
    let p_out = storage.kw_rated;
    let q_out = if storage.pf.abs() < 1.0 - 1e-6 {
        p_out * (1.0 - storage.pf * storage.pf).sqrt() / storage.pf.abs() * storage.pf.signum()
    } else {
        0.0
    };

    let n_active = phase_mask.count_ones().max(1) as f64;
    let kw_per_phase = -p_out / n_active;
    let kvar_per_phase = -q_out / n_active;

    Some(ThreePhaseLoad {
        bus_idx,
        pa_kw: if phase_mask & 0b001 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qa_kvar: if phase_mask & 0b001 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        pb_kw: if phase_mask & 0b010 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qb_kvar: if phase_mask & 0b010 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        pc_kw: if phase_mask & 0b100 != 0 {
            kw_per_phase
        } else {
            0.0
        },
        qc_kvar: if phase_mask & 0b100 != 0 {
            kvar_per_phase
        } else {
            0.0
        },
        model: ThreePhaseLoadModel::ConstantPQ,
        conn: LoadConnection::Wye,
    })
}

/// Convert a DSS Fault to a low-impedance ThreePhaseBranch.
///
/// A Fault element is a short-circuit path between two buses (or bus to ground).
fn fault_to_3ph_branch(
    fault: &super::objects::FaultData,
    bus_map: &HashMap<String, usize>,
) -> Option<ThreePhaseBranch> {
    if fault.bus1.is_empty() || fault.bus2.is_empty() {
        return None; // bus-to-ground faults not modeled as branches
    }

    let bus1_lc = fault.bus1.to_lowercase();
    let bus2_lc = fault.bus2.to_lowercase();
    let (from_name, phase_mask) = parse_phase_spec(&bus1_lc);
    let (to_name, _) = parse_phase_spec(&bus2_lc);
    let from_bus = *bus_map.get(from_name)?;
    let to_bus = *bus_map.get(to_name)?;

    if from_bus == to_bus {
        return None;
    }

    let r = fault.r.max(1e-6); // prevent zero impedance
    let mut z = [[0.0f64; 2]; 9];
    for ph in 0..3 {
        z[ph * 3 + ph] = [r, 0.0];
    }
    apply_phase_mask_to_z(&mut z, phase_mask);

    Some(ThreePhaseBranch {
        from_bus,
        to_bus,
        z_matrix: PhaseImpedanceMatrix { z },
        b_shunt_us: [0.0; 3], // switches have no charging
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Reactor → ThreePhaseBranch (series reactor = line with R+jX)
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a 2-bus DSS Reactor to a ThreePhaseBranch.
///
/// A 2-bus reactor (bus1 and bus2 both specified) is a series element with
/// impedance R + jX Ω. This is topologically equivalent to a short line.
/// IEEE 8500 uses a reactor to connect SourceBus to the substation HV bus.
fn reactor_to_3ph_branch(
    reactor: &super::objects::ReactorData,
    bus_map: &HashMap<String, usize>,
) -> Option<ThreePhaseBranch> {
    if reactor.bus1.is_empty() || reactor.bus2.is_empty() {
        return None; // 1-bus (shunt) reactor — skip
    }

    let bus1_lc = reactor.bus1.to_lowercase();
    let bus2_lc = reactor.bus2.to_lowercase();
    let (from_name, phase_mask) = parse_phase_spec(&bus1_lc);
    let (to_name, _) = parse_phase_spec(&bus2_lc);
    let from_bus = *bus_map.get(from_name)?;
    let to_bus = *bus_map.get(to_name)?;

    if from_bus == to_bus {
        return None;
    }

    let r = reactor.r;
    let x = reactor.x;

    // Build balanced diagonal z_matrix
    let mut z = [[0.0f64; 2]; 9];
    for ph in 0..3 {
        z[ph * 3 + ph] = [r, x];
    }
    apply_phase_mask_to_z(&mut z, phase_mask);

    Some(ThreePhaseBranch {
        from_bus,
        to_bus,
        z_matrix: PhaseImpedanceMatrix { z },
        b_shunt_us: [0.0; 3], // reactors have no charging
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Transformer → ThreePhaseTransformer
// ─────────────────────────────────────────────────────────────────────────────

fn xfmr_to_3ph_transformer(
    xfmr: &super::objects::TransformerData,
    bus_map: &HashMap<String, usize>,
) -> Option<ThreePhaseTransformer> {
    if xfmr.buses.len() < 2 || xfmr.kvs.len() < 2 {
        return None;
    }

    let bus0_lc = xfmr.buses[0].to_lowercase();
    let bus1_lc = xfmr.buses[1].to_lowercase();
    // Parse phase spec from winding 1 bus to determine active phases
    let (from_name, phase_mask) = parse_phase_spec(&bus0_lc);
    let (to_name, _) = parse_phase_spec(&bus1_lc);
    let from_bus = *bus_map.get(from_name)?;
    let to_bus = *bus_map.get(to_name)?;

    if from_bus == to_bus {
        return None;
    }

    let kv_primary = xfmr.kvs[0];
    let kv_secondary = xfmr.kvs[1];
    // If either kv is zero/missing (e.g., "like=" syntax without explicit kvs),
    // use 1:1 turns ratio.
    let turns_ratio = if kv_primary > 1e-6 && kv_secondary > 1e-6 {
        kv_primary / kv_secondary
    } else {
        1.0
    };
    // Use primary kv for impedance base, defaulting to 4.16 kV if missing
    let kv_primary = if kv_primary > 1e-6 { kv_primary } else { 4.16 };

    let phase_shift_rad = match (xfmr.conns.first(), xfmr.conns.get(1)) {
        (Some(WdgConn::Delta), Some(WdgConn::Wye | WdgConn::Ln)) => (-30.0_f64).to_radians(),
        (Some(WdgConn::Wye | WdgConn::Ln), Some(WdgConn::Delta)) => 30.0_f64.to_radians(),
        _ => 0.0,
    };

    let kva_rating = xfmr.kvas.first().copied().unwrap_or(1000.0).max(1.0);

    // Series impedance referred to the SECONDARY side.
    //
    // The BFS forward sweep uses secondary-side currents (I = conj(S/V_sec)).
    // For the correct voltage equation V_sec = V_pri/tr - Z_sec * I_sec / 1000,
    // we must store the leakage impedance referred to the secondary winding.
    //
    //   Z_base_secondary = (kV_secondary)^2 * 1000 / kVA   [Ω]
    //   Z_base_primary   = (kV_primary)^2   * 1000 / kVA   [Ω]
    //   Z_secondary = Z_primary / tr^2  (tr = kV_pri / kV_sec)
    //
    // Using the secondary base directly gives the same result more simply:
    //   Z_secondary = %z / 100 * Z_base_secondary
    //
    // For the %R and Xhl values (which are in percent of the transformer rating,
    // invariant of which side they are referred to), this is the correct formula.
    let kv_sec = if kv_secondary > 1e-6 {
        kv_secondary
    } else {
        kv_primary
    };
    let z_base_secondary = kv_sec * kv_sec * 1000.0 / kva_rating;
    // Total copper loss percentage (referred to rated kVA):
    //
    // DSS allows two ways to specify winding resistance:
    //   a) %r per winding (pct_rs array): total = sum of all windings
    //   b) %LoadLoss: directly specifies total copper loss %
    //
    // If %LoadLoss was given explicitly (non-zero), use it directly.
    // Otherwise, sum all winding %r values (default pct_r = 0.5% per winding,
    // so two windings → 1.0% total).
    let r_total_pct = if xfmr.pct_load_loss > 0.0 {
        // %LoadLoss = total copper loss % of rated kVA
        xfmr.pct_load_loss
    } else {
        // Sum of per-winding %r values (each winding contributes independently)
        xfmr.pct_rs.iter().sum::<f64>()
    };
    let r_ohm = r_total_pct / 100.0 * z_base_secondary;
    let x_ohm = xfmr.xhl / 100.0 * z_base_secondary;

    // Check if windings are delta-connected.
    let pri_delta = xfmr
        .conns
        .first()
        .is_some_and(|c| matches!(c, WdgConn::Delta));
    let sec_delta = xfmr
        .conns
        .get(1)
        .is_some_and(|c| matches!(c, WdgConn::Delta));
    let is_delta_delta = pri_delta && sec_delta;

    // Build z_matrix.  For delta-connected windings, the leakage impedance
    // acts on winding (line-to-line) currents, not line currents.  The
    // equivalent per-phase impedance seen from the line side is coupled:
    //   Z_line = Z_winding × [[2/3, −1/3, −1/3],
    //                         [−1/3, 2/3, −1/3],
    //                         [−1/3, −1/3, 2/3]]
    // For wye windings, the per-phase impedance is simply diagonal.
    let has_delta_winding = pri_delta || sec_delta;
    let mut z_matrix = if has_delta_winding && phase_mask == 0b111 {
        let mut z = [[0.0f64; 2]; 9];
        for i in 0..3usize {
            for j in 0..3usize {
                let scale = if i == j { 2.0 / 3.0 } else { -1.0 / 3.0 };
                z[i * 3 + j] = [r_ohm * scale, x_ohm * scale];
            }
        }
        PhaseImpedanceMatrix { z }
    } else {
        let mut z = PhaseImpedanceMatrix::balanced(r_ohm, x_ohm);
        if phase_mask != 0b111 {
            apply_phase_mask_to_z(&mut z.z, phase_mask);
        }
        z
    };

    // No-load (core) losses and magnetizing current as shunt on secondary side.
    let kv_ln_sec = kv_sec / 3.0_f64.sqrt();
    let v_ln2 = kv_ln_sec * kv_ln_sec;
    let g_mag_siemens = if xfmr.pct_no_load_loss > 0.0 && v_ln2 > 1e-12 {
        (kva_rating * xfmr.pct_no_load_loss / 100.0 / 3.0) / (v_ln2 * 1000.0)
    } else {
        0.0
    };
    let b_mag_siemens = if xfmr.pct_imag > 0.0 && v_ln2 > 1e-12 {
        -(kva_rating * xfmr.pct_imag / 100.0 / 3.0) / (v_ln2 * 1000.0)
    } else {
        0.0
    };

    Some(ThreePhaseTransformer {
        from_bus,
        to_bus,
        z_matrix,
        turns_ratio,
        phase_shift_rad,
        rated_kva: kva_rating,
        is_delta_delta,
        g_mag_siemens,
        b_mag_siemens,
        regulators: [None, None, None], // populated later by regulator linking pass
        ganged_regulator: false,        // set true for 3-phase ganged regulators
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Main converter
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// Transformer deduplication
// ─────────────────────────────────────────────────────────────────────────────

/// Merge multiple single-phase transformers connecting the same bus pair into
/// one 3-phase transformer.
///
/// When DSS defines 3 single-phase regulators (e.g., 650.1→RG60.1, 650.2→RG60.2,
/// 650.3→RG60.3), they all map to the same from_bus/to_bus after phase stripping.
/// We merge them into one 3-phase transformer with combined z_matrix.
fn deduplicate_transformers(
    transformers: Vec<ThreePhaseTransformer>,
) -> Vec<ThreePhaseTransformer> {
    use std::collections::BTreeMap;

    // Key: (from_bus, to_bus) — order-normalized so (a,b) and (b,a) are the same
    let mut groups: BTreeMap<(usize, usize), Vec<ThreePhaseTransformer>> = BTreeMap::new();

    for xfmr in transformers {
        let key = if xfmr.from_bus <= xfmr.to_bus {
            (xfmr.from_bus, xfmr.to_bus)
        } else {
            (xfmr.to_bus, xfmr.from_bus)
        };
        groups.entry(key).or_default().push(xfmr);
    }

    let mut result = Vec::new();
    for (_, group) in groups {
        if group.len() == 1 {
            result.push(
                group
                    .into_iter()
                    .next()
                    .expect("group.len() == 1 so iter has exactly one element"),
            );
        } else {
            // Merge: use from/to from first entry; combine z_matrices by OR-ing active phases
            let first = &group[0];
            let mut merged_z = [[0.0f64; 2]; 9];
            let mut turns_ratio = first.turns_ratio;
            let mut phase_shift_rad = first.phase_shift_rad;
            let mut rated_kva = 0.0;

            // Count how many component transformers have each phase active.
            // For open-delta (2 components), a phase active in both is the
            // "common" node (jumper) and should have near-zero impedance.
            let mut phase_count = [0u32; 3];

            for xfmr in &group {
                rated_kva += xfmr.rated_kva;
                // Merge z_matrix: for each active diagonal (non-1e6), take it
                for (ph, count) in phase_count.iter_mut().enumerate() {
                    let diag = ph * 3 + ph;
                    let self_r = xfmr.z_matrix.z[diag][0];
                    // If this phase is active in this transformer (not blocked)
                    if self_r < 1e5 {
                        merged_z[diag] = xfmr.z_matrix.z[diag];
                        *count += 1;
                    }
                }
                // Average turns ratio (should be identical for parallel transformers)
                turns_ratio = (turns_ratio + xfmr.turns_ratio) / 2.0;
                // Use most significant phase shift
                if xfmr.phase_shift_rad.abs() > phase_shift_rad.abs() {
                    phase_shift_rad = xfmr.phase_shift_rad;
                }
            }

            // For open-delta banks (exactly 2 single-phase transformers),
            // a phase that appears in both is the common/jumper phase.
            // Set its impedance to near-zero (the physical jumper has ~1 mΩ).
            if group.len() == 2 {
                for (ph, &count) in phase_count.iter().enumerate() {
                    if count == 2 {
                        let diag = ph * 3 + ph;
                        merged_z[diag] = [1e-3, 0.0]; // ~1 mΩ jumper
                    }
                }
            }

            // Fill any remaining zero diagonals (truly blocked phases) with 1e6
            for (ph, _) in phase_count.iter().enumerate() {
                let diag = ph * 3 + ph;
                if merged_z[diag][0] == 0.0 && merged_z[diag][1] == 0.0 {
                    merged_z[diag] = [1e6, 0.0];
                }
            }

            // Merge per-phase regulators: each component transformer contributes its
            // regulator to its active phases.  This correctly handles open-delta banks
            // where two single-phase regulators control different phases and one phase
            // passes through a jumper (no regulator).
            let mut merged_regs: [Option<RegulatorControl>; 3] = [None, None, None];
            for xfmr in &group {
                for (ph, reg) in merged_regs.iter_mut().enumerate() {
                    let diag = ph * 3 + ph;
                    // This transformer's phase is active if impedance is not blocked
                    if xfmr.z_matrix.z[diag][0] < 1e5 && xfmr.regulators[ph].is_some() {
                        *reg = xfmr.regulators[ph].clone();
                    }
                }
            }
            // Delta-delta if any component was delta-delta
            let merged_dd = group.iter().any(|x| x.is_delta_delta);
            let merged_g_mag: f64 = group.iter().map(|x| x.g_mag_siemens).sum();
            let merged_b_mag: f64 = group.iter().map(|x| x.b_mag_siemens).sum();
            result.push(ThreePhaseTransformer {
                from_bus: first.from_bus,
                to_bus: first.to_bus,
                z_matrix: PhaseImpedanceMatrix { z: merged_z },
                turns_ratio,
                phase_shift_rad,
                rated_kva,
                is_delta_delta: merged_dd,
                g_mag_siemens: merged_g_mag,
                b_mag_siemens: merged_b_mag,
                regulators: merged_regs,
                ganged_regulator: false, // merged single-phase units → independent
            });
        }
    }
    result
}

/// Parse a DSS file and convert to a three-phase distribution network.
///
/// Returns `(network, bus_names)` where `bus_names[i]` is the name of bus `i`.
pub fn dss_to_3ph_dist(path: &Path) -> Result<(ThreePhaseNetwork, Vec<String>), DssParseError> {
    let mut catalog = build_dss_catalog(path)?;

    let circ = catalog.circuit.as_ref().ok_or(DssParseError::NoCircuit)?;

    // Source voltage: circuit base_kv is line-to-line (nominal).
    // circ.pu is the per-unit setpoint (e.g. 1.05 for IEEE 34).
    let source_voltage_kv = circ.base_kv;
    let source_pu = circ.pu;

    let (bus_map, bus_names) = build_3ph_bus_map(&catalog);
    let n_buses = bus_names.len();

    // Source bus is always index 0 (the circuit bus)
    let source_bus = 0usize;

    // We need to re-borrow catalog after bus_map is built
    // Collect elements into local vecs to avoid borrow conflicts
    let mut branches = Vec::new();
    let mut loads = Vec::new();
    let mut transformers = Vec::new();

    // System frequency from the DSS circuit object (default 60 Hz).
    let system_freq_hz = catalog
        .circuit
        .as_ref()
        .map(|c| c.frequency)
        .filter(|f| *f > 0.0)
        .unwrap_or(60.0);

    // Clone objects to avoid borrow conflict
    let objects: Vec<DssObject> = catalog.objects.drain(..).collect();

    for obj in &objects {
        match obj {
            DssObject::Line(line) => {
                if let Some(br) = line_to_3ph_branch(line, &bus_map, system_freq_hz) {
                    branches.push(br);
                }
            }
            DssObject::Load(load) => {
                if let Some(ld) = load_to_3ph_load(load, &bus_map) {
                    loads.push(ld);
                }
            }
            DssObject::Capacitor(cap) => {
                // Model capacitors as negative-Q loads (reactive power injection).
                if let Some(ld) = capacitor_to_3ph_load(cap, &bus_map) {
                    loads.push(ld);
                }
            }
            DssObject::Transformer(xfmr) => {
                if let Some(tx) = xfmr_to_3ph_transformer(xfmr, &bus_map) {
                    transformers.push(tx);
                }
            }
            DssObject::AutoTrans(at) => {
                if let Some(tx) = xfmr_to_3ph_transformer(&at.transformer, &bus_map) {
                    transformers.push(tx);
                }
            }
            DssObject::Reactor(reactor) => {
                // 2-bus reactors are series elements (like lines)
                if let Some(br) = reactor_to_3ph_branch(reactor, &bus_map) {
                    branches.push(br);
                }
            }
            DssObject::Generator(gendata) => {
                // Generators inject P+Q (modeled as negative loads)
                if let Some(ld) = generator_to_3ph_load(gendata, &bus_map) {
                    loads.push(ld);
                }
            }
            DssObject::PvSystem(pv) => {
                // PV systems inject P (modeled as negative loads)
                if let Some(ld) = pvsystem_to_3ph_load(pv, &bus_map) {
                    loads.push(ld);
                }
            }
            DssObject::Storage(st) => {
                // Storage injects P when discharging (modeled as negative loads)
                if let Some(ld) = storage_to_3ph_load(st, &bus_map) {
                    loads.push(ld);
                }
            }
            DssObject::Fault(fault) => {
                // Faults are low-impedance branches between buses
                if let Some(br) = fault_to_3ph_branch(fault, &bus_map) {
                    branches.push(br);
                }
            }
            _ => {}
        }
    }

    // ── Regulator control linking ─────────────────────────────────────────────
    // For each VoltageRegulator (RegControl) object, find the transformer it controls
    // and attach the control parameters so the BFS can adjust the tap each iteration.
    //
    // Strategy: build name→index map over the flat transformer list (same order as
    // the DSS Transformer objects were processed above), then for each RegControl
    // link it to the matching transformer.
    {
        // Build name → ThreePhaseTransformer index (same order as xfmr_to_3ph_transformer calls)
        let mut xfmr_by_name: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut tx_idx = 0usize;
        for obj in &objects {
            match obj {
                DssObject::Transformer(xfmr) => {
                    xfmr_by_name.insert(xfmr.name.to_lowercase(), tx_idx);
                    tx_idx += 1;
                }
                DssObject::AutoTrans(at) => {
                    xfmr_by_name.insert(at.transformer.name.to_lowercase(), tx_idx);
                    tx_idx += 1;
                }
                _ => {}
            }
        }

        // Build a lookup from transformer name → original TransformerData
        // so we can determine connection type and bus spec for delta regulators.
        let xfmr_data_by_name: std::collections::HashMap<String, &super::objects::TransformerData> =
            objects
                .iter()
                .filter_map(|o| {
                    if let DssObject::Transformer(td) = o {
                        Some((td.name.to_lowercase(), td))
                    } else {
                        None
                    }
                })
                .collect();

        for obj in &objects {
            if let DssObject::VoltageRegulator(reg) = obj {
                let xfmr_name = reg.transformer.to_lowercase();
                if let Some(&idx) = xfmr_by_name.get(&xfmr_name)
                    && idx < transformers.len()
                {
                    // Determine if this is a delta-connected single-phase regulator.
                    // For delta regulators, the PT measures line-to-line voltage and
                    // only the first-listed terminal (regulated phase) gets the tap.
                    let orig = xfmr_data_by_name.get(&xfmr_name);
                    let is_single_phase_delta = orig.is_some_and(|td| {
                        td.phases == 1 && td.conns.iter().any(|c| matches!(c, WdgConn::Delta))
                    });

                    // For single-phase delta regulators, parse the bus spec to find
                    // (regulated_phase, reference_phase).  E.g., bus=799.1.2 →
                    // regulated=phase 0 (node 1), reference=phase 1 (node 2).
                    let delta_pair = if is_single_phase_delta {
                        orig.and_then(|td| {
                            let bus_spec = td.buses.first()?;
                            parse_delta_pair(bus_spec)
                        })
                    } else {
                        None
                    };

                    // vreg is the target voltage in V on a 120V PT secondary basis.
                    let v_set_pu = reg.vreg / 120.0;
                    let ctrl = RegulatorControl {
                        v_set_pu,
                        tap_max_pu: 0.10,               // ±10% standard range
                        tap_step_pu: 5.0 / 8.0 / 100.0, // 5/8% per step = 0.00625
                        // R and X are in volts on 120V base (LDC settings)
                        r_ldc_pu: reg.r,
                        x_ldc_pu: reg.x,
                        ct_prim: reg.ct_prim,   // CT primary rating in amps
                        pt_ratio: reg.pt_ratio, // PT ratio (primary V / 120V)
                        band_v: reg.band,       // bandwidth in V on 120V base
                        delta_pair,
                    };

                    if let Some((reg_ph, _ref_ph)) = delta_pair {
                        // Single-phase delta: assign regulator ONLY to the regulated
                        // phase (first terminal).  The reference phase (second terminal)
                        // is the unregulated return path (jumper/common node).
                        if transformers[idx].regulators[reg_ph].is_none() {
                            transformers[idx].regulators[reg_ph] = Some(ctrl);
                        }
                    } else {
                        // Wye-connected or 3-phase: assign to all active phases.
                        let is_3ph_ganged = orig.is_some_and(|td| td.phases >= 2);
                        for ph in 0..3usize {
                            let diag = ph * 3 + ph;
                            if transformers[idx].z_matrix.z[diag][0] < 1e5
                                && transformers[idx].regulators[ph].is_none()
                            {
                                transformers[idx].regulators[ph] = Some(ctrl.clone());
                            }
                        }
                        if is_3ph_ganged {
                            transformers[idx].ganged_regulator = true;
                        }
                    }
                }
            }
        }
    }

    // Deduplicate transformers with same from_bus/to_bus pair.
    // Multiple single-phase transformers connecting the same bus pair (e.g., 3 single-phase
    // regulators at 650.1/2/3 → RG60.1/2/3) should be merged into one 3-phase transformer.
    let mut transformers = deduplicate_transformers(transformers);

    // Absorb branches (lines/jumpers) that connect the same bus pair as an existing
    // transformer.  This handles IEEE 37's open-delta regulator topology where two
    // single-phase regulators + a jumper line all connect buses 799↔799r.
    // Without this, the BFS may discover 799r through the jumper (phase B only),
    // making the transformer a back-edge and losing phases A and C.
    {
        use std::collections::HashSet;
        let xfmr_pairs: HashSet<(usize, usize)> = transformers
            .iter()
            .map(|x| {
                let a = x.from_bus.min(x.to_bus);
                let b = x.from_bus.max(x.to_bus);
                (a, b)
            })
            .collect();
        // Remove branches that are parallel with a transformer on the same bus pair.
        // The transformer already provides phase connectivity (and regulator control).
        // The branch (typically a zero-impedance jumper) is redundant.
        branches.retain(|br| {
            let a = br.from_bus.min(br.to_bus);
            let b = br.from_bus.max(br.to_bus);
            !xfmr_pairs.contains(&(a, b))
        });
    }

    // ── Feeder-head detection ─────────────────────────────────────────────────
    // DSS circuits define a transmission-level VSource (e.g., 115 kV) feeding a
    // step-down substation transformer into the distribution feeder (e.g., 4.16 kV).
    // That substation transformer is NOT part of the distribution network: it just
    // sets the feeder-head voltage.  We remove it and re-root the BFS at its
    // secondary bus, updating source_voltage_kv to the distribution level.
    //
    // The algorithm walks from source_bus through:
    //   1. Step-down transformers (turns_ratio > 1.1) — removes them and steps voltage
    //   2. Series branches (reactors/lines) connecting to exactly one other bus — absorbs
    //      them to bridge the gap to the next step-down transformer
    //
    // This handles IEEE 8500's topology: SourceBus → (Reactor) → HVMV_Sub_HSB →
    // (Transformer 115/12.47 kV) → distribution bus.
    let mut source_bus = source_bus;
    let mut source_voltage_kv = source_voltage_kv;
    let mut source_impedance: Option<PhaseImpedanceMatrix> = None;
    loop {
        // Step 1: Look for a step-down transformer directly on source_bus
        let xfmr_pos = transformers.iter().position(|xfmr| {
            (xfmr.from_bus == source_bus || xfmr.to_bus == source_bus) && xfmr.turns_ratio > 1.1
        });
        if let Some(p) = xfmr_pos {
            let sub = transformers.remove(p);
            source_bus = if sub.from_bus == source_bus {
                sub.to_bus
            } else {
                sub.from_bus
            };
            source_voltage_kv /= sub.turns_ratio;

            // For delta-delta transformers, the per-phase Thevenin impedance
            // requires a coupled 3×3 matrix.  A delta winding distributes each
            // line current across two phase windings, producing mutual coupling:
            //   Z_phase = Z_leakage × [[2/3, −1/3, −1/3],
            //                          [−1/3, 2/3, −1/3],
            //                          [−1/3, −1/3, 2/3]]
            // For wye-connected windings, the per-phase impedance is simply the
            // diagonal leakage impedance (no coupling).
            if sub.is_delta_delta {
                let mut coupled = [[0.0f64; 2]; 9];
                for i in 0..3usize {
                    let z_diag = sub.z_matrix.z[i * 3 + i]; // [R, X] of this phase
                    for j in 0..3usize {
                        let scale = if i == j { 2.0 / 3.0 } else { -1.0 / 3.0 };
                        coupled[i * 3 + j] = [z_diag[0] * scale, z_diag[1] * scale];
                    }
                }
                source_impedance = Some(PhaseImpedanceMatrix { z: coupled });
            } else {
                source_impedance = Some(sub.z_matrix);
            }
            continue;
        }

        // Step 2: Look for a branch (reactor/line) connecting source_bus to another
        // bus. This bridges gaps like SourceBus → (Reactor) → HV_bus where the
        // step-down transformer is on HV_bus, not on source_bus directly.
        let branch_pos = branches
            .iter()
            .position(|br| br.from_bus == source_bus || br.to_bus == source_bus);
        if let Some(p) = branch_pos {
            // Only absorb if there IS a step-down transformer on the other end;
            // otherwise this is a real distribution branch — don't remove it.
            let br = &branches[p];
            let other_bus = if br.from_bus == source_bus {
                br.to_bus
            } else {
                br.from_bus
            };
            let has_stepdown = transformers.iter().any(|xfmr| {
                (xfmr.from_bus == other_bus || xfmr.to_bus == other_bus) && xfmr.turns_ratio > 1.1
            });
            if has_stepdown {
                branches.remove(p);
                source_bus = other_bus;
                // Voltage doesn't change across a branch (same zone)
                continue;
            }
        }

        break;
    }

    let network = ThreePhaseNetwork {
        n_buses,
        source_bus,
        source_voltage_kv,
        source_pu,
        branches,
        loads,
        transformers,
        source_impedance,
    };

    Ok((network, bus_names))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_phase_spec_three_phase() {
        let (name, mask) = parse_phase_spec("650.1.2.3");
        assert_eq!(name, "650");
        assert_eq!(mask, 0b111);
    }

    #[test]
    fn test_parse_phase_spec_single_phase_a() {
        let (name, mask) = parse_phase_spec("node.1");
        assert_eq!(name, "node");
        assert_eq!(mask, 0b001);
    }

    #[test]
    fn test_parse_phase_spec_single_phase_b() {
        let (name, mask) = parse_phase_spec("node.2");
        assert_eq!(name, "node");
        assert_eq!(mask, 0b010);
    }

    #[test]
    fn test_parse_phase_spec_single_phase_c() {
        let (name, mask) = parse_phase_spec("node.3");
        assert_eq!(name, "node");
        assert_eq!(mask, 0b100);
    }

    #[test]
    fn test_parse_phase_spec_two_phase_ab() {
        let (name, mask) = parse_phase_spec("node.1.2");
        assert_eq!(name, "node");
        assert_eq!(mask, 0b011);
    }

    #[test]
    fn test_parse_phase_spec_no_dot() {
        let (name, mask) = parse_phase_spec("node");
        assert_eq!(name, "node");
        assert_eq!(mask, 0b111);
    }

    #[test]
    fn test_expand_lower_tri_3x3() {
        // Symmetric 3×3: diag=[1,5,9], lower=[4, 7, 8]
        let tri = [1.0, 2.0, 5.0, 3.0, 6.0, 9.0];
        let m = expand_lower_tri_3x3(&tri);
        assert_eq!(m[0][0], 1.0);
        assert_eq!(m[1][1], 5.0);
        assert_eq!(m[2][2], 9.0);
        assert_eq!(m[1][0], 2.0);
        assert_eq!(m[0][1], 2.0); // symmetrized
        assert_eq!(m[2][0], 3.0);
        assert_eq!(m[0][2], 3.0); // symmetrized
    }

    #[test]
    fn test_compile_recurses_into_child_base_dir() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("sub");
        std::fs::create_dir_all(&subdir).unwrap();

        std::fs::write(
            dir.path().join("main.dss"),
            "New Circuit.main basekv=12.47 bus1=source\nCompile sub/child.dss\n",
        )
        .unwrap();
        std::fs::write(subdir.join("child.dss"), "Compile grandchild.dss\n").unwrap();
        std::fs::write(
            subdir.join("grandchild.dss"),
            "New Load.Load1 bus1=load.1 kw=90 kvar=30\n",
        )
        .unwrap();

        let catalog = build_catalog_from_str(
            "New Circuit.main basekv=12.47 bus1=source\nCompile sub/child.dss\n",
            Some(dir.path()),
        )
        .expect("compile recursion should resolve nested child files");

        assert!(
            catalog
                .objects
                .iter()
                .any(|obj| matches!(obj, DssObject::Load(load) if load.name == "Load1")),
            "nested compile should include the grandchild load"
        );
    }
}
