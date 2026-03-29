// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Stress test for DYR round-trip: parse → write → parse → compare.
//!
//! Tests every model type, edge cases, and parameter fidelity.

use surge_io::psse::dyr::{dumps, loads};

use serde_json::Value;
use surge_network::dynamics::*;

const TOL: f64 = 1e-9;

/// Helper: compare two f64 values within tolerance.
fn close(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let diff = (a - b).abs();
    // Use relative tolerance for large values, absolute for small
    if a.abs().max(b.abs()) > 1.0 {
        diff / a.abs().max(b.abs()) < TOL
    } else {
        diff < TOL
    }
}

/// Collect all round-trip failures for a test.
struct Failures {
    items: Vec<String>,
}

impl Failures {
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    fn record(&mut self, msg: String) {
        eprintln!("  FAIL: {}", msg);
        self.items.push(msg);
    }

    fn assert_empty(self) {
        if !self.items.is_empty() {
            panic!(
                "\n=== {} round-trip failures ===\n{}\n",
                self.items.len(),
                self.items.join("\n")
            );
        }
    }
}

// =========================================================================
// Test 1: Generator models round-trip
// =========================================================================
#[test]
fn test_all_generator_models_round_trip() {
    let dyr = r#"
  1 'GENCLS' 1  3.0 0.5 /
  2 'GENROU' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
  3 'GENROU' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 0.003 /
  4 'GENSAL' 1  5.0 0.04 0.06 3.0 0.2 1.0 0.6 0.3 0.25 0.15 0.05 0.08 0.28 /
  5 'GENTPJ' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
  6 'GENTPJ' 2  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 0.5 /
  7 'GENTPJ' 3  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 0.5 0.003 /
  8 'GENQEC' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
  9 'GENTRA' 1  3.0 0.2 0.003 1.0 0.3 5.0 0.6 /
 10 'GENTPF' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
 11 'REGCA' 1  0 0.02 0 0 0 0 0 0 0 1.1 0.02 /
 12 'REGCB' 1  0 0.02 0 0 0 0 0 0 0 1.1 0.02 0.05 /
 13 'WT3G2U' 1  0.02 0.15 0 0 0.05 0 1.1 0.5 /
 14 'WT4G1' 1  0.02 0.15 1.1 /
 15 'REGFM_A1' 1  0.15 3.0 0.5 1.1 /
 16 'REGFM_B1' 1  0.15 3.0 0.5 1.1 /
 17 'DERA' 1  0.15 0.02 1.1 0.05 /
 18 'DERC' 1  0.05 0.03 0.02 100 0.8 /
 19 'REGCC' 1  0.02 0.15 1.1 0.05 0.1 /
 20 'WT4G2' 1  0.02 0.15 1.1 /
 21 'GENROA' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
 22 'GENSAA' 1  5.0 0.04 0.06 3.0 0.2 1.0 0.6 0.3 0.25 0.15 0.05 0.08 0.28 /
 23 'REGFM_C1' 1  0.5 0.1 0.2 0.02 0.01 0.01 1.0 0.0 0.5 -0.5 100 /
 24 'PVGU1' 1  1.0 0.1 0.9 0.4 0.8 0.5 0.4 0.1 1.1 0.02 1.2 0.5 -0.5 0.7 1.1 100 /
 25 'PVDG' 1  0.05 0.03 0.88 0.5 1.1 59.5 60.5 1.0 0.5 -0.5 100 /
 26 'WT3G3' 1  0.02 0.15 0 0 0.05 0 1.1 0.5 /
 27 'REGCO1' 1  0.02 0.5 0.1 0.5 0.1 1.1 -1.1 0.5 -0.5 1.0 0.0 100 /
 28 'GENWTG' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
 29 'GENROE' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
 30 'GENSAL3' 1  5.0 3.0 0.2 1.0 0.6 0.3 0.15 0.05 0.08 /
 31 'DERP' 1  0.15 0.02 1.1 0.05 59.0 61.0 0.88 1.1 0.5 30.0 0.02 /
 32 'REGFM_D1' 1  0.1 0.2 0.3 0.4 0.5 0.6 0.7 0.8 0.9 1.1 0.01 0.02 0.15 100 /
 33 'GENSAE' 1  5.0 0.04 0.06 3.0 0.2 1.0 0.6 0.3 0.25 0.15 0.05 0.08 0.28 /
 34 'WT1G1' 1  3.0 0.5 0.003 0.15 1.1 /
 35 'WT2G1' 1  3.0 0.5 0.003 0.15 1.1 /
 36 'PVD1' 1  0.05 0.03 0.88 0.5 1.1 59.5 60.5 1.0 0.5 -0.5 100 /
 37 'PVDU1' 1  1.0 0.1 0.9 0.4 0.8 0.5 0.4 0.1 1.1 0.02 1.2 0.5 -0.5 0.7 1.1 100 /
"#;

    let dm1 = loads(dyr).expect("parse original");
    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("parse round-trip");

    let mut f = Failures::new();

    // Check total count
    if dm1.generators.len() != dm2.generators.len() {
        f.record(format!(
            "Generator count mismatch: {} vs {}",
            dm1.generators.len(),
            dm2.generators.len()
        ));
    }

    // Check each generator's model type tag survived
    for (i, (g1, g2)) in dm1.generators.iter().zip(dm2.generators.iter()).enumerate() {
        if g1.bus != g2.bus {
            f.record(format!("gen[{}] bus mismatch: {} vs {}", i, g1.bus, g2.bus));
        }
        if g1.machine_id != g2.machine_id {
            f.record(format!(
                "gen[{}] machine_id mismatch: {:?} vs {:?}",
                i, g1.machine_id, g2.machine_id
            ));
        }
        // Check model variant matches
        let tag1 = std::mem::discriminant(&g1.model);
        let tag2 = std::mem::discriminant(&g2.model);
        if tag1 != tag2 {
            f.record(format!(
                "gen[{}] bus={} model type mismatch: {:?} vs {:?}",
                i, g1.bus, g1.model, g2.model
            ));
        }
    }

    // Spot-check specific parameters
    // GENCLS
    match (&dm1.generators[0].model, &dm2.generators[0].model) {
        (GeneratorModel::Gencls(a), GeneratorModel::Gencls(b)) => {
            if !close(a.h, b.h) {
                f.record(format!("GENCLS h: {} vs {}", a.h, b.h));
            }
            if !close(a.d, b.d) {
                f.record(format!("GENCLS d: {} vs {}", a.d, b.d));
            }
        }
        _ => f.record("gen[0] not GENCLS".into()),
    }

    // GENROU without ra
    match (&dm1.generators[1].model, &dm2.generators[1].model) {
        (GeneratorModel::Genrou(a), GeneratorModel::Genrou(b)) => {
            if !close(a.td0_prime, b.td0_prime) {
                f.record(format!("GENROU td0': {} vs {}", a.td0_prime, b.td0_prime));
            }
            if !close(a.h, b.h) {
                f.record(format!("GENROU h: {} vs {}", a.h, b.h));
            }
            if !close(a.s12, b.s12) {
                f.record(format!("GENROU s12: {} vs {}", a.s12, b.s12));
            }
            if a.ra != b.ra {
                f.record(format!("GENROU ra: {:?} vs {:?}", a.ra, b.ra));
            }
        }
        _ => f.record("gen[1] not GENROU".into()),
    }

    // GENROU with ra
    match (&dm1.generators[2].model, &dm2.generators[2].model) {
        (GeneratorModel::Genrou(a), GeneratorModel::Genrou(b)) => {
            if a.ra.is_none() {
                f.record("GENROU+ra: original lost ra".into());
            }
            if a.ra != b.ra {
                f.record(format!("GENROU+ra ra: {:?} vs {:?}", a.ra, b.ra));
            }
        }
        _ => f.record("gen[2] not GENROU".into()),
    }

    // GENSAL
    match (&dm1.generators[3].model, &dm2.generators[3].model) {
        (GeneratorModel::Gensal(a), GeneratorModel::Gensal(b)) => {
            if !close(a.xtran, b.xtran) {
                f.record(format!("GENSAL xtran: {} vs {}", a.xtran, b.xtran));
            }
        }
        _ => f.record("gen[3] not GENSAL".into()),
    }

    // GENTPJ variants (no kii/ra, kii only, kii+ra)
    match (&dm1.generators[4].model, &dm2.generators[4].model) {
        (GeneratorModel::Gentpj(a), GeneratorModel::Gentpj(b)) => {
            if a.kii != b.kii {
                f.record(format!("GENTPJ(plain) kii: {:?} vs {:?}", a.kii, b.kii));
            }
            if a.ra != b.ra {
                f.record(format!("GENTPJ(plain) ra: {:?} vs {:?}", a.ra, b.ra));
            }
        }
        _ => f.record("gen[4] not GENTPJ".into()),
    }
    match (&dm1.generators[5].model, &dm2.generators[5].model) {
        (GeneratorModel::Gentpj(a), GeneratorModel::Gentpj(b)) => {
            if a.kii != b.kii {
                f.record(format!("GENTPJ(kii) kii: {:?} vs {:?}", a.kii, b.kii));
            }
        }
        _ => f.record("gen[5] not GENTPJ".into()),
    }
    match (&dm1.generators[6].model, &dm2.generators[6].model) {
        (GeneratorModel::Gentpj(a), GeneratorModel::Gentpj(b)) => {
            if a.kii != b.kii {
                f.record(format!("GENTPJ(kii+ra) kii: {:?} vs {:?}", a.kii, b.kii));
            }
            if a.ra != b.ra {
                f.record(format!("GENTPJ(kii+ra) ra: {:?} vs {:?}", a.ra, b.ra));
            }
        }
        _ => f.record("gen[6] not GENTPJ".into()),
    }

    eprintln!(
        "Generator models: {} original, {} round-trip",
        dm1.generators.len(),
        dm2.generators.len()
    );
    f.assert_empty();
}

