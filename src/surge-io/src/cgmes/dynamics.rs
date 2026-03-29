// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES DY (Dynamics) profile parser.
//!
//! Parses CGMES DY XML files and produces a [`DynamicModel`] that the surge
//! dynamics engine can use directly.
//!
//! ## CGMES DY reference chain
//!
//! A dynamics object (e.g. `ExcIEEEST1A`) links to a generator via:
//!
//! ```text
//! ExcIEEEST1A.SynchronousMachineDynamics  →  SynchronousMachineDynamics
//! SynchronousMachineDynamics.SynchronousMachine  →  SynchronousMachine MRID
//! ```
//!
//! Some files use a shorter direct reference:
//! ```text
//! ExcIEEEST1A.SynchronousMachine  →  SynchronousMachine MRID
//! ```
//!
//! Some PSS objects use an indirect 3-hop reference via the exciter's SMD:
//! ```text
//! PssIEEE2B.ExcitationSystemDynamics  →  SynchronousMachineDynamics MRID
//! SynchronousMachineDynamics.SynchronousMachine  →  SynchronousMachine MRID
//! ```
//!
//! The [`parse_cgmes_dy`] function accepts a pre-built `sm_bus_map` that maps
//! SynchronousMachine MRID → `(bus_number, machine_id)`.  Build this map using
//! `build_sm_bus_map` or by parsing the EQ/SSH profiles first.

use std::collections::HashMap;

use surge_network::dynamics::{
    DynamicModel, Esdc1aParams, Esst1aParams, ExciterDyn, ExciterModel, GastParams, GenclsParams,
    GeneratorDyn, GeneratorModel, GenrouParams, GensalParams, GovernorDyn, GovernorModel,
    HygovParams, Oel1bParams, OelDyn, OelModel, Pss1aParams, Pss2bParams, PssDyn, PssModel,
    ScrxParams, SexsParams, Tgov1Params, Uel1Params, UelDyn, UelModel,
};
use thiserror::Error;

