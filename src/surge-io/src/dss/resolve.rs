// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Cross-reference resolution for DSS objects.
//!
//! 1. Resolve LineCode references → fill LineData impedance values.
//! 2. Resolve XfmrCode references → fill TransformerData.
//! 3. Resolve LineGeometry + WireData → compute impedance via Carson equations.
//! 4. Build bus name → bus number map from all referenced bus strings.

use std::collections::HashMap;

use super::objects::{DssCatalog, DssObject, LineCodeData, LineData};

/// Strip the phase specification from a DSS bus string.
/// e.g. `"650.1.2.3"` → `"650"`, `"rg60.1"` → `"rg60"`.
pub fn strip_phases(bus: &str) -> &str {
    bus.split('.').next().unwrap_or(bus)
}

/// Resolve LineCode references in all Line objects.
///
/// When a `Line` has a `linecode` property set, copy the impedance values
/// from the matching `LineCode` object into the line (if the line doesn't
/// already have explicit impedance values set).
pub fn resolve_linecodes(catalog: &mut DssCatalog) {
    // Collect line codes into a local map first to avoid borrow conflicts.
    let linecodes: HashMap<String, LineCodeData> = catalog
        .objects
        .iter()
        .filter_map(|o| {
            if let DssObject::LineCode(lc) = o {
                Some((lc.name.to_lowercase(), lc.clone()))
            } else {
                None
            }
        })
        .collect();

    for obj in &mut catalog.objects {
        if let DssObject::Line(line) = obj {
            if line.linecode.is_empty() {
                continue;
            }
            let key = line.linecode.to_lowercase();
            if let Some(lc) = linecodes.get(&key) {
                apply_linecode_to_line(line, lc);
            } else {
                tracing::warn!(
                    "Line.{}: linecode '{}' not found — using defaults",
                    line.name,
                    line.linecode
                );
            }
        }
    }
}

/// Copy impedance values from a LineCode into a Line (only overwrites zeros).
///
/// # Unit conversion
///
/// LineCode r/x values are stored in Ω/unit where "unit" is `lc.units` (e.g. Ω/mi).
/// We need to normalise them to Ω/km so that `line_to_3ph_branch` can later multiply
/// by the line length in km to get total Ω.
///
/// `to_km_factor()` converts a **distance** to km: `len_km = len_unit × factor`.
/// For **impedance per length** the direction is reversed:
///   Ω/km = (Ω/mi) × (1 mi / 1 km) = (Ω/mi) / 1.609344
///
/// In other words: Ω/km = Ω/unit / to_km_factor().
///
/// Example: rmatrix = 0.3465 Ω/mi, factor = 1.609344
///   → 0.3465 / 1.609344 ≈ 0.2153 Ω/km   (correct — impedance per km is less than per mile)
fn apply_linecode_to_line(line: &mut LineData, lc: &LineCodeData) {
    let factor = lc.units.to_km_factor();
    // Guard: if factor is essentially 1.0 (Km or None), dividing is a no-op.
    let inv_factor = if factor > 1e-12 { 1.0 / factor } else { 1.0 };

    if !lc.rmatrix.is_empty() && line.rmatrix.is_empty() {
        // Convert Ω/unit → Ω/km by dividing by the km-factor.
        line.rmatrix = lc.rmatrix.iter().map(|&v| v * inv_factor).collect();
    }
    if !lc.xmatrix.is_empty() && line.xmatrix.is_empty() {
        line.xmatrix = lc.xmatrix.iter().map(|&v| v * inv_factor).collect();
    }
    if !lc.cmatrix.is_empty() && line.cmatrix.is_empty() {
        // Convert nF/unit → nF/km (same factor as R/X).
        line.cmatrix = lc.cmatrix.iter().map(|&v| v * inv_factor).collect();
    }

    // Sequence values (only if no matrix and no explicit r1/x1 set).
    if line.rmatrix.is_empty() {
        if lc.r1 != 0.0 {
            line.r1 = lc.r1 * inv_factor;
        }
        if lc.x1 != 0.0 {
            line.x1 = lc.x1 * inv_factor;
        }
        if lc.r0 != 0.0 {
            line.r0 = lc.r0 * inv_factor;
        }
        if lc.x0 != 0.0 {
            line.x0 = lc.x0 * inv_factor;
        }
        if lc.c1 != 0.0 {
            line.c1 = lc.c1 * inv_factor;
        }
        if lc.c0 != 0.0 {
            line.c0 = lc.c0 * inv_factor;
        }
    }

    if line.phases == 3 && lc.phases != 0 {
        line.phases = lc.phases;
    }
}