// =========================================================================
// Test 2: Exciter models round-trip
// =========================================================================
#[test]
fn test_all_exciter_models_round_trip() {
    let dyr = r#"
  1 'EXST1' 1   0.02 99.0 -99.0 0.5 0.02 50.0 0.02 9999.0 -9999.0 0.1 0.01 1.0 /
  2 'EXST1' 1   0.02 99.0 -99.0 0.5 0.02 50.0 0.02 9999.0 -9999.0 0.1 0.01 1.0 0.5 0.2 /
  3 'ESST3A' 1  0.02 99.0 -99.0 10.0 0.5 0.02 50.0 0.02 9999.0 -9999.0 1.0 1.0 0.1 10.0 /
  4 'ESDC2A' 1  0.02 50.0 0.02 0.5 0.5 9999.0 -9999.0 1.0 0.5 0.01 1.0 0.0 /
  5 'EXDC2' 1   0.02 50.0 0.02 0.5 0.5 9999.0 -9999.0 1.0 0.5 0.01 1.0 0.0 /
  6 'EXDC2' 2   0.02 50.0 0.02 0.5 0.5 9999.0 -9999.0 1.0 0.5 0.01 1.0 0.0 3.0 0.1 4.0 0.2 /
  7 'IEEEX1' 1  0.02 50.0 0.02 0.5 0.5 9999.0 -9999.0 1.0 0.5 0.01 1.0 0.1 0.2 /
  8 'SEXS' 1    0.5 0.02 50.0 0.02 -5.0 5.0 /
  9 'IEEET1' 1  0.02 50.0 0.02 1.0 0.5 0.01 1.0 3.0 0.1 4.0 0.2 /
 10 'IEEET1' 2  0.02 50.0 0.02 1.0 0.5 0.01 1.0 3.0 0.1 4.0 0.2 9999.0 -9999.0 /
 11 'SCRX' 1    0.02 50.0 0.02 -5.0 5.0 /
 12 'SCRX' 2    0.02 50.0 0.02 -5.0 5.0 0.5 /
 13 'REECA' 1   0.9 1.1 0.02 -0.1 0.1 10.0 1.5 -1.5 1.0 0.05 0.5 -0.5 0 0 5.0 0.1 /
 14 'ESST1A' 1  0.02 99.0 -99.0 0.5 0.02 0.5 0.02 50.0 0.02 9999.0 -9999.0 9999.0 -9999.0 0.1 0.01 1.0 /
 15 'EXAC1' 1   0.02 0.5 0.5 50.0 0.02 9999.0 -9999.0 0.5 0.01 1.0 1.0 3.0 0.1 4.0 0.2 0.1 /
 16 'ESAC1A' 1  0.02 0.5 0.5 50.0 0.02 9999.0 -9999.0 0.5 0.01 1.0 1.0 3.0 0.1 4.0 0.2 0.1 /
 17 'ESAC7B' 1  0.02 5.0 0.1 9999.0 -9999.0 0.5 10.0 1.0 0.5 1.0 3.0 0.1 4.0 0.2 0.5 0.1 1.0 /
 18 'ESST4B' 1  0.02 5.0 0.1 9999.0 -9999.0 0.5 0.1 10.0 -10.0 1.0 1.0 0.1 10.0 5.0 /
 19 'REECD' 1   -0.1 0.1 1.1 -1.1 1.1 0.02 /
 20 'REECCU' 1  -0.1 1.1 1.1 0.02 /
 21 'REXS' 1    0.5 1.0 1.0 0.01 3.0 4.0 0.1 0.2 /
 22 'ESAC2A' 1  0.02 0.5 0.5 50.0 0.02 9999.0 -9999.0 9999.0 -9999.0 1.0 0.5 0.01 1.0 3.0 0.1 4.0 0.2 0.5 0.1 0.1 0.5 /
 23 'ESAC5A' 1  50.0 0.02 1.0 0.5 0.01 1.0 3.0 0.1 4.0 0.2 9999.0 -9999.0 /
 24 'ESST5B' 1  0.02 0.1 0.01 1.0 50.0 0.5 0.5 9999.0 -9999.0 1.0 2.0 /
 25 'EXAC4' 1   0.02 0.5 0.5 50.0 0.02 9999.0 -9999.0 0.1 /
 26 'ESST6B' 1  0.02 0.5 0.1 50.0 0.02 0.1 9999.0 -9999.0 1.0 5.0 1.0 2.0 /
 27 'ESST7B' 1  0.02 5.0 0.1 9999.0 -9999.0 1.0 0.5 10.0 -10.0 1.0 2.0 3.0 4.0 1.0 /
 28 'ESAC6A' 1  0.02 50.0 0.02 1.0 0.5 0.5 9999.0 -9999.0 9999.0 -9999.0 0.5 1.0 0.01 1.0 0.1 0.5 1.0 /
 29 'ESDC1A' 1  0.02 50.0 0.02 0.01 1.0 1.0 0.5 0.1 3.0 0.2 4.0 9999.0 -9999.0 /
 30 'EXST2' 1   0.02 50.0 0.02 9999.0 -9999.0 0.1 0.1 1.0 0.5 /
 31 'AC8B' 1    0.02 50.0 0.02 0.1 9999.0 -9999.0 0.5 1.0 0.5 5.0 0.1 0.01 /
 32 'BBSEX1' 1  0.5 1.0 1.5 2.0 0.5 1.0 50.0 0.02 9999.0 -9999.0 /
 33 'IEEET3' 1  0.02 50.0 0.02 9999.0 -9999.0 0.01 1.0 1.0 0.5 3.0 0.1 4.0 0.2 1.0 0.1 0.1 /
 34 'WT3E1' 1   5.0 0.1 10.0 0.15 5.0 0.1 0.02 0.0 1.0 -0.5 0.5 1.1 /
 35 'WT3E2' 1   5.0 0.1 10.0 0.15 5.0 0.1 0.02 0.0 1.0 -0.5 0.5 1.1 0.02 /
 36 'WT4E1' 1   5.0 0.1 0.02 0.0 1.0 -0.5 0.5 1.1 /
 37 'WT4E2' 1   5.0 0.1 0.02 0.0 1.0 -0.5 0.5 1.1 /
 38 'REPCB' 1   0.05 0.02 5.0 0.1 0.5 0.5 0.5 -0.5 1.1 -1.1 0.1 1.0 /
 39 'REPCC' 1   0.05 0.02 5.0 0.1 0.5 0.5 0.5 -0.5 1.1 -1.1 0.1 1.0 /
 40 'EXST3' 1   0.02 50.0 0.02 0.5 0.5 9999.0 -9999.0 0.1 0.1 10.0 10.0 -10.0 0.5 /
 41 'CBUFR' 1   10.0 1.0 0.05 100.0 -50.0 50.0 200.0 /
 42 'CBUFD' 1   10.0 1.0 0.05 0.03 100.0 -50.0 50.0 50.0 -50.0 50.0 200.0 /
 43 'PVEU1' 1   0.02 1.0 1.0 0.02 0.1 10.0 1.5 -1.5 1.0 0.0 0.5 -0.5 1.1 -1.1 0.05 100 /
 44 'IEEET2' 1  0.02 50.0 0.02 9999.0 -9999.0 1.0 0.5 3.0 0.1 4.0 0.2 0.01 1.0 /
 45 'EXAC2' 1   0.02 0.5 0.5 50.0 0.02 9999.0 -9999.0 0.5 0.01 1.0 1.0 3.0 0.1 4.0 0.2 0.1 0.5 0.5 /
 46 'EXAC3' 1   0.02 0.1 0.1 -1.0 5.0 1.0 0.5 0.01 1.0 3.0 0.1 4.0 0.2 50.0 0.02 0.5 /
 47 'ESAC3A' 1  0.02 0.5 0.5 50.0 0.02 9999.0 -9999.0 0.5 1.0 0.01 1.0 3.0 0.1 4.0 0.2 0.1 0.5 0.1 0.5 0.1 10.0 /
 48 'ESST8C' 1  0.02 5.0 0.1 9999.0 -9999.0 50.0 0.02 0.1 10.0 0.5 0.01 1.0 /
 49 'ESST9B' 1  0.02 5.0 0.1 9999.0 -9999.0 50.0 0.02 10.0 0.1 1.0 2.0 3.0 4.0 /
 50 'ESST10C' 1 0.02 5.0 0.1 5.0 0.1 9999.0 -9999.0 50.0 0.02 10.0 0.1 1.0 2.0 /
 51 'ESDC3A' 1  0.02 50.0 0.02 9999.0 -9999.0 0.5 1.0 3.0 0.1 4.0 0.2 1.0 0.1 0.01 1.0 /
 52 'EXDC1' 1   0.02 50.0 0.02 9999.0 -9999.0 1.0 0.5 0.01 1.0 3.0 0.1 4.0 0.2 /
 53 'ESST2A' 1  0.02 50.0 0.02 0.5 0.5 1.0 0.5 0.01 1.0 9999.0 -9999.0 3.0 0.1 4.0 0.2 0.1 1.0 0.1 0.02 /
 54 'EXDC3' 1   0.02 1.0 0.5 2.0 0.5 0.5 9999.0 -9999.0 1.0 10.0 1.0 1.0 0.5 /
 55 'WT3C2' 1   5.0 0.1 10.0 0.15 5.0 0.1 0.02 0.0 1.0 -0.5 0.5 1.1 /
 56 'ESAC7C' 1  0.02 5.0 0.1 0.01 1.0 9999.0 -9999.0 50.0 0.02 1.0 1.0 0.5 1.0 10.0 -10.0 3.0 0.1 4.0 0.2 /
 57 'ESDC4C' 1  0.02 50.0 0.02 9999.0 -9999.0 1.0 0.5 0.01 1.0 3.0 0.1 4.0 0.2 5.0 0.1 0.01 1.0 /
 58 'REECBU1' 1 -0.1 1.1 1.1 0.02 /
 59 'REECE' 1   0.9 1.1 0.02 -0.1 0.1 10.0 1.5 -1.5 1.0 0.05 0.5 -0.5 0 0 5.0 0.1 /
 60 'REECEU1' 1 -0.1 1.1 1.1 0.02 /
 61 'ESAC8C' 1  0.02 50.0 0.02 0.1 9999.0 -9999.0 0.5 1.0 0.5 5.0 0.1 0.01 /
 62 'ESAC9C' 1  0.02 5.0 0.1 9999.0 -9999.0 0.5 10.0 1.0 0.5 1.0 3.0 0.1 4.0 0.2 0.5 0.1 1.0 /
 63 'ESAC10C' 1 0.02 5.0 0.1 0.01 1.0 9999.0 -9999.0 50.0 0.02 1.0 1.0 0.5 1.0 10.0 -10.0 3.0 0.1 4.0 0.2 /
 64 'ESAC11C' 1 0.02 50.0 0.02 0.1 9999.0 -9999.0 0.5 1.0 0.5 5.0 0.1 0.01 /
"#;

    let dm1 = loads(dyr).expect("parse original");
    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("parse round-trip");

    let mut f = Failures::new();

    if dm1.exciters.len() != dm2.exciters.len() {
        f.record(format!(
            "Exciter count mismatch: {} vs {}",
            dm1.exciters.len(),
            dm2.exciters.len()
        ));
    }

    // Check each exciter model type tag survived
    for (i, (e1, e2)) in dm1.exciters.iter().zip(dm2.exciters.iter()).enumerate() {
        if e1.bus != e2.bus {
            f.record(format!("exc[{}] bus mismatch: {} vs {}", i, e1.bus, e2.bus));
        }
        let tag1 = std::mem::discriminant(&e1.model);
        let tag2 = std::mem::discriminant(&e2.model);
        if tag1 != tag2 {
            f.record(format!("exc[{}] bus={} model type mismatch", i, e1.bus));
        }
    }

    eprintln!(
        "Exciter models: {} original, {} round-trip",
        dm1.exciters.len(),
        dm2.exciters.len()
    );
    f.assert_empty();
}