use super::{CgmesError, CimObj, ObjMap, collect_objects};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by the CGMES DY profile parser.
#[derive(Error, Debug)]
pub enum CgmesDyError {
    /// A required parameter was not present in the CIM object.
    #[error("missing required parameter '{0}' on {1}")]
    MissingParam(String, String),
    /// Underlying CGMES XML parse error.
    #[error("CGMES parse error: {0}")]
    Cgmes(#[from] CgmesError),
    /// No SynchronousMachine could be resolved for this dynamics object.
    #[error("could not resolve SynchronousMachine for dynamics object '{0}'")]
    UnresolvedMachine(String),
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parse one or more CGMES DY (Dynamics) profile XML strings and produce a
/// [`DynamicModel`].
///
/// # Arguments
/// * `dy_xml` — slice of DY XML strings (each a complete RDF/XML document).
/// * `sm_bus_map` — mapping from SynchronousMachine mRID → `(bus_number, machine_id)`.
///   Build this with `build_sm_bus_map` after parsing the EQ/SSH profiles.
///
/// # Behaviour for unknown models
/// Unknown CGMES class names are silently logged with [`tracing::warn!`] and
/// skipped.  Missing required parameters return [`CgmesDyError::MissingParam`].
pub fn parse_cgmes_dy(
    dy_xml: &[&str],
    sm_bus_map: &HashMap<String, (u32, String)>,
) -> Result<DynamicModel, CgmesDyError> {
    // Stage 1: collect all DY objects into a unified map.
    let mut objects: ObjMap = ObjMap::new();
    for xml in dy_xml {
        collect_objects(xml, &mut objects)?;
    }

    // Stage 2: build a lookup from SynchronousMachineDynamics mRID → SM mRID.
    // CGMES typically links:  ExcXxx.SynchronousMachineDynamics → SmdXxx
    //                         SmdXxx.SynchronousMachine → SM mRID
    let smd_to_sm: HashMap<String, String> = objects
        .iter()
        .filter(|(_, o)| {
            // Any of the concrete SM dynamics classes
            matches!(
                o.class.as_str(),
                "SynchronousMachineTimeConstantReactance"
                    | "SynchronousMachineSimplified"
                    | "SynchronousMachineEquivalentCircuit"
                    | "SynchronousMachineDetailedFDX"
                    | "SynchronousMachineDetailed"
            )
        })
        .filter_map(|(id, o)| {
            let sm_ref = o.get_ref("SynchronousMachine")?;
            Some((id.clone(), sm_ref.to_string()))
        })
        .collect();

    // Stage 3: for every dynamics object, resolve the SM mRID and emit a record.
    let mut dm = DynamicModel::default();

    for (obj_id, obj) in &objects {
        let cls = obj.class.as_str();

        // Dispatch by class name
        match cls {
            // ----------------------------------------------------------------
            // Generator models
            // ----------------------------------------------------------------
            "SynchronousMachineTimeConstantReactance" => {
                // This IS the SMDynamics object — it references the SM directly.
                let sm_mrid_direct = obj.get_ref("SynchronousMachine").map(|s| s.to_string());
                let effective_sm = sm_mrid_direct.as_deref().unwrap_or("");

                let (bus, machine_id) = match sm_bus_map.get(effective_sm) {
                    Some(pair) => pair.clone(),
                    None => {
                        tracing::warn!(
                            obj_id,
                            sm_mrid = effective_sm,
                            "SynchronousMachineTimeConstantReactance: cannot resolve SM to bus — skipping"
                        );
                        continue;
                    }
                };

                let rotor_type = obj.get_text("rotorType").unwrap_or("roundRotor");
                let is_salient = rotor_type.contains("salientPole")
                    || rotor_type.ends_with("salient")
                    || rotor_type.contains("Salient");

                let h = require_f64(obj, "inertia", obj_id)?;
                let d = obj.parse_f64("damping").unwrap_or(0.0);
                let xd = require_f64(obj, "xDirectSync", obj_id)?;
                let xq = require_f64(obj, "xQuadSync", obj_id)?;
                let xd_prime = require_f64(obj, "xDirectTrans", obj_id)?;
                let xd_pprime = require_f64(obj, "xDirectSubtrans", obj_id)?;
                let xl = obj.parse_f64("statorLeakageReactance").unwrap_or(0.0);
                let td0_prime = require_f64(obj, "tpdo", obj_id)?;
                let td0_pprime = require_f64(obj, "tppdo", obj_id)?;
                let tq0_pprime = require_f64(obj, "tppqo", obj_id)?;
                let s1 = obj.parse_f64("saturationFactor").unwrap_or(0.0);
                let s12 = obj.parse_f64("saturationFactor120").unwrap_or(0.0);

                if is_salient {
                    // GENSAL: no tq0_prime, no xq_prime
                    let xtran = obj.parse_f64("xQuadTrans").unwrap_or(xd_prime);
                    dm.generators.push(GeneratorDyn {
                        bus,
                        machine_id,
                        model: GeneratorModel::Gensal(GensalParams {
                            td0_prime,
                            td0_pprime,
                            tq0_pprime,
                            h,
                            d,
                            xd,
                            xq,
                            xd_prime,
                            xd_pprime,
                            xl,
                            s1,
                            s12,
                            xtran,
                        }),
                    });
                } else {
                    // GENROU: round rotor — needs tq0_prime and xq_prime
                    let tq0_prime = obj.parse_f64("tpqo").unwrap_or(0.4);
                    let xq_prime = obj.parse_f64("xQuadTrans").unwrap_or(xq * 0.6);
                    dm.generators.push(GeneratorDyn {
                        bus,
                        machine_id,
                        model: GeneratorModel::Genrou(GenrouParams {
                            td0_prime,
                            td0_pprime,
                            tq0_prime,
                            tq0_pprime,
                            h,
                            d,
                            xd,
                            xq,
                            xd_prime,
                            xq_prime,
                            xd_pprime,
                            xl,
                            s1,
                            s12,
                            ra: obj.parse_f64("statorResistance"),
                        }),
                    });
                }
            }

            "SynchronousMachineSimplified" => {
                let sm_mrid_direct = obj.get_ref("SynchronousMachine").map(|s| s.to_string());
                let effective_sm = sm_mrid_direct.as_deref().unwrap_or("");

                let (bus, machine_id) = match sm_bus_map.get(effective_sm) {
                    Some(pair) => pair.clone(),
                    None => {
                        tracing::warn!(
                            obj_id,
                            sm_mrid = effective_sm,
                            "SynchronousMachineSimplified: cannot resolve SM to bus — skipping"
                        );
                        continue;
                    }
                };

                let h = require_f64(obj, "inertia", obj_id)?;
                let d = obj.parse_f64("damping").unwrap_or(0.0);
                dm.generators.push(GeneratorDyn {
                    bus,
                    machine_id,
                    model: GeneratorModel::Gencls(GenclsParams { h, d }),
                });
            }

            // ----------------------------------------------------------------
            // Exciter models
            // ----------------------------------------------------------------
            "ExcIEEEST1A" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let tr = obj.parse_f64("tr").unwrap_or(0.0);
                let vimax = obj.parse_f64("vimax").unwrap_or(999.0);
                let vimin = obj.parse_f64("vimin").unwrap_or(-999.0);
                let tc = require_f64(obj, "tc", obj_id)?;
                let tb = require_f64(obj, "tb", obj_id)?;
                let tc1 = obj.parse_f64("tc1").unwrap_or(0.0);
                let tb1 = obj.parse_f64("tb1").unwrap_or(0.0);
                let ka = require_f64(obj, "ka", obj_id)?;
                let ta = obj.parse_f64("ta").unwrap_or(0.0);
                let vamax = obj.parse_f64("vamax").unwrap_or(14.5);
                let vamin = obj.parse_f64("vamin").unwrap_or(-14.5);
                let vrmax = require_f64(obj, "vrmax", obj_id)?;
                let vrmin = require_f64(obj, "vrmin", obj_id)?;
                let kc = obj.parse_f64("kc").unwrap_or(0.0);
                let kf = obj.parse_f64("kf").unwrap_or(0.0);
                let tf = obj.parse_f64("tf").unwrap_or(1.0);
                let klr = obj.parse_f64("klr").unwrap_or(0.0);
                let ilr = obj.parse_f64("ilr").unwrap_or(0.0);
                dm.exciters.push(ExciterDyn {
                    bus,
                    machine_id,
                    model: ExciterModel::Esst1a(Esst1aParams {
                        tr,
                        vimax,
                        vimin,
                        tc,
                        tb,
                        tc1,
                        tb1,
                        ka,
                        ta,
                        vamax,
                        vamin,
                        vrmax,
                        vrmin,
                        kc,
                        kf,
                        tf,
                        klr,
                        ilr,
                    }),
                });
            }

            "ExcDC1A" | "ExcIEEEDC1A" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let tr = obj.parse_f64("tr").unwrap_or(0.0);
                let ka = require_f64(obj, "ka", obj_id)?;
                let ta = require_f64(obj, "ta", obj_id)?;
                let vrmax = require_f64(obj, "vrmax", obj_id)?;
                let vrmin = require_f64(obj, "vrmin", obj_id)?;
                let ke = obj.parse_f64("ke").unwrap_or(1.0);
                let te = require_f64(obj, "te", obj_id)?;
                let kf = require_f64(obj, "kf", obj_id)?;
                let tf = obj
                    .parse_f64("tf1")
                    .or_else(|| obj.parse_f64("tf"))
                    .unwrap_or(1.0);
                let e1 = obj.parse_f64("e1").unwrap_or(0.0);
                let se1 = obj.parse_f64("se1").unwrap_or(0.0);
                let e2 = obj.parse_f64("e2").unwrap_or(0.0);
                let se2 = obj.parse_f64("se2").unwrap_or(0.0);
                dm.exciters.push(ExciterDyn {
                    bus,
                    machine_id,
                    model: ExciterModel::Esdc1a(Esdc1aParams {
                        tr,
                        ka,
                        ta,
                        kf,
                        tf,
                        ke,
                        te,
                        e1,
                        se1,
                        e2,
                        se2,
                        vrmax,
                        vrmin,
                    }),
                });
            }

            "ExcSEXS" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let tb = require_f64(obj, "tb", obj_id)?;
                let tc = require_f64(obj, "tc", obj_id)?;
                let k = require_f64(obj, "k", obj_id)?;
                let te = require_f64(obj, "te", obj_id)?;
                let emin = require_f64(obj, "emin", obj_id)?;
                let emax = require_f64(obj, "emax", obj_id)?;
                dm.exciters.push(ExciterDyn {
                    bus,
                    machine_id,
                    model: ExciterModel::Sexs(SexsParams {
                        tb,
                        tc,
                        k,
                        te,
                        emin,
                        emax,
                    }),
                });
            }

            "ExcSCRX" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let tr = obj.parse_f64("tr").unwrap_or(0.0);
                let k = require_f64(obj, "k", obj_id)?;
                let te = require_f64(obj, "te", obj_id)?;
                let emin = require_f64(obj, "emin", obj_id)?;
                let emax = require_f64(obj, "emax", obj_id)?;
                let rcrfd = obj.parse_f64("rcrfd");
                dm.exciters.push(ExciterDyn {
                    bus,
                    machine_id,
                    model: ExciterModel::Scrx(ScrxParams {
                        tr,
                        k,
                        te,
                        emin,
                        emax,
                        rcrfd,
                    }),
                });
            }

