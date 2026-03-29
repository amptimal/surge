// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E `.dyr` dynamic data file writer.
//!
//! Serializes a [`DynamicModel`] back into the PSS/E DYR text format.
//! Each record is emitted as:
//! ```text
//!   bus_number 'MODEL_NAME' machine_id  param1 param2 ... paramN /
//! ```
//!
//! # Limitations
//!
//! Some reader builders apply non-trivial transformations (defaults, derived
//! quantities, index remapping).  The writer emits the *stored* parameter
//! values in DYR-compatible order, so a read-write round-trip may not produce
//! a byte-identical file but will be semantically equivalent.

use std::fmt::Write as FmtWrite;
use std::path::Path;

use surge_network::dynamics::*;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum DyrWriteError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("format error: {0}")]
    Fmt(#[from] std::fmt::Error),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Write a [`DynamicModel`] to a PSS/E `.dyr` file.
pub fn write_dyr(model: &DynamicModel, path: &Path) -> Result<(), DyrWriteError> {
    let content = to_dyr_string(model)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Serialize a [`DynamicModel`] to a PSS/E `.dyr` format string.
pub fn to_dyr_string(model: &DynamicModel) -> Result<String, DyrWriteError> {
    let mut out = String::new();

    // Generators
    for g in &model.generators {
        let (name, params) = generator_to_dyr(&g.model);
        write_record(&mut out, g.bus, name, &g.machine_id, &params)?;
    }

    // Exciters
    for e in &model.exciters {
        let (name, params) = exciter_to_dyr(&e.model);
        write_record(&mut out, e.bus, name, &e.machine_id, &params)?;
    }

    // Governors
    for g in &model.governors {
        let (name, params) = governor_to_dyr(&g.model);
        write_record(&mut out, g.bus, name, &g.machine_id, &params)?;
    }

    // PSS
    for p in &model.pss {
        let (name, params) = pss_to_dyr(&p.model);
        write_record(&mut out, p.bus, name, &p.machine_id, &params)?;
    }

    // Loads
    for l in &model.loads {
        let (name, params) = load_to_dyr(&l.model);
        write_record(&mut out, l.bus, name, &l.load_id, &params)?;
    }

    // FACTS
    for f in &model.facts {
        let (name, params) = facts_to_dyr(&f.model);
        write_record(&mut out, f.bus, name, &f.device_id, &params)?;
    }

    // OEL limiters
    for o in &model.oels {
        let (name, params) = oel_to_dyr(&o.model);
        write_record(&mut out, o.bus, name, &o.machine_id, &params)?;
    }

    // UEL limiters
    for u in &model.uels {
        let (name, params) = uel_to_dyr(&u.model);
        write_record(&mut out, u.bus, name, &u.machine_id, &params)?;
    }

    // Unknown records (preserved verbatim)
    for u in &model.unknown_records {
        write_record(&mut out, u.bus, &u.model_name, &u.machine_id, &u.params)?;
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Record formatting
// ---------------------------------------------------------------------------

/// Format a single DYR record line.
fn write_record(
    out: &mut String,
    bus: u32,
    model_name: &str,
    machine_id: &str,
    params: &[f64],
) -> Result<(), DyrWriteError> {
    write!(out, "  {} '{}' {}", bus, model_name, machine_id)?;
    for v in params {
        write!(out, " {}", fmt_param(*v))?;
    }
    writeln!(out, " /")?;
    Ok(())
}

/// Format a floating-point parameter for DYR output.
///
/// Integers are emitted without a decimal point (e.g. `1` not `1.0`).
/// Small values use full precision; others use up to 12 significant digits.
fn fmt_param(v: f64) -> String {
    // If the value is exactly an integer and fits in a reasonable range,
    // emit it without a decimal point for readability.
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        // Use enough precision to round-trip; trim trailing zeros.
        let s = format!("{:.12}", v);
        let s = s.trim_end_matches('0');
        let s = s.trim_end_matches('.');
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Generator model → DYR params
// ---------------------------------------------------------------------------

fn generator_to_dyr(model: &GeneratorModel) -> (&'static str, Vec<f64>) {
    match model {
        GeneratorModel::Gencls(p) => ("GENCLS", vec![p.h, p.d]),
        GeneratorModel::Genrou(p) => ("GENROU", genrou_params(p)),
        GeneratorModel::Gensal(p) => ("GENSAL", gensal_params(p)),
        GeneratorModel::Regca(p) => (
            "REGCA",
            vec![
                0.0, p.tg, p.rrpwr, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, p.imax, p.tfltr, p.x_eq,
            ],
        ),
        GeneratorModel::Gentpj(p) => ("GENTPJ", gentpj_params(p)),
        GeneratorModel::Genqec(p) => ("GENQEC", genqec_params(p)),
        GeneratorModel::Regcb(p) => (
            "REGCB",
            vec![
                0.0, p.tg, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, p.imax, p.tfltr, p.tip, p.kp_pll,
                p.ki_pll, p.x_eq,
            ],
        ),
        GeneratorModel::Wt3g2u(p) => (
            "WT3G2U",
            vec![
                p.tg, p.x_eq, 0.0, 0.0, p.tfltr, 0.0, p.imax, p.kpll, p.kipll, p.h_rotor, p.d_rotor,
            ],
        ),
        GeneratorModel::Wt4g1(p) => ("WT4G1", vec![p.tg, p.x_eq, p.imax]),
        GeneratorModel::RegfmA1(p) => ("REGFM_A1", vec![p.x_eq, p.h, p.d, p.imax, p.tg]),
        GeneratorModel::RegfmB1(p) => ("REGFM_B1", vec![p.x_eq, p.h, p.d, p.imax, p.tg]),
        GeneratorModel::Dera(p) => ("DERA", vec![p.x_eq, p.trf, p.imax, p.trv]),
        GeneratorModel::Gentra(p) => {
            let mut v = vec![p.h, p.d, p.ra, p.xd, p.xd_prime, p.td0_prime, p.xq];
            if p.s1.abs() > 1e-15 || p.s12.abs() > 1e-15 {
                v.push(p.s1);
                v.push(p.s12);
            }
            ("GENTRA", v)
        }
        GeneratorModel::Gentpf(p) => ("GENTPF", gentpj_params(p)),
        GeneratorModel::Regcc(p) => ("REGCC", vec![p.tg, p.x_eq, p.imax, p.tfltr, p.t_pll]),
        GeneratorModel::Wt4g2(p) => ("WT4G2", vec![p.tg, p.x_eq, p.imax]),
        GeneratorModel::Derc(p) => ("DERC", vec![p.tp, p.tq, p.tv, p.mbase, p.lfac, p.x_eq]),
        GeneratorModel::Genroa(p) => ("GENROA", genrou_params(p)),
        GeneratorModel::Gensaa(p) => ("GENSAA", gensal_params(p)),
        GeneratorModel::RegfmC1(p) => (
            "REGFM_C1",
            vec![
                p.kd, p.ki, p.kq, p.tg, p.ddn, p.dup, p.pmax, p.pmin, p.qmax, p.qmin, p.mbase,
            ],
        ),
        GeneratorModel::Pvgu1(p) => (
            "PVGU1",
            vec![
                p.lvplsw, p.rrpwr, p.brkpt, p.zerox, p.lvpl1, p.volim, p.lvpnt1, p.lvpnt0, p.iolim,
                p.tfltr, p.khv, p.iqrmax, p.iqrmin, p.accel, p.vsmax, p.mbase,
            ],
        ),
        GeneratorModel::Pvdg(p) => (
            "PVDG",
            vec![
                p.tp, p.tq, p.vtrip1, p.vtrip2, p.vtrip3, p.ftrip1, p.ftrip2, p.pmax, p.qmax,
                p.qmin, p.mbase,
            ],
        ),
        GeneratorModel::Wt3g3(p) => (
            "WT3G3",
            vec![p.tg, p.x_eq, 0.0, 0.0, p.tfltr, 0.0, p.imax, p.kpll],
        ),
        GeneratorModel::Regco1(p) => (
            "REGCO1",
            vec![
                p.tr, p.kp_v, p.ki_v, p.kp_i, p.ki_i, p.vmax, p.vmin, p.iqmax, p.iqmin, p.pmax,
                p.pmin, p.mbase,
            ],
        ),
        GeneratorModel::Genwtg(p) => ("GENWTG", genrou_params(p)),
        GeneratorModel::Genroe(p) => ("GENROE", genrou_params(p)),
        GeneratorModel::Gensal3(p) => (
            "GENSAL3",
            vec![
                p.td0_prime,
                p.h,
                p.d,
                p.xd,
                p.xq,
                p.xd_prime,
                p.xl,
                p.s1,
                p.s12,
            ],
        ),
        GeneratorModel::Derp(p) => (
            "DERP",
            vec![
                p.x_eq,
                p.trf,
                p.imax,
                p.trv,
                p.flow,
                p.fhigh,
                p.vlow,
                p.vhigh,
                p.trip,
                p.treconnect,
                p.tpll,
            ],
        ),
        GeneratorModel::RegfmD1(p) => (
            "REGFM_D1",
            vec![
                p.rrv, p.lrv, p.kpv, p.kiv, p.kpg, p.kig, p.kdroop, p.kvir, p.kfir, p.imax, p.dpf,
                p.dqf, p.x_eq, p.mbase, p.tpll, p.tv,
            ],
        ),
        GeneratorModel::Gensae(p) => ("GENSAE", gensal_params(p)),
        GeneratorModel::Wt1g1(p) => ("WT1G1", vec![p.h, p.d, p.ra, p.x_eq, p.imax]),
        GeneratorModel::Wt2g1(p) => ("WT2G1", vec![p.h, p.d, p.ra, p.x_eq, p.imax]),
        GeneratorModel::Pvd1(p) => (
            "PVD1",
            vec![
                p.tp, p.tq, p.vtrip1, p.vtrip2, p.vtrip3, p.ftrip1, p.ftrip2, p.pmax, p.qmax,
                p.qmin, p.mbase,
            ],
        ),
        GeneratorModel::Pvdu1(p) => (
            "PVDU1",
            vec![
                p.lvplsw, p.rrpwr, p.brkpt, p.zerox, p.lvpl1, p.volim, p.lvpnt1, p.lvpnt0, p.iolim,
                p.tfltr, p.khv, p.iqrmax, p.iqrmin, p.accel, p.vsmax, p.mbase,
            ],
        ),
    }
}

fn genrou_params(p: &GenrouParams) -> Vec<f64> {
    let mut v = vec![
        p.td0_prime,
        p.td0_pprime,
        p.tq0_prime,
        p.tq0_pprime,
        p.h,
        p.d,
        p.xd,
        p.xq,
        p.xd_prime,
        p.xq_prime,
        p.xd_pprime,
        p.xl,
        p.s1,
        p.s12,
    ];
    if let Some(ra) = p.ra {
        v.push(ra);
    }
    v
}

fn gensal_params(p: &GensalParams) -> Vec<f64> {
    vec![
        p.td0_prime,
        p.td0_pprime,
        p.tq0_pprime,
        p.h,
        p.d,
        p.xd,
        p.xq,
        p.xd_prime,
        p.xd_pprime,
        p.xl,
        p.s1,
        p.s12,
        p.xtran,
    ]
}

fn gentpj_params(p: &GentpjParams) -> Vec<f64> {
    let mut v = vec![
        p.td0_prime,
        p.td0_pprime,
        p.tq0_prime,
        p.tq0_pprime,
        p.h,
        p.d,
        p.xd,
        p.xq,
        p.xd_prime,
        p.xq_prime,
        p.xd_pprime,
        p.xl,
        p.s1,
        p.s12,
    ];
    if let Some(kii) = p.kii {
        v.push(kii);
    }
    if let Some(ra) = p.ra {
        // Ensure kii slot is filled if ra is present
        if p.kii.is_none() {
            v.push(0.0);
        }
        v.push(ra);
    }
    v
}

fn genqec_params(p: &GenqecParams) -> Vec<f64> {
    let mut v = vec![
        p.td0_prime,
        p.td0_pprime,
        p.tq0_prime,
        p.tq0_pprime,
        p.h,
        p.d,
        p.xd,
        p.xq,
        p.xd_prime,
        p.xq_prime,
        p.xd_pprime,
        p.xl,
        p.s1,
        p.s12,
    ];
    if let Some(ra) = p.ra {
        v.push(ra);
    }
    v
}

// ---------------------------------------------------------------------------
// Exciter model → DYR params
// ---------------------------------------------------------------------------

fn exciter_to_dyr(model: &ExciterModel) -> (&'static str, Vec<f64>) {
    match model {
        ExciterModel::Exst1(p) => {
            let mut v = vec![
                p.tr, p.vimax, p.vimin, p.tc, p.tb, p.ka, p.ta, p.vrmax, p.vrmin, p.kc, p.kf, p.tf,
            ];
            push_opt(&mut v, p.klr);
            push_opt(&mut v, p.ilr);
            ("EXST1", v)
        }
        ExciterModel::Esst3a(p) => (
            "ESST3A",
            vec![
                p.tr, p.vimax, p.vimin, p.km, p.tc, p.tb, p.ka, p.ta, p.vrmax, p.vrmin, p.kg, p.kp,
                p.ki, p.vbmax,
            ],
        ),
        ExciterModel::Esdc2a(p) => (
            "ESDC2A",
            vec![
                p.tr, p.ka, p.ta, p.tb, p.tc, p.vrmax, p.vrmin, p.ke, p.te, p.kf, p.tf1, p.switch_,
            ],
        ),
        ExciterModel::Exdc2(p) => {
            let mut v = vec![
                p.tr, p.ka, p.ta, p.tb, p.tc, p.vrmax, p.vrmin, p.ke, p.te, p.kf, p.tf1, p.switch_,
            ];
            push_opt(&mut v, p.e1);
            push_opt(&mut v, p.se1);
            push_opt(&mut v, p.e2);
            push_opt(&mut v, p.se2);
            ("EXDC2", v)
        }
        ExciterModel::Ieeex1(p) => {
            let mut v = vec![
                p.tr, p.ka, p.ta, p.tb, p.tc, p.vrmax, p.vrmin, p.ke, p.te, p.kf, p.tf, p.aex,
                p.bex,
            ];
            push_opt(&mut v, p.e1);
            push_opt(&mut v, p.se1);
            push_opt(&mut v, p.e2);
            push_opt(&mut v, p.se2);
            ("IEEEX1", v)
        }
        ExciterModel::Sexs(p) => (
            "SEXS",
            vec![
                if p.tb.abs() > 1e-10 { p.tc / p.tb } else { 0.0 }, // TA/TB ratio
                p.tb,
                p.k,
                p.te,
                p.emin,
                p.emax,
            ],
        ),
        ExciterModel::Ieeet1(p) => {
            let mut v = vec![
                p.tr, p.ka, p.ta, p.ke, p.te, p.kf, p.tf, p.e1, p.se1, p.e2, p.se2,
            ];
            push_opt(&mut v, p.vrmax);
            push_opt(&mut v, p.vrmin);
            ("IEEET1", v)
        }
        ExciterModel::Scrx(p) => {
            let mut v = vec![p.tr, p.k, p.te, p.emin, p.emax];
            push_opt(&mut v, p.rcrfd);
            ("SCRX", v)
        }
        ExciterModel::Reeca(p) => (
            "REECA",
            vec![
                p.vdip, p.vup, p.trv, p.dbd1, p.dbd2, p.kqv, p.iqh1, p.iql1, p.vref0, p.tp, p.qmax,
                p.qmin, 0.0, 0.0, p.kqp, p.kqi, p.tpfilt, p.tqfilt, p.rrpwr, p.rrpwr_dn,
            ],
        ),
        ExciterModel::Esst1a(p) => (
            "ESST1A",
            vec![
                p.tr, p.vimax, p.vimin, p.tc, p.tb, p.tc1, p.tb1, p.ka, p.ta, p.vamax, p.vamin,
                p.vrmax, p.vrmin, p.kc, p.kf, p.tf, p.klr, p.ilr,
            ],
        ),
        ExciterModel::Exac1(p) => (
            "EXAC1",
            vec![
                p.tr, p.tb, p.tc, p.ka, p.ta, p.vrmax, p.vrmin, p.te, p.kf, p.tf, p.kc, p.kd, p.ke,
                p.e1, p.se1, p.e2, p.se2,
            ],
        ),
        ExciterModel::Esac1a(p) => (
            "ESAC1A",
            vec![
                p.tr, p.tb, p.tc, p.ka, p.ta, p.vrmax, p.vrmin, p.te, p.kf, p.tf, p.kc, p.kd, p.ke,
                p.e1, p.se1, p.e2, p.se2,
            ],
        ),
        ExciterModel::Esac7b(p) => ("ESAC7B", esac7b_params(p)),
        ExciterModel::Esst4b(p) => (
            "ESST4B",
            vec![
                p.tr, p.kpr, p.kir, p.vrmax, p.vrmin, p.kpm, p.kim, p.vmmax, p.vmmin, p.kg, p.kp,
                p.ki, p.vbmax, p.vgmax,
            ],
        ),
        ExciterModel::Reecd(p) => (
            "REECD",
            vec![
                p.dbd1, p.dbd2, p.kqv, p.kqi, p.trv, p.tp, p.iqmax, p.iqmin, p.ipmax, p.rrpwr,
                p.ddn, p.dup, p.fdbd1, p.fdbd2, p.vdip, p.vup, p.pref, p.pmax, p.pmin,
            ],
        ),
        ExciterModel::Reeccu(p) => (
            "REECCU",
            vec![
                p.dbd1, p.kqv, p.kqi, p.trv, p.tp, p.rrpwr, p.vdip, p.vup, p.pref, p.pmax, p.pmin,
            ],
        ),
        ExciterModel::Rexs(p) => (
            "REXS",
            vec![p.te, p.tf, p.ke, p.kf, p.efd1, p.efd2, p.sefd1, p.sefd2],
        ),
        ExciterModel::Esac2a(p) => (
            "ESAC2A",
            vec![
                p.tr, p.tb, p.tc, p.ka, p.ta, p.vamax, p.vamin, p.vrmax, p.vrmin, p.ke, p.te, p.kf,
                p.tf, p.e1, p.se1, p.e2, p.se2, p.kb, p.kc, p.kd, p.kh,
            ],
        ),
        ExciterModel::Esac5a(p) => (
            "ESAC5A",
            vec![
                p.ka, p.ta, p.ke, p.te, p.kf, p.tf, p.e1, p.se1, p.e2, p.se2, p.vrmax, p.vrmin,
            ],
        ),
        ExciterModel::Esst5b(p) => (
            "ESST5B",
            vec![
                p.tr, p.kc, p.kf, p.tf, p.ka, p.tb, p.tc, p.vrmax, p.vrmin, p.t1, p.t2,
            ],
        ),
        ExciterModel::Exac4(p) => (
            "EXAC4",
            vec![p.tr, p.tc, p.tb, p.ka, p.ta, p.vrmax, p.vrmin, p.kc],
        ),
        ExciterModel::Esst6b(p) => (
            "ESST6B",
            vec![
                p.tr, p.ilr, p.klr, p.ka, p.ta, p.kc, p.vrmax, p.vrmin, p.kff, p.kgff, p.t1, p.t2,
            ],
        ),
        ExciterModel::Esst7b(p) => (
            "ESST7B",
            vec![
                p.tr, p.kpa, p.kia, p.vrmax, p.vrmin, p.kpff, p.kh, p.vmax, p.vmin, p.t1, p.t2,
                p.t3, p.t4, p.kl,
            ],
        ),
        ExciterModel::Esac6a(p) => (
            "ESAC6A",
            vec![
                p.tr, p.ka, p.ta, p.tk, p.tb, p.tc, p.vamax, p.vamin, p.vrmax, p.vrmin, p.te, p.kh,
                p.kf, p.tf, p.kc, p.kd, p.ke,
            ],
        ),
        ExciterModel::Esdc1a(p) => (
            "ESDC1A",
            vec![
                p.tr, p.ka, p.ta, p.kf, p.tf, p.ke, p.te, p.se1, p.e1, p.se2, p.e2, p.vrmax,
                p.vrmin,
            ],
        ),
        ExciterModel::Exst2(p) => (
            "EXST2",
            vec![p.tr, p.ka, p.ta, p.vrmax, p.vrmin, p.kc, p.ki, p.ke, p.te],
        ),
        ExciterModel::Ac8b(p) => ("AC8B", ac8b_params(p)),
        ExciterModel::Bbsex1(p) => (
            "BBSEX1",
            vec![
                p.t1r, p.t2r, p.t3r, p.t4r, p.t1i, p.t2i, p.ka, p.ta, p.vrmax, p.vrmin,
            ],
        ),
        ExciterModel::Ieeet3(p) => (
            "IEEET3",
            vec![
                p.tr, p.ka, p.ta, p.vrmax, p.vrmin, p.kf, p.tf, p.ke, p.te, p.e1, p.se1, p.e2,
                p.se2, p.kp, p.ki, p.kc,
            ],
        ),
        ExciterModel::Wt3e1(p) => (
            "WT3E1",
            vec![
                p.kpv, p.kiv, p.kqv, p.xd, p.kpq, p.kiq, p.tpe, p.pmin, p.pmax, p.qmin, p.qmax,
                p.imax, p.tv,
            ],
        ),
        ExciterModel::Wt3e2(p) => (
            "WT3E2",
            vec![
                p.kpv, p.kiv, p.kqv, p.xd, p.kpq, p.kiq, p.tpe, p.pmin, p.pmax, p.qmin, p.qmax,
                p.imax, p.tiq, p.tv,
            ],
        ),
        ExciterModel::Wt4e1(p) => (
            "WT4E1",
            vec![p.kpv, p.kiv, p.tpe, p.pmin, p.pmax, p.qmin, p.qmax, p.imax],
        ),
        ExciterModel::Wt4e2(p) => (
            "WT4E2",
            vec![p.kpv, p.kiv, p.tpe, p.pmin, p.pmax, p.qmin, p.qmax, p.imax],
        ),
        ExciterModel::Repcb(p) => ("REPCB", repcb_params(p)),
        ExciterModel::Repcc(p) => ("REPCC", repcb_params(p)),
        ExciterModel::Exst3(p) => (
            "EXST3",
            vec![
                p.tr, p.ka, p.ta, p.tb, p.tc, p.vrmax, p.vrmin, p.kc, p.ki, p.km, p.vmmax, p.vmmin,
                p.xm,
            ],
        ),
        ExciterModel::Cbufr(p) => (
            "CBUFR",
            vec![
                p.kf, p.tf, p.tp, p.p_base, p.p_min, p.p_max, p.e_cap, p.soc_init,
            ],
        ),
        ExciterModel::Cbufd(p) => (
            "CBUFD",
            vec![
                p.kf, p.tf, p.tp, p.tq, p.p_base, p.p_min, p.p_max, p.q_base, p.q_min, p.q_max,
                p.e_cap, p.soc_init,
            ],
        ),
        ExciterModel::Pveu1(p) => (
            "PVEU1",
            vec![
                p.tiq, p.dflag, p.vref0, p.tv, p.dbd, p.kqv, p.iqhl, p.iqll, p.pmax, p.pmin,
                p.qmax, p.qmin, p.vmax, p.vmin, p.tpord, p.mbase,
            ],
        ),
        ExciterModel::Ieeet2(p) => (
            "IEEET2",
            vec![
                p.tr, p.ka, p.ta, p.vrmax, p.vrmin, p.ke, p.te, p.e1, p.se1, p.e2, p.se2, p.kf,
                p.tf,
            ],
        ),
        ExciterModel::Exac2(p) => (
            "EXAC2",
            vec![
                p.tr, p.tb, p.tc, p.ka, p.ta, p.vamax, p.vamin, p.te, p.kf, p.tf, p.ke, p.e1,
                p.se1, p.e2, p.se2, p.kc, p.kd, p.kh,
            ],
        ),
        ExciterModel::Exac3(p) => (
            "EXAC3",
            vec![
                p.tr, p.kc, p.ki, p.vmin, p.vmax, p.ke, p.te, p.kf, p.tf, p.e1, p.se1, p.e2, p.se2,
                p.ka, p.ta, p.efdn,
            ],
        ),
        ExciterModel::Esac3a(p) => (
            "ESAC3A",
            vec![
                p.tr, p.tb, p.tc, p.ka, p.ta, p.vamax, p.vamin, p.te, p.ke, p.kf1, p.tf, p.e1,
                p.se1, p.e2, p.se2, p.kc, p.kd, p.ki, p.efdn, p.kn, p.vfemax,
            ],
        ),
        ExciterModel::Esst8c(p) => (
            "ESST8C",
            vec![
                p.tr, p.kpr, p.kir, p.vrmax, p.vrmin, p.ka, p.ta, p.kc, p.vbmax, p.xl, p.kf, p.tf,
            ],
        ),
        ExciterModel::Esst9b(p) => (
            "ESST9B",
            vec![
                p.tr, p.kpa, p.kia, p.vrmax, p.vrmin, p.ka, p.ta, p.vbmax, p.kc, p.t1, p.t2, p.t3,
                p.t4,
            ],
        ),
        ExciterModel::Esst10c(p) => (
            "ESST10C",
            vec![
                p.tr, p.kpa, p.kia, p.kpb, p.kib, p.vrmax, p.vrmin, p.ka, p.ta, p.vbmax, p.kc,
                p.t1, p.t2,
            ],
        ),
        ExciterModel::Esdc3a(p) => (
            "ESDC3A",
            vec![
                p.tr, p.ka, p.ta, p.vrmax, p.vrmin, p.te, p.ke, p.e1, p.se1, p.e2, p.se2, p.kp,
                p.ki, p.kf, p.tf,
            ],
        ),
        ExciterModel::Exdc1(p) => (
            "EXDC1",
            vec![
                p.tr, p.ka, p.ta, p.vrmax, p.vrmin, p.ke, p.te, p.kf, p.tf, p.e1, p.se1, p.e2,
                p.se2,
            ],
        ),
        ExciterModel::Esst2a(p) => (
            "ESST2A",
            vec![
                p.tr, p.ka, p.ta, p.tb, p.tc, p.ke, p.te, p.kf, p.tf, p.vrmax, p.vrmin, p.e1,
                p.se1, p.e2, p.se2, p.kc, p.kp, p.ki, p.tp,
            ],
        ),
        ExciterModel::Exdc3(p) => (
            "EXDC3",
            vec![
                p.tr, p.kv, p.tstall, p.tcon, p.tb, p.tc, p.vrmax, p.vrmin, p.veff, p.tlim, p.vlim,
                p.ke, p.te,
            ],
        ),
        ExciterModel::Wt3c2(p) => (
            "WT3C2",
            vec![
                p.kpv, p.kiv, p.kqv, p.xd, p.kpq, p.kiq, p.tpe, p.pmin, p.pmax, p.qmin, p.qmax,
                p.imax,
            ],
        ),
        ExciterModel::Esac7c(p) => ("ESAC7C", esac7c_params(p)),
        ExciterModel::Esdc4c(p) => (
            "ESDC4C",
            vec![
                p.tr, p.ka, p.ta, p.vrmax, p.vrmin, p.ke, p.te, p.kf, p.tf, p.e1, p.se1, p.e2,
                p.se2, p.kpr, p.kir, p.kdr, p.tdr,
            ],
        ),
        ExciterModel::Reecbu1(p) => (
            "REECBU1",
            vec![
                p.dbd1, p.kqv, p.kqi, p.trv, p.tp, p.rrpwr, p.vdip, p.vup, p.pref, p.pmax, p.pmin,
            ],
        ),
        ExciterModel::Reece(p) => (
            "REECE",
            vec![
                p.vdip, p.vup, p.trv, p.dbd1, p.dbd2, p.kqv, p.iqh1, p.iql1, p.vref0, p.tp, p.qmax,
                p.qmin, 0.0, 0.0, p.kqp, p.kqi,
            ],
        ),
        ExciterModel::Reeceu1(p) => (
            "REECEU1",
            vec![
                p.dbd1, p.kqv, p.kqi, p.trv, p.tp, p.rrpwr, p.vdip, p.vup, p.pref, p.pmax, p.pmin,
            ],
        ),
        ExciterModel::Esac8c(p) => ("ESAC8C", ac8b_params(p)),
        ExciterModel::Esac9c(p) => ("ESAC9C", esac7b_params(p)),
        ExciterModel::Esac10c(p) => ("ESAC10C", esac7c_params(p)),
        ExciterModel::Esac11c(p) => ("ESAC11C", ac8b_params(p)),
    }
}

fn esac7b_params(p: &Esac7bParams) -> Vec<f64> {
    vec![
        p.tr, p.kpa, p.kia, p.vrh, p.vrl, p.kpf, p.vfh, p.tf, p.te, p.ke, p.e1, p.se1, p.e2, p.se2,
        p.kd, p.kc, p.kl,
    ]
}

fn ac8b_params(p: &Ac8bParams) -> Vec<f64> {
    vec![
        p.tr, p.ka, p.ta, p.kc, p.vrmax, p.vrmin, p.kd, p.ke, p.te, p.pid_kp, p.pid_ki, p.pid_kd,
    ]
}

fn esac7c_params(p: &Esac7cParams) -> Vec<f64> {
    vec![
        p.tr, p.kpr, p.kir, p.kdr, p.tdr, p.vrmax, p.vrmin, p.ka, p.ta, p.kp, p.kl, p.te, p.ke,
        p.vfemax, p.vemin, p.e1, p.se1, p.e2, p.se2,
    ]
}

fn repcb_params(p: &RepcbParams) -> Vec<f64> {
    vec![
        p.tp, p.tfltr, p.kp, p.ki, p.tft, p.tfv, p.qmax, p.qmin, p.vmax, p.vmin, p.kc, p.refs,
    ]
}

// ---------------------------------------------------------------------------
// Governor model → DYR params
// ---------------------------------------------------------------------------

fn governor_to_dyr(model: &GovernorModel) -> (&'static str, Vec<f64>) {
    match model {
        GovernorModel::Tgov1(p) => {
            let mut v = vec![p.r, p.t1, p.vmax, p.vmin, p.t2, p.t3];
            push_opt(&mut v, p.dt);
            ("TGOV1", v)
        }
        GovernorModel::Ieeeg1(p) => ("IEEEG1", ieeeg1_params(p)),
        GovernorModel::Ggov1(p) => {
            // Reconstruct the full GGOV1 DYR positional layout:
            // 0:R  1:RSELECT  2:TPELEC  3:MAXERR  4:MINERR
            // 5:KPGOV  6:KIGOV  7:KDGOV  8:TDGOV
            // 9:VMAX  10:VMIN  11:TSA  12:FSR  13:TSB  14:TSE  15:IANG  16:KCMF
            // 17:KTURB  18:WFNL  19:TB  20:TC  21:TRATE  22:FLAG
            //
            // The reader only stores a subset. We emit the stored fields at the
            // correct indices and fill gaps with zeros/defaults.
            let mut v = vec![
                p.r,      // 0: R
                0.0,      // 1: RSELECT
                p.tpelec, // 2: TPELEC
                0.1,      // 3: MAXERR
                -0.1,     // 4: MINERR
                p.kpgov,  // 5: KPGOV
                p.kigov,  // 6: KIGOV
                0.0,      // 7: KDGOV
                0.1,      // 8: TDGOV
                p.vmax,   // 9: VMAX
                p.vmin,   // 10: VMIN
                0.5,      // 11: TSA
                0.0,      // 12: FSR
                0.5,      // 13: TSB
                0.0,      // 14: TSE
                0.0,      // 15: IANG
                0.0,      // 16: KCMF
                p.kturb,  // 17: KTURB
                p.wfnl,   // 18: WFNL
                p.tb,     // 19: TB
                p.tc,     // 20: TC
            ];
            if let Some(trate) = p.trate {
                v.push(trate); // 21: TRATE
            }
            ("GGOV1", v)
        }
        GovernorModel::Gast(p) => (
            "GAST",
            vec![p.r, p.t1, p.t2, p.t3, p.at, p.kt, p.vmin, p.vmax],
        ),
        GovernorModel::Repca(p) => (
            "REPCA",
            vec![
                0.0, 0.0, 0.0, p.vrflag, 60.0, p.tfltr, p.kp, p.ki, p.tlag, p.vmax, p.vmin, p.qmax,
                p.qmin, p.rc, 0.0, 0.0, p.ddn, p.dup, p.fdbd1, p.fdbd2, 0.0, 0.0, p.pmax, p.pmin,
                0.0, p.kpg, p.kig, p.tp, p.rrpwr,
            ],
        ),
        GovernorModel::Hygov(p) => (
            "HYGOV",
            vec![
                p.r, p.tp, p.velm, p.tg, p.gmax, p.gmin, p.tw, p.at, p.dturb, p.qnl,
            ],
        ),
        GovernorModel::Hygovd(p) => (
            "HYGOVD",
            vec![
                p.r, p.tp, p.velm, p.tg, p.gmax, p.gmin, p.tw, p.at, p.dturb, p.qnl, p.db1, p.db2,
            ],
        ),
        GovernorModel::Tgov1d(p) => {
            let mut v = vec![p.r, p.t1, p.vmax, p.vmin, p.t2, p.t3];
            if let Some(dt) = p.dt {
                v.push(dt);
            }
            v.push(p.db1);
            v.push(p.db2);
            ("TGOV1D", v)
        }
        GovernorModel::Ieeeg1d(p) => {
            // IEEEG1 base params + DB1 DB2 at end
            let mut v = vec![p.k, p.t1, p.t2, p.t3, p.uo, p.uc, p.pmax, p.pmin, p.t4];
            push_opt(&mut v, p.k1);
            push_opt(&mut v, p.k2);
            push_opt(&mut v, p.t5);
            push_opt(&mut v, p.k3);
            push_opt(&mut v, p.k4);
            push_opt(&mut v, p.t6);
            push_opt(&mut v, p.k5);
            push_opt(&mut v, p.k6);
            push_opt(&mut v, p.t7);
            push_opt(&mut v, p.k7);
            push_opt(&mut v, p.k8);
            v.push(p.db1);
            v.push(p.db2);
            ("IEEEG1D", v)
        }
        GovernorModel::Wsieg1(p) => ("WSIEG1", ieeeg1_params(p)),
        GovernorModel::Ieeeg2(p) => (
            "IEEEG2",
            vec![
                p.k, p.t1, p.t2, p.t3, p.pmin, p.pmax, p.at, p.dturb, p.qnl, p.rt,
            ],
        ),
        GovernorModel::Repcd(p) => ("REPCD", vec![p.tp, p.kpg, p.kig, p.pmax, p.pmin, p.tlag]),
        GovernorModel::Wt3t1(p) => ("WT3T1", vec![p.h, p.damp, p.ka, p.theta]),
        GovernorModel::Wt3p1(p) => ("WT3P1", vec![p.tp, p.kpp, p.kip, p.pmax, p.pmin]),
        GovernorModel::Ggov1d(p) => (
            "GGOV1D",
            vec![
                p.r, p.t_pelec, p.maxerr, p.minerr, p.kpgov, p.kigov, p.kdgov, p.fdbd1, p.fdbd2,
                p.pmax, p.pmin, p.tact, p.kturb, p.wfnl, p.tb, p.tc, p.flag, p.teng, p.tfload,
            ],
        ),
        GovernorModel::Tgov1n(p) => (
            "TGOV1N",
            vec![p.r, p.dt, p.t1, p.vmax, p.vmin, p.t2, p.t3, p.d, p.db],
        ),
        GovernorModel::Cbest(p) => (
            "CBEST",
            vec![
                p.p_max, p.p_min, p.q_max, p.q_min, p.tp, p.tq, p.e_cap, p.mbase, p.soc_init,
            ],
        ),
        GovernorModel::Chaaut(p) => ("CHAAUT", vec![p.kf, p.tf, p.p_max, p.p_min, p.tp, p.mbase]),
        GovernorModel::Pidgov(p) => ("PIDGOV", vec![p.pmax, p.pmin, p.kp, p.ki, p.kd, p.td, p.tf]),
        GovernorModel::Degov1(p) => (
            "DEGOV1",
            vec![p.r, p.t1, p.t2, p.t3, p.at, p.kt, p.vmax, p.vmin, p.td],
        ),
        GovernorModel::Tgov5(p) => (
            "TGOV5",
            vec![
                p.r, p.t1, p.t2, p.t3, p.t4, p.k1, p.k2, p.k3, p.pmax, p.pmin,
            ],
        ),
        GovernorModel::Gast2a(p) => (
            "GAST2A",
            vec![p.r, p.t1, p.t2, p.t3, p.t4, p.at, p.kt, p.vmin, p.vmax],
        ),
        GovernorModel::H6e(p) => (
            "H6E",
            vec![
                p.r, p.tr, p.tf, p.tg, p.tw, p.t1, p.t2, p.t3, p.t4, p.t5, p.dt, p.pmax, p.pmin,
            ],
        ),
        GovernorModel::Wshygp(p) => (
            "WSHYGP",
            vec![p.r, p.tf, p.tg, p.tw, p.kd, p.pmax, p.pmin, p.kp, p.ki],
        ),
        GovernorModel::Ggov2(p) => (
            "GGOV2",
            vec![
                p.r, p.rselect, p.tpelec, p.maxerr, p.minerr, p.kpgov, p.kigov, p.kdgov, p.tdgov,
                p.vmax, p.vmin, p.tact, p.kturb, p.wfnl, p.tb, p.tc, p.flag, p.teng, p.tfload,
                p.kpload, p.kiload, p.ldref, p.dm, p.ropen, p.rclose, p.kimw, p.pmwset, p.aset,
                p.ka, p.ta, p.db, p.tsa, p.tsb, p.rup, p.rdown, p.pmax, p.pmin,
            ],
        ),
        GovernorModel::Ggov3(p) => (
            "GGOV3",
            vec![
                p.r, p.rselect, p.tpelec, p.maxerr, p.minerr, p.kpgov, p.kigov, p.kdgov, p.tdgov,
                p.vmax, p.vmin, p.tact, p.kturb, p.wfnl, p.tb, p.tc, p.flag, p.teng, p.tfload,
                p.kpload, p.kiload, p.ldref, p.dm, p.ropen, p.rclose, p.kimw, p.pmwset, p.aset,
                p.ka, p.ta, p.db, p.tsa, p.tsb, p.tw, p.rup, p.rdown, p.pmax, p.pmin,
            ],
        ),
        GovernorModel::Wpidhy(p) => (
            "WPIDHY",
            vec![
                p.gatmax, p.gatmin, p.reg, p.kp, p.ki, p.kd, p.ta, p.tb, p.tw, p.at, p.dturb,
                p.gmax, p.gmin, p.pmax, p.pmin,
            ],
        ),
        GovernorModel::H6b(p) => (
            "H6B",
            vec![
                p.tg, p.tp, p.uo, p.uc, p.pmax, p.pmin, p.beta, p.tw, p.dbinf, p.dbsup,
            ],
        ),
        GovernorModel::Wshydd(p) => (
            "WSHYDD",
            vec![
                p.r, p.tf, p.tg, p.tw, p.db, p.kd, p.pmax, p.pmin, p.kp, p.ki,
            ],
        ),
        GovernorModel::Repcgfmc1(p) => (
            "REPCGFMC1",
            vec![
                p.kp_v, p.ki_v, p.vmax, p.vmin, p.kp_q, p.ki_q, p.qmax, p.qmin, p.tlag, p.fdroop,
                p.dbd1, p.dbd2,
            ],
        ),
        GovernorModel::Wtdta1(p) => ("WTDTA1", vec![p.h, p.dshaft, p.kshaft, p.d2]),
        GovernorModel::Wtara1(p) => ("WTARA1", vec![p.ka, p.ta, p.km, p.tm, p.pmax, p.pmin]),
        GovernorModel::Wtpta1(p) => (
            "WTPTA1",
            vec![
                p.kpp,
                p.kip,
                p.theta_max,
                p.theta_min,
                p.rate_max,
                p.rate_min,
                p.te,
                p.kp_pitch,
            ],
        ),
        GovernorModel::Ieesgo(p) => (
            "IEESGO",
            vec![
                p.t1, p.t2, p.t3, p.t4, p.t5, p.t6, p.k1, p.k2, p.k3, p.pmax, p.pmin,
            ],
        ),
        GovernorModel::Wttqa1(p) => ("WTTQA1", vec![p.kp, p.ki, p.tp, p.pmax, p.pmin]),
        GovernorModel::Hygov4(p) => (
            "HYGOV4",
            vec![
                p.tr, p.tf, p.dturb, p.hdam, p.tw, p.qnl, p.at, p.dg, p.gmax, p.gmin, p.ts, p.ks,
                p.pmax, p.pmin,
            ],
        ),
        GovernorModel::Wehgov(p) => (
            "WEHGOV",
            vec![
                p.r, p.tr, p.tf, p.tg, p.tw, p.at, p.dturb, p.qnl, p.gmax, p.gmin, p.dbd1, p.dbd2,
                p.pmax, p.pmin,
            ],
        ),
        GovernorModel::Ieeeg3(p) => (
            "IEEEG3",
            vec![
                p.tg, p.tp, p.uo, p.uc, p.pmax, p.pmin, p.tw, p.at, p.dturb, p.qnl,
            ],
        ),
        GovernorModel::Ieeeg4(p) => (
            "IEEEG4",
            vec![
                p.t1, p.t2, p.t3, p.ki, p.pmax, p.pmin, p.tw, p.at, p.dturb, p.qnl,
            ],
        ),
        GovernorModel::Govct1(p) => ("GOVCT1", govct1_params(p)),
        GovernorModel::Govct2(p) => ("GOVCT2", govct2_params(p)),
        GovernorModel::Tgov3(p) => ("TGOV3", tgov3_params(p)),
        GovernorModel::Tgov4(p) => ("TGOV4", tgov3_params(p)),
        GovernorModel::Wt2e1(p) => ("WT2E1", vec![p.kp, p.ki, p.pmax, p.pmin, p.te]),
        GovernorModel::Wt12t1(p) => ("WT12T1", vec![p.h, p.damp, p.ka, p.theta]),
        GovernorModel::Wt12a1(p) => ("WT12A1", vec![p.tp, p.kpp, p.kip, p.pmax, p.pmin]),
        GovernorModel::Wtaero(p) => {
            let mut v = vec![p.rho, p.r_rotor, p.gear_ratio, p.v_wind_base, p.mbase_mw];
            if let Some(h) = p.h_rotor {
                v.push(h);
            }
            if let Some(k) = p.k_shaft {
                v.push(k);
            }
            if let Some(d) = p.d_shaft {
                v.push(d);
            }
            ("WTAERO", v)
        }
    }
}

fn ieeeg1_params(p: &Ieeeg1Params) -> Vec<f64> {
    let mut v = vec![p.k, p.t1, p.t2, p.t3, p.uo, p.uc, p.pmax, p.pmin, p.t4];
    push_opt(&mut v, p.k1);
    push_opt(&mut v, p.k2);
    push_opt(&mut v, p.t5);
    push_opt(&mut v, p.k3);
    push_opt(&mut v, p.k4);
    push_opt(&mut v, p.t6);
    push_opt(&mut v, p.k5);
    push_opt(&mut v, p.k6);
    push_opt(&mut v, p.t7);
    push_opt(&mut v, p.k7);
    push_opt(&mut v, p.k8);
    v
}

fn govct1_params(p: &Govct1Params) -> Vec<f64> {
    vec![
        p.r, p.t1, p.vmax, p.vmin, p.t2, p.t3, p.k1, p.k2, p.k3, p.t4, p.t5, p.t6, p.k7, p.k8,
        p.pmax, p.pmin, p.td,
    ]
}

fn govct2_params(p: &Govct2Params) -> Vec<f64> {
    vec![
        p.r, p.t1, p.vmax, p.vmin, p.t2, p.t3, p.k1, p.k2, p.k3, p.t4, p.t5, p.t6, p.k7, p.k8,
        p.pmax, p.pmin, p.td, p.t_hrsg, p.k_st, p.t_st,
    ]
}

fn tgov3_params(p: &Tgov3Params) -> Vec<f64> {
    vec![p.r, p.t1, p.vmax, p.vmin, p.t2, p.t3, p.dt, p.kd]
}

// ---------------------------------------------------------------------------
// PSS model → DYR params
// ---------------------------------------------------------------------------

fn pss_to_dyr(model: &PssModel) -> (&'static str, Vec<f64>) {
    match model {
        PssModel::Ieeest(p) => {
            let mut v = vec![
                p.a1, p.a2, p.a3, p.a4, p.a5, p.a6, p.t1, p.t2, p.t3, p.t4, p.t5, p.t6, p.ks,
            ];
            push_opt(&mut v, p.lsmax);
            push_opt(&mut v, p.lsmin);
            push_opt(&mut v, p.vcu);
            push_opt(&mut v, p.vcl);
            ("IEEEST", v)
        }
        PssModel::St2cut(p) => {
            // Reconstruct ST2CUT DYR layout with 4 control integers (MODE/BUSR/MODE2/BUSR2)
            // set to defaults, then the model params.
            let mut v = vec![
                0.0, 0.0, 0.0, 0.0, // MODE, BUSR, MODE2, BUSR2
                p.k1, p.k2, p.t1, p.t2, p.t3, p.t4, p.t5, p.t6, p.t7, p.t8, 0.0,
                0.0, // T9, T10 (not stored)
                p.lsmax,
            ];
            push_opt(&mut v, p.lsmin);
            push_opt(&mut v, p.vcu);
            push_opt(&mut v, p.vcl);
            ("ST2CUT", v)
        }
        PssModel::Pss2a(p) => (
            "PSS2A",
            vec![
                p.m1, p.t6, p.t7, p.ks2, p.t8, p.t9, p.m2, p.tw1, p.tw2, p.tw3, p.tw4, p.t1, p.t2,
                p.t3, p.t4, p.ks1, p.ks3, p.vstmax, p.vstmin,
            ],
        ),
        PssModel::Pss2b(p) => (
            "PSS2B",
            vec![
                p.m1, p.t6, p.t7, p.ks2, p.t8, p.t9, p.m2, p.tw1, p.tw2, p.tw3, p.tw4, p.t1, p.t2,
                p.t3, p.t4, p.ks1, p.ks3, p.vstmax, p.vstmin, p.t10, p.t11,
            ],
        ),
        PssModel::Stab1(p) => ("STAB1", vec![p.ks, p.t1, p.t2, p.t3, p.t4, p.hlim]),
        PssModel::Pss1a(p) => (
            "PSS1A",
            vec![p.ks, p.t1, p.t2, p.t3, p.t4, p.vstmax, p.vstmin],
        ),
        PssModel::Stab2a(p) => ("STAB2A", vec![p.ks, p.t1, p.t2, p.t3, p.t4, p.t5, p.hlim]),
        PssModel::Pss4b(p) => ("PSS4B", pss4b_params(p)),
        PssModel::Stab3(p) => (
            "STAB3",
            vec![p.ks, p.t1, p.t2, p.t3, p.t4, p.t5, p.t6, p.vstmax, p.vstmin],
        ),
        PssModel::Pss3b(p) => ("PSS3B", pss3b_params(p)),
        PssModel::Pss2c(p) => (
            "PSS2C",
            vec![
                p.m1, p.t6, p.m2, p.t7, p.tw1, p.tw2, p.tw3, p.tw4, p.t1, p.t2, p.t3, p.t4, p.t8,
                p.t9, p.n as f64, p.ks1, p.ks2, p.ks3, p.vstmax, p.vstmin,
            ],
        ),
        PssModel::Pss5(p) => ("PSS5", pss5_params(p)),
        PssModel::Pss6c(p) => (
            "PSS6C",
            vec![
                p.kl, p.km, p.kh, p.kl2, p.km2, p.kh2, p.tw1, p.tw2, p.tw3, p.tw4, p.tw5, p.tw6,
                p.t1, p.t2, p.t3, p.t4, p.vstmax, p.vstmin,
            ],
        ),
        PssModel::Psssb(p) => (
            "PSSSB",
            vec![
                p.ks, p.t1, p.t2, p.t3, p.t4, p.t5, p.t6, p.tw, p.vstmax, p.vstmin,
            ],
        ),
        PssModel::Stab4(p) => (
            "STAB4",
            vec![p.ks, p.t1, p.t2, p.t3, p.t4, p.t5, p.t6, p.t7, p.t8, p.hlim],
        ),
        PssModel::Stab5(p) => (
            "STAB5",
            vec![
                p.ks, p.t1, p.t2, p.t3, p.t4, p.t5, p.t6, p.t7, p.t8, p.t9, p.t10, p.hlim,
            ],
        ),
        PssModel::Pss3c(p) => ("PSS3C", pss3b_params(p)),
        PssModel::Pss4c(p) => ("PSS4C", pss4b_params(p)),
        PssModel::Pss5c(p) => ("PSS5C", pss5_params(p)),
        PssModel::Pss7c(p) => (
            "PSS7C",
            vec![
                p.kss, p.tw1, p.tw2, p.t1, p.t2, p.t3, p.t4, p.vsmax, p.vsmin,
                // Per-band params (multi-band PSS7C extension):
                p.kl, p.tw_l, p.t1_l, p.t2_l, p.ki, p.tw_i, p.t1_i, p.t2_i, p.kh, p.tw_h, p.t1_h,
                p.t2_h, p.vstmax, p.vstmin,
            ],
        ),
    }
}

fn pss3b_params(p: &Pss3bParams) -> Vec<f64> {
    vec![
        p.a1, p.a2, p.a3, p.a4, p.a5, p.a6, p.a7, p.a8, p.vsi1max, p.vsi1min, p.vsi2max, p.vsi2min,
        p.vstmax, p.vstmin,
    ]
}

fn pss4b_params(p: &Pss4bParams) -> Vec<f64> {
    vec![
        p.kl, p.kh, p.tw1, p.tw2, p.t1, p.t2, p.t3, p.t4, p.vstmax, p.vstmin,
    ]
}

fn pss5_params(p: &Pss5Params) -> Vec<f64> {
    vec![
        p.kl, p.km, p.kh, p.tw1, p.tw2, p.tw3, p.t1, p.t2, p.t3, p.t4, p.t5, p.t6, p.vstmax,
        p.vstmin,
    ]
}

// ---------------------------------------------------------------------------
// Load model → DYR params
// ---------------------------------------------------------------------------

fn load_to_dyr(model: &LoadModel) -> (&'static str, Vec<f64>) {
    match model {
        LoadModel::Clod(p) => (
            "CLOD",
            vec![
                p.lfac, p.rfrac, p.xfrac, p.lfrac_dl, p.nfrac, p.dsli, p.tv, p.tf, p.vtd, p.vtu,
                p.ftd, p.ftu, p.td,
            ],
        ),
        LoadModel::Indmot(p) => (
            "INDMOT",
            vec![p.h, p.d, p.ra, p.xs, p.xr, p.xm, p.rr, p.mbase, p.lfac],
        ),
        LoadModel::Motor(p) => (
            "MOTOR",
            vec![p.h, p.ra, p.xs, p.x0p, p.t0p, p.mbase, p.lfac],
        ),
        LoadModel::Cmpldw(p) => ("CMPLDW", cmpldw_params(p)),
        LoadModel::Cmpldwg(p) => ("CMPLDWG", cmpldwg_params(p)),
        LoadModel::Cmldblu2(p) => ("CMLDBLU2", cmldblu2_params(p)),
        LoadModel::Cmldaru2(p) => ("CMLDARU2", cmldblu2_params(p)),
        LoadModel::Motorw(p) => (
            "MOTORW",
            vec![
                p.ra, p.xm, p.r1, p.x1, p.r2, p.x2, p.h, p.vtr1, p.vtr2, p.mbase,
            ],
        ),
        LoadModel::Cim5(p) => (
            "CIM5",
            vec![
                p.ra, p.xs, p.xm, p.xr1, p.xr2, p.rr1, p.rr2, p.h, p.e1, p.s1, p.e2, p.s2, p.mbase,
            ],
        ),
        LoadModel::Lcfb1(p) => ("LCFB1", vec![p.tc, p.tb, p.kf, p.pmax, p.pmin, p.mbase]),
        LoadModel::Ldfral(p) => (
            "LDFRAL",
            vec![p.tc, p.tb, p.kf, p.kp, p.pmax, p.pmin, p.mbase],
        ),
        LoadModel::Frqtplt(p) => ("FRQTPLT", vec![p.tf, p.fmin, p.fmax, p.p_trip]),
        LoadModel::Lvshbl(p) => ("LVSHBL", lvshbl_params(p)),
        LoadModel::Cim6(p) => (
            "CIM6",
            vec![
                p.ra, p.xs, p.xm, p.xr1, p.xr2, p.rr1, p.rr2, p.h, p.e1, p.s1, p.e2, p.s2, p.mbase,
                p.tq0p, p.xq_prime,
            ],
        ),
        LoadModel::Cimw(p) => (
            "CIMW",
            vec![p.h, p.d, p.ra, p.xs, p.xr, p.xm, p.rr, p.mbase, p.lfac],
        ),
        LoadModel::Extl(p) => ("EXTL", extl_params(p)),
        LoadModel::Ieelar(p) => ("IEELAR", extl_params(p)),
        LoadModel::Cmldowu2(p) => ("CMLDOWU2", cmpldw_params(p)),
        LoadModel::Cmldxnu2(p) => ("CMLDXNU2", cmpldw_params(p)),
        LoadModel::Cmldalu2(p) => ("CMLDALU2", cmpldw_params(p)),
        LoadModel::Cmldblu2w(p) => ("CMLDBLU2W", cmldblu2_params(p)),
        LoadModel::Cmldaru2w(p) => ("CMLDARU2W", cmldblu2_params(p)),
        LoadModel::Vtgtpat(p) => ("VTGTPAT", vtgtpat_params(p)),
        LoadModel::Vtgdcat(p) => ("VTGDCAT", vtgtpat_params(p)),
        LoadModel::Frqtpat(p) => ("FRQTPAT", frqtpat_params(p)),
        LoadModel::Frqdcat(p) => ("FRQDCAT", frqtpat_params(p)),
        LoadModel::Distr1(p) => (
            "DISTR1",
            vec![
                p.z1,
                p.z2,
                p.t1,
                p.t2,
                p.mbase,
                p.lfac,
                p.z3,
                p.t3,
                p.reach_angle_deg,
                p.branch_from as f64,
                p.branch_to as f64,
                p.branch_r,
                p.branch_x,
                p.tf,
            ],
        ),
        LoadModel::Bfr50(p) => ("BFR50", vec![p.t_bfr, p.i_sup, p.branch_idx as f64]),
        LoadModel::Lvshc1(p) => ("LVSHC1", lvshbl_params(p)),
        LoadModel::Cmlddgu2(p) => ("CMLDDGU2", cmpldw_params(p)),
        LoadModel::Cmlddggu2(p) => ("CMLDDGGU2", cmpldwg_params(p)),
        LoadModel::Cmldowdgu2(p) => ("CMLDOWDGU2", cmpldw_params(p)),
        LoadModel::Cmldxndgu2(p) => ("CMLDXNDGU2", cmpldw_params(p)),
        LoadModel::TransDiff87(p) => (
            "TRANSDIFF87",
            vec![
                p.slope1,
                p.slope2,
                p.i_pickup,
                p.harmonic_restraint,
                p.turns_ratio,
                p.tf,
            ],
        ),
        LoadModel::LineDiff87l(p) => ("LINEDIFF87L", vec![p.slope1, p.slope2, p.i_pickup, p.tf]),
        LoadModel::Recloser79(p) => (
            "RECLOSER79",
            vec![
                p.dead_time_1,
                p.dead_time_2,
                p.dead_time_3,
                p.max_attempts as f64,
                p.reset_time,
            ],
        ),
        LoadModel::Uvls1(p) => (
            "UVLS1",
            vec![
                p.tv,
                p.vmin,
                p.t_delay,
                p.p_shed,
                p.v_reconnect,
                p.t_reconnect,
            ],
        ),
    }
}

fn cmpldw_params(p: &CmpldwParams) -> Vec<f64> {
    vec![
        p.lfma, p.lfmb, p.lfmc, p.kp1, p.np1, p.kp2, p.np2, p.kq1, p.nq1, p.kq2, p.nq2, p.ra, p.xm,
        p.r1, p.x1, p.r2, p.x2, p.vtr1, p.vtr2, p.mbase,
    ]
}

fn cmpldwg_params(p: &CmpldwgParams) -> Vec<f64> {
    vec![
        p.lfma, p.lfmb, p.lfmc, p.kp1, p.np1, p.kp2, p.np2, p.kq1, p.nq1, p.kq2, p.nq2, p.ra, p.xm,
        p.r1, p.x1, p.r2, p.x2, p.vtr1, p.vtr2, p.mbase, p.gen_mw,
    ]
}

fn cmldblu2_params(p: &Cmldblu2Params) -> Vec<f64> {
    vec![
        p.t1, p.t2, p.k1, p.k2, p.pf, p.kp, p.kq, p.vmin, p.vmax, p.mbase,
    ]
}

fn extl_params(p: &ExtlParams) -> Vec<f64> {
    vec![p.tp, p.tq, p.kpv, p.kqv, p.kpf, p.kqf, p.mbase, p.lfac]
}

fn vtgtpat_params(p: &VtgtpatParams) -> Vec<f64> {
    vec![p.tv, p.vtrip, p.vreset]
}

fn frqtpat_params(p: &FrqtpatParams) -> Vec<f64> {
    vec![p.tf, p.ftrip_hi, p.ftrip_lo, p.freset]
}

fn lvshbl_params(p: &LvshblParams) -> Vec<f64> {
    vec![p.tv, p.vmin, p.p_block]
}

// ---------------------------------------------------------------------------
// FACTS model → DYR params
// ---------------------------------------------------------------------------

fn facts_to_dyr(model: &FACTSModel) -> (&'static str, Vec<f64>) {
    match model {
        FACTSModel::Csvgn1(p) => (
            "CSVGN1",
            vec![
                p.t1, p.t2, p.t3, p.t4, p.t5, p.k, p.vmax, p.vmin, p.bmax, p.bmin, p.mbase,
            ],
        ),
        FACTSModel::Cstcon(p) => ("CSTCON", vec![p.tr, p.k, p.tiq, p.imax, p.imin, p.mbase]),
        FACTSModel::Tcsc(p) => ("TCSC", vec![p.t1, p.t2, p.t3, p.xmax, p.xmin, p.k, p.mbase]),
        FACTSModel::Cdc4t(p) => (
            "CDC4T",
            vec![
                p.setvl,
                p.vschd,
                p.mbase,
                p.tr,
                p.td,
                p.alpha_min,
                p.alpha_max,
                p.gamma_min,
                p.rectifier_bus as f64,
                p.inverter_bus as f64,
            ],
        ),
        FACTSModel::Vscdct(p) => (
            "VSCDCT",
            vec![
                p.p_order,
                p.vdc_ref,
                p.t_dc,
                p.t_ac,
                p.imax,
                p.mbase,
                p.rectifier_bus as f64,
                p.inverter_bus as f64,
                p.kp_vdc.unwrap_or(2.0),
                p.ki_vdc.unwrap_or(50.0),
                p.kp_q.unwrap_or(5.0),
                p.ki_q.unwrap_or(20.0),
                p.t_vdc_filt.unwrap_or(0.01),
                p.kp_id.unwrap_or(1.0),
                p.ki_id.unwrap_or(100.0),
                p.kp_iq.unwrap_or(1.0),
                p.ki_iq.unwrap_or(100.0),
            ],
        ),
        FACTSModel::Csvgn3(p) => (
            "CSVGN3",
            vec![
                p.t1, p.t2, p.t3, p.t4, p.t5, p.k, p.slope, p.vmax, p.vmin, p.bmax, p.bmin, p.mbase,
            ],
        ),
        FACTSModel::Cdc7t(p) => (
            "CDC7T",
            vec![
                p.setvl,
                p.vschd,
                p.mbase,
                p.tr,
                p.td,
                p.alpha_min,
                p.alpha_max,
                p.gamma_min,
                p.rectifier_bus as f64,
                p.inverter_bus as f64,
                p.runback_rate,
                p.current_order_max,
            ],
        ),
        FACTSModel::Csvgn4(p) => (
            "CSVGN4",
            vec![
                p.t1, p.t2, p.t3, p.t4, p.t5, p.k, p.slope, p.kpod, p.tpod, p.vmax, p.vmin, p.bmax,
                p.bmin, p.mbase,
            ],
        ),
        FACTSModel::Csvgn5(p) => (
            "CSVGN5",
            vec![
                p.t1, p.t2, p.t3, p.t4, p.t5, p.k, p.kv, p.kpod, p.tpod, p.vmax, p.vmin, p.bmax,
                p.bmin, p.mbase,
            ],
        ),
        FACTSModel::Cdc6t(p) => (
            "CDC6T",
            vec![
                p.setvl,
                p.vschd,
                p.mbase,
                p.tr,
                p.td,
                p.alpha_min,
                p.alpha_max,
                p.gamma_min,
                p.rectifier_bus as f64,
                p.inverter_bus as f64,
                p.i_limit,
            ],
        ),
        FACTSModel::Cstcnt(p) => (
            "CSTCNT",
            vec![p.t1, p.t2, p.t3, p.ka, p.ta, p.iqmax, p.iqmin, p.mbase],
        ),
        FACTSModel::Mmc1(p) => (
            "MMC1",
            vec![
                p.tr, p.kp_v, p.ki_v, p.kp_i, p.ki_i, p.vdc, p.larm, p.pmax, p.pmin, p.qmax,
                p.qmin, p.mbase,
            ],
        ),
        FACTSModel::Hvdcplu1(p) => (
            "HVDCPLU1",
            vec![
                p.setvl,
                p.vschd,
                p.mbase,
                p.xcr,
                p.xci,
                p.rdc,
                p.td,
                p.tr,
                p.alpha_min,
                p.alpha_max,
                p.gamma_min,
                p.kp_id,
                p.ki_id,
                p.t_ramp,
                p.pmax,
                p.pmin,
            ],
        ),
        FACTSModel::Csvgn6(p) => (
            "CSVGN6",
            vec![
                p.t1, p.t2, p.t3, p.t4, p.t5, p.k, p.k_aux, p.t_aux, p.vmax, p.vmin, p.bmax, p.bmin,
            ],
        ),
        FACTSModel::Stcon1(p) => (
            "STCON1",
            vec![
                p.tr, p.kp, p.ki, p.kp_i, p.ki_i, p.vmax, p.vmin, p.iqmax, p.iqmin, p.mbase,
            ],
        ),
        FACTSModel::Gcsc(p) => ("GCSC", vec![p.tr, p.kp, p.ki, p.xmax, p.xmin, p.mbase]),
        FACTSModel::Sssc(p) => (
            "SSSC",
            vec![p.tr, p.kp, p.ki, p.kp_i, p.ki_i, p.vqmax, p.vqmin, p.mbase],
        ),
        FACTSModel::Upfc(p) => (
            "UPFC",
            vec![
                p.tr, p.kp_p, p.ki_p, p.kp_q, p.ki_q, p.kp_v, p.ki_v, p.pmax, p.pmin, p.qmax,
                p.qmin, p.mbase,
            ],
        ),
        FACTSModel::Cdc3t(p) => (
            "CDC3T",
            vec![
                p.tr, p.kp1, p.ki1, p.kp2, p.ki2, p.kp3, p.ki3, p.pmax, p.pmin, p.mbase,
            ],
        ),
        FACTSModel::Svsmo1(p) => ("SVSMO1", vec![p.tr, p.k, p.ta, p.b_min, p.b_max]),
        FACTSModel::Svsmo2(p) => ("SVSMO2", vec![p.tr, p.k, p.ta, p.iq_min, p.iq_max]),
        FACTSModel::Svsmo3(p) => ("SVSMO3", vec![p.tr, p.ka, p.ta, p.tb, p.b_min, p.b_max]),
    }
}

// ---------------------------------------------------------------------------
// OEL model → DYR params
// ---------------------------------------------------------------------------

fn oel_to_dyr(model: &OelModel) -> (&'static str, Vec<f64>) {
    match model {
        OelModel::Oel1b(p) => (
            "OEL1B",
            vec![p.ifdmax, p.ifdlim, p.vrmax, p.vamin, p.kramp, p.tff],
        ),
        OelModel::Oel2c(p) => ("OEL2C", oel2c_params(p)),
        OelModel::Oel3c(p) => ("OEL3C", oel2c_params(p)),
        OelModel::Oel4c(p) => ("OEL4C", oel2c_params(p)),
        OelModel::Oel5c(p) => ("OEL5C", oel2c_params(p)),
        OelModel::Scl1c(p) => ("SCL1C", vec![p.irated, p.kr, p.tr, p.vclmax, p.vclmin]),
    }
}

fn oel2c_params(p: &Oel2cParams) -> Vec<f64> {
    vec![p.ifdmax, p.t_oel, p.vamin, p.vrmax, p.k_oel]
}

// ---------------------------------------------------------------------------
// UEL model → DYR params
// ---------------------------------------------------------------------------

fn uel_to_dyr(model: &UelModel) -> (&'static str, Vec<f64>) {
    match model {
        UelModel::Uel1(p) => ("UEL1", vec![p.kul, p.tu1, p.vucmax, p.vucmin, p.kur]),
        UelModel::Uel2c(p) => (
            "UEL2C",
            vec![
                p.kul, p.tu1, p.tu2, p.tu3, p.tu4, p.vuimax, p.vuimin, p.p0, p.q0,
            ],
        ),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Push an optional f64 onto a Vec, extending the param list only if present.
fn push_opt(v: &mut Vec<f64>, opt: Option<f64>) {
    if let Some(val) = opt {
        v.push(val);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psse::dyr_impl::parse_str;

    /// Parse a DYR string, write it back, and parse again — verify the model counts match.
    #[test]
    fn test_round_trip_minimal() {
        let input = r#"
            1 'GENROU' 1  8.0 0.03 0.4 0.05 6.5 0.0 1.8 1.7 0.3 0.55 0.25 0.06 0.0 0.0 /
            1 'EXST1'  1  0.02 99.0 -99.0 0.0 0.02 50.0 0.02 9999.0 -9999.0 0.0 0.01 1.0 /
            1 'TGOV1'  1  0.05 0.49 33.0 0.4 2.1 7.0 0.0 /
            2 'GENCLS' 1  3.0 0.0 /
            1 'IEEEST' 1  0 1 -5 5 200 0 1 1 1 0 0 0.2 10 /
            1 'IEEEG1' 1  20 0.05 0.2 1.0 0.3 0 0.6 0.4 0.3 10 0 1 3.3 0 8 0 0 /
        "#;

        let dm1 = parse_str(input).expect("parse original failed");
        let written = to_dyr_string(&dm1).expect("write failed");
        let dm2 = parse_str(&written).expect("parse round-trip failed");

        assert_eq!(dm1.generators.len(), dm2.generators.len());
        assert_eq!(dm1.exciters.len(), dm2.exciters.len());
        assert_eq!(dm1.governors.len(), dm2.governors.len());
        assert_eq!(dm1.pss.len(), dm2.pss.len());

        // Verify a specific parameter survived the round-trip
        let g = dm2.find_generator(1, "1").unwrap();
        if let GeneratorModel::Genrou(p) = &g.model {
            assert!((p.h - 6.5).abs() < 1e-9, "GENROU H mismatch: {}", p.h);
            assert!((p.td0_prime - 8.0).abs() < 1e-9);
        } else {
            panic!("expected GENROU");
        }
    }

    #[test]
    fn test_fmt_param_integer() {
        assert_eq!(fmt_param(0.0), "0");
        assert_eq!(fmt_param(1.0), "1");
        assert_eq!(fmt_param(-5.0), "-5");
        assert_eq!(fmt_param(100.0), "100");
    }

    #[test]
    fn test_fmt_param_float() {
        let s = fmt_param(0.05);
        assert!(s.starts_with("0.05"), "got {s}");
        let s = fmt_param(3.15);
        assert!(s.starts_with("3.15"), "got {s}");
    }

    #[test]
    fn test_unknown_records_preserved() {
        let input = "1 'MYMODEL' 1  1.0 2.0 3.0 /\n";
        let dm = parse_str(input).expect("parse failed");
        assert_eq!(dm.unknown_records.len(), 1);

        let written = to_dyr_string(&dm).expect("write failed");
        assert!(written.contains("'MYMODEL'"), "written: {written}");
        assert!(written.contains("1 2 3"), "written: {written}");
    }
}