// =========================================================================
// Test 3: Governor models round-trip
// =========================================================================
#[test]
fn test_all_governor_models_round_trip() {
    let dyr = r#"
  1 'TGOV1' 1   0.05 0.49 33.0 0.4 2.1 7.0 /
  2 'TGOV1' 1   0.05 0.49 33.0 0.4 2.1 7.0 0.5 /
  3 'IEEEG1' 1  20 0.05 0.2 1.0 0.3 -0.3 0.6 0.4 0.3 /
  4 'IEEEG1' 2  20 0.05 0.2 1.0 0.3 -0.3 0.6 0.4 0.3 10 0 1 3.3 0 8 0 0 /
  5 'GGOV1' 1   0.05 0 0.5 0.1 -0.1 10.0 2.0 0 0.1 1.0 0.0 0.5 0 0.5 0 0 0 1.5 0.1 0.5 0.0 /
  6 'GAST' 1    0.05 0.4 0.1 3.0 1.0 0.9 0.0 1.0 /
  7 'REPCA' 1   0 0 0 1 60.0 0.02 0.0 0.0 0.1 1.1 0.9 0.5 -0.5 0.1 0.0 0.0 20.0 20.0 -0.004 0.004 0.0 0.0 1.0 0.0 0.0 0.1 0.0 0.05 10.0 /
  8 'HYGOV' 1   0.05 0.04 0.2 0.5 1.0 0.0 1.0 1.0 0.5 0.05 /
  9 'HYGOVD' 1  0.05 0.04 0.2 0.5 1.0 0.0 1.0 1.0 0.5 0.05 0.001 0.001 /
 10 'TGOV1D' 1  0.05 0.49 33.0 0.4 2.1 7.0 0.001 0.001 /
 11 'TGOV1D' 2  0.05 0.49 33.0 0.4 2.1 7.0 0.5 0.001 0.001 /
 12 'IEEEG1D' 1 20 0.05 0.2 1.0 0.3 -0.3 0.6 0.4 0.3 10 0 1 3.3 0 8 0 0 0.001 0.001 /
 13 'WSIEG1' 1  20 0.05 0.2 1.0 0.3 -0.3 0.6 0.4 0.3 /
 14 'IEEEG2' 1  20 0.05 0.2 0.5 /
 15 'REPCD' 1   0.05 5.0 0.1 1.0 0.0 0.02 /
 16 'WT3T1' 1   3.0 0.5 1.0 0.1 /
 17 'WT3P1' 1   0.05 5.0 0.1 1.0 0.0 /
 18 'GGOV1D' 1  0.05 0.5 0.1 -0.1 10.0 2.0 0.0 -0.0006 0.0006 1.0 0.0 0.5 1.5 0.1 0.5 0.0 1.0 0.0 3.0 /
 19 'TGOV1N' 1  0.05 0.5 0.49 33.0 0.4 2.1 7.0 0.0 0.001 /
 20 'CBEST' 1   50.0 -50.0 25.0 -25.0 0.05 0.03 200.0 100.0 /
 21 'CHAAUT' 1  10.0 1.0 50.0 -50.0 0.05 100.0 /
 22 'PIDGOV' 1  1.0 0.0 5.0 0.1 0.01 0.5 1.0 /
 23 'DEGOV1' 1  0.05 0.4 0.1 3.0 1.0 0.9 1.0 0.0 0.5 /
 24 'TGOV5' 1   0.05 0.4 0.1 0.2 3.0 0.3 0.3 0.4 1.0 0.0 /
 25 'GAST2A' 1  0.05 0.4 0.1 3.0 5.0 1.0 0.9 0.0 1.0 /
 26 'H6E' 1     0.05 0.02 1.0 0.5 1.0 0.4 0.1 0.2 3.0 5.0 0.5 1.0 0.0 /
 27 'WSHYGP' 1  0.05 1.0 0.5 1.0 0.5 1.0 0.0 5.0 0.1 /
 28 'GGOV2' 1   0.05 0 0.5 0.1 -0.1 10.0 2.0 0 0.1 1.0 0.0 0.5 1.5 0.1 0.5 0.0 1.0 0.0 3.0 0.5 0.1 1.0 0.5 0.1 -0.1 0.0 0.0 10.0 10.0 0.1 0.0 4.0 5.0 99.0 -99.0 1.0 0.0 /
 29 'GGOV3' 1   0.05 0 0.5 0.1 -0.1 10.0 2.0 0 0.1 1.0 0.0 0.5 1.5 0.1 0.5 0.0 1.0 0.0 3.0 0.5 0.1 1.0 0.5 0.1 -0.1 0.0 0.0 10.0 10.0 0.1 0.0 4.0 5.0 1.0 99.0 -99.0 1.0 0.0 /
 30 'WPIDHY' 1  1.0 0.0 0.05 5.0 0.1 0.01 0.5 1.0 1.0 1.0 0.5 1.0 0.0 1.0 0.0 /
 31 'H6B' 1     0.5 0.04 0.3 -0.3 1.0 0.0 0.5 1.0 -0.001 0.001 /
 32 'WSHYDD' 1  0.05 1.0 0.5 1.0 0.001 0.5 1.0 0.0 5.0 0.1 /
 33 'REPCGFMC1' 1 5.0 0.1 1.1 -1.1 5.0 0.1 0.5 -0.5 0.02 0.05 -0.001 0.001 /
 34 'WTDTA1' 1  3.0 0.5 100.0 0.1 /
 35 'WTARA1' 1  1.0 0.02 1.0 0.5 1.0 0.0 /
 36 'WTPTA1' 1  5.0 0.1 30.0 0.0 10.0 -10.0 0.02 1.0 /
 37 'IEESGO' 1  0.4 0.1 0.2 3.0 5.0 10.0 0.3 0.3 0.4 1.0 0.0 /
 38 'WTTQA1' 1  5.0 0.1 0.02 1.0 0.0 /
 39 'HYGOV4' 1  0.02 1.0 0.5 0.5 1.0 0.05 1.0 0.5 1.0 0.0 0.5 1.0 1.0 0.0 /
 40 'WEHGOV' 1  0.05 0.02 1.0 0.5 1.0 1.0 0.5 0.05 1.0 0.0 -0.001 0.001 1.0 0.0 /
 41 'IEEEG3' 1  0.5 0.04 0.3 -0.3 1.0 0.0 1.0 1.0 0.5 0.05 /
 42 'IEEEG4' 1  0.4 0.1 0.2 0.1 1.0 0.0 1.0 1.0 0.5 0.05 /
 43 'GOVCT1' 1  0.05 0.4 1.0 0.0 0.1 0.2 0.3 0.3 0.4 3.0 5.0 10.0 0.5 0.5 1.0 0.0 0.5 /
 44 'GOVCT2' 1  0.05 0.4 1.0 0.0 0.1 0.2 0.3 0.3 0.4 3.0 5.0 10.0 0.5 0.5 1.0 0.0 0.5 /
 45 'TGOV3' 1   0.05 0.49 33.0 0.4 2.1 7.0 0.5 0.5 /
 46 'TGOV4' 1   0.05 0.49 33.0 0.4 2.1 7.0 0.5 0.5 /
 47 'WT2E1' 1   5.0 0.1 1.0 0.0 0.02 /
 48 'WT12T1' 1  3.0 0.5 1.0 0.1 /
 49 'WT12A1' 1  0.05 5.0 0.1 1.0 0.0 /
"#;

    let dm1 = loads(dyr).expect("parse original");
    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("parse round-trip");

    let mut f = Failures::new();

    if dm1.governors.len() != dm2.governors.len() {
        f.record(format!(
            "Governor count mismatch: {} vs {}",
            dm1.governors.len(),
            dm2.governors.len()
        ));
    }

    for (i, (g1, g2)) in dm1.governors.iter().zip(dm2.governors.iter()).enumerate() {
        if g1.bus != g2.bus {
            f.record(format!("gov[{}] bus mismatch: {} vs {}", i, g1.bus, g2.bus));
        }
        let tag1 = std::mem::discriminant(&g1.model);
        let tag2 = std::mem::discriminant(&g2.model);
        if tag1 != tag2 {
            f.record(format!("gov[{}] bus={} model type mismatch", i, g1.bus));
        }
    }

    eprintln!(
        "Governor models: {} original, {} round-trip",
        dm1.governors.len(),
        dm2.governors.len()
    );
    f.assert_empty();
}

// =========================================================================
// Test 4: PSS models round-trip
// =========================================================================
#[test]
fn test_all_pss_models_round_trip() {
    let dyr = r#"
  1 'IEEEST' 1  0 1 -5 5 200 0 1 1 1 0 0 0.2 10 /
  2 'IEEEST' 2  0 1 -5 5 200 0 1 1 1 0 0 0.2 10 0.1 -0.1 1.1 0.9 /
  3 'ST2CUT' 1  0 0 0 0 1.0 2.0 0.5 1.0 1.5 2.0 0.5 1.0 1.5 2.0 0 0 0.1 /
  4 'ST2CUT' 2  0 0 0 0 1.0 2.0 0.5 1.0 1.5 2.0 0.5 1.0 1.5 2.0 0 0 0.1 -0.1 1.1 0.9 /
  5 'PSS2A' 1   1 0.5 1.0 2.0 0.5 1.0 2 3.0 3.0 3.0 3.0 0.5 1.0 1.5 2.0 10.0 1.0 0.2 -0.2 /
  6 'PSS2B' 1   1 0.5 1.0 2.0 0.5 1.0 2 3.0 3.0 3.0 3.0 0.5 1.0 1.5 2.0 10.0 1.0 0.2 -0.2 1.0 2.0 /
  7 'STAB1' 1   10.0 0.5 1.0 1.5 2.0 0.1 /
  8 'PSS1A' 1   10.0 0.5 1.0 1.5 2.0 0.2 -0.2 /
  9 'STAB2A' 1  10.0 0.5 1.0 1.5 2.0 0.5 0.1 /
 10 'PSS4B' 1   1.0 2.0 3.0 3.0 0.5 1.0 1.5 2.0 0.2 -0.2 /
 11 'STAB3' 1   10.0 0.5 1.0 1.5 2.0 0.5 1.0 0.2 -0.2 /
 12 'PSS3B' 1   1.0 2.0 3.0 4.0 5.0 6.0 7.0 8.0 0.2 -0.2 0.1 -0.1 0.3 -0.3 /
 13 'PSS2C' 1   1 0.5 2 1.0 3.0 3.0 3.0 3.0 0.5 1.0 1.5 2.0 0.5 1.0 3 10.0 2.0 1.0 0.2 -0.2 /
 14 'PSS5' 1    1.0 2.0 3.0 3.0 3.0 3.0 0.5 1.0 1.5 2.0 0.5 1.0 0.2 -0.2 /
 15 'PSS6C' 1   1.0 2.0 3.0 1.0 2.0 3.0 3.0 3.0 3.0 3.0 3.0 3.0 0.5 1.0 1.5 2.0 0.2 -0.2 /
 16 'PSSSB' 1   10.0 0.5 1.0 1.5 2.0 0.5 1.0 3.0 0.2 -0.2 /
 17 'STAB4' 1   10.0 0.5 1.0 1.5 2.0 0.5 1.0 1.5 2.0 0.1 /
 18 'STAB5' 1   10.0 0.5 1.0 1.5 2.0 0.5 1.0 1.5 2.0 0.5 1.0 0.1 /
 19 'PSS3C' 1   1.0 2.0 3.0 4.0 5.0 6.0 7.0 8.0 0.2 -0.2 0.1 -0.1 0.3 -0.3 /
 20 'PSS4C' 1   1.0 2.0 3.0 3.0 0.5 1.0 1.5 2.0 0.2 -0.2 /
 21 'PSS5C' 1   1.0 2.0 3.0 3.0 3.0 3.0 0.5 1.0 1.5 2.0 0.5 1.0 0.2 -0.2 /
 22 'PSS7C' 1   10.0 3.0 3.0 0.5 1.0 1.5 2.0 0.2 -0.2 /
"#;

    let dm1 = loads(dyr).expect("parse original");
    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("parse round-trip");

    let mut f = Failures::new();

    if dm1.pss.len() != dm2.pss.len() {
        f.record(format!(
            "PSS count mismatch: {} vs {}",
            dm1.pss.len(),
            dm2.pss.len()
        ));
    }

    for (i, (p1, p2)) in dm1.pss.iter().zip(dm2.pss.iter()).enumerate() {
        let tag1 = std::mem::discriminant(&p1.model);
        let tag2 = std::mem::discriminant(&p2.model);
        if tag1 != tag2 {
            f.record(format!("pss[{}] bus={} model type mismatch", i, p1.bus));
        }
    }

    eprintln!(
        "PSS models: {} original, {} round-trip",
        dm1.pss.len(),
        dm2.pss.len()
    );
    f.assert_empty();
}

// =========================================================================
// Test 5: Load models round-trip
// =========================================================================
#[test]
fn test_all_load_models_round_trip() {
    let dyr = r#"
  1 'CLOD' 1    0.8 0.5 0.3 0.1 0.1 1.0 0.02 0.02 0.88 1.1 59.5 60.5 0.5 /
  2 'INDMOT' 1  3.0 0.5 0.003 0.5 0.3 3.0 0.01 100.0 0.8 /
  3 'MOTOR' 1   3.0 0.003 0.5 0.3 0.5 100.0 0.8 /
  4 'CMPLDW' 1  0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 /
  5 'CMPLDWG' 1 0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 50 /
  6 'CMLDBLU2' 1 0.4 0.1 0.5 0.3 0.9 1.0 2.0 0.88 1.1 100 /
  7 'CMLDARU2' 1 0.4 0.1 0.5 0.3 0.9 1.0 2.0 0.88 1.1 100 /
  8 'MOTORW' 1  0.003 3.0 0.01 0.5 0.01 0.3 3.0 0.88 1.1 100 /
  9 'CIM5' 1    0.003 0.5 3.0 0.3 0.3 0.01 0.01 3.0 3.0 0.1 4.0 0.2 100 /
 10 'LCFB1' 1   0.5 0.02 10.0 1.0 0.0 100 /
 11 'LDFRAL' 1  0.5 0.02 10.0 5.0 1.0 0.0 100 /
 12 'FRQTPLT' 1 0.02 59.5 60.5 0.5 /
 13 'LVSHBL' 1  0.02 0.88 0.5 /
 14 'CIM6' 1    0.003 0.5 3.0 0.3 0.3 0.01 0.01 3.0 3.0 0.1 4.0 0.2 100 0.5 0.3 /
 15 'CIMW' 1    3.0 0.5 0.003 0.5 0.3 3.0 0.01 100.0 0.8 /
 16 'EXTL' 1    0.05 0.03 1.0 2.0 0.5 0.3 100 0.8 /
 17 'IEELAR' 1  0.05 0.03 1.0 2.0 0.5 0.3 100 0.8 /
 18 'CMLDOWU2' 1 0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 /
 19 'CMLDXNU2' 1 0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 /
 20 'CMLDALU2' 1 0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 /
 21 'CMLDBLU2W' 1 0.4 0.1 0.5 0.3 0.9 1.0 2.0 0.88 1.1 100 /
 22 'CMLDARU2W' 1 0.4 0.1 0.5 0.3 0.9 1.0 2.0 0.88 1.1 100 /
 23 'VTGTPAT' 1 0.02 0.88 1.1 /
 24 'VTGDCAT' 1 0.02 0.88 1.1 /
 25 'FRQTPAT' 1 0.02 60.5 59.5 60.0 /
 26 'FRQDCAT' 1 0.02 60.5 59.5 60.0 /
 27 'DISTR1' 1  1.0 2.0 0.5 1.0 100 0.8 /
 28 'LVSHC1' 1  0.02 0.88 0.5 /
 29 'CMLDDGU2' 1 0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 /
 30 'CMLDDGGU2' 1 0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 50 /
 31 'CMLDOWDGU2' 1 0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 /
 32 'CMLDXNDGU2' 1 0.3 0.3 0.2 0.5 1.0 0.3 2.0 0.3 1.0 0.3 2.0 0.003 3.0 0.01 0.5 0.01 0.3 0.88 1.1 100 /
"#;

    let dm1 = loads(dyr).expect("parse original");
    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("parse round-trip");

    let mut f = Failures::new();

    if dm1.loads.len() != dm2.loads.len() {
        f.record(format!(
            "Load count mismatch: {} vs {}",
            dm1.loads.len(),
            dm2.loads.len()
        ));
    }

    for (i, (l1, l2)) in dm1.loads.iter().zip(dm2.loads.iter()).enumerate() {
        let tag1 = std::mem::discriminant(&l1.model);
        let tag2 = std::mem::discriminant(&l2.model);
        if tag1 != tag2 {
            f.record(format!("load[{}] bus={} model type mismatch", i, l1.bus));
        }
    }

    eprintln!(
        "Load models: {} original, {} round-trip",
        dm1.loads.len(),
        dm2.loads.len()
    );
    f.assert_empty();
}