            // ----------------------------------------------------------------
            // Governor models
            // ----------------------------------------------------------------
            "GovGAST" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let r = require_f64(obj, "r", obj_id)?;
                let t1 = require_f64(obj, "t1", obj_id)?;
                let t2 = require_f64(obj, "t2", obj_id)?;
                let t3 = require_f64(obj, "t3", obj_id)?;
                let at = obj.parse_f64("at").unwrap_or(1.0);
                let kt = obj.parse_f64("kt").unwrap_or(2.0);
                let vmin = obj.parse_f64("vmin").unwrap_or(0.0);
                let vmax = obj.parse_f64("vmax").unwrap_or(1.0);
                dm.governors.push(GovernorDyn {
                    bus,
                    machine_id,
                    model: GovernorModel::Gast(GastParams {
                        r,
                        t1,
                        t2,
                        t3,
                        at,
                        kt,
                        vmin,
                        vmax,
                    }),
                });
            }

            "GovSteamIEEE1" | "GovSteam1" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let r = require_f64(obj, "r", obj_id)?;
                let t1 = require_f64(obj, "t1", obj_id)?;
                let vmax = require_f64(obj, "vmax", obj_id)?;
                let vmin = require_f64(obj, "vmin", obj_id)?;
                let t2 = obj.parse_f64("t2").unwrap_or(0.0);
                let t3 = require_f64(obj, "t3", obj_id)?;
                let dt = obj.parse_f64("dt");
                dm.governors.push(GovernorDyn {
                    bus,
                    machine_id,
                    model: GovernorModel::Tgov1(Tgov1Params {
                        r,
                        t1,
                        vmax,
                        vmin,
                        t2,
                        t3,
                        dt,
                    }),
                });
            }

            "GovHydro1" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let r = require_f64(obj, "r", obj_id)?;
                let tp = require_f64(obj, "tp", obj_id)?;
                let velm = obj.parse_f64("velm").unwrap_or(0.2);
                let tg = require_f64(obj, "tg", obj_id)?;
                let gmax = obj.parse_f64("gmax").unwrap_or(1.0);
                let gmin = obj.parse_f64("gmin").unwrap_or(0.0);
                let tw = require_f64(obj, "tw", obj_id)?;
                let at = obj.parse_f64("at").unwrap_or(1.2);
                let dturb = obj.parse_f64("dturb").unwrap_or(0.5);
                let qnl = obj.parse_f64("qnl").unwrap_or(0.08);
                dm.governors.push(GovernorDyn {
                    bus,
                    machine_id,
                    model: GovernorModel::Hygov(HygovParams {
                        r,
                        tp,
                        velm,
                        tg,
                        gmax,
                        gmin,
                        tw,
                        at,
                        dturb,
                        qnl,
                    }),
                });
            }

            // ----------------------------------------------------------------
            // PSS models
            // ----------------------------------------------------------------
            "PssIEEE1A" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let ks = require_f64(obj, "ks", obj_id)?;
                let t1 = require_f64(obj, "t1", obj_id)?;
                let t2 = require_f64(obj, "t2", obj_id)?;
                let t3 = require_f64(obj, "t3", obj_id)?;
                let t4 = require_f64(obj, "t4", obj_id)?;
                let vstmax = require_f64(obj, "vstmax", obj_id)?;
                let vstmin = require_f64(obj, "vstmin", obj_id)?;
                dm.pss.push(PssDyn {
                    bus,
                    machine_id,
                    model: PssModel::Pss1a(Pss1aParams {
                        ks,
                        t1,
                        t2,
                        t3,
                        t4,
                        vstmax,
                        vstmin,
                    }),
                });
            }

            "PssIEEE2B" | "Pss2B" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let m1 = obj.parse_f64("m").unwrap_or(5.0);
                let t6 = obj.parse_f64("t6").unwrap_or(0.0);
                let t7 = require_f64(obj, "t7", obj_id)?;
                let ks2 = obj.parse_f64("ks2").unwrap_or(0.99);
                let t8 = require_f64(obj, "t8", obj_id)?;
                let t9 = require_f64(obj, "t9", obj_id)?;
                let m2 = obj.parse_f64("n").unwrap_or(1.0);
                let tw1 = require_f64(obj, "tw1", obj_id)?;
                let tw2 = require_f64(obj, "tw2", obj_id)?;
                let tw3 = require_f64(obj, "tw3", obj_id)?;
                let tw4 = obj.parse_f64("tw4").unwrap_or(0.0);
                let t1 = require_f64(obj, "t1", obj_id)?;
                let t2 = require_f64(obj, "t2", obj_id)?;
                let t3 = require_f64(obj, "t3", obj_id)?;
                let t4 = require_f64(obj, "t4", obj_id)?;
                let ks1 = require_f64(obj, "ks1", obj_id)?;
                let ks3 = obj.parse_f64("ks3").unwrap_or(1.0);
                let vstmax = require_f64(obj, "vstmax", obj_id)?;
                let vstmin = require_f64(obj, "vstmin", obj_id)?;
                let t10 = obj.parse_f64("t10").unwrap_or(0.0);
                let t11 = obj.parse_f64("t11").unwrap_or(0.0);
                dm.pss.push(PssDyn {
                    bus,
                    machine_id,
                    model: PssModel::Pss2b(Pss2bParams {
                        m1,
                        t6,
                        t7,
                        ks2,
                        t8,
                        t9,
                        m2,
                        tw1,
                        tw2,
                        tw3,
                        tw4,
                        t1,
                        t2,
                        t3,
                        t4,
                        ks1,
                        ks3,
                        vstmax,
                        vstmin,
                        t10,
                        t11,
                    }),
                });
            }

            // ----------------------------------------------------------------
            // OEL / UEL models
            // ----------------------------------------------------------------
            "OverexcLimIEEE" | "OverexcLimX1" | "OverexcLimX2" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let ifdmax = require_f64(obj, "ifdmax", obj_id)?;
                let ifdlim = obj.parse_f64("ifdlim").unwrap_or(ifdmax * 1.05);
                let vrmax = obj.parse_f64("vrmax").unwrap_or(5.0);
                let vamin = obj.parse_f64("vamin").unwrap_or(-5.0);
                let kramp = obj.parse_f64("kramp").unwrap_or(10.0);
                let tff = obj.parse_f64("tff").unwrap_or(0.05);
                dm.oels.push(OelDyn {
                    bus,
                    machine_id,
                    model: OelModel::Oel1b(Oel1bParams {
                        ifdmax,
                        ifdlim,
                        vrmax,
                        vamin,
                        kramp,
                        tff,
                    }),
                });
            }

            "UnderexcLimIEEE1" | "UnderexcLim2Simplified" => {
                let (bus, machine_id) = match resolve_sm(obj, &smd_to_sm, sm_bus_map, obj_id) {
                    Some(pair) => pair,
                    None => continue,
                };
                let kul = require_f64(obj, "kul", obj_id)?;
                let tu1 = obj.parse_f64("tu1").unwrap_or(0.0);
                let vucmax = obj.parse_f64("vucmax").unwrap_or(5.0);
                let vucmin = obj.parse_f64("vucmin").unwrap_or(-5.0);
                let kur = obj.parse_f64("kur").unwrap_or(0.0);
                dm.uels.push(UelDyn {
                    bus,
                    machine_id,
                    model: UelModel::Uel1(Uel1Params {
                        kul,
                        tu1,
                        vucmax,
                        vucmin,
                        kur,
                    }),
                });
            }

            // ----------------------------------------------------------------
            // Known-irrelevant classes in DY profile — skip silently
            // ----------------------------------------------------------------
            "SynchronousMachineDynamics"
            | "SynchronousMachineEquivalentCircuit"
            | "SynchronousMachineDetailedFDX"
            | "SynchronousMachineDetailed"
            | "FullModel"
            | "Model"
            | "Analog"
            | "Control"
            | "Terminal"
            | "TopologicalNode" => {
                // Skip — these are structural/reference objects in the DY profile.
            }

            // ----------------------------------------------------------------
            // Unknown class — warn and continue
            // ----------------------------------------------------------------
            _ => {
                tracing::warn!(
                    class = cls,
                    obj_id,
                    "CGMES DY: unrecognised dynamics class — skipping"
                );
            }
        }
    }

    Ok(dm)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Require a numeric parameter from a CIM object, returning `MissingParam` on failure.