/// Resolve XfmrCode references in Transformer objects.
pub fn resolve_xfmrcodes(catalog: &mut DssCatalog) {
    use super::objects::XfmrCodeData;

    let codes: HashMap<String, XfmrCodeData> = catalog
        .objects
        .iter()
        .filter_map(|o| {
            if let DssObject::XfmrCode(xc) = o {
                Some((xc.name.to_lowercase(), xc.clone()))
            } else {
                None
            }
        })
        .collect();

    for obj in &mut catalog.objects {
        if let DssObject::Transformer(xfmr) = obj {
            if xfmr.xfmrcode.is_empty() {
                continue;
            }
            let key = xfmr.xfmrcode.to_lowercase();
            if let Some(xc) = codes.get(&key) {
                // XfmrCode defines all electrical properties; the Transformer instance
                // only specifies buses (and optionally overrides specific properties).
                //
                // Apply ALL code properties: windings, phases, kvs, kvas, impedances,
                // connections, %Rs. This handles 3-winding center-tapped transformers
                // (IEEE 8500) where the code defines windings=3, kvs=[7.2,0.12,0.12].
                xfmr.windings = xc.windings;
                xfmr.phases = xc.phases;
                xfmr.xhl = xc.xhl;
                xfmr.xht = xc.xht;
                xfmr.xlt = xc.xlt;
                xfmr.pct_load_loss = xc.pct_load_loss;
                xfmr.pct_no_load_loss = xc.pct_no_load_loss;
                xfmr.pct_imag = xc.pct_imag;

                // Resize arrays to match the code's winding count
                let nw = xc.windings as usize;
                xfmr.buses.resize(nw, String::new());
                xfmr.conns.resize(nw, super::objects::WdgConn::Wye);
                xfmr.kvs.resize(nw, 0.0);
                xfmr.kvas.resize(nw, 0.0);
                xfmr.pct_rs.resize(nw, 0.5);
                xfmr.taps.resize(nw, 1.0);

                // Copy kv, kva, conns, %r from code
                for (i, &kv) in xc.kvs.iter().enumerate() {
                    if i < xfmr.kvs.len() {
                        xfmr.kvs[i] = kv;
                    }
                }
                for (i, &kva) in xc.kvas.iter().enumerate() {
                    if i < xfmr.kvas.len() {
                        xfmr.kvas[i] = kva;
                    }
                }
                for (i, conn) in xc.conns.iter().enumerate() {
                    if i < xfmr.conns.len() {
                        xfmr.conns[i] = conn.clone();
                    }
                }
                for (i, &r) in xc.pct_rs.iter().enumerate() {
                    if i < xfmr.pct_rs.len() {
                        xfmr.pct_rs[i] = r;
                    }
                }
            } else {
                tracing::warn!(
                    "Transformer.{}: xfmrcode '{}' not found",
                    xfmr.name,
                    xfmr.xfmrcode
                );
            }
        }
    }
}

/// Build a bus name → bus number mapping (1-based, sorted alphabetically).
///
/// Collects all bus names referenced in lines, transformers, loads, generators,
/// capacitors, reactors, and the circuit source bus.
pub fn build_bus_map(catalog: &DssCatalog) -> HashMap<String, u32> {
    let mut names: std::collections::BTreeSet<String> = Default::default();

    // Circuit source bus.
    if let Some(ref circ) = catalog.circuit {
        names.insert(circ.bus.to_lowercase());
    }

    for obj in &catalog.objects {
        match obj {
            DssObject::Line(l) => {
                if !l.bus1.is_empty() {
                    names.insert(strip_phases(&l.bus1.to_lowercase()).to_string());
                }
                if !l.bus2.is_empty() {
                    names.insert(strip_phases(&l.bus2.to_lowercase()).to_string());
                }
            }
            DssObject::Transformer(t) => {
                for b in &t.buses {
                    if !b.is_empty() {
                        names.insert(strip_phases(&b.to_lowercase()).to_string());
                    }
                }
            }
            DssObject::AutoTrans(a) => {
                for b in &a.transformer.buses {
                    if !b.is_empty() {
                        names.insert(strip_phases(&b.to_lowercase()).to_string());
                    }
                }
            }
            DssObject::Load(l) => {
                if !l.bus1.is_empty() {
                    names.insert(strip_phases(&l.bus1.to_lowercase()).to_string());
                }
            }
            DssObject::Generator(g) => {
                if !g.bus1.is_empty() {
                    names.insert(strip_phases(&g.bus1.to_lowercase()).to_string());
                }
            }
            DssObject::PvSystem(p) => {
                if !p.bus1.is_empty() {
                    names.insert(strip_phases(&p.bus1.to_lowercase()).to_string());
                }
            }
            DssObject::Storage(s) => {
                if !s.bus1.is_empty() {
                    names.insert(strip_phases(&s.bus1.to_lowercase()).to_string());
                }
            }
            DssObject::Capacitor(c) => {
                if !c.bus1.is_empty() {
                    names.insert(strip_phases(&c.bus1.to_lowercase()).to_string());
                }
            }
            DssObject::Reactor(r) => {
                if !r.bus1.is_empty() {
                    names.insert(strip_phases(&r.bus1.to_lowercase()).to_string());
                }
                // 2-terminal reactor: also add bus2 to create a network bus.
                if !r.bus2.is_empty() {
                    names.insert(strip_phases(&r.bus2.to_lowercase()).to_string());
                }
            }
            DssObject::VsConverter(v) => {
                if !v.bus.is_empty() {
                    names.insert(strip_phases(&v.bus.to_lowercase()).to_string());
                }
            }
            _ => {}
        }
    }

    names
        .into_iter()
        .enumerate()
        .map(|(i, name)| (name, (i + 1) as u32))
        .collect()
}