// =========================================================================
// Test 6: FACTS models round-trip
// =========================================================================
#[test]
fn test_all_facts_models_round_trip() {
    let dyr = r#"
  1 'CSVGN1' 1  0.5 1.0 0.02 1.0 0.5 50.0 1.1 -1.1 5.0 -5.0 100 /
  2 'CSTCON' 1  0.02 50.0 0.02 1.1 -1.1 100 /
  3 'TCSC' 1    0.5 1.0 0.02 5.0 -5.0 50.0 100 /
  4 'CDC4T' 1   500 1.0 100 0.02 0.5 5.0 170.0 15.0 10 20 /
  5 'VSCDCT' 1  500 1.0 0.02 0.05 1.1 100 10 20 /
  6 'CSVGN3' 1  0.5 1.0 0.02 1.0 0.5 50.0 0.05 1.1 -1.1 5.0 -5.0 100 /
  7 'CDC7T' 1   500 1.0 100 0.02 0.5 5.0 170.0 15.0 10 20 0.1 1.5 /
  8 'CSVGN4' 1  0.5 1.0 0.02 1.0 0.5 50.0 0.05 5.0 0.5 1.1 -1.1 5.0 -5.0 100 /
  9 'CSVGN5' 1  0.5 1.0 0.02 1.0 0.5 50.0 5.0 5.0 0.5 1.1 -1.1 5.0 -5.0 100 /
 10 'CDC6T' 1   500 1.0 100 0.02 0.5 5.0 170.0 15.0 10 20 1.5 /
 11 'CSTCNT' 1  0.5 1.0 0.02 50.0 0.02 1.1 -1.1 100 /
 12 'MMC1' 1    0.02 5.0 0.1 5.0 0.1 1.0 0.1 1.0 0.0 0.5 -0.5 100 /
 13 'HVDCPLU1' 1 0.02 5.0 0.1 5.0 0.1 1.0 1.0 0.0 0.5 -0.5 100 /
 14 'CSVGN6' 1  0.5 1.0 0.02 1.0 0.5 50.0 5.0 0.5 1.1 -1.1 5.0 -5.0 /
 15 'STCON1' 1  0.02 5.0 0.1 5.0 0.1 1.1 -1.1 1.1 -1.1 100 /
 16 'GCSC' 1    0.02 5.0 0.1 5.0 -5.0 100 /
 17 'SSSC' 1    0.02 5.0 0.1 5.0 0.1 1.1 -1.1 100 /
 18 'UPFC' 1    0.02 5.0 0.1 5.0 0.1 5.0 0.1 1.0 0.0 0.5 -0.5 100 /
 19 'CDC3T' 1   0.02 5.0 0.1 5.0 0.1 5.0 0.1 1.0 0.0 100 /
 20 'SVSMO1' 1  0.02 50.0 0.5 -5.0 5.0 /
 21 'SVSMO2' 1  0.02 50.0 0.5 -1.1 1.1 /
 22 'SVSMO3' 1  0.02 50.0 0.5 0.02 -5.0 5.0 /
"#;

    let dm1 = loads(dyr).expect("parse original");
    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("parse round-trip");

    let mut f = Failures::new();

    if dm1.facts.len() != dm2.facts.len() {
        f.record(format!(
            "FACTS count mismatch: {} vs {}",
            dm1.facts.len(),
            dm2.facts.len()
        ));
    }

    for (i, (f1, f2)) in dm1.facts.iter().zip(dm2.facts.iter()).enumerate() {
        let tag1 = std::mem::discriminant(&f1.model);
        let tag2 = std::mem::discriminant(&f2.model);
        if tag1 != tag2 {
            f.record(format!("facts[{}] bus={} model type mismatch", i, f1.bus));
        }
    }

    eprintln!(
        "FACTS models: {} original, {} round-trip",
        dm1.facts.len(),
        dm2.facts.len()
    );
    f.assert_empty();
}

// =========================================================================
// Test 7: OEL/UEL models round-trip
// =========================================================================
#[test]
fn test_all_oel_uel_models_round_trip() {
    let dyr = r#"
  1 'OEL1B' 1   2.0 1.5 5.0 -5.0 0.1 0.5 /
  2 'OEL2C' 1   2.0 10.0 -5.0 5.0 1.0 /
  3 'OEL3C' 1   2.0 10.0 -5.0 5.0 1.0 /
  4 'OEL4C' 1   2.0 10.0 -5.0 5.0 1.0 /
  5 'OEL5C' 1   2.0 10.0 -5.0 5.0 1.0 /
  6 'SCL1C' 1   1.5 10.0 0.02 5.0 -5.0 /
  7 'UEL1' 1    10.0 0.5 5.0 -5.0 0.5 /
  8 'UEL2C' 1   10.0 0.5 1.0 1.5 2.0 5.0 -5.0 1.0 0.5 /
"#;

    let dm1 = loads(dyr).expect("parse original");
    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("parse round-trip");

    let mut f = Failures::new();

    if dm1.oels.len() != dm2.oels.len() {
        f.record(format!(
            "OEL count mismatch: {} vs {}",
            dm1.oels.len(),
            dm2.oels.len()
        ));
    }
    if dm1.uels.len() != dm2.uels.len() {
        f.record(format!(
            "UEL count mismatch: {} vs {}",
            dm1.uels.len(),
            dm2.uels.len()
        ));
    }

    for (i, (o1, o2)) in dm1.oels.iter().zip(dm2.oels.iter()).enumerate() {
        let tag1 = std::mem::discriminant(&o1.model);
        let tag2 = std::mem::discriminant(&o2.model);
        if tag1 != tag2 {
            f.record(format!("oel[{}] bus={} model type mismatch", i, o1.bus));
        }
    }

    for (i, (u1, u2)) in dm1.uels.iter().zip(dm2.uels.iter()).enumerate() {
        let tag1 = std::mem::discriminant(&u1.model);
        let tag2 = std::mem::discriminant(&u2.model);
        if tag1 != tag2 {
            f.record(format!("uel[{}] bus={} model type mismatch", i, u1.bus));
        }
    }

    eprintln!(
        "OEL: {} original {} round-trip, UEL: {} original {} round-trip",
        dm1.oels.len(),
        dm2.oels.len(),
        dm1.uels.len(),
        dm2.uels.len()
    );
    f.assert_empty();
}

// =========================================================================
// Test 8: Edge cases
// =========================================================================
#[test]
fn test_empty_dyr() {
    let dm = loads("").expect("empty parse");
    assert_eq!(dm.generators.len(), 0);
    assert_eq!(dm.exciters.len(), 0);
    assert_eq!(dm.governors.len(), 0);
    assert_eq!(dm.pss.len(), 0);
    assert_eq!(dm.loads.len(), 0);
    assert_eq!(dm.facts.len(), 0);
    assert_eq!(dm.oels.len(), 0);
    assert_eq!(dm.uels.len(), 0);
    assert_eq!(dm.unknown_records.len(), 0);

    let written = dumps(&dm).expect("write empty");
    assert!(written.is_empty() || written.trim().is_empty());
}

#[test]
fn test_unknown_records_round_trip() {
    let dyr = r#"
  1 'MYMODEL' 1  1.0 2.0 3.0 /
  2 'FOOBAR' 1   99.0 -99.0 /
  3 'GENROU' 1   8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
"#;

    let dm1 = loads(dyr).expect("parse");
    assert_eq!(dm1.unknown_records.len(), 2);
    assert_eq!(dm1.generators.len(), 1);

    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("re-parse");

    assert_eq!(dm2.unknown_records.len(), 2);
    assert_eq!(dm2.generators.len(), 1);

    // Check unknown record content
    for (i, (u1, u2)) in dm1
        .unknown_records
        .iter()
        .zip(dm2.unknown_records.iter())
        .enumerate()
    {
        assert_eq!(u1.bus, u2.bus, "unknown[{}] bus", i);
        assert_eq!(u1.model_name, u2.model_name, "unknown[{}] model_name", i);
        assert_eq!(
            u1.params.len(),
            u2.params.len(),
            "unknown[{}] param count",
            i
        );
        for (j, (a, b)) in u1.params.iter().zip(u2.params.iter()).enumerate() {
            assert!(close(*a, *b), "unknown[{}] param[{}]: {} vs {}", i, j, a, b);
        }
    }
}

#[test]
fn test_duplicate_bus_id_combinations() {
    // Two GENROU on same bus with different IDs
    let dyr = r#"
  1 'GENROU' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
  1 'GENROU' 2  7.0 0.04 0.5 0.06 5.5 0.2 1.7 1.6 0.4 0.45 0.3 0.07 0.02 0.03 /
  1 'EXST1' 1   0.02 99.0 -99.0 0.5 0.02 50.0 0.02 9999.0 -9999.0 0.1 0.01 1.0 /
  1 'EXST1' 2   0.03 88.0 -88.0 0.6 0.03 40.0 0.03 8888.0 -8888.0 0.2 0.02 2.0 /
"#;

    let dm1 = loads(dyr).expect("parse");
    assert_eq!(dm1.generators.len(), 2);
    assert_eq!(dm1.exciters.len(), 2);

    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("re-parse");

    assert_eq!(dm2.generators.len(), 2);
    assert_eq!(dm2.exciters.len(), 2);

    // Verify the two generators have distinct H values
    match (&dm2.generators[0].model, &dm2.generators[1].model) {
        (GeneratorModel::Genrou(a), GeneratorModel::Genrou(b)) => {
            assert!(close(a.h, 6.5), "gen1 h={}", a.h);
            assert!(close(b.h, 5.5), "gen2 h={}", b.h);
        }
        _ => panic!("expected two GENROU"),
    }
}

#[test]
fn test_extreme_parameter_values() {
    let dyr = r#"
  1 'GENCLS' 1  1e10 -1e10 /
  2 'GENCLS' 2  0.0 0.0 /
  3 'GENCLS' 3  1e-15 -1e-15 /
"#;

    let dm1 = loads(dyr).expect("parse");
    assert_eq!(dm1.generators.len(), 3);

    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("re-parse");

    assert_eq!(dm2.generators.len(), 3);

    match &dm2.generators[0].model {
        GeneratorModel::Gencls(p) => {
            assert!(close(p.h, 1e10), "large h: {}", p.h);
            assert!(close(p.d, -1e10), "large d: {}", p.d);
        }
        _ => panic!("expected GENCLS"),
    }

    match &dm2.generators[1].model {
        GeneratorModel::Gencls(p) => {
            assert_eq!(p.h, 0.0, "zero h");
            assert_eq!(p.d, 0.0, "zero d");
        }
        _ => panic!("expected GENCLS"),
    }

    match &dm2.generators[2].model {
        GeneratorModel::Gencls(p) => {
            assert!(close(p.h, 1e-15), "tiny h: {}", p.h);
            assert!(close(p.d, -1e-15), "tiny d: {}", p.d);
        }
        _ => panic!("expected GENCLS"),
    }
}