fn require_f64(obj: &CimObj, key: &str, obj_id: &str) -> Result<f64, CgmesDyError> {
    obj.parse_f64(key).ok_or_else(|| {
        CgmesDyError::MissingParam(key.to_string(), format!("{}({})", obj.class, obj_id))
    })
}

/// Resolve a dynamics object's SM mRID and look it up in `sm_bus_map`.
///
/// Tries three reference chains:
/// 1. `obj.SynchronousMachineDynamics` → `smd_to_sm` lookup
/// 2. `obj.SynchronousMachine` directly
/// 3. `obj.ExcitationSystemDynamics` → `smd_to_sm` lookup (PSS 3-hop linkage)
///
/// Returns `None` (after logging a warning) when no SM can be resolved or
/// when the SM mRID is not in `sm_bus_map`.
fn resolve_sm(
    obj: &CimObj,
    smd_to_sm: &HashMap<String, String>,
    sm_bus_map: &HashMap<String, (u32, String)>,
    obj_id: &str,
) -> Option<(u32, String)> {
    let sm_mrid: Option<String> = obj
        .get_ref("SynchronousMachineDynamics")
        .and_then(|smd_id| smd_to_sm.get(smd_id))
        .map(|s| s.to_string())
        .or_else(|| obj.get_ref("SynchronousMachine").map(|s| s.to_string()))
        // Path 3: PSS models reference ExcitationSystemDynamics which points
        // to the SMD object (not an exciter). Same smd_to_sm lookup.
        .or_else(|| {
            obj.get_ref("ExcitationSystemDynamics")
                .and_then(|smd_id| smd_to_sm.get(smd_id))
                .map(|s| s.to_string())
        });

    let sm_mrid = match sm_mrid {
        Some(m) => m,
        None => {
            tracing::warn!(
                class = obj.class.as_str(),
                obj_id,
                "CGMES DY: cannot resolve SynchronousMachine reference — skipping"
            );
            return None;
        }
    };

    match sm_bus_map.get(&sm_mrid) {
        Some(pair) => Some(pair.clone()),
        None => {
            tracing::warn!(
                class = obj.class.as_str(),
                obj_id,
                sm_mrid,
                "CGMES DY: SM mRID not found in bus map (EQ profile may not include this SM) — skipping"
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build sm_bus_map with a single entry
    fn single_sm_map(mrid: &str, bus: u32, id: &str) -> HashMap<String, (u32, String)> {
        let mut m = HashMap::new();
        m.insert(mrid.to_string(), (bus, id.to_string()));
        m
    }

    // -----------------------------------------------------------------------
    // Test 1: round-rotor generator (GENROU)
    // -----------------------------------------------------------------------

    #[test]
    fn test_cgmes_dy_genrou() {
        let dy_xml = r##"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:SynchronousMachineTimeConstantReactance rdf:ID="gen-dyn-001">
    <cim:SynchronousMachineDynamics.SynchronousMachine rdf:resource="#sm-001"/>
    <cim:SynchronousMachineTimeConstantReactance.rotorType>roundRotor</cim:SynchronousMachineTimeConstantReactance.rotorType>
    <cim:RotatingMachineDynamics.inertia>6.5</cim:RotatingMachineDynamics.inertia>
    <cim:RotatingMachineDynamics.damping>0.0</cim:RotatingMachineDynamics.damping>
    <cim:SynchronousMachineTimeConstantReactance.tpdo>8.0</cim:SynchronousMachineTimeConstantReactance.tpdo>
    <cim:SynchronousMachineTimeConstantReactance.tppdo>0.03</cim:SynchronousMachineTimeConstantReactance.tppdo>
    <cim:SynchronousMachineTimeConstantReactance.tpqo>0.4</cim:SynchronousMachineTimeConstantReactance.tpqo>
    <cim:SynchronousMachineTimeConstantReactance.tppqo>0.05</cim:SynchronousMachineTimeConstantReactance.tppqo>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSync>1.8</cim:SynchronousMachineTimeConstantReactance.xDirectSync>
    <cim:SynchronousMachineTimeConstantReactance.xQuadSync>1.7</cim:SynchronousMachineTimeConstantReactance.xQuadSync>
    <cim:SynchronousMachineTimeConstantReactance.xDirectTrans>0.3</cim:SynchronousMachineTimeConstantReactance.xDirectTrans>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>0.25</cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>
    <cim:SynchronousMachineTimeConstantReactance.xQuadTrans>0.55</cim:SynchronousMachineTimeConstantReactance.xQuadTrans>
    <cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>0.2</cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>
  </cim:SynchronousMachineTimeConstantReactance>
</rdf:RDF>"##;

        let sm_bus_map = single_sm_map("sm-001", 1, "1");
        let dm = parse_cgmes_dy(&[dy_xml], &sm_bus_map).unwrap();
        assert_eq!(dm.generators.len(), 1, "should have 1 generator");
        let gdyn = &dm.generators[0];
        assert_eq!(gdyn.bus, 1);
        match &gdyn.model {
            GeneratorModel::Genrou(p) => {
                assert!((p.h - 6.5).abs() < 1e-9, "inertia H");
                assert!((p.xd - 1.8).abs() < 1e-9, "xd");
                assert!((p.xq - 1.7).abs() < 1e-9, "xq");
                assert!((p.td0_prime - 8.0).abs() < 1e-9, "td0'");
                assert!((p.xl - 0.2).abs() < 1e-9, "xl");
            }
            other => panic!("expected Genrou, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 2: salient-pole generator (GENSAL)
    // -----------------------------------------------------------------------

    #[test]
    fn test_cgmes_dy_gensal() {
        let dy_xml = r##"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:SynchronousMachineTimeConstantReactance rdf:ID="gen-dyn-002">
    <cim:SynchronousMachineDynamics.SynchronousMachine rdf:resource="#sm-002"/>
    <cim:SynchronousMachineTimeConstantReactance.rotorType>salientPole</cim:SynchronousMachineTimeConstantReactance.rotorType>
    <cim:RotatingMachineDynamics.inertia>4.0</cim:RotatingMachineDynamics.inertia>
    <cim:RotatingMachineDynamics.damping>2.0</cim:RotatingMachineDynamics.damping>
    <cim:SynchronousMachineTimeConstantReactance.tpdo>5.9</cim:SynchronousMachineTimeConstantReactance.tpdo>
    <cim:SynchronousMachineTimeConstantReactance.tppdo>0.033</cim:SynchronousMachineTimeConstantReactance.tppdo>
    <cim:SynchronousMachineTimeConstantReactance.tppqo>0.078</cim:SynchronousMachineTimeConstantReactance.tppqo>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSync>1.05</cim:SynchronousMachineTimeConstantReactance.xDirectSync>
    <cim:SynchronousMachineTimeConstantReactance.xQuadSync>0.66</cim:SynchronousMachineTimeConstantReactance.xQuadSync>
    <cim:SynchronousMachineTimeConstantReactance.xDirectTrans>0.32</cim:SynchronousMachineTimeConstantReactance.xDirectTrans>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>0.25</cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>
    <cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>0.15</cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>
  </cim:SynchronousMachineTimeConstantReactance>
</rdf:RDF>"##;

        let sm_bus_map = single_sm_map("sm-002", 2, "1");
        let dm = parse_cgmes_dy(&[dy_xml], &sm_bus_map).unwrap();
        assert_eq!(dm.generators.len(), 1);
        match &dm.generators[0].model {
            GeneratorModel::Gensal(p) => {
                assert!((p.h - 4.0).abs() < 1e-9, "H");
                assert!((p.xd - 1.05).abs() < 1e-9, "xd");
                assert!((p.td0_prime - 5.9).abs() < 1e-9, "td0'");
            }
            other => panic!("expected Gensal, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 3: ExcIEEEST1A exciter
    // -----------------------------------------------------------------------

    #[test]
    fn test_cgmes_dy_exciter_st1a() {
        let dy_xml = r##"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:SynchronousMachineTimeConstantReactance rdf:ID="smd-003">
    <cim:SynchronousMachineDynamics.SynchronousMachine rdf:resource="#sm-003"/>
    <cim:SynchronousMachineTimeConstantReactance.rotorType>roundRotor</cim:SynchronousMachineTimeConstantReactance.rotorType>
    <cim:RotatingMachineDynamics.inertia>5.0</cim:RotatingMachineDynamics.inertia>
    <cim:RotatingMachineDynamics.damping>0.0</cim:RotatingMachineDynamics.damping>
    <cim:SynchronousMachineTimeConstantReactance.tpdo>6.0</cim:SynchronousMachineTimeConstantReactance.tpdo>
    <cim:SynchronousMachineTimeConstantReactance.tppdo>0.04</cim:SynchronousMachineTimeConstantReactance.tppdo>
    <cim:SynchronousMachineTimeConstantReactance.tpqo>0.5</cim:SynchronousMachineTimeConstantReactance.tpqo>
    <cim:SynchronousMachineTimeConstantReactance.tppqo>0.05</cim:SynchronousMachineTimeConstantReactance.tppqo>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSync>1.79</cim:SynchronousMachineTimeConstantReactance.xDirectSync>
    <cim:SynchronousMachineTimeConstantReactance.xQuadSync>1.71</cim:SynchronousMachineTimeConstantReactance.xQuadSync>
    <cim:SynchronousMachineTimeConstantReactance.xDirectTrans>0.169</cim:SynchronousMachineTimeConstantReactance.xDirectTrans>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>0.135</cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>
    <cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>0.13</cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>
  </cim:SynchronousMachineTimeConstantReactance>
  <cim:ExcIEEEST1A rdf:ID="exc-003">
    <cim:ExcitationSystemDynamics.SynchronousMachineDynamics rdf:resource="#smd-003"/>
    <cim:ExcIEEEST1A.tc>10.0</cim:ExcIEEEST1A.tc>
    <cim:ExcIEEEST1A.tb>10.0</cim:ExcIEEEST1A.tb>
    <cim:ExcIEEEST1A.ka>200.0</cim:ExcIEEEST1A.ka>
    <cim:ExcIEEEST1A.vrmax>6.43</cim:ExcIEEEST1A.vrmax>
    <cim:ExcIEEEST1A.vrmin>-6.43</cim:ExcIEEEST1A.vrmin>
  </cim:ExcIEEEST1A>
</rdf:RDF>"##;

        let sm_bus_map = single_sm_map("sm-003", 3, "1");
        let dm = parse_cgmes_dy(&[dy_xml], &sm_bus_map).unwrap();
        assert_eq!(dm.generators.len(), 1, "generator");
        assert_eq!(dm.exciters.len(), 1, "exciter");
        let exc = &dm.exciters[0];
        assert_eq!(exc.bus, 3);
        match &exc.model {
            ExciterModel::Esst1a(p) => {
                assert!((p.ka - 200.0).abs() < 1e-9, "ka");
                assert!((p.vrmax - 6.43).abs() < 1e-9, "vrmax");
            }
            other => panic!("expected Esst1a, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 4: GovGAST governor
    // -----------------------------------------------------------------------

    #[test]
    fn test_cgmes_dy_governor_gast() {
        let dy_xml = r##"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:SynchronousMachineTimeConstantReactance rdf:ID="smd-004">
    <cim:SynchronousMachineDynamics.SynchronousMachine rdf:resource="#sm-004"/>
    <cim:SynchronousMachineTimeConstantReactance.rotorType>roundRotor</cim:SynchronousMachineTimeConstantReactance.rotorType>
    <cim:RotatingMachineDynamics.inertia>7.0</cim:RotatingMachineDynamics.inertia>
    <cim:RotatingMachineDynamics.damping>0.0</cim:RotatingMachineDynamics.damping>
    <cim:SynchronousMachineTimeConstantReactance.tpdo>7.0</cim:SynchronousMachineTimeConstantReactance.tpdo>
    <cim:SynchronousMachineTimeConstantReactance.tppdo>0.04</cim:SynchronousMachineTimeConstantReactance.tppdo>
    <cim:SynchronousMachineTimeConstantReactance.tpqo>0.5</cim:SynchronousMachineTimeConstantReactance.tpqo>
    <cim:SynchronousMachineTimeConstantReactance.tppqo>0.05</cim:SynchronousMachineTimeConstantReactance.tppqo>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSync>1.8</cim:SynchronousMachineTimeConstantReactance.xDirectSync>
    <cim:SynchronousMachineTimeConstantReactance.xQuadSync>1.7</cim:SynchronousMachineTimeConstantReactance.xQuadSync>
    <cim:SynchronousMachineTimeConstantReactance.xDirectTrans>0.3</cim:SynchronousMachineTimeConstantReactance.xDirectTrans>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>0.25</cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>
    <cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>0.2</cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>
  </cim:SynchronousMachineTimeConstantReactance>
  <cim:GovGAST rdf:ID="gov-004">
    <cim:TurbineGovernorDynamics.SynchronousMachineDynamics rdf:resource="#smd-004"/>
    <cim:GovGAST.r>0.05</cim:GovGAST.r>
    <cim:GovGAST.t1>0.5</cim:GovGAST.t1>
    <cim:GovGAST.t2>3.0</cim:GovGAST.t2>
    <cim:GovGAST.t3>10.0</cim:GovGAST.t3>
    <cim:GovGAST.at>1.0</cim:GovGAST.at>
    <cim:GovGAST.kt>2.0</cim:GovGAST.kt>
    <cim:GovGAST.voltage_min_pu>0.0</cim:GovGAST.voltage_min_pu>
    <cim:GovGAST.voltage_max_pu>1.0</cim:GovGAST.voltage_max_pu>
  </cim:GovGAST>
</rdf:RDF>"##;

        let sm_bus_map = single_sm_map("sm-004", 4, "G4");
        let dm = parse_cgmes_dy(&[dy_xml], &sm_bus_map).unwrap();
        assert_eq!(dm.governors.len(), 1, "governor");
        let gov = &dm.governors[0];
        assert_eq!(gov.bus, 4);
        assert_eq!(gov.machine_id, "G4");
        match &gov.model {
            GovernorModel::Gast(p) => {
                assert!((p.r - 0.05).abs() < 1e-9, "r");
                assert!((p.t1 - 0.5).abs() < 1e-9, "t1");
                assert!((p.t3 - 10.0).abs() < 1e-9, "t3");
            }
            other => panic!("expected Gast, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 5: PssIEEE2B
    // -----------------------------------------------------------------------

    #[test]
    fn test_cgmes_dy_pss_ieee2b() {
        let dy_xml = r##"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:SynchronousMachineTimeConstantReactance rdf:ID="smd-005">
    <cim:SynchronousMachineDynamics.SynchronousMachine rdf:resource="#sm-005"/>
    <cim:SynchronousMachineTimeConstantReactance.rotorType>roundRotor</cim:SynchronousMachineTimeConstantReactance.rotorType>
    <cim:RotatingMachineDynamics.inertia>3.0</cim:RotatingMachineDynamics.inertia>
    <cim:RotatingMachineDynamics.damping>0.0</cim:RotatingMachineDynamics.damping>
    <cim:SynchronousMachineTimeConstantReactance.tpdo>6.0</cim:SynchronousMachineTimeConstantReactance.tpdo>
    <cim:SynchronousMachineTimeConstantReactance.tppdo>0.04</cim:SynchronousMachineTimeConstantReactance.tppdo>
    <cim:SynchronousMachineTimeConstantReactance.tpqo>0.5</cim:SynchronousMachineTimeConstantReactance.tpqo>
    <cim:SynchronousMachineTimeConstantReactance.tppqo>0.05</cim:SynchronousMachineTimeConstantReactance.tppqo>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSync>1.8</cim:SynchronousMachineTimeConstantReactance.xDirectSync>
    <cim:SynchronousMachineTimeConstantReactance.xQuadSync>1.7</cim:SynchronousMachineTimeConstantReactance.xQuadSync>
    <cim:SynchronousMachineTimeConstantReactance.xDirectTrans>0.3</cim:SynchronousMachineTimeConstantReactance.xDirectTrans>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>0.25</cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>
    <cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>0.2</cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>
  </cim:SynchronousMachineTimeConstantReactance>
  <cim:PssIEEE2B rdf:ID="pss-005">
    <cim:PowerSystemStabilizerDynamics.ExcitationSystemDynamics rdf:resource="#smd-005"/>
    <cim:PssIEEE2B.tw1>10.0</cim:PssIEEE2B.tw1>
    <cim:PssIEEE2B.tw2>10.0</cim:PssIEEE2B.tw2>
    <cim:PssIEEE2B.tw3>2.0</cim:PssIEEE2B.tw3>
    <cim:PssIEEE2B.t1>0.12</cim:PssIEEE2B.t1>
    <cim:PssIEEE2B.t2>0.02</cim:PssIEEE2B.t2>
    <cim:PssIEEE2B.t3>0.3</cim:PssIEEE2B.t3>
    <cim:PssIEEE2B.t4>0.15</cim:PssIEEE2B.t4>
    <cim:PssIEEE2B.t7>2.0</cim:PssIEEE2B.t7>
    <cim:PssIEEE2B.t8>0.5</cim:PssIEEE2B.t8>
    <cim:PssIEEE2B.t9>0.1</cim:PssIEEE2B.t9>
    <cim:PssIEEE2B.ks1>12.0</cim:PssIEEE2B.ks1>
    <cim:PssIEEE2B.vstmax>0.1</cim:PssIEEE2B.vstmax>
    <cim:PssIEEE2B.vstmin>-0.1</cim:PssIEEE2B.vstmin>
  </cim:PssIEEE2B>
</rdf:RDF>"##;

        let sm_bus_map = single_sm_map("sm-005", 5, "1");
        let dm = parse_cgmes_dy(&[dy_xml], &sm_bus_map).unwrap();
        // PSS links via ExcitationSystemDynamics → SMD → SM (3-hop chain).
        // resolve_sm() now handles the ExcitationSystemDynamics path.
        assert_eq!(
            dm.generators.len(),
            1,
            "generator from SynchronousMachineTimeConstantReactance"
        );
        assert_eq!(
            dm.pss.len(),
            1,
            "PSS resolved via ExcitationSystemDynamics 3-hop"
        );
        assert_eq!(dm.pss[0].bus, 5);
        assert_eq!(dm.pss[0].machine_id, "1");
    }

    // -----------------------------------------------------------------------
    // Test 6: unknown class doesn't error
    // -----------------------------------------------------------------------

    #[test]
    fn test_cgmes_dy_unsupported_warns() {
        let dy_xml = r##"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:SomeFutureModel2035 rdf:ID="future-001">
    <cim:SomeFutureModel2035.SynchronousMachine rdf:resource="#sm-006"/>
    <cim:SomeFutureModel2035.param1>42.0</cim:SomeFutureModel2035.param1>
  </cim:SomeFutureModel2035>
</rdf:RDF>"##;

        let sm_bus_map = single_sm_map("sm-006", 6, "1");
        // Must not return an error — unknown classes are silently warned and skipped.
        let dm = parse_cgmes_dy(&[dy_xml], &sm_bus_map).unwrap();
        assert_eq!(dm.generators.len(), 0);
        assert_eq!(dm.exciters.len(), 0);
        assert_eq!(dm.governors.len(), 0);
    }

    // -----------------------------------------------------------------------
    // Test 7: full machine — generator + exciter + governor linked to same SM
    // -----------------------------------------------------------------------

    #[test]
    fn test_cgmes_dy_full_machine() {
        let dy_xml = r##"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/CIM100#">
  <!-- Generator dynamics -->
  <cim:SynchronousMachineTimeConstantReactance rdf:ID="smd-007">
    <cim:SynchronousMachineDynamics.SynchronousMachine rdf:resource="#sm-007"/>
    <cim:SynchronousMachineTimeConstantReactance.rotorType>roundRotor</cim:SynchronousMachineTimeConstantReactance.rotorType>
    <cim:RotatingMachineDynamics.inertia>6.0</cim:RotatingMachineDynamics.inertia>
    <cim:RotatingMachineDynamics.damping>0.0</cim:RotatingMachineDynamics.damping>
    <cim:SynchronousMachineTimeConstantReactance.tpdo>8.0</cim:SynchronousMachineTimeConstantReactance.tpdo>
    <cim:SynchronousMachineTimeConstantReactance.tppdo>0.03</cim:SynchronousMachineTimeConstantReactance.tppdo>
    <cim:SynchronousMachineTimeConstantReactance.tpqo>0.4</cim:SynchronousMachineTimeConstantReactance.tpqo>
    <cim:SynchronousMachineTimeConstantReactance.tppqo>0.05</cim:SynchronousMachineTimeConstantReactance.tppqo>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSync>1.8</cim:SynchronousMachineTimeConstantReactance.xDirectSync>
    <cim:SynchronousMachineTimeConstantReactance.xQuadSync>1.7</cim:SynchronousMachineTimeConstantReactance.xQuadSync>
    <cim:SynchronousMachineTimeConstantReactance.xDirectTrans>0.3</cim:SynchronousMachineTimeConstantReactance.xDirectTrans>
    <cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>0.25</cim:SynchronousMachineTimeConstantReactance.xDirectSubtrans>
    <cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>0.2</cim:SynchronousMachineTimeConstantReactance.statorLeakageReactance>
  </cim:SynchronousMachineTimeConstantReactance>
  <!-- Exciter directly referencing the SM dynamics via SynchronousMachineDynamics -->
  <cim:ExcIEEEST1A rdf:ID="exc-007">
    <cim:ExcitationSystemDynamics.SynchronousMachineDynamics rdf:resource="#smd-007"/>
    <cim:ExcIEEEST1A.tc>10.0</cim:ExcIEEEST1A.tc>
    <cim:ExcIEEEST1A.tb>10.0</cim:ExcIEEEST1A.tb>
    <cim:ExcIEEEST1A.ka>200.0</cim:ExcIEEEST1A.ka>
    <cim:ExcIEEEST1A.vrmax>6.43</cim:ExcIEEEST1A.vrmax>
    <cim:ExcIEEEST1A.vrmin>-6.43</cim:ExcIEEEST1A.vrmin>
  </cim:ExcIEEEST1A>
  <!-- Governor directly referencing the SM dynamics -->
  <cim:GovGAST rdf:ID="gov-007">
    <cim:TurbineGovernorDynamics.SynchronousMachineDynamics rdf:resource="#smd-007"/>
    <cim:GovGAST.r>0.05</cim:GovGAST.r>
    <cim:GovGAST.t1>0.5</cim:GovGAST.t1>
    <cim:GovGAST.t2>3.0</cim:GovGAST.t2>
    <cim:GovGAST.t3>10.0</cim:GovGAST.t3>
  </cim:GovGAST>
</rdf:RDF>"##;

        let sm_bus_map = single_sm_map("sm-007", 7, "G7");
        let dm = parse_cgmes_dy(&[dy_xml], &sm_bus_map).unwrap();
        assert_eq!(dm.generators.len(), 1, "generator");
        assert_eq!(dm.exciters.len(), 1, "exciter");
        assert_eq!(dm.governors.len(), 1, "governor");

        // All must reference the same bus + machine_id
        assert_eq!(dm.generators[0].bus, 7);
        assert_eq!(dm.generators[0].machine_id, "G7");
        assert_eq!(dm.exciters[0].bus, 7);
        assert_eq!(dm.exciters[0].machine_id, "G7");
        assert_eq!(dm.governors[0].bus, 7);
        assert_eq!(dm.governors[0].machine_id, "G7");
    }

    // -----------------------------------------------------------------------
    // Test 8: SynchronousMachineSimplified → GENCLS
    // -----------------------------------------------------------------------

    #[test]
    fn test_cgmes_dy_gencls() {
        let dy_xml = r##"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:cim="http://iec.ch/TC57/CIM100#">
  <cim:SynchronousMachineSimplified rdf:ID="sms-008">
    <cim:SynchronousMachineDynamics.SynchronousMachine rdf:resource="#sm-008"/>
    <cim:RotatingMachineDynamics.inertia>3.0</cim:RotatingMachineDynamics.inertia>
    <cim:RotatingMachineDynamics.damping>1.0</cim:RotatingMachineDynamics.damping>
  </cim:SynchronousMachineSimplified>
</rdf:RDF>"##;

        let sm_bus_map = single_sm_map("sm-008", 8, "CLK");
        let dm = parse_cgmes_dy(&[dy_xml], &sm_bus_map).unwrap();
        assert_eq!(dm.generators.len(), 1);
        match &dm.generators[0].model {
            GeneratorModel::Gencls(p) => {
                assert!((p.h - 3.0).abs() < 1e-9);
                assert!((p.d - 1.0).abs() < 1e-9);
            }
            other => panic!("expected Gencls, got {other:?}"),
        }
    }
}