#[test]
fn test_comments_and_whitespace() {
    let dyr = r#"
@ This is a full-line comment
  1 'GENCLS' 1  3.0 0.5 /  ! inline comment
  ! another inline comment style
  2 'GENROU' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7
     0.3 0.55 0.25 0.06 0.01 0.02 /
"#;

    let dm = loads(dyr).expect("parse with comments");
    assert_eq!(dm.generators.len(), 2, "gen count with comments");

    match &dm.generators[0].model {
        GeneratorModel::Gencls(p) => {
            assert!(close(p.h, 3.0));
            assert!(close(p.d, 0.5));
        }
        _ => panic!("expected GENCLS"),
    }
}

// =========================================================================
// Test 9: Deep parameter fidelity for models with lossy round-trip
// =========================================================================

/// Programmatic round-trip: build DynamicModel in code → write → parse → compare.
/// This tests the writer→parser path directly.
#[test]
fn test_programmatic_genrou_round_trip() {
    let model = DynamicModel {
        generators: vec![GeneratorDyn {
            bus: 1,
            machine_id: "1".to_string(),
            model: GeneratorModel::Genrou(GenrouParams {
                td0_prime: 8.123456789,
                td0_pprime: 0.031415926,
                tq0_prime: 0.412345678,
                tq0_pprime: 0.051234567,
                h: 6.543210987,
                d: 0.112345678,
                xd: 1.812345678,
                xq: 1.712345678,
                xd_prime: 0.312345678,
                xq_prime: 0.5512345678,
                xd_pprime: 0.2512345678,
                xl: 0.0612345678,
                s1: 0.0112345678,
                s12: 0.0212345678,
                ra: Some(0.00312345678),
            }),
        }],
        exciters: vec![],
        governors: vec![],
        pss: vec![],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");

    assert_eq!(parsed.generators.len(), 1);
    match &parsed.generators[0].model {
        GeneratorModel::Genrou(p) => {
            let orig = match &model.generators[0].model {
                GeneratorModel::Genrou(o) => o,
                _ => unreachable!(),
            };
            assert!(close(p.td0_prime, orig.td0_prime), "td0_prime");
            assert!(close(p.td0_pprime, orig.td0_pprime), "td0_pprime");
            assert!(close(p.tq0_prime, orig.tq0_prime), "tq0_prime");
            assert!(close(p.tq0_pprime, orig.tq0_pprime), "tq0_pprime");
            assert!(close(p.h, orig.h), "h");
            assert!(close(p.d, orig.d), "d");
            assert!(close(p.xd, orig.xd), "xd");
            assert!(close(p.xq, orig.xq), "xq");
            assert!(close(p.xd_prime, orig.xd_prime), "xd_prime");
            assert!(close(p.xq_prime, orig.xq_prime), "xq_prime");
            assert!(close(p.xd_pprime, orig.xd_pprime), "xd_pprime");
            assert!(close(p.xl, orig.xl), "xl");
            assert!(close(p.s1, orig.s1), "s1");
            assert!(close(p.s12, orig.s12), "s12");
            match (p.ra, orig.ra) {
                (Some(a), Some(b)) => assert!(close(a, b), "ra: {} vs {}", a, b),
                (None, None) => {}
                _ => panic!("ra presence mismatch: {:?} vs {:?}", p.ra, orig.ra),
            }
        }
        _ => panic!("expected GENROU"),
    }
}

/// Test the GGOV1 lossy round-trip: the writer fills in defaults for params
/// that the reader doesn't store. Verify that stored params survive.
#[test]
fn test_ggov1_lossy_round_trip() {
    let model = DynamicModel {
        generators: vec![],
        exciters: vec![],
        governors: vec![GovernorDyn {
            bus: 1,
            machine_id: "1".to_string(),
            model: GovernorModel::Ggov1(Ggov1Params {
                r: 0.05,
                tpelec: 0.5,
                vmax: 1.0,
                vmin: 0.0,
                kpgov: 10.0,
                kigov: 2.0,
                kturb: 1.5,
                wfnl: 0.1,
                tb: 0.5,
                tc: 0.1,
                trate: Some(100.0),
                ldref: None,
                dm: None,
            }),
        }],
        pss: vec![],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    eprintln!("GGOV1 written: {}", written.trim());
    let parsed = loads(&written).expect("parse");

    assert_eq!(parsed.governors.len(), 1);
    match &parsed.governors[0].model {
        GovernorModel::Ggov1(p) => {
            assert!(close(p.r, 0.05), "r: {}", p.r);
            assert!(close(p.tpelec, 0.5), "tpelec: {}", p.tpelec);
            assert!(close(p.vmax, 1.0), "vmax: {}", p.vmax);
            assert!(close(p.vmin, 0.0), "vmin: {}", p.vmin);
            assert!(close(p.kpgov, 10.0), "kpgov: {}", p.kpgov);
            assert!(close(p.kigov, 2.0), "kigov: {}", p.kigov);
            assert!(close(p.kturb, 1.5), "kturb: {}", p.kturb);
            assert!(close(p.wfnl, 0.1), "wfnl: {}", p.wfnl);
            assert!(close(p.tb, 0.5), "tb: {}", p.tb);
            assert!(close(p.tc, 0.1), "tc: {}", p.tc);
            // trate should survive
            assert_eq!(p.trate, Some(100.0), "trate: {:?}", p.trate);
        }
        other => panic!("expected GGOV1, got {:?}", other),
    }
}

/// Test that REGCA round-trips all stored params including x_eq.
#[test]
fn test_regca_round_trip() {
    let model = DynamicModel {
        generators: vec![GeneratorDyn {
            bus: 1,
            machine_id: "1".to_string(),
            model: GeneratorModel::Regca(RegcaParams {
                tg: 0.025,
                x_eq: 0.05,
                imax: 1.2,
                tfltr: 0.03,
                kp_pll: 0.0,
                ki_pll: 0.0,
                rrpwr: 10.0,
                vdip: 0.0,
                vup: 999.0,
            }),
        }],
        exciters: vec![],
        governors: vec![],
        pss: vec![],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");

    assert_eq!(parsed.generators.len(), 1);
    match &parsed.generators[0].model {
        GeneratorModel::Regca(p) => {
            assert!(close(p.tg, 0.025), "tg: {}", p.tg);
            assert!(close(p.imax, 1.2), "imax: {}", p.imax);
            assert!(close(p.tfltr, 0.03), "tfltr: {}", p.tfltr);
            assert!(close(p.x_eq, 0.05), "x_eq: {} (expected 0.05)", p.x_eq);
            assert!(close(p.rrpwr, 10.0), "rrpwr: {} (expected 10.0)", p.rrpwr);
        }
        _ => panic!("expected REGCA"),
    }
}

// =========================================================================
// Test 10: Comprehensive parameter-level fidelity for non-lossy models
// =========================================================================

/// For every exciter model written → parsed, verify each param field matches.
/// Uses programmatic DynamicModel construction.
#[test]
fn test_sexs_full_param_fidelity() {
    let model = DynamicModel {
        generators: vec![],
        exciters: vec![ExciterDyn {
            bus: 1,
            machine_id: "1".to_string(),
            model: ExciterModel::Sexs(SexsParams {
                tb: 0.123,
                tc: 0.456,
                k: 78.9,
                te: 0.0111,
                emin: -4.567,
                emax: 8.901,
            }),
        }],
        governors: vec![],
        pss: vec![],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");

    assert_eq!(parsed.exciters.len(), 1);
    match &parsed.exciters[0].model {
        ExciterModel::Sexs(p) => {
            assert!(close(p.tb, 0.123), "tb: {}", p.tb);
            assert!(close(p.tc, 0.456), "tc: {}", p.tc);
            assert!(close(p.k, 78.9), "k: {}", p.k);
            assert!(close(p.te, 0.0111), "te: {}", p.te);
            assert!(close(p.emin, -4.567), "emin: {}", p.emin);
            assert!(close(p.emax, 8.901), "emax: {}", p.emax);
        }
        _ => panic!("expected SEXS"),
    }
}

#[test]
fn test_ieeest_optional_params_fidelity() {
    // With optional params
    let model = DynamicModel {
        generators: vec![],
        exciters: vec![],
        governors: vec![],
        pss: vec![
            PssDyn {
                bus: 1,
                machine_id: "1".to_string(),
                model: PssModel::Ieeest(IeeestParams {
                    a1: 0.1,
                    a2: 0.2,
                    a3: 0.3,
                    a4: 0.4,
                    a5: 0.5,
                    a6: 0.6,
                    t1: 1.1,
                    t2: 1.2,
                    t3: 1.3,
                    t4: 1.4,
                    t5: 1.5,
                    t6: 1.6,
                    ks: 10.5,
                    lsmax: Some(0.15),
                    lsmin: Some(-0.15),
                    vcu: Some(1.1),
                    vcl: Some(0.9),
                }),
            },
            PssDyn {
                bus: 2,
                machine_id: "1".to_string(),
                model: PssModel::Ieeest(IeeestParams {
                    a1: 0.1,
                    a2: 0.2,
                    a3: 0.3,
                    a4: 0.4,
                    a5: 0.5,
                    a6: 0.6,
                    t1: 1.1,
                    t2: 1.2,
                    t3: 1.3,
                    t4: 1.4,
                    t5: 1.5,
                    t6: 1.6,
                    ks: 10.5,
                    lsmax: None,
                    lsmin: None,
                    vcu: None,
                    vcl: None,
                }),
            },
        ],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");

    assert_eq!(parsed.pss.len(), 2);

    // First: all optional present
    match &parsed.pss[0].model {
        PssModel::Ieeest(p) => {
            assert!(close(p.ks, 10.5), "ks");
            assert_eq!(p.lsmax, Some(0.15), "lsmax");
            assert_eq!(p.lsmin, Some(-0.15), "lsmin");
            assert_eq!(p.vcu, Some(1.1), "vcu");
            assert_eq!(p.vcl, Some(0.9), "vcl");
        }
        _ => panic!("expected IEEEST"),
    }

    // Second: no optional
    match &parsed.pss[1].model {
        PssModel::Ieeest(p) => {
            assert!(close(p.ks, 10.5), "ks");
            assert_eq!(p.lsmax, None, "lsmax");
            assert_eq!(p.lsmin, None, "lsmin");
            assert_eq!(p.vcu, None, "vcu");
            assert_eq!(p.vcl, None, "vcl");
        }
        _ => panic!("expected IEEEST"),
    }
}

// =========================================================================
// Test 12: Deep JSON-based parameter comparison for ALL model types.
// For each model, build programmatically, write, parse, and compare
// every field via JSON serialization.
// =========================================================================

/// Compare two serde_json::Value objects, collecting mismatches with path info.
/// Handles Option<f64> (null vs number) and f64 tolerance.
fn compare_json(path: &str, a: &Value, b: &Value, failures: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            for (k, va) in ma {
                let child_path = format!("{}.{}", path, k);
                if let Some(vb) = mb.get(k) {
                    compare_json(&child_path, va, vb, failures);
                } else {
                    failures.push(format!("{}: missing in round-trip", child_path));
                }
            }
            for k in mb.keys() {
                if !ma.contains_key(k) {
                    failures.push(format!("{}.{}: extra in round-trip", path, k));
                }
            }
        }
        (Value::Number(na), Value::Number(nb)) => {
            let fa = na.as_f64().unwrap_or(0.0);
            let fb = nb.as_f64().unwrap_or(0.0);
            if !close(fa, fb) {
                failures.push(format!("{}: {} vs {}", path, fa, fb));
            }
        }
        (Value::Null, Value::Null) => {}
        (Value::String(sa), Value::String(sb)) => {
            if sa != sb {
                failures.push(format!("{}: {:?} vs {:?}", path, sa, sb));
            }
        }
        (Value::Array(aa), Value::Array(ab)) => {
            if aa.len() != ab.len() {
                failures.push(format!("{}: array len {} vs {}", path, aa.len(), ab.len()));
            }
            for (i, (va, vb)) in aa.iter().zip(ab.iter()).enumerate() {
                compare_json(&format!("{}[{}]", path, i), va, vb, failures);
            }
        }
        _ => {
            if a != b {
                failures.push(format!("{}: {:?} vs {:?}", path, a, b));
            }
        }
    }
}

/// Build a DynamicModel with key generator types, write->parse->compare via JSON.
#[test]
fn test_deep_generator_param_fidelity() {
    let model = DynamicModel {
        generators: vec![
            GeneratorDyn {
                bus: 1,
                machine_id: "1".into(),
                model: GeneratorModel::Gencls(GenclsParams { h: 3.15, d: 0.5 }),
            },
            GeneratorDyn {
                bus: 2,
                machine_id: "1".into(),
                model: GeneratorModel::Genrou(GenrouParams {
                    td0_prime: 8.1,
                    td0_pprime: 0.03,
                    tq0_prime: 0.4,
                    tq0_pprime: 0.05,
                    h: 6.5,
                    d: 0.1,
                    xd: 1.8,
                    xq: 1.7,
                    xd_prime: 0.3,
                    xq_prime: 0.55,
                    xd_pprime: 0.25,
                    xl: 0.06,
                    s1: 0.01,
                    s12: 0.02,
                    ra: None,
                }),
            },
            GeneratorDyn {
                bus: 3,
                machine_id: "1".into(),
                model: GeneratorModel::Gensal(GensalParams {
                    td0_prime: 5.0,
                    td0_pprime: 0.04,
                    tq0_pprime: 0.06,
                    h: 3.0,
                    d: 0.2,
                    xd: 1.0,
                    xq: 0.6,
                    xd_prime: 0.3,
                    xd_pprime: 0.25,
                    xl: 0.15,
                    s1: 0.05,
                    s12: 0.08,
                    xtran: 0.28,
                }),
            },
            GeneratorDyn {
                bus: 4,
                machine_id: "1".into(),
                model: GeneratorModel::Gentpj(GentpjParams {
                    td0_prime: 8.0,
                    td0_pprime: 0.03,
                    tq0_prime: 0.4,
                    tq0_pprime: 0.05,
                    h: 6.5,
                    d: 0.1,
                    xd: 1.8,
                    xq: 1.7,
                    xd_prime: 0.3,
                    xq_prime: 0.55,
                    xd_pprime: 0.25,
                    xl: 0.06,
                    s1: 0.01,
                    s12: 0.02,
                    kii: None,
                    ra: None,
                }),
            },
            GeneratorDyn {
                bus: 5,
                machine_id: "1".into(),
                model: GeneratorModel::Genqec(GenqecParams {
                    td0_prime: 8.0,
                    td0_pprime: 0.03,
                    tq0_prime: 0.4,
                    tq0_pprime: 0.05,
                    h: 6.5,
                    d: 0.1,
                    xd: 1.8,
                    xq: 1.7,
                    xd_prime: 0.3,
                    xq_prime: 0.55,
                    xd_pprime: 0.25,
                    xl: 0.06,
                    s1: 0.01,
                    s12: 0.02,
                    ra: None,
                }),
            },
            GeneratorDyn {
                bus: 6,
                machine_id: "1".into(),
                model: GeneratorModel::Gentra(GentraParams {
                    h: 3.0,
                    d: 0.2,
                    ra: 0.003,
                    xd: 1.0,
                    xd_prime: 0.3,
                    td0_prime: 5.0,
                    xq: 0.6,
                    s1: 0.0,
                    s12: 0.0,
                }),
            },
            GeneratorDyn {
                bus: 7,
                machine_id: "1".into(),
                model: GeneratorModel::Gensal3(Gensal3Params {
                    td0_prime: 5.0,
                    h: 3.0,
                    d: 0.2,
                    xd: 1.0,
                    xq: 0.6,
                    xd_prime: 0.3,
                    xl: 0.15,
                    s1: 0.05,
                    s12: 0.08,
                }),
            },
        ],
        exciters: vec![],
        governors: vec![],
        pss: vec![],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");
    let mut all_failures = Vec::new();

    assert_eq!(model.generators.len(), parsed.generators.len(), "gen count");
    for (i, (orig, rt)) in model
        .generators
        .iter()
        .zip(parsed.generators.iter())
        .enumerate()
    {
        let a_json = serde_json::to_value(&orig.model).unwrap();
        let b_json = serde_json::to_value(&rt.model).unwrap();
        let mut failures = Vec::new();
        compare_json(&format!("gen[{}]", i), &a_json, &b_json, &mut failures);
        if !failures.is_empty() {
            eprintln!("Generator bus={} failures:", orig.bus);
            for f in &failures {
                eprintln!("  {}", f);
            }
        }
        all_failures.extend(failures);
    }

    if !all_failures.is_empty() {
        panic!(
            "\n=== {} generator parameter failures ===\n{}",
            all_failures.len(),
            all_failures.join("\n")
        );
    }
}

/// Build a DynamicModel with key exciter models, write->parse->compare.
#[test]
fn test_deep_exciter_param_fidelity() {
    let model = DynamicModel {
        generators: vec![],
        governors: vec![],
        pss: vec![],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        exciters: vec![
            ExciterDyn {
                bus: 1,
                machine_id: "1".into(),
                model: ExciterModel::Sexs(SexsParams {
                    tb: 0.5,
                    tc: 0.02,
                    k: 50.0,
                    te: 0.02,
                    emin: -5.0,
                    emax: 5.0,
                }),
            },
            ExciterDyn {
                bus: 2,
                machine_id: "1".into(),
                model: ExciterModel::Esdc2a(Esdc2aParams {
                    tr: 0.02,
                    ka: 50.0,
                    ta: 0.02,
                    tb: 0.5,
                    tc: 0.5,
                    vrmax: 9999.0,
                    vrmin: -9999.0,
                    ke: 1.0,
                    te: 0.5,
                    kf: 0.01,
                    tf1: 1.0,
                    switch_: 0.0,
                }),
            },
            ExciterDyn {
                bus: 3,
                machine_id: "1".into(),
                model: ExciterModel::Esst3a(Esst3aParams {
                    tr: 0.02,
                    vimax: 99.0,
                    vimin: -99.0,
                    km: 10.0,
                    tc: 0.5,
                    tb: 0.02,
                    ka: 50.0,
                    ta: 0.02,
                    vrmax: 9999.0,
                    vrmin: -9999.0,
                    kg: 1.0,
                    kp: 1.0,
                    ki: 0.1,
                    vbmax: 10.0,
                }),
            },
            ExciterDyn {
                bus: 4,
                machine_id: "1".into(),
                model: ExciterModel::Exac4(Exac4Params {
                    tr: 0.02,
                    tc: 0.5,
                    tb: 0.5,
                    ka: 50.0,
                    ta: 0.02,
                    vrmax: 9999.0,
                    vrmin: -9999.0,
                    kc: 0.1,
                }),
            },
            ExciterDyn {
                bus: 5,
                machine_id: "1".into(),
                model: ExciterModel::Rexs(RexsParams {
                    te: 0.5,
                    tf: 1.0,
                    ke: 1.0,
                    kf: 0.01,
                    efd1: 3.0,
                    efd2: 4.0,
                    sefd1: 0.1,
                    sefd2: 0.2,
                    tc: 0.0,
                    tb: 0.0,
                }),
            },
        ],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");
    let mut all_failures = Vec::new();

    assert_eq!(model.exciters.len(), parsed.exciters.len(), "exciter count");
    for (i, (orig, rt)) in model
        .exciters
        .iter()
        .zip(parsed.exciters.iter())
        .enumerate()
    {
        let a_json = serde_json::to_value(&orig.model).unwrap();
        let b_json = serde_json::to_value(&rt.model).unwrap();
        let mut failures = Vec::new();
        compare_json(&format!("exc[{}]", i), &a_json, &b_json, &mut failures);
        if !failures.is_empty() {
            eprintln!("Exciter bus={} failures:", orig.bus);
            for f in &failures {
                eprintln!("  {}", f);
            }
        }
        all_failures.extend(failures);
    }

    if !all_failures.is_empty() {
        panic!(
            "\n=== {} exciter parameter failures ===\n{}",
            all_failures.len(),
            all_failures.join("\n")
        );
    }
}

/// Build a DynamicModel with key governor models, write->parse->compare.
#[test]
fn test_deep_governor_param_fidelity() {
    let model = DynamicModel {
        generators: vec![],
        exciters: vec![],
        pss: vec![],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        governors: vec![
            GovernorDyn {
                bus: 1,
                machine_id: "1".into(),
                model: GovernorModel::Tgov1(Tgov1Params {
                    r: 0.05,
                    t1: 0.49,
                    vmax: 33.0,
                    vmin: 0.4,
                    t2: 2.1,
                    t3: 7.0,
                    dt: Some(0.5),
                }),
            },
            GovernorDyn {
                bus: 2,
                machine_id: "1".into(),
                model: GovernorModel::Gast(GastParams {
                    r: 0.05,
                    t1: 0.4,
                    t2: 0.1,
                    t3: 3.0,
                    at: 1.0,
                    kt: 0.9,
                    vmin: 0.0,
                    vmax: 1.0,
                }),
            },
            GovernorDyn {
                bus: 3,
                machine_id: "1".into(),
                model: GovernorModel::Ieeeg2(Ieeeg2Params {
                    k: 20.0,
                    t1: 0.05,
                    t2: 0.2,
                    t3: 0.0,
                    rt: 0.0,
                    pmin: 0.0,
                    pmax: 1.0,
                    at: 1.0,
                    dturb: 0.0,
                    qnl: 0.5,
                }),
            },
            GovernorDyn {
                bus: 4,
                machine_id: "1".into(),
                model: GovernorModel::Repca(RepcaParams {
                    vrflag: 1.0,
                    rc: 0.1,
                    tfltr: 0.02,
                    kp: 0.0,
                    ki: 0.0,
                    vmax: 1.1,
                    vmin: 0.9,
                    vref: 1.0, // default
                    qref: 0.0, // default
                    qmax: 0.5,
                    qmin: -0.5,
                    fdbd1: -0.004,
                    fdbd2: 0.004,
                    ddn: 20.0,
                    dup: 20.0,
                    tp: 0.05,
                    kpg: 0.1,
                    kig: 0.0,
                    pref: 0.0, // default
                    pmax: 1.0,
                    pmin: 0.0,
                    rrpwr: 10.0,
                    tlag: 0.1,
                }),
            },
            GovernorDyn {
                bus: 5,
                machine_id: "1".into(),
                model: GovernorModel::Hygov(HygovParams {
                    r: 0.05,
                    tp: 0.04,
                    velm: 0.2,
                    tg: 0.5,
                    gmax: 1.0,
                    gmin: 0.0,
                    tw: 1.0,
                    at: 1.0,
                    dturb: 0.5,
                    qnl: 0.05,
                }),
            },
            GovernorDyn {
                bus: 6,
                machine_id: "1".into(),
                model: GovernorModel::Wt3t1(Wt3t1Params {
                    h: 3.0,
                    damp: 0.5,
                    ka: 1.0,
                    theta: 0.1,
                }),
            },
            GovernorDyn {
                bus: 7,
                machine_id: "1".into(),
                model: GovernorModel::Wt3p1(Wt3p1Params {
                    tp: 0.05,
                    kpp: 5.0,
                    kip: 0.1,
                    pmax: 1.0,
                    pmin: 0.0,
                }),
            },
            GovernorDyn {
                bus: 8,
                machine_id: "1".into(),
                model: GovernorModel::Pidgov(PidgovParams {
                    pmax: 1.0,
                    pmin: 0.0,
                    kp: 5.0,
                    ki: 0.1,
                    kd: 0.01,
                    td: 0.5,
                    tf: 1.0,
                }),
            },
            GovernorDyn {
                bus: 9,
                machine_id: "1".into(),
                model: GovernorModel::Degov1(Degov1Params {
                    r: 0.05,
                    t1: 0.4,
                    t2: 0.1,
                    t3: 3.0,
                    t4: 0.0,
                    t5: 0.01,
                    t6: 0.01,
                    td: 0.5,
                    k: 1.0,
                    at: 1.0,
                    kt: 0.9,
                    vmax: 1.0,
                    vmin: 0.0,
                    velm: 99.0,
                }),
            },
        ],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");
    let mut all_failures = Vec::new();

    assert_eq!(
        model.governors.len(),
        parsed.governors.len(),
        "governor count"
    );
    for (i, (orig, rt)) in model
        .governors
        .iter()
        .zip(parsed.governors.iter())
        .enumerate()
    {
        let a_json = serde_json::to_value(&orig.model).unwrap();
        let b_json = serde_json::to_value(&rt.model).unwrap();
        let mut failures = Vec::new();
        compare_json(&format!("gov[{}]", i), &a_json, &b_json, &mut failures);
        if !failures.is_empty() {
            eprintln!("Governor bus={} failures:", orig.bus);
            for f in &failures {
                eprintln!("  {}", f);
            }
        }
        all_failures.extend(failures);
    }

    if !all_failures.is_empty() {
        panic!(
            "\n=== {} governor parameter failures ===\n{}",
            all_failures.len(),
            all_failures.join("\n")
        );
    }
}

/// Build a DynamicModel with every PSS model, write->parse->compare.
#[test]
fn test_deep_pss_param_fidelity() {
    let model = DynamicModel {
        generators: vec![],
        exciters: vec![],
        governors: vec![],
        loads: vec![],
        facts: vec![],
        unknown_records: vec![],
        oels: vec![],
        uels: vec![],
        pss: vec![
            PssDyn {
                bus: 1,
                machine_id: "1".into(),
                model: PssModel::Stab1(Stab1Params {
                    ks: 10.0,
                    t1: 0.5,
                    t2: 1.0,
                    t3: 1.5,
                    t4: 2.0,
                    hlim: 0.1,
                }),
            },
            PssDyn {
                bus: 2,
                machine_id: "1".into(),
                model: PssModel::Pss1a(Pss1aParams {
                    ks: 10.0,
                    t1: 0.5,
                    t2: 1.0,
                    t3: 1.5,
                    t4: 2.0,
                    vstmax: 0.2,
                    vstmin: -0.2,
                }),
            },
            PssDyn {
                bus: 3,
                machine_id: "1".into(),
                model: PssModel::Pss7c(Pss7cParams {
                    kss: 10.0,
                    tw1: 3.0,
                    tw2: 3.0,
                    t1: 0.5,
                    t2: 1.0,
                    t3: 1.5,
                    t4: 2.0,
                    vsmax: 0.2,
                    vsmin: -0.2,
                    kl: 0.0,
                    tw_l: 3.0,
                    t1_l: 0.0,
                    t2_l: 1.0,
                    ki: 0.0,
                    tw_i: 3.0,
                    t1_i: 0.0,
                    t2_i: 1.0,
                    kh: 0.0,
                    tw_h: 3.0,
                    t1_h: 0.0,
                    t2_h: 1.0,
                    vstmax: 0.2,
                    vstmin: -0.2,
                }),
            },
        ],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");
    let mut all_failures = Vec::new();

    assert_eq!(model.pss.len(), parsed.pss.len(), "pss count");
    for (i, (orig, rt)) in model.pss.iter().zip(parsed.pss.iter()).enumerate() {
        let a_json = serde_json::to_value(&orig.model).unwrap();
        let b_json = serde_json::to_value(&rt.model).unwrap();
        let mut failures = Vec::new();
        compare_json(&format!("pss[{}]", i), &a_json, &b_json, &mut failures);
        if !failures.is_empty() {
            eprintln!("PSS bus={} failures:", orig.bus);
            for f in &failures {
                eprintln!("  {}", f);
            }
        }
        all_failures.extend(failures);
    }

    if !all_failures.is_empty() {
        panic!(
            "\n=== {} PSS parameter failures ===\n{}",
            all_failures.len(),
            all_failures.join("\n")
        );
    }
}

/// Build loads, FACTS, OELs, UELs, write->parse->compare.
#[test]
fn test_deep_load_facts_oel_uel_param_fidelity() {
    let model = DynamicModel {
        generators: vec![],
        exciters: vec![],
        governors: vec![],
        pss: vec![],
        unknown_records: vec![],
        loads: vec![
            LoadDyn {
                bus: 1,
                load_id: "1".into(),
                model: LoadModel::Clod(ClodParams {
                    mbase: 100.0,
                    lfac: 0.8,
                    rfrac: 0.5,
                    xfrac: 0.3,
                    lfrac_dl: 0.1,
                    nfrac: 0.1,
                    dsli: 1.0,
                    tv: 0.02,
                    tf: 0.02,
                    vtd: 0.88,
                    vtu: 1.1,
                    ftd: 59.5,
                    ftu: 60.5,
                    td: 0.5,
                }),
            },
            LoadDyn {
                bus: 2,
                load_id: "1".into(),
                model: LoadModel::Indmot({
                    // t0p and x0p are derived from raw params by the reader, so set them
                    // to the values the reader will compute.
                    let xs = 0.5;
                    let xr = 0.3;
                    let xm = 3.0;
                    let rr = 0.01;
                    let omega0 = 2.0 * std::f64::consts::PI * 60.0;
                    let x0p: f64 = xs + xr * xm / (xr + xm);
                    let slip0: f64 = (rr / x0p).min(0.1);
                    let ra: f64 = 0.003;
                    let xm_frac = xm / (xr + xm);
                    let eq0 = xm_frac;
                    let z_sq = ra * ra + x0p * x0p;
                    let iq0 = (-x0p * 0.0 + ra * (eq0 - 1.0)) / z_sq;
                    let id0 = (ra * 0.0 + x0p * (eq0 - 1.0)) / z_sq;
                    let te0 = (0.0 * id0 + eq0 * iq0).abs().max(1e-4);
                    IndmotParams {
                        h: 3.0,
                        d: 0.5,
                        ra,
                        xs,
                        xr,
                        xm,
                        rr,
                        t0p: (xr + xm) / (omega0 * rr),
                        x0p,
                        mbase: 100.0,
                        lfac: 0.8,
                        slip0,
                        te0,
                    }
                }),
            },
            LoadDyn {
                bus: 3,
                load_id: "1".into(),
                model: LoadModel::Motor(MotorParams {
                    h: 3.0,
                    ra: 0.003,
                    xs: 0.5,
                    x0p: 0.3,
                    t0p: 0.5,
                    mbase: 100.0,
                    lfac: 0.8,
                }),
            },
            LoadDyn {
                bus: 4,
                load_id: "1".into(),
                model: LoadModel::Frqtplt(FrqtpltParams {
                    tf: 0.02,
                    fmin: 59.5,
                    fmax: 60.5,
                    p_trip: 0.5,
                }),
            },
            LoadDyn {
                bus: 5,
                load_id: "1".into(),
                model: LoadModel::Lvshbl(LvshblParams {
                    tv: 0.02,
                    vmin: 0.88,
                    p_block: 0.5,
                }),
            },
        ],
        facts: vec![
            FACTSDyn {
                bus: 1,
                device_id: "1".into(),
                to_bus: None,
                model: FACTSModel::Csvgn1(Csvgn1Params {
                    t1: 0.5,
                    t2: 1.0,
                    t3: 0.02,
                    t4: 1.0,
                    t5: 0.5,
                    k: 50.0,
                    vmax: 1.1,
                    vmin: -1.1,
                    bmax: 5.0,
                    bmin: -5.0,
                    mbase: 100.0,
                    b_l: None,
                    b_c: None,
                    t_alpha: None,
                }),
            },
            FACTSDyn {
                bus: 2,
                device_id: "1".into(),
                to_bus: None,
                model: FACTSModel::Svsmo1(Svsmo1Params {
                    tr: 0.02,
                    k: 50.0,
                    ta: 0.5,
                    b_min: -5.0,
                    b_max: 5.0,
                }),
            },
        ],
        oels: vec![
            OelDyn {
                bus: 1,
                machine_id: "1".into(),
                model: OelModel::Oel1b(Oel1bParams {
                    ifdmax: 2.0,
                    ifdlim: 1.5,
                    vrmax: 5.0,
                    vamin: -5.0,
                    kramp: 0.1,
                    tff: 0.5,
                }),
            },
            OelDyn {
                bus: 2,
                machine_id: "1".into(),
                model: OelModel::Scl1c(Scl1cParams {
                    irated: 1.5,
                    kr: 10.0,
                    tr: 0.02,
                    vclmax: 5.0,
                    vclmin: -5.0,
                }),
            },
        ],
        uels: vec![
            UelDyn {
                bus: 1,
                machine_id: "1".into(),
                model: UelModel::Uel1(Uel1Params {
                    kul: 10.0,
                    tu1: 0.5,
                    vucmax: 5.0,
                    vucmin: -5.0,
                    kur: 0.5,
                }),
            },
            UelDyn {
                bus: 2,
                machine_id: "1".into(),
                model: UelModel::Uel2c(Uel2cParams {
                    kul: 10.0,
                    tu1: 0.5,
                    tu2: 1.0,
                    tu3: 1.5,
                    tu4: 2.0,
                    vuimax: 5.0,
                    vuimin: -5.0,
                    p0: 1.0,
                    q0: 0.5,
                }),
            },
        ],
        shafts: vec![],
    };

    let written = dumps(&model).expect("write");
    let parsed = loads(&written).expect("parse");
    let mut all_failures = Vec::new();

    // Loads
    assert_eq!(model.loads.len(), parsed.loads.len(), "load count");
    for (i, (orig, rt)) in model.loads.iter().zip(parsed.loads.iter()).enumerate() {
        let a_json = serde_json::to_value(&orig.model).unwrap();
        let b_json = serde_json::to_value(&rt.model).unwrap();
        let mut failures = Vec::new();
        compare_json(&format!("load[{}]", i), &a_json, &b_json, &mut failures);
        for f in &failures {
            eprintln!("Load bus={}: {}", orig.bus, f);
        }
        all_failures.extend(failures);
    }

    // FACTS
    assert_eq!(model.facts.len(), parsed.facts.len(), "facts count");
    for (i, (orig, rt)) in model.facts.iter().zip(parsed.facts.iter()).enumerate() {
        let a_json = serde_json::to_value(&orig.model).unwrap();
        let b_json = serde_json::to_value(&rt.model).unwrap();
        let mut failures = Vec::new();
        compare_json(&format!("facts[{}]", i), &a_json, &b_json, &mut failures);
        for f in &failures {
            eprintln!("FACTS bus={}: {}", orig.bus, f);
        }
        all_failures.extend(failures);
    }

    // OELs
    assert_eq!(model.oels.len(), parsed.oels.len(), "oel count");
    for (i, (orig, rt)) in model.oels.iter().zip(parsed.oels.iter()).enumerate() {
        let a_json = serde_json::to_value(&orig.model).unwrap();
        let b_json = serde_json::to_value(&rt.model).unwrap();
        let mut failures = Vec::new();
        compare_json(&format!("oel[{}]", i), &a_json, &b_json, &mut failures);
        for f in &failures {
            eprintln!("OEL bus={}: {}", orig.bus, f);
        }
        all_failures.extend(failures);
    }

    // UELs
    assert_eq!(model.uels.len(), parsed.uels.len(), "uel count");
    for (i, (orig, rt)) in model.uels.iter().zip(parsed.uels.iter()).enumerate() {
        let a_json = serde_json::to_value(&orig.model).unwrap();
        let b_json = serde_json::to_value(&rt.model).unwrap();
        let mut failures = Vec::new();
        compare_json(&format!("uel[{}]", i), &a_json, &b_json, &mut failures);
        for f in &failures {
            eprintln!("UEL bus={}: {}", orig.bus, f);
        }
        all_failures.extend(failures);
    }

    if !all_failures.is_empty() {
        panic!(
            "\n=== {} load/FACTS/OEL/UEL parameter failures ===\n{}",
            all_failures.len(),
            all_failures.join("\n")
        );
    }
}

// =========================================================================
// Test 13: fmt_param precision — verify no precision loss on edge values
// =========================================================================
#[test]
fn test_fmt_param_precision_round_trip() {
    // Values that could be tricky for formatting
    let tricky_values: Vec<f64> = vec![
        0.0,
        1.0,
        -1.0,
        0.1,
        0.01,
        0.001,
        0.123456789012, // 12 significant digits
        1e-10,
        1e10,
        -0.0,
        std::f64::consts::PI,
        99.9999999999,
        100.0,
        0.05,
    ];

    for &v in &tricky_values {
        let dyr = format!("  1 'GENCLS' 1  {} 0.0 /\n", v);
        let dm = loads(&dyr).expect("parse");
        let written = dumps(&dm).expect("write");
        let dm2 = loads(&written).expect("re-parse");

        match &dm2.generators[0].model {
            GeneratorModel::Gencls(p) => {
                if !close(p.h, v) {
                    eprintln!("Precision loss for value {}: got {}", v, p.h);
                    panic!("Precision loss for value {}: got {}", v, p.h);
                }
            }
            _ => panic!("expected GENCLS"),
        }
    }
}

// =========================================================================
// Test 14: Full DYR text round-trip with JSON parameter comparison.
// Parse a comprehensive DYR string, write it back, parse again, and compare
// every single parameter in every model using JSON serialization.
// This catches any writer/reader mismatch at the parameter level.
// =========================================================================
#[test]
fn test_full_text_round_trip_param_fidelity() {
    // A comprehensive DYR string with one instance of every model that has
    // a clean (non-lossy) round-trip path.
    let dyr = r#"
  1 'GENCLS' 1  3.0 0.5 /
  2 'GENROU' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
  3 'GENSAL' 1  5.0 0.04 0.06 3.0 0.2 1.0 0.6 0.3 0.25 0.15 0.05 0.08 0.28 /
  4 'GENTPJ' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
  5 'GENTRA' 1  3.0 0.2 0.003 1.0 0.3 5.0 0.6 /
  6 'GENSAL3' 1 5.0 3.0 0.2 1.0 0.6 0.3 0.15 0.05 0.08 /
  7 'GENQEC' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
  8 'DERC' 1    0.05 0.03 0.02 100 0.8 /
  9 'WT4G1' 1   0.02 0.15 1.1 /
 10 'WT4G2' 1   0.02 0.15 1.1 /
 11 'REGCC' 1   0.02 0.15 1.1 0.05 0.1 /
 12 'REGFM_A1' 1 0.15 3.0 0.5 1.1 /
 13 'REGFM_B1' 1 0.15 3.0 0.5 1.1 /
 14 'DERA' 1    0.15 0.02 1.1 0.05 /
 15 'GENROA' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
 16 'GENSAA' 1  5.0 0.04 0.06 3.0 0.2 1.0 0.6 0.3 0.25 0.15 0.05 0.08 0.28 /
 17 'GENSAE' 1  5.0 0.04 0.06 3.0 0.2 1.0 0.6 0.3 0.25 0.15 0.05 0.08 0.28 /
 18 'GENROE' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
 19 'GENWTG' 1  8.0 0.03 0.4 0.05 6.5 0.1 1.8 1.7 0.3 0.55 0.25 0.06 0.01 0.02 /
 20 'WT1G1' 1   3.0 0.5 0.003 0.15 1.1 /
 21 'WT2G1' 1   3.0 0.5 0.003 0.15 1.1 /
101 'EXST1' 1   0.02 99.0 -99.0 0.5 0.02 50.0 0.02 9999.0 -9999.0 0.1 0.01 1.0 /
102 'ESST3A' 1  0.02 99.0 -99.0 10.0 0.5 0.02 50.0 0.02 9999.0 -9999.0 1.0 1.0 0.1 10.0 /
103 'ESDC2A' 1  0.02 50.0 0.02 0.5 0.5 9999.0 -9999.0 1.0 0.5 0.01 1.0 0.0 /
104 'SEXS' 1    0.5 0.02 50.0 0.02 -5.0 5.0 /
105 'IEEET1' 1  0.02 50.0 0.02 1.0 0.5 0.01 1.0 3.0 0.1 4.0 0.2 /
106 'EXAC4' 1   0.02 0.5 0.5 50.0 0.02 9999.0 -9999.0 0.1 /
107 'REXS' 1    0.5 1.0 1.0 0.01 3.0 4.0 0.1 0.2 /
108 'ESST5B' 1  0.02 0.1 0.01 1.0 50.0 0.5 0.5 9999.0 -9999.0 1.0 2.0 /
109 'EXST2' 1   0.02 50.0 0.02 9999.0 -9999.0 0.1 0.1 1.0 0.5 /
110 'BBSEX1' 1  0.5 1.0 1.5 2.0 0.5 1.0 50.0 0.02 9999.0 -9999.0 /
201 'TGOV1' 1   0.05 0.49 33.0 0.4 2.1 7.0 /
202 'GAST' 1    0.05 0.4 0.1 3.0 1.0 0.9 0.0 1.0 /
203 'IEEEG2' 1  20 0.05 0.2 0.5 /
204 'REPCA' 1   0 0 0 1 60.0 0.02 0.0 0.0 0.1 1.1 0.9 0.5 -0.5 0.1 0.0 0.0 20.0 20.0 -0.004 0.004 0.0 0.0 1.0 0.0 0.0 0.1 0.0 0.05 10.0 /
205 'HYGOV' 1   0.05 0.04 0.2 0.5 1.0 0.0 1.0 1.0 0.5 0.05 /
206 'WT3T1' 1   3.0 0.5 1.0 0.1 /
207 'WT3P1' 1   0.05 5.0 0.1 1.0 0.0 /
208 'PIDGOV' 1  1.0 0.0 5.0 0.1 0.01 0.5 1.0 /
209 'DEGOV1' 1  0.05 0.4 0.1 3.0 1.0 0.9 1.0 0.0 0.5 /
210 'TGOV5' 1   0.05 0.4 0.1 0.2 3.0 0.3 0.3 0.4 1.0 0.0 /
211 'GAST2A' 1  0.05 0.4 0.1 3.0 5.0 1.0 0.9 0.0 1.0 /
212 'WTTQA1' 1  5.0 0.1 0.02 1.0 0.0 /
213 'IEESGO' 1  0.4 0.1 0.2 3.0 5.0 10.0 0.3 0.3 0.4 1.0 0.0 /
301 'IEEEST' 1  0 1 -5 5 200 0 1 1 1 0 0 0.2 10 /
302 'STAB1' 1   10.0 0.5 1.0 1.5 2.0 0.1 /
303 'PSS1A' 1   10.0 0.5 1.0 1.5 2.0 0.2 -0.2 /
304 'PSS7C' 1   10.0 3.0 3.0 0.5 1.0 1.5 2.0 0.2 -0.2 /
401 'CLOD' 1    0.8 0.5 0.3 0.1 0.1 1.0 0.02 0.02 0.88 1.1 59.5 60.5 0.5 /
402 'MOTOR' 1   3.0 0.003 0.5 0.3 0.5 100.0 0.8 /
403 'FRQTPLT' 1 0.02 59.5 60.5 0.5 /
404 'LVSHBL' 1  0.02 0.88 0.5 /
501 'CSVGN1' 1  0.5 1.0 0.02 1.0 0.5 50.0 1.1 -1.1 5.0 -5.0 100 /
502 'SVSMO1' 1  0.02 50.0 0.5 -5.0 5.0 /
601 'OEL1B' 1   2.0 1.5 5.0 -5.0 0.1 0.5 /
602 'OEL2C' 1   2.0 10.0 -5.0 5.0 1.0 /
603 'SCL1C' 1   1.5 10.0 0.02 5.0 -5.0 /
701 'UEL1' 1    10.0 0.5 5.0 -5.0 0.5 /
702 'UEL2C' 1   10.0 0.5 1.0 1.5 2.0 5.0 -5.0 1.0 0.5 /
"#;

    let dm1 = loads(dyr).expect("parse original");
    let written = dumps(&dm1).expect("write");
    let dm2 = loads(&written).expect("parse round-trip");

    let mut all_failures = Vec::new();

    // Compare generators
    assert_eq!(dm1.generators.len(), dm2.generators.len(), "gen count");
    for (i, (g1, g2)) in dm1.generators.iter().zip(dm2.generators.iter()).enumerate() {
        let a = serde_json::to_value(&g1.model).unwrap();
        let b = serde_json::to_value(&g2.model).unwrap();
        let mut f = Vec::new();
        compare_json(&format!("gen[{}] bus={}", i, g1.bus), &a, &b, &mut f);
        for ff in &f {
            eprintln!("  {}", ff);
        }
        all_failures.extend(f);
    }

    // Compare exciters
    assert_eq!(dm1.exciters.len(), dm2.exciters.len(), "exc count");
    for (i, (e1, e2)) in dm1.exciters.iter().zip(dm2.exciters.iter()).enumerate() {
        let a = serde_json::to_value(&e1.model).unwrap();
        let b = serde_json::to_value(&e2.model).unwrap();
        let mut f = Vec::new();
        compare_json(&format!("exc[{}] bus={}", i, e1.bus), &a, &b, &mut f);
        for ff in &f {
            eprintln!("  {}", ff);
        }
        all_failures.extend(f);
    }

    // Compare governors
    assert_eq!(dm1.governors.len(), dm2.governors.len(), "gov count");
    for (i, (g1, g2)) in dm1.governors.iter().zip(dm2.governors.iter()).enumerate() {
        let a = serde_json::to_value(&g1.model).unwrap();
        let b = serde_json::to_value(&g2.model).unwrap();
        let mut f = Vec::new();
        compare_json(&format!("gov[{}] bus={}", i, g1.bus), &a, &b, &mut f);
        for ff in &f {
            eprintln!("  {}", ff);
        }
        all_failures.extend(f);
    }

    // Compare PSS
    assert_eq!(dm1.pss.len(), dm2.pss.len(), "pss count");
    for (i, (p1, p2)) in dm1.pss.iter().zip(dm2.pss.iter()).enumerate() {
        let a = serde_json::to_value(&p1.model).unwrap();
        let b = serde_json::to_value(&p2.model).unwrap();
        let mut f = Vec::new();
        compare_json(&format!("pss[{}] bus={}", i, p1.bus), &a, &b, &mut f);
        for ff in &f {
            eprintln!("  {}", ff);
        }
        all_failures.extend(f);
    }

    // Compare loads
    assert_eq!(dm1.loads.len(), dm2.loads.len(), "load count");
    for (i, (l1, l2)) in dm1.loads.iter().zip(dm2.loads.iter()).enumerate() {
        let a = serde_json::to_value(&l1.model).unwrap();
        let b = serde_json::to_value(&l2.model).unwrap();
        let mut f = Vec::new();
        compare_json(&format!("load[{}] bus={}", i, l1.bus), &a, &b, &mut f);
        for ff in &f {
            eprintln!("  {}", ff);
        }
        all_failures.extend(f);
    }

    // Compare FACTS
    assert_eq!(dm1.facts.len(), dm2.facts.len(), "facts count");
    for (i, (f1, f2)) in dm1.facts.iter().zip(dm2.facts.iter()).enumerate() {
        let a = serde_json::to_value(&f1.model).unwrap();
        let b = serde_json::to_value(&f2.model).unwrap();
        let mut f = Vec::new();
        compare_json(&format!("facts[{}] bus={}", i, f1.bus), &a, &b, &mut f);
        for ff in &f {
            eprintln!("  {}", ff);
        }
        all_failures.extend(f);
    }

    // Compare OELs
    assert_eq!(dm1.oels.len(), dm2.oels.len(), "oel count");
    for (i, (o1, o2)) in dm1.oels.iter().zip(dm2.oels.iter()).enumerate() {
        let a = serde_json::to_value(&o1.model).unwrap();
        let b = serde_json::to_value(&o2.model).unwrap();
        let mut f = Vec::new();
        compare_json(&format!("oel[{}] bus={}", i, o1.bus), &a, &b, &mut f);
        for ff in &f {
            eprintln!("  {}", ff);
        }
        all_failures.extend(f);
    }

    // Compare UELs
    assert_eq!(dm1.uels.len(), dm2.uels.len(), "uel count");
    for (i, (u1, u2)) in dm1.uels.iter().zip(dm2.uels.iter()).enumerate() {
        let a = serde_json::to_value(&u1.model).unwrap();
        let b = serde_json::to_value(&u2.model).unwrap();
        let mut f = Vec::new();
        compare_json(&format!("uel[{}] bus={}", i, u1.bus), &a, &b, &mut f);
        for ff in &f {
            eprintln!("  {}", ff);
        }
        all_failures.extend(f);
    }

    eprintln!(
        "\nFull text round-trip: {} generators, {} exciters, {} governors, {} pss, {} loads, {} facts, {} oels, {} uels",
        dm1.generators.len(),
        dm1.exciters.len(),
        dm1.governors.len(),
        dm1.pss.len(),
        dm1.loads.len(),
        dm1.facts.len(),
        dm1.oels.len(),
        dm1.uels.len()
    );

    if !all_failures.is_empty() {
        panic!(
            "\n=== {} full text round-trip parameter failures ===\n{}",
            all_failures.len(),
            all_failures.join("\n")
        );
    }
}
