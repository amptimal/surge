// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E `.dyr` dynamic data file parser.
//!
//! `.dyr` files contain electromechanical model definitions that accompany a
//! static `.raw` network file for transient stability analysis.  Each record
//! is terminated by a `/` and may span multiple physical lines.  Comments are
//! introduced by `!` (inline) or `@` (full-line).
//!
//! # Example
//! ```text
//!       1 'GENROU' 1   6.5 0.05 0.4 0.05 6.5 0.0 1.8 1.7 0.3 0.55 0.25 0.06 0.0 0.0 /
//!       1 'EXST1'  1   0.02 99.0 -99.0 0.0 0.02 50.0 0.02 9999.0 -9999.0 0.0 0.01 1.0 /
//!       1 'TGOV1'  1   0.05 0.49 33.0 0.4 2.1 7.0 0.0 /
//! ```

use std::path::Path;

use surge_network::dynamics::{
    Ac8bParams,
    Bbsex1Params,
    Bfr50Params,
    CbestParams,
    CbufdParams,
    CbufrParams,
    // Phase 26
    Cdc3tParams,
    Cdc4tParams,
    Cdc6tParams,
    Cdc7tParams,
    ChaautParams,
    Cim5Params,
    // Wave 34
    Cim6Params,
    ClodParams,
    Cmldblu2Params,
    CmpldwParams,
    CmpldwgParams,
    CpTable,
    CstcntParams,
    CstconParams,
    Csvgn1Params,
    Csvgn3Params,
    Csvgn4Params,
    Csvgn5Params,
    Csvgn6Params,
    Degov1Params,
    DeraParams,
    DercParams,
    // Phase 28
    DerpParams,
    // Wave 36
    Distr1Params,
    DynamicModel,
    Esac2aParams,
    Esac3aParams,
    Esac5aParams,
    Esac6aParams,
    Esac7bParams,
    // Wave 35
    Esac7cParams,
    Esdc1aParams,
    Esdc2aParams,
    Esdc3aParams,
    Esdc4cParams,
    Esst1aParams,
    Esst2aParams,
    Esst3aParams,
    Esst4bParams,
    Esst5bParams,
    Esst6bParams,
    Esst7bParams,
    Esst8cParams,
    Esst9bParams,
    Esst10cParams,
    Exac1Params,
    Exac2Params,
    Exac3Params,
    Exac4Params,
    ExciterDyn,
    ExciterModel,
    // Wave 32
    Exdc1Params,
    Exdc2Params,
    // Wave 33
    Exdc3Params,
    Exst1Params,
    Exst2Params,
    Exst3Params,
    ExtlParams,
    FACTSDyn,
    FACTSModel,
    FrqtpatParams,
    // Phase 27
    FrqtpltParams,
    Gast2aParams,
    GastParams,
    GcscParams,
    GenclsParams,
    GeneratorDyn,
    GeneratorModel,
    GenqecParams,
    GenrouParams,
    Gensal3Params,
    GensalParams,
    GentpjParams,
    GentraParams,
    Ggov1Params,
    Ggov1dParams,
    // Phase 25
    Ggov2Params,
    Ggov3Params,
    Govct1Params,
    Govct2Params,
    GovernorDyn,
    GovernorModel,
    H6bParams,
    H6eParams,
    HvdcPlu1Params,
    Hygov4Params,
    HygovParams,
    HygovdParams,
    Ieeeg1Params,
    Ieeeg1dParams,
    Ieeeg2Params,
    Ieeeg3Params,
    Ieeeg4Params,
    IeeestParams,
    Ieeet1Params,
    Ieeet2Params,
    Ieeet3Params,
    Ieeex1Params,
    IeesgoParams,
    IndmotParams,
    Lcfb1Params,
    LdfralParams,
    LineDiff87lParams,
    LoadDyn,
    LoadModel,
    LvshblParams,
    Mmc1Params,
    MotorParams,
    MotorwParams,
    // Wave 37: OEL/UEL limiter types
    Oel1bParams,
    Oel2cParams,
    OelDyn,
    OelModel,
    PidgovParams,
    Pss1aParams,
    Pss2aParams,
    Pss2bParams,
    Pss2cParams,
    Pss3bParams,
    Pss4bParams,
    Pss5Params,
    Pss6cParams,
    Pss7cParams,
    PssDyn,
    PssModel,
    PsssbParams,
    PvdgParams,
    Pveu1Params,
    Pvgu1Params,
    Recloser79Params,
    ReecaParams,
    ReeccuParams,
    ReecdParams,
    RegcaParams,
    RegcbParams,
    RegccParams,
    Regco1Params,
    RegfmA1Params,
    RegfmB1Params,
    RegfmC1Params,
    Regfmd1Params,
    RepcaParams,
    RepcbParams,
    Repcgfmc1Params,
    RepdcParams,
    RexsParams,
    Scl1cParams,
    ScrxParams,
    SexsParams,
    SsscParams,
    St2cutParams,
    Stab1Params,
    Stab2aParams,
    Stab3Params,
    Stab4Params,
    Stab5Params,
    Stcon1Params,
    Svsmo1Params,
    Svsmo2Params,
    Svsmo3Params,
    TcscParams,
    Tgov1Params,
    Tgov1dParams,
    Tgov1nParams,
    Tgov3Params,
    Tgov5Params,
    TransDiff87Params,
    Uel1Params,
    Uel2cParams,
    UelDyn,
    UelModel,
    UnknownDyrRecord,
    UpfcParams,
    // UVLS1: under-voltage load shedding relay
    Uvls1Params,
    VscdctParams,
    VtgtpatParams,
    WehgovParams,
    WpidhyParams,
    WshyddParams,
    WshygpParams,
    Wt1g1Params,
    Wt2e1Params,
    Wt3e1Params,
    Wt3e2Params,
    Wt3g2uParams,
    Wt3p1Params,
    Wt3t1Params,
    Wt4e1Params,
    Wt4g1Params,
    Wt4g2Params,
    WtaeroParams,
    Wtara1Params,
    Wtdta1Params,
    Wtpta1Params,
    Wttqa1Params,
};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum DyrError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error at line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("insufficient parameters for {model}: expected >= {expected}, got {got}")]
    InsufficientParams {
        model: String,
        expected: usize,
        got: usize,
    },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a PSS/E `.dyr` file from disk.
pub fn parse_file(path: &Path) -> Result<DynamicModel, DyrError> {
    let content = std::fs::read_to_string(path)?;
    parse_str(&content)
}

/// Parse a PSS/E `.dyr` format string.
pub fn parse_str(content: &str) -> Result<DynamicModel, DyrError> {
    let mut model = DynamicModel::default();

    for (start_line, record_text) in join_records(content) {
        match parse_record(start_line, &record_text) {
            Ok(record) => dispatch_record(record, &mut model),
            Err(e) => {
                // Non-fatal: warn and skip malformed records
                tracing::warn!("skipping malformed DYR record at line {start_line}: {e}");
            }
        }
    }

    Ok(model)
}

// ---------------------------------------------------------------------------
// Record joining — iterate lines, collect tokens until `/` terminator
// ---------------------------------------------------------------------------

/// A single parsed DYR record with its start line and raw token text.
struct RawRecord {
    start_line: usize,
    bus_str: String,
    model_name: String,
    machine_id: String,
    params: Vec<f64>,
}

/// Join physical lines into logical records terminated by `/`.
///
/// Returns `(start_line, record_text)` pairs where `record_text` is the
/// concatenated token text of one record (everything before the `/`).
fn join_records(content: &str) -> Vec<(usize, String)> {
    let mut records = Vec::new();
    let mut current = String::new();
    let mut start_line = 1usize;
    let mut current_start = 1usize;

    for (line_idx, line) in content.lines().enumerate() {
        let line_no = line_idx + 1;

        // Strip full-line `@` comment
        let line = if line.trim_start().starts_with('@') {
            ""
        } else {
            line
        };

        // Strip inline `!` comment
        let line = strip_dyr_comment(line);
        let line = line.trim();

        if line.is_empty() {
            if current.is_empty() {
                current_start = line_no + 1;
            }
            continue;
        }

        // Check if the line (or part of it) contains a `/` terminator.
        // A `/` inside a quoted string is NOT a terminator, but DYR model names
        // are always quoted identifiers that don't contain `/`, so we can split
        // on the first unquoted `/`.
        if let Some(pos) = find_record_terminator(line) {
            current.push(' ');
            current.push_str(&line[..pos]);
            if !current.trim().is_empty() {
                records.push((current_start, current.trim().to_string()));
            }
            current = String::new();
            current_start = line_no + 1;
            start_line = current_start;

            // There might be content after the `/` on the same line — ignore
            // (the next record would start on the following line in real files,
            // but handle it just in case by feeding the remainder back).
            let remainder = line[pos + 1..].trim();
            if !remainder.is_empty() && !remainder.starts_with('@') {
                let remainder = strip_dyr_comment(remainder).trim().to_string();
                if !remainder.is_empty() {
                    current.push_str(&remainder);
                    current_start = line_no;
                }
            }
        } else {
            if current.is_empty() {
                current_start = line_no;
            }
            current.push(' ');
            current.push_str(line);
        }
        let _ = start_line; // suppress unused warning
    }

    records
}

/// Find the position of the first `/` that is not inside single-quoted text.
fn find_record_terminator(s: &str) -> Option<usize> {
    let mut in_quote = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '\'' => in_quote = !in_quote,
            '/' if !in_quote => return Some(i),
            _ => {}
        }
    }
    None
}

/// Strip everything from the first `!` (that is not inside a quoted string).
fn strip_dyr_comment(s: &str) -> &str {
    let mut in_quote = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '\'' => in_quote = !in_quote,
            '!' if !in_quote => return &s[..i],
            _ => {}
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Record parsing — tokenise and extract bus / model / machine_id / params
// ---------------------------------------------------------------------------

/// Parse a single joined record string into a `RawRecord`.
fn parse_record(start_line: usize, text: &str) -> Result<RawRecord, DyrError> {
    let tokens = tokenize(text);

    if tokens.len() < 3 {
        return Err(DyrError::Parse {
            line: start_line,
            message: format!("too few tokens ({}) in record", tokens.len()),
        });
    }

    let bus_str = tokens[0].clone();
    let model_name = unquote(&tokens[1]);
    let machine_id = unquote(&tokens[2]);

    // Parse numeric params from tokens[3..]
    let mut params = Vec::with_capacity(tokens.len().saturating_sub(3));
    for tok in &tokens[3..] {
        match tok.parse::<f64>() {
            Ok(v) => params.push(v),
            Err(_) => {
                // Integer tokens like `0` parse fine as f64, but non-numeric
                // tokens (e.g. trailing text) are silently skipped.
                tracing::trace!("skipping non-numeric token in DYR record: {tok:?}");
            }
        }
    }

    Ok(RawRecord {
        start_line,
        bus_str,
        model_name,
        machine_id,
        params,
    })
}

/// Tokenize a string, handling single-quoted tokens as atomic units.
fn tokenize(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = s.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if ch.is_ascii_whitespace() || ch == ',' {
            continue;
        }
        if ch == '\'' {
            // Collect everything until the closing quote.
            let mut tok = String::new();
            tok.push('\'');
            for (_, c) in chars.by_ref() {
                tok.push(c);
                if c == '\'' {
                    break;
                }
            }
            tokens.push(tok);
        } else {
            // Collect a whitespace/comma-delimited token.
            let start = i;
            let mut end = start + ch.len_utf8();
            while let Some(&(j, c)) = chars.peek() {
                if c.is_ascii_whitespace() || c == ',' {
                    break;
                }
                end = j + c.len_utf8();
                chars.next();
            }
            tokens.push(s[start..end].to_string());
        }
    }

    tokens
}

use crate::parse_utils::unquote;

// ---------------------------------------------------------------------------
// Dispatch — route a raw record into the correct DynamicModel bucket
// ---------------------------------------------------------------------------

fn dispatch_record(rec: RawRecord, model: &mut DynamicModel) {
    let RawRecord {
        start_line,
        bus_str,
        model_name,
        machine_id,
        params,
    } = rec;

    // Try to parse the bus number.  Non-numeric bus fields (e.g. "Line" in
    // PSS/E toggle records) go to unknown_records with bus=0.
    let bus: u32 = match bus_str.parse() {
        Ok(b) => b,
        Err(_) => {
            model.unknown_records.push(UnknownDyrRecord {
                bus: 0,
                model_name,
                machine_id,
                params,
            });
            return;
        }
    };

    let name_upper = model_name.to_ascii_uppercase();

    match name_upper.as_str() {
        // ---- Generator models -----------------------------------------------
        "GENTPJ" => match build_gentpj(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Gentpj(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENTPF" => match build_gentpj(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Gentpf(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENTRA" => match build_gentra(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Gentra(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REGCC" => match build_regcc(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Regcc(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT4G2" => match build_wt4g2(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Wt4g2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "DER_C" | "DERC" => match build_derc(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Derc(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENQEC" => match build_genqec(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Genqec(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENCLS" => match build_gencls(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Gencls(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENROU" => match build_genrou(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Genrou(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENSAL" => match build_gensal(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Gensal(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Exciter models -------------------------------------------------
        "EXST1" => match build_exst1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exst1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESST3A" => match build_esst3a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst3a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESDC2A" => match build_esdc2a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esdc2a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "EXDC2" => match build_exdc2(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exdc2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEEX1" | "IEEEXC1" => match build_ieeex1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Ieeex1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESST1A" => match build_esst1a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst1a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "EXAC1" => match build_exac1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exac1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC1A" => match build_exac1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac1a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC7B" | "AC7B" => match build_esac7b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac7b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESST4B" => match build_esst4b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst4b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "SEXS" => match build_sexs(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Sexs(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEET1" => match build_ieeet1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Ieeet1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "SCRX" => match build_scrx(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Scrx(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REEC_A" | "REECA" => match build_reeca(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Reeca(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REEC_D" | "REECD" => match build_reecd(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Reecd(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REEC_C" | "REECCU" | "REECCU1" => match build_reeccu(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Reeccu(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REXS" => match build_rexs(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Rexs(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC2A" | "AC2A" => match build_esac2a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac2a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC5A" | "AC5A" => match build_esac5a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac5a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Phase 15 exciters
        "ESST5B" | "ST5B" => match build_esst5b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst5b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "EXAC4" | "AC4A" => match build_exac4(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exac4(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Generator models (IBR) -----------------------------------------
        "REGC_A" | "REGCA" => match build_regca(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Regca(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REGC_B" | "REGCB" | "REGCB1" => match build_regcb(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Regcb(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT3G2U" => match build_wt3g2u(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Wt3g2u(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT4G1" => match build_wt4g1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Wt4g1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REGFM_A1" => match build_regfm_a1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::RegfmA1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REGFM_B1" => match build_regfm_b1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::RegfmB1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "DER_A" | "DERA" => match build_dera(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Dera(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Governor models ------------------------------------------------
        "TGOV1" => match build_tgov1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Tgov1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEEG1" => match build_ieeeg1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ieeeg1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GGOV1" => match build_ggov1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ggov1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "HYGOV" => match build_hygov(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Hygov(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "HYGOVD" => match build_hygovd(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Hygovd(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "TGOV1D" => match build_tgov1d(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Tgov1d(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEEG1D" => match build_ieeeg1d(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ieeeg1d(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WSIEG1" => match build_ieeeg1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wsieg1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEEG2" => match build_ieeeg2(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ieeeg2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GAST" => match build_gast(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Gast(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REPC_A" | "REPCA" => match build_repca(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Repca(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REPC_D" | "REPCD" => match build_repcd(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Repcd(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT3T1" | "WT3T1U" => match build_wt3t1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wt3t1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT3P1" | "WT3P1U" => match build_wt3p1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wt3p1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GGOV1D" => match build_ggov1d(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ggov1d(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "TGOV1N" | "TGOV1NDB" => match build_tgov1n(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Tgov1n(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CBEST" => match build_cbest(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Cbest(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CHAAUT" => match build_chaaut(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Chaaut(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PIDGOV" => match build_pidgov(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Pidgov(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "DEGOV1" => match build_degov1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Degov1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Phase 15 governors
        "TGOV5" => match build_tgov5(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Tgov5(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GAST2A" => match build_gast2a(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Gast2a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- PSS models -----------------------------------------------------
        "PSS2A" => match build_pss2a(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss2a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS2B" => match build_pss2b(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss2b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "STAB1" => match build_stab1(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Stab1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEEST" => match build_ieeest(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Ieeest(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ST2CUT" => match build_st2cut(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::St2cut(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS1A" => match build_pss1a(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss1a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Phase 15 PSS
        "STAB2A" => match build_stab2a(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Stab2a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS4B" => match build_pss4b(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss4b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Load dynamic models (Phase 12) ---------------------------------
        "CLOD" => match build_clod(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Clod(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "INDMOT" => match build_indmot(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Indmot(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "MOTOR" => match build_motor(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Motor(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 13: FACTS / HVDC models ----------------------------------
        "CSVGN1" => match build_csvgn1(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Csvgn1(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CSTCON" => match build_cstcon(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Cstcon(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "TCSC" => match build_tcsc(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Tcsc(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CDC4T" => match build_cdc4t(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Cdc4t(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "VSCDCT" => match build_vscdct(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Vscdct(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Phase 15 FACTS
        "CSVGN3" => match build_csvgn3(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Csvgn3(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CDC7T" => match build_cdc7t(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Cdc7t(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 16: Load models ------------------------------------------
        "CMPLDW" => match build_cmpldw(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmpldw(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMPLDWG" => match build_cmpldwg(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmpldwg(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDBLU2" => match build_cmldblu2(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldblu2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDARU2" => match build_cmldblu2(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldaru2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "MOTORW" => match build_motorw(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Motorw(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CIM5" => match build_cim5(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cim5(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: CIM6 — CIM-type induction motor with Q-axis dynamics
        "CIM6" => match build_cim6(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cim6(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: CIMW — CIM-type induction motor (WECC variant, reuses IndmotParams)
        "CIMW" => match build_indmot(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cimw(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: EXTL — External load model with P/Q voltage/frequency sensitivity
        "EXTL" => match build_extl(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Extl(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: IEELAR — IEEE load-area representation (same params as EXTL)
        "IEELAR" => match build_extl(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Ieelar(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: CMLD variant aliases (reuse CMPLDW/CMLDBLU2 builders)
        "CMLDOWU2" => match build_cmpldw(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldowu2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDXNU2" => match build_cmpldw(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldxnu2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDALU2" => match build_cmpldw(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldalu2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDBLU2W" => match build_cmldblu2(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldblu2w(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDARU2W" => match build_cmldblu2(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldaru2w(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 17: Exciter models ----------------------------------------
        "ESST6B" => match build_esst6b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst6b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESST7B" => match build_esst7b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst7b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC6A" => match build_esac6a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac6a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESDC1A" => match build_esdc1a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esdc1a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "EXST2" => match build_exst2(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exst2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "AC8B" => match build_ac8b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Ac8b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "BBSEX1" => match build_bbsex1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Bbsex1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEET3" => match build_ieeet3(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Ieeet3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 18: Governor + PSS models --------------------------------
        "H6E" => match build_h6e(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::H6e(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WSHYGP" => match build_wshygp(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wshygp(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "STAB3" => match build_stab3(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Stab3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS3B" => match build_pss3b(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss3b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 19: IBR wind controllers (exciter slot) ------------------
        "WT3E1" => match build_wt3e1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Wt3e1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT3E2" => match build_wt3e2(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Wt3e2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT4E1" => match build_wt4e1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Wt4e1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT4E2" => match build_wt4e1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Wt4e2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REPCB" => match build_repcb(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Repcb(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REPCC" => match build_repcb(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Repcc(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 20: FACTS/HVDC models ------------------------------------
        "CSVGN4" => match build_csvgn4(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Csvgn4(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CSVGN5" => match build_csvgn5(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Csvgn5(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CDC6T" => match build_cdc6t(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Cdc6t(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CSTCNT" => match build_cstcnt(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Cstcnt(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "MMC1" => match build_mmc1(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Mmc1(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 21: Remaining models -------------------------------------
        "GENROA" => match build_genrou(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Genroa(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENSAA" => match build_gensal(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Gensaa(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: GENSAE — exponential saturation salient-pole (same params as GENSAL)
        "GENSAE" => match build_gensal(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Gensae(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REGFM_C1" => match build_regfm_c1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::RegfmC1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "EXST3" => match build_exst3(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exst3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CBUFR" => match build_cbufr(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Cbufr(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CBUFD" => match build_cbufd(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Cbufd(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 22: Solar PV generators ----------------------------------
        "PVGU1" => match build_pvgu1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Pvgu1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PVDG" => match build_pvdg(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Pvdg(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // PVEU1 occupies the exciter slot (electrical controller)
        "PVEU1" => match build_pveu1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Pveu1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 23: Exciter models ----------------------------------------
        "IEEET2" => match build_ieeet2(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Ieeet2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "EXAC2" => match build_exac2(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exac2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "EXAC3" | "AC3A" => match build_exac3(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exac3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC3A" => match build_esac3a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac3a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESST8C" | "ST8C" => match build_esst8c(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst8c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESST9B" | "ST9B" => match build_esst9b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst9b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESST10C" | "ST10C" => match build_esst10c(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst10c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESDC3A" | "DC3A" => match build_esdc3a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esdc3a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 33: EXDC3 ---------------------------------------------------
        "EXDC3" => match build_exdc3(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exdc3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 32: EXDC1 + ESST2A -----------------------------------------
        "EXDC1" | "DC1" => match build_exdc1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Exdc1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESST2A" | "ST2A" => match build_esst2a(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esst2a(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 24: PSS variant models ------------------------------------
        "PSS2C" => match build_pss2c(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss2c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS5" => match build_pss5(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss5(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS6C" => match build_pss6c(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss6c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSSSB" => match build_psssb(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Psssb(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "STAB4" => match build_stab4(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Stab4(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "STAB5" => match build_stab5(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Stab5(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 25: Governor variants ------------------------------------
        "GGOV2" => match build_ggov2(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ggov2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GGOV3" => match build_ggov3(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ggov3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WPIDHY" => match build_wpidhy(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wpidhy(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "H6B" => match build_h6b(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::H6b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WSHYDD" => match build_wshydd(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wshydd(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 26: HVDC/FACTS advanced models ---------------------------
        "HVDCPLU1" | "HVDC_PLU1" => match build_hvdcplu1(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Hvdcplu1(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CSVGN6" => match build_csvgn6(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Csvgn6(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "STCON1" => match build_stcon1(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Stcon1(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GCSC" => match build_gcsc(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Gcsc(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "SSSC" => match build_sssc(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Sssc(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "UPFC" => match build_upfc(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Upfc(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CDC3T" => match build_cdc3t(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Cdc3t(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: SVSMO1 — Static Var System Operator (reuses Csvgn1Params)
        "SVSMO1" => match build_svsmo1(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Svsmo1(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: SVSMO2 — Static Var System Operator type 2 (reuses Svsmo2Params)
        "SVSMO2" => match build_svsmo2(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Svsmo2(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: SVSMO3 — Static Var System Operator type 3 (lead-lag + SVC)
        "SVSMO3" => match build_svsmo3(start_line, &params) {
            Ok(p) => model.facts.push(FACTSDyn {
                bus,
                device_id: machine_id,
                model: FACTSModel::Svsmo3(p),
                to_bus: None,
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 27: IBR/Generator/Load/Protection models -----------------
        "WT3G3" => match build_wt3g2u(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Wt3g3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REGCO1" => match build_regco1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Regco1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENWTG" => match build_genrou(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Genwtg(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENROE" => match build_genrou(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Genroe(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GENSAL3" => match build_gensal3(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Gensal3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT3C2" => match build_wt3e1(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Wt3c2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "LCFB1" => match build_lcfb1(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Lcfb1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "LDFRAL" => match build_ldfral(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Ldfral(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "FRQTPLT" => match build_frqtplt(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Frqtplt(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "LVSHBL" => match build_lvshbl(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Lvshbl(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "UVLS1" => match build_uvls1(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Uvls1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Phase 28: Wind aerodynamic / pitch / drive-train + GFM/DER -----
        "REPCGFM_C1" | "REPCGFMC1" => match build_repcgfmc1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Repcgfmc1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "DERP" | "DER_P" => match build_derp(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Derp(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REGFM_D1" | "REGFMD1" => match build_regfmd1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::RegfmD1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WTDTA1" => match build_wtdta1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wtdta1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WTARA1" => match build_wtara1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wtara1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WTAERO" => match build_wtaero(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wtaero(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WTPTA1" => match build_wtpta1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wtpta1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: IEESGO — IEEE Standard Governor (steam turbine, 5 cascaded TFs)
        "IEESGO" => match build_ieesgo(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ieesgo(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 34: WTTQA1 — WECC Type-2 Wind Torque Controller
        "WTTQA1" => match build_wttqa1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wttqa1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 35: new hydro governors -----------------------------------
        "HYGOV4" => match build_hygov4(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Hygov4(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WEHGOV" => match build_wehgov(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wehgov(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEEG3" => match build_ieeeg3(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ieeeg3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "IEEEG4" => match build_ieeeg4(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Ieeeg4(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 35: C-series exciters -------------------------------------
        "ESAC7C" | "AC7C" => match build_esac7c(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac7c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESDC4C" | "DC4C" => match build_esdc4c(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esdc4c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 35: C-series PSS ------------------------------------------
        "PSS3C" => match build_pss3b(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss3c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS4C" => match build_pss4b(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss4c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS5C" => match build_pss5(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss5c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PSS7C" => match build_pss7c(start_line, &params) {
            Ok(p) => model.pss.push(PssDyn {
                bus,
                machine_id,
                model: PssModel::Pss7c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 35: generator protection relays ---------------------------
        "VTGTPAT" | "VTGPAT" => match build_vtgtpat(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Vtgtpat(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "VTGDCAT" => match build_vtgtpat(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Vtgdcat(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "FRQTPAT" | "FRQPAT" => match build_frqtpat(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Frqtpat(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "FRQDCAT" => match build_frqtpat(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Frqdcat(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 36: Combined Cycle Governors --------------------------------
        "GOVCT1" | "GOVCT" => match build_govct1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Govct1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "GOVCT2" => match build_govct2(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Govct2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "TGOV3" => match build_tgov3(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Tgov3(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "TGOV4" => match build_tgov3(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Tgov4(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT2E1" | "WT2E" => match build_wt2e1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wt2e1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT12T1" => match build_wt3t1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wt12t1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT12A1" => match build_wt3p1(start_line, &params) {
            Ok(p) => model.governors.push(GovernorDyn {
                bus,
                machine_id,
                model: GovernorModel::Wt12a1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 36: Legacy Wind Generators --------------------------------
        "WT1G1" | "WT1G" => match build_wt1g1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Wt1g1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "WT2G1" | "WT2G" => match build_wt1g1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Wt2g1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PVD1" => match build_pvdg(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Pvd1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "PVDU1" => match build_pvgu1(start_line, &params) {
            Ok(p) => model.generators.push(GeneratorDyn {
                bus,
                machine_id,
                model: GeneratorModel::Pvdu1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 36: New REEC variants -------------------------------------
        "REECBU1" => match build_reeccu(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Reecbu1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REECE" | "REEC_E" => match build_reeca(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Reece(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "REECEU1" | "REECE1" => match build_reeccu(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Reeceu1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 36: Protection Models -------------------------------------
        "DISTR1" | "DISTR" => match build_distr1(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Distr1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "BFR50" | "BFR50BF" => match build_bfr50(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Bfr50(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "LVSHC1" | "LVSHCB" => match build_lvshbl(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Lvshc1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        // Wave 7 (B10): additional protection relay models
        "87T" | "TRANSDIFF87" => match build_trans_diff_87(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::TransDiff87(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "87L" | "LINEDIFF87" => match build_line_diff_87l(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::LineDiff87l(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "79" | "RECLOSER79" => match build_recloser_79(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Recloser79(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 37: IEEE 421.5-2016 C-series AC exciters ----------------
        "ESAC8C" | "AC8C" => match build_ac8b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac8c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC9C" | "AC9C" => match build_esac9c(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac9c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC10C" | "AC10C" => match build_esac7c(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac10c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "ESAC11C" | "AC11C" => match build_ac8b(start_line, &params) {
            Ok(p) => model.exciters.push(ExciterDyn {
                bus,
                machine_id,
                model: ExciterModel::Esac11c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 37: CLM DG load variants ----------------------------------
        "CMLDDGU2" => match build_cmpldw(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmlddgu2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDDGGU2" => match build_cmpldwg(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmlddggu2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDOWDGU2" => match build_cmpldw(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldowdgu2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "CMLDXNDGU2" => match build_cmpldw(start_line, &params) {
            Ok(p) => model.loads.push(LoadDyn {
                bus,
                load_id: machine_id,
                model: LoadModel::Cmldxndgu2(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 37: OEL limiters ------------------------------------------
        "OEL1B" | "OEL1" => match build_oel1b(start_line, &params) {
            Ok(p) => model.oels.push(OelDyn {
                bus,
                machine_id,
                model: OelModel::Oel1b(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "OEL2C" => match build_oel2c(start_line, &params) {
            Ok(p) => model.oels.push(OelDyn {
                bus,
                machine_id,
                model: OelModel::Oel2c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "OEL3C" => match build_oel2c(start_line, &params) {
            Ok(p) => model.oels.push(OelDyn {
                bus,
                machine_id,
                model: OelModel::Oel3c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "OEL4C" => match build_oel2c(start_line, &params) {
            Ok(p) => model.oels.push(OelDyn {
                bus,
                machine_id,
                model: OelModel::Oel4c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "OEL5C" => match build_oel2c(start_line, &params) {
            Ok(p) => model.oels.push(OelDyn {
                bus,
                machine_id,
                model: OelModel::Oel5c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "SCL1C" => match build_scl1c(start_line, &params) {
            Ok(p) => model.oels.push(OelDyn {
                bus,
                machine_id,
                model: OelModel::Scl1c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Wave 37: UEL limiters ------------------------------------------
        "UEL1" => match build_uel1(start_line, &params) {
            Ok(p) => model.uels.push(UelDyn {
                bus,
                machine_id,
                model: UelModel::Uel1(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },
        "UEL2C" => match build_uel2c(start_line, &params) {
            Ok(p) => model.uels.push(UelDyn {
                bus,
                machine_id,
                model: UelModel::Uel2c(p),
            }),
            Err(e) => tracing::warn!("{e}"),
        },

        // ---- Unknown --------------------------------------------------------
        _ => {
            tracing::warn!(
                "DYR: unknown model '{model_name}' at bus {bus} machine '{machine_id}' — stored in unknown_records"
            );
            model.unknown_records.push(UnknownDyrRecord {
                bus,
                model_name,
                machine_id,
                params,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter extraction helpers
// ---------------------------------------------------------------------------

fn check_params(model: &str, params: &[f64], min: usize, _line: usize) -> Result<(), DyrError> {
    if params.len() < min {
        Err(DyrError::InsufficientParams {
            model: model.to_string(),
            expected: min,
            got: params.len(),
        })
    } else {
        Ok(())
    }
}

fn opt(params: &[f64], idx: usize) -> Option<f64> {
    params.get(idx).copied()
}

fn p(params: &[f64], idx: usize) -> f64 {
    params[idx]
}

// ---------------------------------------------------------------------------
// Generator builders
// ---------------------------------------------------------------------------

fn build_gencls(line: usize, params: &[f64]) -> Result<GenclsParams, DyrError> {
    check_params("GENCLS", params, 2, line)?;
    Ok(GenclsParams {
        h: p(params, 0),
        d: p(params, 1),
    })
}

fn build_genrou(line: usize, params: &[f64]) -> Result<GenrouParams, DyrError> {
    check_params("GENROU", params, 14, line)?;
    Ok(GenrouParams {
        td0_prime: p(params, 0),
        td0_pprime: p(params, 1),
        tq0_prime: p(params, 2),
        tq0_pprime: p(params, 3),
        h: p(params, 4),
        d: p(params, 5),
        xd: p(params, 6),
        xq: p(params, 7),
        xd_prime: p(params, 8),
        xq_prime: p(params, 9),
        xd_pprime: p(params, 10),
        xl: p(params, 11),
        s1: p(params, 12),
        s12: p(params, 13),
        ra: opt(params, 14),
    })
}

fn build_gensal(line: usize, params: &[f64]) -> Result<GensalParams, DyrError> {
    check_params("GENSAL", params, 13, line)?;
    Ok(GensalParams {
        td0_prime: p(params, 0),
        td0_pprime: p(params, 1),
        tq0_pprime: p(params, 2),
        h: p(params, 3),
        d: p(params, 4),
        xd: p(params, 5),
        xq: p(params, 6),
        xd_prime: p(params, 7),
        xd_pprime: p(params, 8),
        xl: p(params, 9),
        s1: p(params, 10),
        s12: p(params, 11),
        xtran: p(params, 12),
    })
}

// ---------------------------------------------------------------------------
// Exciter builders
// ---------------------------------------------------------------------------

fn build_exst1(line: usize, params: &[f64]) -> Result<Exst1Params, DyrError> {
    check_params("EXST1", params, 12, line)?;
    Ok(Exst1Params {
        tr: p(params, 0),
        vimax: p(params, 1),
        vimin: p(params, 2),
        tc: p(params, 3),
        tb: p(params, 4),
        ka: p(params, 5),
        ta: p(params, 6),
        vrmax: p(params, 7),
        vrmin: p(params, 8),
        kc: p(params, 9),
        kf: p(params, 10),
        tf: p(params, 11),
        klr: opt(params, 12),
        ilr: opt(params, 13),
    })
}

fn build_esst3a(line: usize, params: &[f64]) -> Result<Esst3aParams, DyrError> {
    check_params("ESST3A", params, 14, line)?;
    Ok(Esst3aParams {
        tr: p(params, 0),
        vimax: p(params, 1),
        vimin: p(params, 2),
        km: p(params, 3),
        tc: p(params, 4),
        tb: p(params, 5),
        ka: p(params, 6),
        ta: p(params, 7),
        vrmax: p(params, 8),
        vrmin: p(params, 9),
        kg: p(params, 10),
        kp: p(params, 11),
        ki: p(params, 12),
        vbmax: p(params, 13),
    })
}

fn build_esdc2a(line: usize, params: &[f64]) -> Result<Esdc2aParams, DyrError> {
    check_params("ESDC2A", params, 12, line)?;
    Ok(Esdc2aParams {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        tb: p(params, 3),
        tc: p(params, 4),
        vrmax: p(params, 5),
        vrmin: p(params, 6),
        ke: p(params, 7),
        te: p(params, 8),
        kf: p(params, 9),
        tf1: p(params, 10),
        switch_: p(params, 11),
    })
}

fn build_exdc2(line: usize, params: &[f64]) -> Result<Exdc2Params, DyrError> {
    check_params("EXDC2", params, 12, line)?;
    Ok(Exdc2Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        tb: p(params, 3),
        tc: p(params, 4),
        vrmax: p(params, 5),
        vrmin: p(params, 6),
        ke: p(params, 7),
        te: p(params, 8),
        kf: p(params, 9),
        tf1: p(params, 10),
        switch_: p(params, 11),
        e1: opt(params, 12),
        se1: opt(params, 13),
        e2: opt(params, 14),
        se2: opt(params, 15),
    })
}

fn build_ieeex1(line: usize, params: &[f64]) -> Result<Ieeex1Params, DyrError> {
    check_params("IEEEX1", params, 12, line)?;
    Ok(Ieeex1Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        tb: p(params, 3),
        tc: p(params, 4),
        vrmax: p(params, 5),
        vrmin: p(params, 6),
        ke: p(params, 7),
        te: p(params, 8),
        kf: p(params, 9),
        tf: p(params, 10),
        aex: p(params, 11),
        bex: if params.len() > 12 {
            p(params, 12)
        } else {
            0.0
        },
        e1: opt(params, 13),
        se1: opt(params, 14),
        e2: opt(params, 15),
        se2: opt(params, 16),
    })
}

// ---------------------------------------------------------------------------
// Governor builders
// ---------------------------------------------------------------------------

fn build_tgov1(line: usize, params: &[f64]) -> Result<Tgov1Params, DyrError> {
    check_params("TGOV1", params, 6, line)?;
    Ok(Tgov1Params {
        r: p(params, 0),
        t1: p(params, 1),
        vmax: p(params, 2),
        vmin: p(params, 3),
        t2: p(params, 4),
        t3: p(params, 5),
        dt: opt(params, 6),
    })
}

fn build_ieeeg1(line: usize, params: &[f64]) -> Result<Ieeeg1Params, DyrError> {
    check_params("IEEEG1", params, 9, line)?;
    Ok(Ieeeg1Params {
        k: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        uo: p(params, 4),
        uc: p(params, 5),
        pmax: p(params, 6),
        pmin: p(params, 7),
        t4: p(params, 8),
        k1: opt(params, 9),
        k2: opt(params, 10),
        t5: opt(params, 11),
        k3: opt(params, 12),
        k4: opt(params, 13),
        t6: opt(params, 14),
        k5: opt(params, 15),
        k6: opt(params, 16),
        t7: opt(params, 17),
        k7: opt(params, 18),
        k8: opt(params, 19),
    })
}

fn build_ggov1(line: usize, params: &[f64]) -> Result<Ggov1Params, DyrError> {
    // PSS/E GGOV1 params (positions in DYR record):
    // 0:R  1:RSELECT  2:TPELEC  3:MAXERR  4:MINERR
    // 5:KPGOV  6:KIGOV  7:KDGOV  8:TDGOV
    // 9:VMAX  10:VMIN  11:TSA  12:FSR  13:TSB  14:TSE  15:IANG  16:KCMF
    // 17:KTURB  18:WFNL  19:TB  20:TC  21:TRATE  22:FLAG
    check_params("GGOV1", params, 19, line)?;
    Ok(Ggov1Params {
        r: p(params, 0),
        tpelec: p(params, 2),
        vmax: p(params, 9),
        vmin: p(params, 10),
        kpgov: p(params, 5),
        kigov: p(params, 6),
        kturb: p(params, 17),
        wfnl: p(params, 18),
        tb: opt(params, 19).unwrap_or(0.5),
        tc: opt(params, 20).unwrap_or(0.0),
        trate: opt(params, 21),
        ldref: None, // set to Pm0 during initialization
        dm: None,
    })
}

// ---------------------------------------------------------------------------
// PSS builders
// ---------------------------------------------------------------------------

fn build_ieeest(line: usize, params: &[f64]) -> Result<IeeestParams, DyrError> {
    check_params("IEEEST", params, 13, line)?;
    Ok(IeeestParams {
        a1: p(params, 0),
        a2: p(params, 1),
        a3: p(params, 2),
        a4: p(params, 3),
        a5: p(params, 4),
        a6: p(params, 5),
        t1: p(params, 6),
        t2: p(params, 7),
        t3: p(params, 8),
        t4: p(params, 9),
        t5: p(params, 10),
        t6: p(params, 11),
        ks: p(params, 12),
        lsmax: opt(params, 13),
        lsmin: opt(params, 14),
        vcu: opt(params, 15),
        vcl: opt(params, 16),
    })
}

fn build_st2cut(line: usize, params: &[f64]) -> Result<St2cutParams, DyrError> {
    // PSS/E ST2CUT DYR format header has 4 control integers before model params:
    //   MODE(0)  BUSR(1)  MODE2(2)  BUSR2(3)  K1(4)  K2(5)  T1(6)  T2(7)  T3(8)  T4(9)
    //   T5(10)  T6(11)  T7(12)  T8(13)  T9(14)  T10(15)  LSMAX(16)  LSMIN(17)  VCU(18)  VCL(19)
    // The MODE/BUSR/MODE2/BUSR2 fields are input-signal selectors (not PSS dynamics params).
    // Also ensure lsmax >= lsmin (some files store positive lsmin that should be negated).
    check_params("ST2CUT", params, 17, line)?;
    let lsmax_raw = p(params, 16);
    let lsmin_raw = opt(params, 17);
    // Normalize: lsmax must be >= lsmin (lsmin is typically negative)
    let (lsmax, lsmin) = match lsmin_raw {
        Some(v) if v > lsmax_raw => {
            // Swap or negate: if both are positive it means the file stores magnitude
            if v >= 0.0 && lsmax_raw >= 0.0 {
                (v, Some(-lsmax_raw.max(v.abs())))
            } else {
                (v.abs(), Some(-lsmax_raw.abs()))
            }
        }
        other => (lsmax_raw, other),
    };
    Ok(St2cutParams {
        k1: p(params, 4),
        t1: p(params, 6),
        t2: p(params, 7),
        t3: p(params, 8),
        t4: p(params, 9),
        k2: p(params, 5),
        t5: p(params, 10),
        t6: p(params, 11),
        t7: p(params, 12),
        t8: p(params, 13),
        lsmax,
        lsmin,
        vcu: opt(params, 18),
        vcl: opt(params, 19),
    })
}

// ---------------------------------------------------------------------------
// New exciter builders (Phase 9 + 8)
// ---------------------------------------------------------------------------

fn build_sexs(line: usize, params: &[f64]) -> Result<SexsParams, DyrError> {
    check_params("SEXS", params, 6, line)?;
    // PSS/E SEXS format: TA/TB  TB  K  TE  EMIN  EMAX
    let tatb_ratio = p(params, 0); // TA/TB ratio
    let tb_val = p(params, 1); // TB (lag time constant)
    let tc_val = tatb_ratio * tb_val; // TC = (TA/TB) × TB = TA (lead time constant)
    Ok(SexsParams {
        tb: tb_val,
        tc: tc_val,
        k: p(params, 2),
        te: p(params, 3),
        emin: p(params, 4),
        emax: p(params, 5),
    })
}

fn build_ieeet1(line: usize, params: &[f64]) -> Result<Ieeet1Params, DyrError> {
    // PSS/E: TR KA TA KE TE KF TF E1 SE1 E2 SE2 [VRMAX VRMIN]
    check_params("IEEET1", params, 11, line)?;
    Ok(Ieeet1Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        ke: p(params, 3),
        te: p(params, 4),
        kf: p(params, 5),
        tf: p(params, 6),
        e1: p(params, 7),
        se1: p(params, 8),
        e2: p(params, 9),
        se2: p(params, 10),
        vrmax: opt(params, 11),
        vrmin: opt(params, 12),
    })
}

fn build_scrx(line: usize, params: &[f64]) -> Result<ScrxParams, DyrError> {
    // PSS/E: TR K TE EMIN EMAX [Rcrfd]
    check_params("SCRX", params, 5, line)?;
    Ok(ScrxParams {
        tr: p(params, 0),
        k: p(params, 1),
        te: p(params, 2),
        emin: p(params, 3),
        emax: p(params, 4),
        rcrfd: opt(params, 5),
    })
}

fn build_reeca(line: usize, params: &[f64]) -> Result<ReecaParams, DyrError> {
    // PSS/E REEC_A params (simplified — use defaults for missing trailing params):
    // 0:Vdip 1:Vup 2:Trv 3:Dbd1 4:Dbd2 5:Kqv 6:Iqh1 7:Iql1 8:Vref0 9:Tp 10:Qmax
    // 11:Qmin 12:Vmax 13:Vmin 14:Kqp 15:Kqi 16:Tpfilt 17:Tqfilt 18:Rrpwr 19:Rrpwr_dn
    // 20:PQFlag 21:Imax 22:Ipmax
    // 23-30: VDL1 breakpoints (Vq1,Iq1,Vq2,Iq2,Vq3,Iq3,Vq4,Iq4)
    // 31-38: VDL2 breakpoints (Vp1,Ip1,Vp2,Ip2,Vp3,Ip3,Vp4,Ip4)
    // We require at least 3 params (Vdip, Vup, Trv); rest default.
    check_params("REEC_A", params, 3, line)?;
    let vdl1 = [
        (
            opt(params, 23).unwrap_or(0.0),
            opt(params, 24).unwrap_or(0.0),
        ),
        (
            opt(params, 25).unwrap_or(0.0),
            opt(params, 26).unwrap_or(0.0),
        ),
        (
            opt(params, 27).unwrap_or(0.0),
            opt(params, 28).unwrap_or(0.0),
        ),
        (
            opt(params, 29).unwrap_or(0.0),
            opt(params, 30).unwrap_or(0.0),
        ),
    ];
    let vdl2 = [
        (
            opt(params, 31).unwrap_or(0.0),
            opt(params, 32).unwrap_or(0.0),
        ),
        (
            opt(params, 33).unwrap_or(0.0),
            opt(params, 34).unwrap_or(0.0),
        ),
        (
            opt(params, 35).unwrap_or(0.0),
            opt(params, 36).unwrap_or(0.0),
        ),
        (
            opt(params, 37).unwrap_or(0.0),
            opt(params, 38).unwrap_or(0.0),
        ),
    ];
    Ok(ReecaParams {
        vdip: p(params, 0),
        vup: p(params, 1),
        trv: p(params, 2),
        dbd1: opt(params, 3).unwrap_or(-0.05),
        dbd2: opt(params, 4).unwrap_or(0.05),
        kqv: opt(params, 5).unwrap_or(0.0),
        iqh1: opt(params, 6).unwrap_or(1.05),
        iql1: opt(params, 7).unwrap_or(-1.05),
        vref0: opt(params, 8).unwrap_or(0.0),
        tp: opt(params, 9).unwrap_or(0.02),
        qmax: opt(params, 10).unwrap_or(0.436),
        qmin: opt(params, 11).unwrap_or(-0.436),
        kqp: opt(params, 14).unwrap_or(0.0),
        kqi: opt(params, 15).unwrap_or(0.0),
        tpfilt: opt(params, 16).unwrap_or(0.02),
        tqfilt: opt(params, 17).unwrap_or(0.02),
        rrpwr: opt(params, 18).unwrap_or(10.0),
        rrpwr_dn: opt(params, 19).unwrap_or(-10.0),
        pqflag: opt(params, 20).map(|v| v as i32).unwrap_or(0),
        imax: opt(params, 21).unwrap_or(1.1),
        ipmax: opt(params, 22).unwrap_or(1.1),
        vdl1,
        vdl2,
    })
}

fn build_regca(line: usize, params: &[f64]) -> Result<RegcaParams, DyrError> {
    // PSS/E REGC_A params (positions in DYR record):
    // 0:LvplSw 1:Tg 2:Rrpwr 3:Brkpt 4:Zerox 5:Lvpl1 6:Volim 7:Lvpnt1
    // 8:Lvpnt0 9:Iolim 10:Tpord 11:Qrpwr 12:Accel
    // We use Tg from index 1; x_eq at index 11 (Surge extension, defaults to 0.02 pu).
    check_params("REGC_A", params, 2, line)?;
    Ok(RegcaParams {
        tg: p(params, 1).max(1e-4),             // converter time constant
        x_eq: opt(params, 11).unwrap_or(0.02),  // Norton reactance (pu)
        imax: opt(params, 9).unwrap_or(1.1),    // Iolim
        tfltr: opt(params, 10).unwrap_or(0.02), // Tpord ~ filter
        // Phase 1: PLL and ramp params (not in standard PSS/E REGC_A record)
        kp_pll: 30.0,
        ki_pll: 1.0,
        rrpwr: opt(params, 2).unwrap_or(10.0), // Rrpwr from DYR field 2
        vdip: 0.5,
        vup: 1.2,
    })
}

// ---------------------------------------------------------------------------
// New governor builders (Phase 9 + 8)
// ---------------------------------------------------------------------------

fn build_gast(line: usize, params: &[f64]) -> Result<GastParams, DyrError> {
    // PSS/E: R T1 T2 T3 AT KT VMIN VMAX
    check_params("GAST", params, 8, line)?;
    Ok(GastParams {
        r: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        at: p(params, 4),
        kt: p(params, 5),
        vmin: p(params, 6),
        vmax: p(params, 7),
    })
}

fn build_repca(line: usize, params: &[f64]) -> Result<RepcaParams, DyrError> {
    // PSS/E REPC_A plant controller.
    // After bus/model/id stripping:
    // 0:IREG 1:BRTEFLAG 2:NREF 3:VCMPFLAG 4:FREF 5:TFLTR 6:KP 7:KI 8:TLAG
    // 9:VMAX 10:VMIN 11:QMAX 12:QMIN 13:RC 14:XC 15:KCINV
    // 16:DDN 17:DUP 18:FDBD1 19:FDBD2 20:FEMAX 21:FEMIN 22:PMAX 23:PMIN 24:TPORD
    // 25:KPG 26:KIG 27:TP 28:RRPWR
    check_params("REPC_A", params, 1, line)?;
    let vrflag = opt(params, 3).unwrap_or(1.0); // VCMPFLAG: 0=Q, 1=V
    Ok(RepcaParams {
        vrflag,
        rc: opt(params, 13).unwrap_or(0.0),
        tfltr: opt(params, 5).unwrap_or(0.02),
        kp: opt(params, 6).unwrap_or(0.0),
        ki: opt(params, 7).unwrap_or(0.0),
        vmax: opt(params, 9).unwrap_or(1.1),
        vmin: opt(params, 10).unwrap_or(0.9),
        vref: 1.0, // set from power flow at init
        qref: 0.0, // set from power flow at init
        qmax: opt(params, 11).unwrap_or(0.436),
        qmin: opt(params, 12).unwrap_or(-0.436),
        fdbd1: opt(params, 18).unwrap_or(-0.004),
        fdbd2: opt(params, 19).unwrap_or(0.004),
        ddn: opt(params, 16).unwrap_or(20.0),
        dup: opt(params, 17).unwrap_or(20.0),
        tp: opt(params, 27).unwrap_or(0.05),
        kpg: opt(params, 25).unwrap_or(0.1),
        kig: opt(params, 26).unwrap_or(0.0),
        pref: 0.0, // set from power flow at init
        pmax: opt(params, 22).unwrap_or(1.0),
        pmin: opt(params, 23).unwrap_or(0.0),
        rrpwr: opt(params, 28).unwrap_or(10.0),
        tlag: opt(params, 8).unwrap_or(0.1),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Phase 10: New generator builders
// ---------------------------------------------------------------------------

fn build_gentpj(line: usize, params: &[f64]) -> Result<GentpjParams, DyrError> {
    check_params("GENTPJ", params, 14, line)?;
    Ok(GentpjParams {
        td0_prime: p(params, 0),
        td0_pprime: p(params, 1),
        tq0_prime: p(params, 2),
        tq0_pprime: p(params, 3),
        h: p(params, 4),
        d: p(params, 5),
        xd: p(params, 6),
        xq: p(params, 7),
        xd_prime: p(params, 8),
        xq_prime: p(params, 9),
        xd_pprime: p(params, 10),
        xl: p(params, 11),
        s1: p(params, 12),
        s12: p(params, 13),
        kii: opt(params, 14),
        ra: opt(params, 15),
    })
}

fn build_genqec(line: usize, params: &[f64]) -> Result<GenqecParams, DyrError> {
    check_params("GENQEC", params, 14, line)?;
    Ok(GenqecParams {
        td0_prime: p(params, 0),
        td0_pprime: p(params, 1),
        tq0_prime: p(params, 2),
        tq0_pprime: p(params, 3),
        h: p(params, 4),
        d: p(params, 5),
        xd: p(params, 6),
        xq: p(params, 7),
        xd_prime: p(params, 8),
        xq_prime: p(params, 9),
        xd_pprime: p(params, 10),
        xl: p(params, 11),
        s1: p(params, 12),
        s12: p(params, 13),
        ra: opt(params, 14),
    })
}

// ---------------------------------------------------------------------------
// Phase 10: New exciter builders
// ---------------------------------------------------------------------------

fn build_esst1a(_line: usize, params: &[f64]) -> Result<Esst1aParams, DyrError> {
    // PSS/E ESST1A: UEL VOS TR VIMAX VIMIN TC TB TC1 TB1 KA TA VAMAX VAMIN
    //               VRMAX VRMIN KC KF TF KLR ILR
    // UEL/VOS are integer flags (0 or 1). Detect by param count:
    //   20 params → UEL VOS prefix present (skip 2)
    //   18 params → no UEL/VOS prefix, includes KLR/ILR
    //   16 params → legacy (no UEL/VOS, no KLR/ILR)
    let off = if params.len() >= 20 {
        2 // skip UEL, VOS
    } else if params.len() >= 16 {
        0
    } else {
        return Err(DyrError::InsufficientParams {
            model: "ESST1A".into(),
            expected: 16,
            got: params.len(),
        });
    };
    let klr = if params.len() >= off + 18 {
        p(params, off + 16)
    } else {
        0.0
    };
    let ilr = if params.len() >= off + 18 {
        p(params, off + 17)
    } else {
        0.0
    };
    Ok(Esst1aParams {
        tr: p(params, off),
        vimax: p(params, off + 1),
        vimin: p(params, off + 2),
        tc: p(params, off + 3),
        tb: p(params, off + 4),
        tc1: p(params, off + 5),
        tb1: p(params, off + 6),
        ka: p(params, off + 7),
        ta: p(params, off + 8),
        vamax: p(params, off + 9),
        vamin: p(params, off + 10),
        vrmax: p(params, off + 11),
        vrmin: p(params, off + 12),
        kc: p(params, off + 13),
        kf: p(params, off + 14),
        tf: p(params, off + 15),
        klr,
        ilr,
    })
}

fn build_exac1(_line: usize, params: &[f64]) -> Result<Exac1Params, DyrError> {
    // PSS/E: TR TB TC KA TA VRMAX VRMIN TE KF TF KC KD KE E1 SE1 E2 SE2
    // Accept 16 (legacy: no KD) or 17 (correct PSS/E)
    if params.len() < 16 {
        return Err(DyrError::InsufficientParams {
            model: "EXAC1".into(),
            expected: 17,
            got: params.len(),
        });
    }
    if params.len() >= 17 {
        // Full PSS/E format with KC, KD, KE in correct positions
        Ok(Exac1Params {
            tr: p(params, 0),
            tb: p(params, 1),
            tc: p(params, 2),
            ka: p(params, 3),
            ta: p(params, 4),
            vrmax: p(params, 5),
            vrmin: p(params, 6),
            te: p(params, 7),
            kf: p(params, 8),
            tf: p(params, 9),
            kc: p(params, 10),
            kd: p(params, 11),
            ke: p(params, 12),
            e1: p(params, 13),
            se1: p(params, 14),
            e2: p(params, 15),
            se2: p(params, 16),
        })
    } else {
        // Legacy 16-param format (no KD): ...TF KC KE E1 SE1 E2 SE2
        Ok(Exac1Params {
            tr: p(params, 0),
            tb: p(params, 1),
            tc: p(params, 2),
            ka: p(params, 3),
            ta: p(params, 4),
            vrmax: p(params, 5),
            vrmin: p(params, 6),
            te: p(params, 7),
            kf: p(params, 8),
            tf: p(params, 9),
            kc: p(params, 10),
            kd: 0.0,
            ke: p(params, 11),
            e1: p(params, 12),
            se1: p(params, 13),
            e2: p(params, 14),
            se2: p(params, 15),
        })
    }
}

fn build_esac7b(line: usize, params: &[f64]) -> Result<Esac7bParams, DyrError> {
    // TR KPA KIA VRH VRL KPF VFH TF TE KE E1 SE1 E2 SE2 KD KC KL
    check_params("ESAC7B", params, 17, line)?;
    Ok(Esac7bParams {
        tr: p(params, 0),
        kpa: p(params, 1),
        kia: p(params, 2),
        vrh: p(params, 3),
        vrl: p(params, 4),
        kpf: p(params, 5),
        vfh: p(params, 6),
        tf: p(params, 7),
        te: p(params, 8),
        ke: p(params, 9),
        e1: p(params, 10),
        se1: p(params, 11),
        e2: p(params, 12),
        se2: p(params, 13),
        kd: p(params, 14),
        kc: p(params, 15),
        kl: p(params, 16),
    })
}

fn build_esst4b(line: usize, params: &[f64]) -> Result<Esst4bParams, DyrError> {
    // TR KPR KIR VRMAX VRMIN KPM KIM VMMAX VMMIN KG KP KI VBMAX VGMAX
    check_params("ESST4B", params, 14, line)?;
    Ok(Esst4bParams {
        tr: p(params, 0),
        kpr: p(params, 1),
        kir: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        kpm: p(params, 5),
        kim: p(params, 6),
        vmmax: p(params, 7),
        vmmin: p(params, 8),
        kg: p(params, 9),
        kp: p(params, 10),
        ki: p(params, 11),
        vbmax: p(params, 12),
        vgmax: p(params, 13),
    })
}

// ---------------------------------------------------------------------------
// Phase 10: New governor builders
// ---------------------------------------------------------------------------

fn build_hygov(line: usize, params: &[f64]) -> Result<HygovParams, DyrError> {
    // R TP VELM TG GMAX GMIN TW AT DTURB QNL
    check_params("HYGOV", params, 10, line)?;
    Ok(HygovParams {
        r: p(params, 0),
        tp: p(params, 1),
        velm: p(params, 2),
        tg: p(params, 3),
        gmax: p(params, 4),
        gmin: p(params, 5),
        tw: p(params, 6),
        at: p(params, 7),
        dturb: p(params, 8),
        qnl: p(params, 9),
    })
}

fn build_hygovd(line: usize, params: &[f64]) -> Result<HygovdParams, DyrError> {
    // R TP VELM TG GMAX GMIN TW AT DTURB QNL DB1 DB2
    check_params("HYGOVD", params, 12, line)?;
    Ok(HygovdParams {
        r: p(params, 0),
        tp: p(params, 1),
        velm: p(params, 2),
        tg: p(params, 3),
        gmax: p(params, 4),
        gmin: p(params, 5),
        tw: p(params, 6),
        at: p(params, 7),
        dturb: p(params, 8),
        qnl: p(params, 9),
        db1: p(params, 10),
        db2: p(params, 11),
    })
}

fn build_tgov1d(line: usize, params: &[f64]) -> Result<Tgov1dParams, DyrError> {
    // R T1 VMAX VMIN T2 T3 [DT] DB1 DB2
    check_params("TGOV1D", params, 8, line)?;
    // If 9 params: R T1 VMAX VMIN T2 T3 DT DB1 DB2
    // If 8 params: R T1 VMAX VMIN T2 T3 DB1 DB2 (no DT)
    let has_dt = params.len() >= 9;
    if has_dt {
        Ok(Tgov1dParams {
            r: p(params, 0),
            t1: p(params, 1),
            vmax: p(params, 2),
            vmin: p(params, 3),
            t2: p(params, 4),
            t3: p(params, 5),
            dt: Some(p(params, 6)),
            db1: p(params, 7),
            db2: p(params, 8),
        })
    } else {
        Ok(Tgov1dParams {
            r: p(params, 0),
            t1: p(params, 1),
            vmax: p(params, 2),
            vmin: p(params, 3),
            t2: p(params, 4),
            t3: p(params, 5),
            dt: None,
            db1: p(params, 6),
            db2: p(params, 7),
        })
    }
}

fn build_ieeeg1d(line: usize, params: &[f64]) -> Result<Ieeeg1dParams, DyrError> {
    // Same as IEEEG1 params + DB1 DB2 at end
    check_params("IEEEG1D", params, 11, line)?;
    // Get all IEEEG1 params first, then DB1 DB2 from end
    let n = params.len();
    let db2 = p(params, n - 1);
    let db1 = p(params, n - 2);
    Ok(Ieeeg1dParams {
        k: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        uo: p(params, 4),
        uc: p(params, 5),
        pmax: p(params, 6),
        pmin: p(params, 7),
        t4: p(params, 8),
        k1: opt(params, 9),
        k2: opt(params, 10),
        t5: opt(params, 11),
        k3: opt(params, 12),
        k4: opt(params, 13),
        t6: opt(params, 14),
        k5: opt(params, 15),
        k6: opt(params, 16),
        t7: opt(params, 17),
        k7: opt(params, 18),
        k8: opt(params, 19),
        db1,
        db2,
    })
}

fn build_ieeeg2(line: usize, params: &[f64]) -> Result<Ieeeg2Params, DyrError> {
    // Full format: K T1 T2 T3 Pmin Pmax At Dturb Qnl
    // Legacy 4-param: K T1 T2 PZ (PZ → Qnl, defaults for rest)
    check_params("IEEEG2", params, 4, line)?;
    if params.len() >= 9 {
        Ok(Ieeeg2Params {
            k: p(params, 0),
            t1: p(params, 1),
            t2: p(params, 2),
            t3: p(params, 3),
            pmin: p(params, 4),
            pmax: p(params, 5),
            at: p(params, 6),
            dturb: p(params, 7),
            qnl: p(params, 8),
            rt: opt(params, 9).unwrap_or(0.0),
        })
    } else {
        // Legacy 4-param format: K T1 T2 PZ
        Ok(Ieeeg2Params {
            k: p(params, 0),
            t1: p(params, 1),
            t2: p(params, 2),
            t3: 0.0,
            rt: 0.0,
            pmin: 0.0,
            pmax: 1.0,
            at: 1.0,
            dturb: 0.0,
            qnl: p(params, 3),
        })
    }
}

// ---------------------------------------------------------------------------
// Phase 10: New PSS builders
// ---------------------------------------------------------------------------

fn build_pss2a(line: usize, params: &[f64]) -> Result<Pss2aParams, DyrError> {
    // M1 T6 T7 KS2 T8 T9 M2 TW1 TW2 TW3 TW4 T1 T2 T3 T4 KS1 KS3 VSTMAX VSTMIN
    check_params("PSS2A", params, 19, line)?;
    Ok(Pss2aParams {
        m1: p(params, 0),
        t6: p(params, 1),
        t7: p(params, 2),
        ks2: p(params, 3),
        t8: p(params, 4),
        t9: p(params, 5),
        m2: p(params, 6),
        tw1: p(params, 7),
        tw2: p(params, 8),
        tw3: p(params, 9),
        tw4: p(params, 10),
        t1: p(params, 11),
        t2: p(params, 12),
        t3: p(params, 13),
        t4: p(params, 14),
        ks1: p(params, 15),
        ks3: p(params, 16),
        vstmax: p(params, 17),
        vstmin: p(params, 18),
    })
}

fn build_pss2b(line: usize, params: &[f64]) -> Result<Pss2bParams, DyrError> {
    // Same as PSS2A + T10 T11
    check_params("PSS2B", params, 21, line)?;
    Ok(Pss2bParams {
        m1: p(params, 0),
        t6: p(params, 1),
        t7: p(params, 2),
        ks2: p(params, 3),
        t8: p(params, 4),
        t9: p(params, 5),
        m2: p(params, 6),
        tw1: p(params, 7),
        tw2: p(params, 8),
        tw3: p(params, 9),
        tw4: p(params, 10),
        t1: p(params, 11),
        t2: p(params, 12),
        t3: p(params, 13),
        t4: p(params, 14),
        ks1: p(params, 15),
        ks3: p(params, 16),
        vstmax: p(params, 17),
        vstmin: p(params, 18),
        t10: p(params, 19),
        t11: p(params, 20),
    })
}

fn build_stab1(line: usize, params: &[f64]) -> Result<Stab1Params, DyrError> {
    // KS T1 T2 T3 T4 HLIM
    check_params("STAB1", params, 6, line)?;
    Ok(Stab1Params {
        ks: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        t4: p(params, 4),
        hlim: p(params, 5),
    })
}

// ---------------------------------------------------------------------------
// Phase 11: IBR / Wind / GFM / DER builder functions
// ---------------------------------------------------------------------------

/// REGCB — Enhanced IBR inner converter.
/// PSS/E DYR params (similar to REGCA):
/// 0:LvplSw 1:Tg 2:Rrpwr 3:Brkpt 4:Zerox 5:Lvpl1 6:Volim 7:Lvpnt1
/// 8:Lvpnt0 9:Iolim 10:Tpord 11:Tip
fn build_regcb(line: usize, params: &[f64]) -> Result<RegcbParams, DyrError> {
    check_params("REGCB", params, 2, line)?;
    Ok(RegcbParams {
        tg: opt(params, 1).unwrap_or(0.02).max(1e-4),
        x_eq: opt(params, 14).unwrap_or(0.02),
        imax: opt(params, 9).unwrap_or(1.1),
        tfltr: opt(params, 10).unwrap_or(0.02),
        tip: opt(params, 11).unwrap_or(0.02),
        kp_pll: opt(params, 12).unwrap_or(30.0),
        ki_pll: opt(params, 13).unwrap_or(1.0),
    })
}

/// WT3G2U — Type 3 DFIG wind turbine generator.
/// PSS/E DYR params: 0:Tg 1:Xeq 2:Rrpwr 3:Brkpt 4:Zerox 5:Lvpl1 6:Imax 7:Kpll
fn build_wt3g2u(line: usize, params: &[f64]) -> Result<Wt3g2uParams, DyrError> {
    check_params("WT3G2U", params, 1, line)?;
    Ok(Wt3g2uParams {
        tg: p(params, 0).max(1e-4),
        x_eq: opt(params, 1).unwrap_or(0.02),
        imax: opt(params, 6).unwrap_or(1.1),
        tfltr: opt(params, 4).unwrap_or(0.02),
        kpll: opt(params, 7).unwrap_or(30.0),
        kipll: opt(params, 8).unwrap_or(1.0),
        h_rotor: opt(params, 9).unwrap_or(3.5),
        d_rotor: opt(params, 10).unwrap_or(0.0),
        t_rotor: opt(params, 11).unwrap_or(0.02),
        lm_over_ls: opt(params, 12).unwrap_or(0.9),
    })
}

/// WT4G1 — Type 4 full-converter wind turbine generator.
/// PSS/E DYR params: 0:Tg 1:Xeq 2:Imax
fn build_wt4g1(line: usize, params: &[f64]) -> Result<Wt4g1Params, DyrError> {
    check_params("WT4G1", params, 1, line)?;
    Ok(Wt4g1Params {
        tg: p(params, 0).max(1e-4),
        x_eq: opt(params, 1).unwrap_or(0.02),
        imax: opt(params, 2).unwrap_or(1.1),
        kp_pll: opt(params, 3).unwrap_or(30.0),
        ki_pll: opt(params, 4).unwrap_or(1.0),
    })
}

/// REGFM_A1 — Grid-forming inverter (droop control, 9 dynamic states).
/// PSS/E DYR params: 0:Xeq 1:H 2:D 3:Imax 4:Tg 5:Tv 6:Tpll 7:Tvi
///                    8:Kp_droop 9:Ki_droop 10:Kq_droop 11:Ki_q 12:Rvi 13:Xvi
fn build_regfm_a1(line: usize, params: &[f64]) -> Result<RegfmA1Params, DyrError> {
    check_params("REGFM_A1", params, 1, line)?;
    Ok(RegfmA1Params {
        x_eq: p(params, 0),
        h: opt(params, 1).unwrap_or(2.0),
        d: opt(params, 2).unwrap_or(20.0),
        imax: opt(params, 3).unwrap_or(1.1),
        tg: opt(params, 4).unwrap_or(0.02),
        tv: opt(params, 5).unwrap_or(0.02),
        tpll: opt(params, 6).unwrap_or(0.02),
        tvi: opt(params, 7).unwrap_or(0.05),
        kp_droop: opt(params, 8).unwrap_or(20.0),
        ki_droop: opt(params, 9).unwrap_or(0.0),
        kq_droop: opt(params, 10).unwrap_or(20.0),
        ki_q: opt(params, 11).unwrap_or(0.0),
        r_vi: opt(params, 12).unwrap_or(0.0),
        x_vi: opt(params, 13).unwrap_or(0.1),
    })
}

/// REGFM_B1 — Grid-forming inverter (virtual synchronous machine, 9 dynamic states).
/// PSS/E DYR params: same layout as REGFM_A1.
fn build_regfm_b1(line: usize, params: &[f64]) -> Result<RegfmB1Params, DyrError> {
    check_params("REGFM_B1", params, 1, line)?;
    Ok(RegfmB1Params {
        x_eq: p(params, 0),
        h: opt(params, 1).unwrap_or(3.0),
        d: opt(params, 2).unwrap_or(30.0),
        imax: opt(params, 3).unwrap_or(1.1),
        tg: opt(params, 4).unwrap_or(0.02),
        tv: opt(params, 5).unwrap_or(0.02),
        tpll: opt(params, 6).unwrap_or(0.02),
        tvi: opt(params, 7).unwrap_or(0.05),
        kp_droop: opt(params, 8).unwrap_or(20.0),
        ki_droop: opt(params, 9).unwrap_or(0.0),
        kq_droop: opt(params, 10).unwrap_or(20.0),
        ki_q: opt(params, 11).unwrap_or(0.0),
        r_vi: opt(params, 12).unwrap_or(0.0),
        x_vi: opt(params, 13).unwrap_or(0.1),
    })
}

/// DER_A — Distributed energy resource aggregate.
/// PSS/E DYR params: 0:Xeq 1:Trf 2:Imax 3:Trv
fn build_dera(line: usize, params: &[f64]) -> Result<DeraParams, DyrError> {
    check_params("DERA", params, 1, line)?;
    Ok(DeraParams {
        x_eq: p(params, 0),
        trf: opt(params, 1).unwrap_or(0.02),
        imax: opt(params, 2).unwrap_or(1.1),
        trv: opt(params, 3).unwrap_or(0.02),
    })
}

/// REECD — IBR electrical controller with curtailment + droop.
/// PSS/E DYR params: 0:Dbd1 1:Dbd2 2:Kqv 3:Kqi 4:Trv 5:Tp 6:Iqmax 7:Iqmin
///   8:Ipmax 9:Rrpwr 10:Ddn 11:Dup 12:Fdbd1 13:Fdbd2 14:Vdip 15:Vup
///   16:Pref 17:Pmax 18:Pmin
fn build_reecd(line: usize, params: &[f64]) -> Result<ReecdParams, DyrError> {
    check_params("REECD", params, 1, line)?;
    Ok(ReecdParams {
        dbd1: p(params, 0),
        dbd2: opt(params, 1).unwrap_or(0.1),
        kqv: opt(params, 2).unwrap_or(0.0),
        kqi: opt(params, 3).unwrap_or(0.1),
        trv: opt(params, 4).unwrap_or(0.02),
        tp: opt(params, 5).unwrap_or(0.05),
        iqmax: opt(params, 6).unwrap_or(1.1),
        iqmin: opt(params, 7).unwrap_or(-1.1),
        ipmax: opt(params, 8).unwrap_or(1.1),
        rrpwr: opt(params, 9).unwrap_or(10.0),
        ddn: opt(params, 10).unwrap_or(0.0),
        dup: opt(params, 11).unwrap_or(0.0),
        fdbd1: opt(params, 12).unwrap_or(-0.004),
        fdbd2: opt(params, 13).unwrap_or(0.004),
        vdip: opt(params, 14).unwrap_or(0.5),
        vup: opt(params, 15).unwrap_or(1.3),
        pref: opt(params, 16).unwrap_or(0.0),
        pmax: opt(params, 17).unwrap_or(1.0),
        pmin: opt(params, 18).unwrap_or(0.0),
    })
}

/// REECCU — IBR electrical controller, current-unlimited (PI + ramp).
/// PSS/E DYR params: 0:Dbd1 1:Kqv 2:Kqi 3:Trv 4:Tp 5:Rrpwr 6:Vdip 7:Vup
///   8:Pref 9:Pmax 10:Pmin
fn build_reeccu(line: usize, params: &[f64]) -> Result<ReeccuParams, DyrError> {
    check_params("REECCU", params, 1, line)?;
    Ok(ReeccuParams {
        dbd1: p(params, 0),
        kqv: opt(params, 1).unwrap_or(0.0),
        kqi: opt(params, 2).unwrap_or(0.1),
        trv: opt(params, 3).unwrap_or(0.02),
        tp: opt(params, 4).unwrap_or(0.05),
        rrpwr: opt(params, 5).unwrap_or(10.0),
        vdip: opt(params, 6).unwrap_or(0.5),
        vup: opt(params, 7).unwrap_or(1.3),
        pref: opt(params, 8).unwrap_or(0.0),
        pmax: opt(params, 9).unwrap_or(1.0),
        pmin: opt(params, 10).unwrap_or(0.0),
    })
}

/// REXS — GE Renewable excitation system.
/// PSS/E DYR params: 0:Te 1:Tf 2:Ke 3:Kf 4:Efd1 5:Efd2 6:SEfd1 7:SEfd2
fn build_rexs(line: usize, params: &[f64]) -> Result<RexsParams, DyrError> {
    check_params("REXS", params, 2, line)?;
    Ok(RexsParams {
        te: p(params, 0).max(1e-4),
        tf: p(params, 1).max(1e-4),
        ke: opt(params, 2).unwrap_or(1.0),
        kf: opt(params, 3).unwrap_or(0.01),
        efd1: opt(params, 4).unwrap_or(3.0),
        efd2: opt(params, 5).unwrap_or(4.0),
        sefd1: opt(params, 6).unwrap_or(0.0),
        sefd2: opt(params, 7).unwrap_or(0.0),
        tc: opt(params, 8).unwrap_or(0.0),
        tb: opt(params, 9).unwrap_or(0.0),
    })
}

/// REPCD — IBR plant power controller (simplified).
/// PSS/E DYR params: 0:Tp 1:Kpg 2:Kig 3:Pmax 4:Pmin 5:Tlag
fn build_repcd(line: usize, params: &[f64]) -> Result<RepdcParams, DyrError> {
    check_params("REPCD", params, 1, line)?;
    Ok(RepdcParams {
        tp: p(params, 0),
        kpg: opt(params, 1).unwrap_or(1.0),
        kig: opt(params, 2).unwrap_or(0.5),
        pmax: opt(params, 3).unwrap_or(1.2),
        pmin: opt(params, 4).unwrap_or(0.0),
        tlag: opt(params, 5).unwrap_or(0.1),
    })
}

/// WT3T1 — Type 3 wind drive train.
/// PSS/E DYR params: 0:H 1:Damp 2:Ka 3:Theta
fn build_wt3t1(line: usize, params: &[f64]) -> Result<Wt3t1Params, DyrError> {
    check_params("WT3T1", params, 1, line)?;
    Ok(Wt3t1Params {
        h: p(params, 0),
        damp: opt(params, 1).unwrap_or(1.5),
        ka: opt(params, 2).unwrap_or(20.0),
        theta: opt(params, 3).unwrap_or(0.0),
    })
}

/// WT3P1 — Type 3 wind pitch controller.
/// PSS/E DYR params: 0:Tp 1:Kpp 2:Kip 3:Pmax 4:Pmin
fn build_wt3p1(line: usize, params: &[f64]) -> Result<Wt3p1Params, DyrError> {
    check_params("WT3P1", params, 1, line)?;
    Ok(Wt3p1Params {
        tp: p(params, 0),
        kpp: opt(params, 1).unwrap_or(150.0),
        kip: opt(params, 2).unwrap_or(25.0),
        pmax: opt(params, 3).unwrap_or(1.12),
        pmin: opt(params, 4).unwrap_or(0.04),
    })
}

/// GGOV1D — GGOV1 with deadband.
/// PSS/E DYR params (34 fields): same as GGOV1 plus FDBD1/FDBD2/DB.
fn build_ggov1d(line: usize, params: &[f64]) -> Result<Ggov1dParams, DyrError> {
    check_params("GGOV1D", params, 19, line)?;
    Ok(Ggov1dParams {
        r: p(params, 0),
        t_pelec: p(params, 1).max(1e-3),
        maxerr: p(params, 2),
        minerr: p(params, 3),
        kpgov: p(params, 4),
        kigov: p(params, 5),
        kdgov: opt(params, 6).unwrap_or(0.0),
        fdbd1: opt(params, 7).unwrap_or(-0.0006),
        fdbd2: opt(params, 8).unwrap_or(0.0006),
        pmax: p(params, 9),
        pmin: p(params, 10),
        tact: opt(params, 11).unwrap_or(0.5),
        kturb: opt(params, 12).unwrap_or(1.5),
        wfnl: opt(params, 13).unwrap_or(0.1),
        tb: opt(params, 14).unwrap_or(0.5).max(1e-3),
        tc: opt(params, 15).unwrap_or(0.0),
        flag: opt(params, 16).unwrap_or(1.0),
        teng: opt(params, 17).unwrap_or(0.0),
        tfload: opt(params, 18).unwrap_or(3.0),
        kpload: 0.0,
        kiload: 0.0,
        ldref: 0.0,
        dm: 0.0,
        ropen: 0.1,
        rclose: -0.1,
        kimw: 0.0,
        pmwset: 0.0,
        aset: 10.0,
        ka: 10.0,
        ta: 0.1,
        db: 0.0,
        tsa: 4.0,
        tsb: 5.0,
        rup: 99.0,
        rdown: -99.0,
        load_ref: 0.0,
    })
}

/// TGOV1N / TGOV1NDB — TGOV1 with null deadband.
/// PSS/E DYR params: 0:R 1:DT 2:T1 3:VMAX 4:VMIN 5:T2 6:T3 7:D [8:DB]
fn build_tgov1n(line: usize, params: &[f64]) -> Result<Tgov1nParams, DyrError> {
    check_params("TGOV1N", params, 6, line)?;
    Ok(Tgov1nParams {
        r: p(params, 0),
        dt: p(params, 1),
        t1: p(params, 2).max(1e-4),
        vmax: p(params, 3),
        vmin: p(params, 4),
        t2: p(params, 5),
        t3: opt(params, 6).unwrap_or(0.0).max(1e-4),
        d: opt(params, 7).unwrap_or(0.0),
        db: opt(params, 8).unwrap_or(0.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 12: Load model builders
// ---------------------------------------------------------------------------

/// Build CLOD composite load parameters from DYR record.
///
/// PSS/E format: `bus 'CLOD' load_id  lfac rfrac xfrac lfrac_dl nfrac dsli tv tf vtd vtu ftd ftu td /`
///
/// Note: `lfac_dl` is the discharge lighting fraction (confusingly named `lfrac` in PSS/E docs
/// but we use `lfrac_dl` internally to avoid conflict with the outer `lfac` load factor).
fn build_clod(line: usize, params: &[f64]) -> Result<ClodParams, DyrError> {
    // Minimum required: lfac rfrac xfrac lfrac_dl nfrac dsli tv tf (8 params).
    check_params("CLOD", params, 8, line)?;
    Ok(ClodParams {
        mbase: 100.0, // default system base; actual mbase comes from load record
        lfac: p(params, 0),
        rfrac: p(params, 1),
        xfrac: p(params, 2),
        lfrac_dl: p(params, 3),
        nfrac: p(params, 4),
        dsli: p(params, 5),
        tv: p(params, 6),
        tf: p(params, 7),
        vtd: opt(params, 8).unwrap_or(0.75),
        vtu: opt(params, 9).unwrap_or(1.2),
        ftd: opt(params, 10).unwrap_or(57.5),
        ftu: opt(params, 11).unwrap_or(61.5),
        td: opt(params, 12).unwrap_or(0.05),
    })
}

/// Build INDMOT 3rd-order induction motor parameters from DYR record.
///
/// PSS/E format: `bus 'INDMOT' load_id  h d ra xs xr xm rr mbase lfac /`
fn build_indmot(line: usize, params: &[f64]) -> Result<IndmotParams, DyrError> {
    check_params("INDMOT", params, 9, line)?;
    let h = p(params, 0);
    let d = p(params, 1);
    let ra = p(params, 2);
    let xs = p(params, 3);
    let xr = p(params, 4);
    let xm = p(params, 5);
    let rr = p(params, 6);
    let mbase = opt(params, 7).unwrap_or(100.0);
    let lfac = opt(params, 8).unwrap_or(1.0);
    // Derived transient parameters.
    const OMEGA0: f64 = 2.0 * std::f64::consts::PI * 60.0;
    let t0p = if rr.abs() > 1e-10 {
        (xr + xm) / (OMEGA0 * rr)
    } else {
        0.5
    };
    let x0p = xs
        + if (xr + xm).abs() > 1e-10 {
            xr * xm / (xr + xm)
        } else {
            xm
        };
    // Rated slip from circuit params: slip ≈ Rr / X0p.
    let slip0 = if rr.abs() > 1e-10 && x0p.abs() > 0.01 {
        (rr / x0p).min(0.1)
    } else {
        0.02
    };
    // Approximate initial electrical torque at rated slip and Vt=1.0.
    let xm_frac = if (xr + xm).abs() > 1e-10 {
        xm / (xr + xm)
    } else {
        0.95
    };
    let eq0 = xm_frac; // Vt=1.0 * xm/(xr+xm)
    let z_sq = (ra * ra + x0p * x0p).max(1e-20);
    let iq0 = (-x0p * 0.0 + ra * (eq0 - 1.0)) / z_sq;
    let id0 = (ra * 0.0 + x0p * (eq0 - 1.0)) / z_sq;
    let te0 = (0.0 * id0 + eq0 * iq0).abs().max(1e-4);
    Ok(IndmotParams {
        h,
        d,
        ra,
        xs,
        xr,
        xm,
        rr,
        t0p,
        x0p,
        mbase,
        lfac,
        slip0,
        te0,
    })
}

/// Build MOTOR 2nd-order single-phase motor parameters from DYR record.
///
/// PSS/E format: `bus 'MOTOR' load_id  h ra xs x0p t0p mbase lfac /`
fn build_motor(line: usize, params: &[f64]) -> Result<MotorParams, DyrError> {
    check_params("MOTOR", params, 5, line)?;
    Ok(MotorParams {
        h: p(params, 0),
        ra: p(params, 1),
        xs: p(params, 2),
        x0p: p(params, 3),
        t0p: p(params, 4),
        mbase: opt(params, 5).unwrap_or(100.0),
        lfac: opt(params, 6).unwrap_or(1.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 13: FACTS / HVDC builders
// ---------------------------------------------------------------------------

/// Build CSVGN1 (SVC) parameters from DYR record.
///
/// PSS/E format: `bus 'CSVGN1' id  t1 t2 t3 t4 t5 k vmax vmin bmax bmin /`
fn build_csvgn1(line: usize, params: &[f64]) -> Result<Csvgn1Params, DyrError> {
    check_params("CSVGN1", params, 10, line)?;
    Ok(Csvgn1Params {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        t4: p(params, 3),
        t5: p(params, 4),
        k: p(params, 5),
        vmax: p(params, 6),
        vmin: p(params, 7),
        bmax: p(params, 8),
        bmin: p(params, 9),
        mbase: opt(params, 10).unwrap_or(100.0),
        b_l: opt(params, 11),
        b_c: opt(params, 12),
        t_alpha: opt(params, 13),
    })
}

/// Build CSTCON (STATCOM) parameters from DYR record.
///
/// PSS/E format: `bus 'CSTCON' id  tr k tiq imax imin /`
fn build_cstcon(line: usize, params: &[f64]) -> Result<CstconParams, DyrError> {
    check_params("CSTCON", params, 5, line)?;
    Ok(CstconParams {
        tr: p(params, 0),
        k: p(params, 1),
        tiq: p(params, 2),
        imax: p(params, 3),
        imin: p(params, 4),
        mbase: opt(params, 5).unwrap_or(100.0),
        c_dc: opt(params, 6),
        vdc_ref: opt(params, 7),
        kp_vdc: opt(params, 8),
    })
}

/// Build TCSC (series capacitor) parameters from DYR record.
///
/// PSS/E format: `bus 'TCSC' id  t1 t2 t3 xmax xmin k /`
fn build_tcsc(line: usize, params: &[f64]) -> Result<TcscParams, DyrError> {
    check_params("TCSC", params, 6, line)?;
    Ok(TcscParams {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        xmax: p(params, 3),
        xmin: p(params, 4),
        k: p(params, 5),
        mbase: opt(params, 6).unwrap_or(100.0),
    })
}

/// Build CDC4T (LCC HVDC) parameters from DYR record.
///
/// PSS/E format: `bus 'CDC4T' id  setvl vschd mbase tr td alpha_min alpha_max gamma_min rectifier_bus inverter_bus /`
fn build_cdc4t(line: usize, params: &[f64]) -> Result<Cdc4tParams, DyrError> {
    check_params("CDC4T", params, 8, line)?;
    Ok(Cdc4tParams {
        setvl: p(params, 0),
        vschd: p(params, 1),
        mbase: opt(params, 2).unwrap_or(100.0),
        tr: p(params, 3),
        td: p(params, 4),
        alpha_min: p(params, 5),
        alpha_max: p(params, 6),
        gamma_min: p(params, 7),
        gamma_ref: None,
        ramp: None,
        kp_alpha: None,
        ki_alpha: None,
        ki_gamma: None,
        rectifier_bus: opt(params, 8).map(|v| v as u32).unwrap_or(0),
        inverter_bus: opt(params, 9).map(|v| v as u32).unwrap_or(0),
        vdcol_v1: opt(params, 10),
        vdcol_v2: opt(params, 11),
        vdcol_i1: opt(params, 12),
        vdcol_i2: opt(params, 13),
        t_iord: opt(params, 14),
    })
}

/// Build VSCDCT (VSC HVDC) parameters from DYR record.
///
/// PSS/E format: `bus 'VSCDCT' id  p_order vdc_ref t_dc t_ac imax mbase rectifier_bus inverter_bus /`
fn build_vscdct(line: usize, params: &[f64]) -> Result<VscdctParams, DyrError> {
    check_params("VSCDCT", params, 5, line)?;
    Ok(VscdctParams {
        p_order: p(params, 0),
        vdc_ref: p(params, 1),
        t_dc: p(params, 2),
        t_ac: p(params, 3),
        imax: p(params, 4),
        q_order: None,
        kp_vdc: opt(params, 8),
        ki_vdc: opt(params, 9),
        kp_q: opt(params, 10),
        ki_q: opt(params, 11),
        t_vdc_filt: opt(params, 12),
        kp_id: opt(params, 13),
        ki_id: opt(params, 14),
        kp_iq: opt(params, 15),
        ki_iq: opt(params, 16),
        mbase: opt(params, 5).unwrap_or(100.0),
        rectifier_bus: opt(params, 6).map(|v| v as u32).unwrap_or(0),
        inverter_bus: opt(params, 7).map(|v| v as u32).unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Phase 14: Build functions for new models
// ---------------------------------------------------------------------------

/// ESAC2A — IEEE AC2A alternator exciter.
/// PSS/E format: TR TB TC KA TA VAMAX VAMIN VRH VRL KE TE KF TF E1 SE1 E2 SE2 KB KC KD KH
fn build_esac2a(line: usize, params: &[f64]) -> Result<Esac2aParams, DyrError> {
    check_params("ESAC2A", params, 16, line)?;
    Ok(Esac2aParams {
        tr: p(params, 0),
        tb: p(params, 1),
        tc: p(params, 2),
        ka: p(params, 3),
        ta: p(params, 4),
        vamax: p(params, 5),
        vamin: p(params, 6),
        vrmax: p(params, 7),
        vrmin: p(params, 8),
        ke: p(params, 9),
        te: p(params, 10),
        kf: p(params, 11),
        tf: p(params, 12),
        e1: p(params, 13),
        se1: p(params, 14),
        e2: p(params, 15),
        se2: opt(params, 16).unwrap_or(0.0),
        kb: opt(params, 17).unwrap_or(25.0),
        kc: opt(params, 18).unwrap_or(0.2),
        kd: opt(params, 19).unwrap_or(0.38),
        kh: opt(params, 20).unwrap_or(1.0),
    })
}

/// ESAC5A — IEEE AC5A simplified brushless exciter.
/// PSS/E format: TR KA TA KE TE KF TF E1 SE1 E2 SE2 VRMAX VRMIN
fn build_esac5a(line: usize, params: &[f64]) -> Result<Esac5aParams, DyrError> {
    check_params("ESAC5A", params, 11, line)?;
    Ok(Esac5aParams {
        ka: p(params, 0),
        ta: p(params, 1),
        ke: p(params, 2),
        te: p(params, 3),
        kf: p(params, 4),
        tf: p(params, 5),
        e1: p(params, 6),
        se1: p(params, 7),
        e2: p(params, 8),
        se2: p(params, 9),
        vrmax: p(params, 10),
        vrmin: opt(params, 11).unwrap_or(-p(params, 10)),
    })
}

/// CBEST — Battery Energy Storage System.
/// PSS/E format: PMAX PMIN QMAX QMIN TP TQ ECAP MBASE
fn build_cbest(line: usize, params: &[f64]) -> Result<CbestParams, DyrError> {
    check_params("CBEST", params, 6, line)?;
    Ok(CbestParams {
        p_max: p(params, 0),
        p_min: p(params, 1),
        q_max: p(params, 2),
        q_min: p(params, 3),
        tp: p(params, 4),
        tq: p(params, 5),
        e_cap: opt(params, 6).unwrap_or(1.0),
        mbase: opt(params, 7).unwrap_or(100.0),
        soc_init: opt(params, 8).unwrap_or(0.5),
    })
}

/// CHAAUT — Frequency-droop BESS controller.
/// PSS/E format: KF TF PMAX PMIN TP MBASE
fn build_chaaut(line: usize, params: &[f64]) -> Result<ChaautParams, DyrError> {
    check_params("CHAAUT", params, 4, line)?;
    Ok(ChaautParams {
        kf: p(params, 0),
        tf: p(params, 1),
        p_max: p(params, 2),
        p_min: p(params, 3),
        tp: opt(params, 4).unwrap_or(0.05),
        mbase: opt(params, 5).unwrap_or(100.0),
    })
}

/// PSS1A — Single-input single lead-lag PSS.
/// PSS/E format: KS T1 T2 T3 T4 VSTMAX VSTMIN
fn build_pss1a(line: usize, params: &[f64]) -> Result<Pss1aParams, DyrError> {
    check_params("PSS1A", params, 7, line)?;
    Ok(Pss1aParams {
        ks: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        t4: p(params, 4),
        vstmax: p(params, 5),
        vstmin: p(params, 6),
    })
}

/// PIDGOV — PID speed governor.
/// PSS/E format: PMAX PMIN KP KI KD TD TF
fn build_pidgov(line: usize, params: &[f64]) -> Result<PidgovParams, DyrError> {
    check_params("PIDGOV", params, 5, line)?;
    Ok(PidgovParams {
        pmax: p(params, 0),
        pmin: p(params, 1),
        kp: p(params, 2),
        ki: p(params, 3),
        kd: p(params, 4),
        td: opt(params, 5).unwrap_or(0.01),
        tf: opt(params, 6).unwrap_or(0.05),
    })
}

/// DEGOV1 — Woodward diesel/gas engine governor (full PID).
/// PSS/E format: R T1 T2 T3 K T4 T5 T6 TD VMAX VMIN VELM AT KT
/// Legacy 8-param format: R T1 T2 T3 AT KT VMAX VMIN [TD]
fn build_degov1(line: usize, params: &[f64]) -> Result<Degov1Params, DyrError> {
    check_params("DEGOV1", params, 8, line)?;
    if params.len() >= 14 {
        // Full WECC format: R T1 T2 T3 K T4 T5 T6 TD VMAX VMIN VELM AT KT
        Ok(Degov1Params {
            r: p(params, 0),
            t1: p(params, 1),
            t2: p(params, 2),
            t3: p(params, 3),
            k: p(params, 4),
            t4: p(params, 5),
            t5: p(params, 6),
            t6: p(params, 7),
            td: p(params, 8),
            vmax: p(params, 9),
            vmin: p(params, 10),
            velm: p(params, 11),
            at: p(params, 12),
            kt: p(params, 13),
        })
    } else {
        // Legacy 8+1 format: R T1 T2 T3 AT KT VMAX VMIN [TD]
        Ok(Degov1Params {
            r: p(params, 0),
            t1: p(params, 1),
            t2: p(params, 2),
            t3: p(params, 3),
            at: p(params, 4),
            kt: p(params, 5),
            vmax: p(params, 6),
            vmin: p(params, 7),
            td: opt(params, 8).unwrap_or(0.0),
            t4: 0.0,
            t5: 0.01,
            t6: 0.01,
            k: 1.0,
            velm: 99.0,
        })
    }
}

// ---------------------------------------------------------------------------
// Phase 15: Generator builders
// ---------------------------------------------------------------------------

/// GENTRA — 3rd-order transient round-rotor generator.
/// PSS/E format: H D RA XD XD' TD0' XQ [S(1.0) S(1.2)]
/// S(1.0) and S(1.2) are optional trailing saturation factors.
fn build_gentra(line: usize, params: &[f64]) -> Result<GentraParams, DyrError> {
    check_params("GENTRA", params, 7, line)?;
    Ok(GentraParams {
        h: p(params, 0),
        d: p(params, 1),
        ra: p(params, 2),
        xd: p(params, 3),
        xd_prime: p(params, 4),
        td0_prime: p(params, 5),
        xq: p(params, 6),
        s1: if params.len() > 7 { p(params, 7) } else { 0.0 },
        s12: if params.len() > 8 { p(params, 8) } else { 0.0 },
    })
}

/// REGCC — Next-gen GFM-capable converter.
/// PSS/E format: TG X_EQ IMAX TFLTR T_PLL
fn build_regcc(line: usize, params: &[f64]) -> Result<RegccParams, DyrError> {
    check_params("REGCC", params, 2, line)?;
    Ok(RegccParams {
        tg: p(params, 0),
        x_eq: p(params, 1),
        imax: opt(params, 2).unwrap_or(1.2),
        tfltr: opt(params, 3).unwrap_or(0.02),
        t_pll: opt(params, 4).unwrap_or(0.02),
    })
}

/// WT4G2 — Type 4 wind generator GE variant.
/// PSS/E format: TG X_EQ IMAX
fn build_wt4g2(line: usize, params: &[f64]) -> Result<Wt4g2Params, DyrError> {
    check_params("WT4G2", params, 1, line)?;
    Ok(Wt4g2Params {
        tg: p(params, 0),
        x_eq: opt(params, 1).unwrap_or(0.15),
        imax: opt(params, 2).unwrap_or(1.1),
        kp_pll: opt(params, 3).unwrap_or(30.0),
        ki_pll: opt(params, 4).unwrap_or(1.0),
    })
}

/// DER_C / DERC — DER aggregate type C.
/// PSS/E format: TP TQ TV MBASE LFAC
fn build_derc(line: usize, params: &[f64]) -> Result<DercParams, DyrError> {
    check_params("DERC", params, 3, line)?;
    Ok(DercParams {
        tp: p(params, 0),
        tq: p(params, 1),
        tv: p(params, 2),
        mbase: opt(params, 3).unwrap_or(100.0),
        lfac: opt(params, 4).unwrap_or(0.0),
        x_eq: opt(params, 5).unwrap_or(0.02),
    })
}

// ---------------------------------------------------------------------------
// Phase 15: Exciter builders
// ---------------------------------------------------------------------------

/// ESST5B — IEEE ST5B static exciter.
/// PSS/E format: TR KC KF TF KA TB TC VRMAX VRMIN T1 T2
fn build_esst5b(line: usize, params: &[f64]) -> Result<Esst5bParams, DyrError> {
    check_params("ESST5B", params, 9, line)?;
    Ok(Esst5bParams {
        tr: p(params, 0),
        kc: p(params, 1),
        kf: p(params, 2),
        tf: p(params, 3),
        ka: p(params, 4),
        tb: p(params, 5),
        tc: p(params, 6),
        vrmax: p(params, 7),
        vrmin: p(params, 8),
        t1: opt(params, 9).unwrap_or(0.0),
        t2: opt(params, 10).unwrap_or(0.0),
    })
}

/// EXAC4 / AC4A — IEEE AC4A controlled-rectifier exciter.
/// PSS/E format: TR TC TB KA TA VRMAX VRMIN KC
fn build_exac4(line: usize, params: &[f64]) -> Result<Exac4Params, DyrError> {
    check_params("EXAC4", params, 7, line)?;
    Ok(Exac4Params {
        tr: p(params, 0),
        tc: p(params, 1),
        tb: p(params, 2),
        ka: p(params, 3),
        ta: p(params, 4),
        vrmax: p(params, 5),
        vrmin: p(params, 6),
        kc: opt(params, 7).unwrap_or(0.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 15: Governor builders
// ---------------------------------------------------------------------------

/// TGOV5 — Multi-reheat steam governor.
/// PSS/E format: R T1 T2 T3 T4 K1 K2 K3 PMAX PMIN
fn build_tgov5(line: usize, params: &[f64]) -> Result<Tgov5Params, DyrError> {
    check_params("TGOV5", params, 5, line)?;
    Ok(Tgov5Params {
        r: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        t4: opt(params, 4).unwrap_or(0.5),
        k1: opt(params, 5).unwrap_or(0.3),
        k2: opt(params, 6).unwrap_or(0.4),
        k3: opt(params, 7).unwrap_or(0.3),
        pmax: opt(params, 8).unwrap_or(1.0),
        pmin: opt(params, 9).unwrap_or(0.0),
    })
}

/// GAST2A — Advanced Rowen gas turbine governor.
/// PSS/E format: R T1 T2 T3 T4 AT KT VMIN VMAX
fn build_gast2a(line: usize, params: &[f64]) -> Result<Gast2aParams, DyrError> {
    check_params("GAST2A", params, 7, line)?;
    Ok(Gast2aParams {
        r: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        t4: opt(params, 4).unwrap_or(0.9),
        at: opt(params, 5).unwrap_or(1.0),
        kt: opt(params, 6).unwrap_or(2.0),
        vmin: opt(params, 7).unwrap_or(0.0),
        vmax: opt(params, 8).unwrap_or(1.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 15: PSS builders
// ---------------------------------------------------------------------------

/// STAB2A — WSCC double lead-lag stabilizer.
/// PSS/E format: KS T1 T2 T3 T4 T5 HLIM
fn build_stab2a(line: usize, params: &[f64]) -> Result<Stab2aParams, DyrError> {
    check_params("STAB2A", params, 5, line)?;
    Ok(Stab2aParams {
        ks: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        t4: opt(params, 4).unwrap_or(0.0),
        t5: opt(params, 5).unwrap_or(0.1),
        hlim: opt(params, 6).unwrap_or(0.1),
    })
}

/// PSS4B — Four-band PSS.
/// PSS/E format: KL KH TW1 TW2 T1 T2 T3 T4 VSTMAX VSTMIN
fn build_pss4b(line: usize, params: &[f64]) -> Result<Pss4bParams, DyrError> {
    check_params("PSS4B", params, 8, line)?;
    Ok(Pss4bParams {
        kl: p(params, 0),
        kh: p(params, 1),
        tw1: p(params, 2),
        tw2: p(params, 3),
        t1: p(params, 4),
        t2: p(params, 5),
        t3: p(params, 6),
        t4: p(params, 7),
        vstmax: opt(params, 8).unwrap_or(0.1),
        vstmin: opt(params, 9).unwrap_or(-0.1),
    })
}

// ---------------------------------------------------------------------------
// Phase 15: FACTS builders
// ---------------------------------------------------------------------------

/// CSVGN3 — SVC with slope/droop regulator.
/// PSS/E format: T1 T2 T3 T4 T5 K SLOPE VMAX VMIN BMAX BMIN MBASE
fn build_csvgn3(line: usize, params: &[f64]) -> Result<Csvgn3Params, DyrError> {
    check_params("CSVGN3", params, 6, line)?;
    Ok(Csvgn3Params {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        t4: p(params, 3),
        t5: p(params, 4),
        k: p(params, 5),
        slope: opt(params, 6).unwrap_or(0.0),
        vmax: opt(params, 7).unwrap_or(1.1),
        vmin: opt(params, 8).unwrap_or(0.9),
        bmax: opt(params, 9).unwrap_or(1.0),
        bmin: opt(params, 10).unwrap_or(-1.0),
        mbase: opt(params, 11).unwrap_or(100.0),
    })
}

/// CDC7T — LCC HVDC + runback.
/// PSS/E format: setvl vschd mbase tr td alpha_min alpha_max gamma_min rectifier_bus inverter_bus runback_rate current_order_max
fn build_cdc7t(line: usize, params: &[f64]) -> Result<Cdc7tParams, DyrError> {
    check_params("CDC7T", params, 8, line)?;
    Ok(Cdc7tParams {
        setvl: p(params, 0),
        vschd: p(params, 1),
        mbase: opt(params, 2).unwrap_or(100.0),
        tr: p(params, 3),
        td: p(params, 4),
        alpha_min: p(params, 5),
        alpha_max: p(params, 6),
        gamma_min: p(params, 7),
        rectifier_bus: opt(params, 8).map(|v| v as u32).unwrap_or(0),
        inverter_bus: opt(params, 9).map(|v| v as u32).unwrap_or(0),
        runback_rate: opt(params, 10).unwrap_or(0.1),
        current_order_max: opt(params, 11).unwrap_or(1.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 16: Load builders
// ---------------------------------------------------------------------------

/// CMPLDW — Composite load with motors.
/// PSS/E format: lfma lfmb lfmc kp1 np1 kp2 np2 kq1 nq1 kq2 nq2 ra xm r1 x1 r2 x2 vtr1 vtr2 mbase
fn build_cmpldw(line: usize, params: &[f64]) -> Result<CmpldwParams, DyrError> {
    check_params("CMPLDW", params, 10, line)?;
    Ok(CmpldwParams {
        lfma: p(params, 0),
        lfmb: p(params, 1),
        lfmc: p(params, 2),
        kp1: p(params, 3),
        np1: p(params, 4),
        kp2: p(params, 5),
        np2: p(params, 6),
        kq1: p(params, 7),
        nq1: p(params, 8),
        kq2: p(params, 9),
        nq2: opt(params, 10).unwrap_or(2.0),
        ra: opt(params, 11).unwrap_or(0.0),
        xm: opt(params, 12).unwrap_or(3.0),
        r1: opt(params, 13).unwrap_or(0.04),
        x1: opt(params, 14).unwrap_or(0.1),
        r2: opt(params, 15).unwrap_or(0.04),
        x2: opt(params, 16).unwrap_or(0.1),
        vtr1: opt(params, 17).unwrap_or(0.75),
        vtr2: opt(params, 18).unwrap_or(0.6),
        mbase: opt(params, 19).unwrap_or(100.0),
        motor_a: None,
        motor_b: None,
        motor_c: None,
        tv: 0.02,
        fel: 0.0,
        pfreq: 0.0,
    })
}

/// CMPLDWG — CMPLDW with embedded generation.
fn build_cmpldwg(line: usize, params: &[f64]) -> Result<CmpldwgParams, DyrError> {
    check_params("CMPLDWG", params, 10, line)?;
    Ok(CmpldwgParams {
        lfma: p(params, 0),
        lfmb: p(params, 1),
        lfmc: p(params, 2),
        kp1: p(params, 3),
        np1: p(params, 4),
        kp2: p(params, 5),
        np2: p(params, 6),
        kq1: p(params, 7),
        nq1: p(params, 8),
        kq2: p(params, 9),
        nq2: opt(params, 10).unwrap_or(2.0),
        ra: opt(params, 11).unwrap_or(0.0),
        xm: opt(params, 12).unwrap_or(3.0),
        r1: opt(params, 13).unwrap_or(0.04),
        x1: opt(params, 14).unwrap_or(0.1),
        r2: opt(params, 15).unwrap_or(0.04),
        x2: opt(params, 16).unwrap_or(0.1),
        vtr1: opt(params, 17).unwrap_or(0.75),
        vtr2: opt(params, 18).unwrap_or(0.6),
        mbase: opt(params, 19).unwrap_or(100.0),
        gen_mw: opt(params, 20).unwrap_or(0.0),
        motor_a: None,
        motor_b: None,
        motor_c: None,
        tv: 0.02,
        fel: 0.0,
        pfreq: 0.0,
    })
}

/// CMLDBLU2 / CMLDARU2 — Composite load simplified model.
/// PSS/E format: t1 t2 k1 k2 pf kp kq vmin vmax mbase
fn build_cmldblu2(line: usize, params: &[f64]) -> Result<Cmldblu2Params, DyrError> {
    check_params("CMLDBLU2", params, 4, line)?;
    Ok(Cmldblu2Params {
        t1: p(params, 0),
        t2: p(params, 1),
        k1: p(params, 2),
        k2: p(params, 3),
        pf: opt(params, 4).unwrap_or(0.9),
        kp: opt(params, 5).unwrap_or(1.0),
        kq: opt(params, 6).unwrap_or(2.0),
        vmin: opt(params, 7).unwrap_or(0.0),
        vmax: opt(params, 8).unwrap_or(1.2),
        mbase: opt(params, 9).unwrap_or(100.0),
    })
}

/// MOTORW — Type W induction motor.
/// PSS/E format: ra xm r1 x1 r2 x2 h vtr1 vtr2 mbase
fn build_motorw(line: usize, params: &[f64]) -> Result<MotorwParams, DyrError> {
    check_params("MOTORW", params, 4, line)?;
    Ok(MotorwParams {
        ra: p(params, 0),
        xm: p(params, 1),
        r1: p(params, 2),
        x1: p(params, 3),
        r2: opt(params, 4).unwrap_or(0.04),
        x2: opt(params, 5).unwrap_or(0.1),
        h: opt(params, 6).unwrap_or(0.5),
        vtr1: opt(params, 7).unwrap_or(0.75),
        vtr2: opt(params, 8).unwrap_or(0.6),
        mbase: opt(params, 9).unwrap_or(100.0),
    })
}

/// CIM5 — 5th-order current injection motor.
/// PSS/E format: ra xs xm xr1 xr2 rr1 rr2 h e1 s1 e2 s2 mbase
fn build_cim5(line: usize, params: &[f64]) -> Result<Cim5Params, DyrError> {
    check_params("CIM5", params, 7, line)?;
    Ok(Cim5Params {
        ra: p(params, 0),
        xs: p(params, 1),
        xm: p(params, 2),
        xr1: p(params, 3),
        xr2: p(params, 4),
        rr1: p(params, 5),
        rr2: p(params, 6),
        h: opt(params, 7).unwrap_or(0.5),
        e1: opt(params, 8).unwrap_or(1.0),
        s1: opt(params, 9).unwrap_or(0.02),
        e2: opt(params, 10).unwrap_or(1.2),
        s2: opt(params, 11).unwrap_or(0.1),
        mbase: opt(params, 12).unwrap_or(100.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 17: Exciter builders
// ---------------------------------------------------------------------------

/// ESST6B — IEEE ST6B Static Exciter.
/// PSS/E format: TR ILR KLR KA TA KC VRMAX VRMIN KFF KGFF T1 T2
fn build_esst6b(line: usize, params: &[f64]) -> Result<Esst6bParams, DyrError> {
    check_params("ESST6B", params, 6, line)?;
    Ok(Esst6bParams {
        tr: p(params, 0),
        ilr: opt(params, 1).unwrap_or(0.0),
        klr: opt(params, 2).unwrap_or(0.0),
        ka: p(params, 3),
        ta: p(params, 4),
        kc: opt(params, 5).unwrap_or(0.0),
        vrmax: p(params, 6),
        vrmin: opt(params, 7).unwrap_or(-14.5),
        kff: opt(params, 8).unwrap_or(1.0),
        kgff: opt(params, 9).unwrap_or(0.0),
        t1: opt(params, 10).unwrap_or(0.0),
        t2: opt(params, 11).unwrap_or(0.0),
    })
}

/// ESST7B — IEEE ST7B Static Exciter.
/// PSS/E format: TR KPA KIA VRMAX VRMIN KPFF KH VMAX VMIN T1 T2 T3 T4 KL
fn build_esst7b(line: usize, params: &[f64]) -> Result<Esst7bParams, DyrError> {
    check_params("ESST7B", params, 5, line)?;
    Ok(Esst7bParams {
        tr: p(params, 0),
        kpa: p(params, 1),
        kia: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        kpff: opt(params, 5).unwrap_or(1.0),
        kh: opt(params, 6).unwrap_or(0.0),
        vmax: opt(params, 7).unwrap_or(1.1),
        vmin: opt(params, 8).unwrap_or(0.9),
        t1: opt(params, 9).unwrap_or(0.0),
        t2: opt(params, 10).unwrap_or(0.0),
        t3: opt(params, 11).unwrap_or(0.0),
        t4: opt(params, 12).unwrap_or(0.0),
        kl: opt(params, 13).unwrap_or(0.0),
    })
}

/// ESAC6A — AC6A Rotating Exciter.
/// PSS/E format: TR KA TA TK TB TC VAMAX VAMIN VRMAX VRMIN TE KH KF TF KC KD KE
fn build_esac6a(line: usize, params: &[f64]) -> Result<Esac6aParams, DyrError> {
    check_params("ESAC6A", params, 9, line)?;
    Ok(Esac6aParams {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        tk: opt(params, 3).unwrap_or(0.0),
        tb: opt(params, 4).unwrap_or(0.0),
        tc: opt(params, 5).unwrap_or(0.0),
        vamax: opt(params, 6).unwrap_or(14.5),
        vamin: opt(params, 7).unwrap_or(-14.5),
        vrmax: p(params, 8),
        vrmin: opt(params, 9).unwrap_or(-14.5),
        te: opt(params, 10).unwrap_or(1.0),
        kh: opt(params, 11).unwrap_or(0.0),
        kf: opt(params, 12).unwrap_or(0.0),
        tf: opt(params, 13).unwrap_or(1.0),
        kc: opt(params, 14).unwrap_or(0.0),
        kd: opt(params, 15).unwrap_or(0.0),
        ke: opt(params, 16).unwrap_or(1.0),
    })
}

/// ESDC1A — DC1A Rotating Exciter.
/// PSS/E format: TR KA TA KF TF KE TE SE1 E1 SE2 E2 VRMAX VRMIN
fn build_esdc1a(line: usize, params: &[f64]) -> Result<Esdc1aParams, DyrError> {
    check_params("ESDC1A", params, 6, line)?;
    Ok(Esdc1aParams {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        kf: opt(params, 3).unwrap_or(0.01),
        tf: opt(params, 4).unwrap_or(1.0),
        ke: opt(params, 5).unwrap_or(1.0),
        te: opt(params, 6).unwrap_or(0.5),
        se1: opt(params, 7).unwrap_or(0.0),
        e1: opt(params, 8).unwrap_or(3.1),
        se2: opt(params, 9).unwrap_or(0.0),
        e2: opt(params, 10).unwrap_or(2.3),
        vrmax: opt(params, 11).unwrap_or(4.95),
        vrmin: opt(params, 12).unwrap_or(-4.95),
    })
}

/// EXST2 — Static Exciter Type ST2.
/// PSS/E format: TR KA TA VRMAX VRMIN KC KI KE TE
fn build_exst2(line: usize, params: &[f64]) -> Result<Exst2Params, DyrError> {
    check_params("EXST2", params, 4, line)?;
    Ok(Exst2Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        vrmax: p(params, 3),
        vrmin: opt(params, 4).unwrap_or(-4.38),
        kc: opt(params, 5).unwrap_or(0.0),
        ki: opt(params, 6).unwrap_or(0.0),
        ke: opt(params, 7).unwrap_or(1.0),
        te: opt(params, 8).unwrap_or(0.5),
    })
}

/// AC8B — IEEE AC8B Exciter.
/// PSS/E format: TR KA TA KC VRMAX VRMIN KD KE TE PID_KP PID_KI PID_KD
fn build_ac8b(line: usize, params: &[f64]) -> Result<Ac8bParams, DyrError> {
    check_params("AC8B", params, 5, line)?;
    Ok(Ac8bParams {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        kc: opt(params, 3).unwrap_or(0.0),
        vrmax: p(params, 4),
        vrmin: opt(params, 5).unwrap_or(-14.5),
        kd: opt(params, 6).unwrap_or(0.0),
        ke: opt(params, 7).unwrap_or(1.0),
        te: opt(params, 8).unwrap_or(1.0),
        pid_kp: opt(params, 9).unwrap_or(10.0),
        pid_ki: opt(params, 10).unwrap_or(0.5),
        pid_kd: opt(params, 11).unwrap_or(0.0),
    })
}

/// BBSEX1 — Bus-Branch Static Exciter 1.
/// PSS/E format: T1R T2R T3R T4R T1I T2I KA TA VRMAX VRMIN
fn build_bbsex1(line: usize, params: &[f64]) -> Result<Bbsex1Params, DyrError> {
    check_params("BBSEX1", params, 4, line)?;
    Ok(Bbsex1Params {
        t1r: p(params, 0),
        t2r: p(params, 1),
        t3r: p(params, 2),
        t4r: p(params, 3),
        t1i: opt(params, 4).unwrap_or(0.0),
        t2i: opt(params, 5).unwrap_or(0.0),
        ka: opt(params, 6).unwrap_or(50.0),
        ta: opt(params, 7).unwrap_or(0.02),
        vrmax: opt(params, 8).unwrap_or(7.3),
        vrmin: opt(params, 9).unwrap_or(-6.6),
    })
}

/// IEEET3 — IEEE Type 3 Rotating Exciter.
/// PSS/E format: TR KA TA VRMAX VRMIN KF TF KE TE E1 SE1 E2 SE2 KP KI KC
fn build_ieeet3(line: usize, params: &[f64]) -> Result<Ieeet3Params, DyrError> {
    check_params("IEEET3", params, 5, line)?;
    Ok(Ieeet3Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        kf: opt(params, 5).unwrap_or(0.0),
        tf: opt(params, 6).unwrap_or(1.0),
        ke: opt(params, 7).unwrap_or(1.0),
        te: opt(params, 8).unwrap_or(0.8),
        e1: opt(params, 9).unwrap_or(3.1),
        se1: opt(params, 10).unwrap_or(0.0),
        e2: opt(params, 11).unwrap_or(2.3),
        se2: opt(params, 12).unwrap_or(0.0),
        kp: opt(params, 13).unwrap_or(1.0),
        ki: opt(params, 14).unwrap_or(0.0),
        kc: opt(params, 15).unwrap_or(0.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 18: Governor + PSS builders
// ---------------------------------------------------------------------------

/// H6E — Hydro Governor 6 Elements.
/// PSS/E format: R TR TF TG TW T1 T2 T3 T4 T5 DT PMAX PMIN
fn build_h6e(line: usize, params: &[f64]) -> Result<H6eParams, DyrError> {
    check_params("H6E", params, 5, line)?;
    Ok(H6eParams {
        r: p(params, 0),
        tr: p(params, 1),
        tf: p(params, 2),
        tg: p(params, 3),
        tw: p(params, 4),
        t1: opt(params, 5).unwrap_or(0.0),
        t2: opt(params, 6).unwrap_or(0.0),
        t3: opt(params, 7).unwrap_or(0.0),
        t4: opt(params, 8).unwrap_or(0.0),
        t5: opt(params, 9).unwrap_or(0.0),
        dt: opt(params, 10).unwrap_or(0.0),
        pmax: opt(params, 11).unwrap_or(1.0),
        pmin: opt(params, 12).unwrap_or(0.0),
    })
}

/// WSHYGP — Wind-Synchronous Hydro Governor+Pitch.
/// PSS/E format: R TF TG TW KD PMAX PMIN KP KI
fn build_wshygp(line: usize, params: &[f64]) -> Result<WshygpParams, DyrError> {
    check_params("WSHYGP", params, 4, line)?;
    Ok(WshygpParams {
        r: p(params, 0),
        tf: p(params, 1),
        tg: p(params, 2),
        tw: p(params, 3),
        kd: opt(params, 4).unwrap_or(0.0),
        pmax: opt(params, 5).unwrap_or(1.0),
        pmin: opt(params, 6).unwrap_or(0.0),
        kp: opt(params, 7).unwrap_or(1.0),
        ki: opt(params, 8).unwrap_or(0.5),
    })
}

/// STAB3 — Three-Band PSS.
/// PSS/E format: KS T1 T2 T3 T4 T5 T6 VSTMAX VSTMIN
fn build_stab3(line: usize, params: &[f64]) -> Result<Stab3Params, DyrError> {
    check_params("STAB3", params, 3, line)?;
    Ok(Stab3Params {
        ks: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: opt(params, 3).unwrap_or(0.0),
        t4: opt(params, 4).unwrap_or(0.0),
        t5: opt(params, 5).unwrap_or(0.0),
        t6: opt(params, 6).unwrap_or(0.0),
        vstmax: opt(params, 7).unwrap_or(0.1),
        vstmin: opt(params, 8).unwrap_or(-0.1),
    })
}

/// PSS3B — Three-Input Power System Stabilizer.
/// PSS/E format: A1 A2 A3 A4 A5 A6 A7 A8 VSI1MAX VSI1MIN VSI2MAX VSI2MIN VSTMAX VSTMIN
fn build_pss3b(line: usize, params: &[f64]) -> Result<Pss3bParams, DyrError> {
    check_params("PSS3B", params, 6, line)?;
    Ok(Pss3bParams {
        a1: p(params, 0),
        a2: p(params, 1),
        a3: p(params, 2),
        a4: p(params, 3),
        a5: p(params, 4),
        a6: p(params, 5),
        a7: opt(params, 6).unwrap_or(0.0),
        a8: opt(params, 7).unwrap_or(0.0),
        vsi1max: opt(params, 8).unwrap_or(0.2),
        vsi1min: opt(params, 9).unwrap_or(-0.2),
        vsi2max: opt(params, 10).unwrap_or(0.2),
        vsi2min: opt(params, 11).unwrap_or(-0.2),
        vstmax: opt(params, 12).unwrap_or(0.1),
        vstmin: opt(params, 13).unwrap_or(-0.1),
    })
}

// ---------------------------------------------------------------------------
// Phase 19: IBR wind controller builders
// ---------------------------------------------------------------------------

/// WT3E1 — Type 3 Wind Electrical Controller.
/// PSS/E format: KPV KIV KQV XD KPQ KIQ TPE PMIN PMAX QMIN QMAX IMAX
fn build_wt3e1(line: usize, params: &[f64]) -> Result<Wt3e1Params, DyrError> {
    check_params("WT3E1", params, 4, line)?;
    Ok(Wt3e1Params {
        kpv: p(params, 0),
        kiv: p(params, 1),
        kqv: opt(params, 2).unwrap_or(1.0),
        xd: opt(params, 3).unwrap_or(0.15),
        kpq: opt(params, 4).unwrap_or(0.5),
        kiq: opt(params, 5).unwrap_or(0.1),
        tpe: opt(params, 6).unwrap_or(0.05),
        pmin: opt(params, 7).unwrap_or(0.04),
        pmax: opt(params, 8).unwrap_or(1.12),
        qmin: opt(params, 9).unwrap_or(-0.436),
        qmax: opt(params, 10).unwrap_or(0.436),
        imax: opt(params, 11).unwrap_or(1.1),
        tv: opt(params, 12).unwrap_or(0.05),
    })
}

/// WT3E2 — Type 3 Wind Electrical Controller Variant 2.
/// PSS/E format: KPV KIV KQV XD KPQ KIQ TPE PMIN PMAX QMIN QMAX IMAX TIQ
fn build_wt3e2(line: usize, params: &[f64]) -> Result<Wt3e2Params, DyrError> {
    check_params("WT3E2", params, 4, line)?;
    Ok(Wt3e2Params {
        kpv: p(params, 0),
        kiv: p(params, 1),
        kqv: opt(params, 2).unwrap_or(1.0),
        xd: opt(params, 3).unwrap_or(0.15),
        kpq: opt(params, 4).unwrap_or(0.5),
        kiq: opt(params, 5).unwrap_or(0.1),
        tpe: opt(params, 6).unwrap_or(0.05),
        pmin: opt(params, 7).unwrap_or(0.04),
        pmax: opt(params, 8).unwrap_or(1.12),
        qmin: opt(params, 9).unwrap_or(-0.436),
        qmax: opt(params, 10).unwrap_or(0.436),
        imax: opt(params, 11).unwrap_or(1.1),
        tiq: opt(params, 12).unwrap_or(0.02),
        tv: opt(params, 13).unwrap_or(0.05),
    })
}

/// WT4E1 / WT4E2 — Type 4 Wind Electrical Controller.
/// PSS/E format: KPV KIV TPE PMIN PMAX QMIN QMAX IMAX
fn build_wt4e1(line: usize, params: &[f64]) -> Result<Wt4e1Params, DyrError> {
    check_params("WT4E1", params, 2, line)?;
    Ok(Wt4e1Params {
        kpv: p(params, 0),
        kiv: p(params, 1),
        tpe: opt(params, 2).unwrap_or(0.05),
        pmin: opt(params, 3).unwrap_or(0.04),
        pmax: opt(params, 4).unwrap_or(1.12),
        qmin: opt(params, 5).unwrap_or(-0.436),
        qmax: opt(params, 6).unwrap_or(0.436),
        imax: opt(params, 7).unwrap_or(1.1),
    })
}

/// REPCB / REPCC — REPCA Variant B/C.
/// PSS/E format: TP TFLTR KP KI TFT TFV QMAX QMIN VMAX VMIN KC REFS
fn build_repcb(line: usize, params: &[f64]) -> Result<RepcbParams, DyrError> {
    check_params("REPCB", params, 4, line)?;
    Ok(RepcbParams {
        tp: p(params, 0),
        tfltr: p(params, 1),
        kp: p(params, 2),
        ki: p(params, 3),
        tft: opt(params, 4).unwrap_or(0.0),
        tfv: opt(params, 5).unwrap_or(0.05),
        qmax: opt(params, 6).unwrap_or(0.436),
        qmin: opt(params, 7).unwrap_or(-0.436),
        vmax: opt(params, 8).unwrap_or(1.1),
        vmin: opt(params, 9).unwrap_or(0.9),
        kc: opt(params, 10).unwrap_or(0.0),
        refs: opt(params, 11).unwrap_or(1.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 20: FACTS builders
// ---------------------------------------------------------------------------

/// CSVGN4 — SVC with 4 states (adds POD).
/// PSS/E format: T1 T2 T3 T4 T5 K SLOPE KPOD TPOD VMAX VMIN BMAX BMIN MBASE
fn build_csvgn4(line: usize, params: &[f64]) -> Result<Csvgn4Params, DyrError> {
    check_params("CSVGN4", params, 6, line)?;
    Ok(Csvgn4Params {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        t4: p(params, 3),
        t5: p(params, 4),
        k: p(params, 5),
        slope: opt(params, 6).unwrap_or(0.0),
        kpod: opt(params, 7).unwrap_or(0.0),
        tpod: opt(params, 8).unwrap_or(0.1),
        vmax: opt(params, 9).unwrap_or(1.1),
        vmin: opt(params, 10).unwrap_or(0.9),
        bmax: opt(params, 11).unwrap_or(1.0),
        bmin: opt(params, 12).unwrap_or(-1.0),
        mbase: opt(params, 13).unwrap_or(100.0),
    })
}

/// CSVGN5 — SVC with 4 states (voltage support mode).
/// PSS/E format: T1 T2 T3 T4 T5 K KV KPOD TPOD VMAX VMIN BMAX BMIN MBASE
fn build_csvgn5(line: usize, params: &[f64]) -> Result<Csvgn5Params, DyrError> {
    check_params("CSVGN5", params, 6, line)?;
    Ok(Csvgn5Params {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        t4: p(params, 3),
        t5: p(params, 4),
        k: p(params, 5),
        kv: opt(params, 6).unwrap_or(1.0),
        kpod: opt(params, 7).unwrap_or(0.0),
        tpod: opt(params, 8).unwrap_or(0.1),
        vmax: opt(params, 9).unwrap_or(1.1),
        vmin: opt(params, 10).unwrap_or(0.9),
        bmax: opt(params, 11).unwrap_or(1.0),
        bmin: opt(params, 12).unwrap_or(-1.0),
        mbase: opt(params, 13).unwrap_or(100.0),
    })
}

/// CDC6T — LCC HVDC with enhanced controls.
/// PSS/E format: SETVL VSCHD MBASE TR TD ALPHA_MIN ALPHA_MAX GAMMA_MIN RECTIFIER_BUS INVERTER_BUS I_LIMIT
fn build_cdc6t(line: usize, params: &[f64]) -> Result<Cdc6tParams, DyrError> {
    check_params("CDC6T", params, 7, line)?;
    Ok(Cdc6tParams {
        setvl: p(params, 0),
        vschd: p(params, 1),
        mbase: opt(params, 2).unwrap_or(100.0),
        tr: p(params, 3),
        td: p(params, 4),
        alpha_min: p(params, 5),
        alpha_max: p(params, 6),
        gamma_min: opt(params, 7).unwrap_or(15.0),
        rectifier_bus: opt(params, 8).map(|v| v as u32).unwrap_or(0),
        inverter_bus: opt(params, 9).map(|v| v as u32).unwrap_or(0),
        i_limit: opt(params, 10).unwrap_or(1.1),
    })
}

/// CSTCNT — STATCOM with N controls.
/// PSS/E format: T1 T2 T3 KA TA IQMAX IQMIN MBASE
fn build_cstcnt(line: usize, params: &[f64]) -> Result<CstcntParams, DyrError> {
    check_params("CSTCNT", params, 4, line)?;
    Ok(CstcntParams {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        ka: p(params, 3),
        ta: opt(params, 4).unwrap_or(0.02),
        iqmax: opt(params, 5).unwrap_or(1.0),
        iqmin: opt(params, 6).unwrap_or(-1.0),
        mbase: opt(params, 7).unwrap_or(100.0),
    })
}

/// MMC1 — Modular Multilevel Converter.
/// PSS/E format: TR KP_V KI_V KP_I KI_I VDC LARM PMAX PMIN QMAX QMIN MBASE
fn build_mmc1(line: usize, params: &[f64]) -> Result<Mmc1Params, DyrError> {
    check_params("MMC1", params, 5, line)?;
    Ok(Mmc1Params {
        tr: p(params, 0),
        kp_v: p(params, 1),
        ki_v: p(params, 2),
        kp_i: p(params, 3),
        ki_i: p(params, 4),
        vdc: opt(params, 5).unwrap_or(1.0),
        larm: opt(params, 6).unwrap_or(0.02),
        pmax: opt(params, 7).unwrap_or(1.0),
        pmin: opt(params, 8).unwrap_or(-1.0),
        qmax: opt(params, 9).unwrap_or(0.5),
        qmin: opt(params, 10).unwrap_or(-0.5),
        mbase: opt(params, 11).unwrap_or(100.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 21: Remaining builders
// ---------------------------------------------------------------------------

/// REGFM_C1 — Grid-forming inverter C1.
/// PSS/E format: KD KI KQ TG DDN DUP PMAX PMIN QMAX QMIN MBASE
fn build_regfm_c1(line: usize, params: &[f64]) -> Result<RegfmC1Params, DyrError> {
    check_params("REGFM_C1", params, 3, line)?;
    Ok(RegfmC1Params {
        kd: p(params, 0),
        ki: p(params, 1),
        kq: p(params, 2),
        tg: opt(params, 3).unwrap_or(0.02),
        ddn: opt(params, 4).unwrap_or(0.0),
        dup: opt(params, 5).unwrap_or(0.0),
        pmax: opt(params, 6).unwrap_or(1.0),
        pmin: opt(params, 7).unwrap_or(0.0),
        qmax: opt(params, 8).unwrap_or(0.5),
        qmin: opt(params, 9).unwrap_or(-0.5),
        mbase: opt(params, 10).unwrap_or(100.0),
    })
}

/// EXST3 — Static Exciter Type ST3.
/// PSS/E format: TR KA TA TB TC VRMAX VRMIN KC KI KM VMMAX VMMIN XM
fn build_exst3(line: usize, params: &[f64]) -> Result<Exst3Params, DyrError> {
    check_params("EXST3", params, 5, line)?;
    Ok(Exst3Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        tb: opt(params, 3).unwrap_or(0.0),
        tc: opt(params, 4).unwrap_or(1.0),
        vrmax: p(params, 5),
        vrmin: opt(params, 6).unwrap_or(-6.81),
        kc: opt(params, 7).unwrap_or(0.0),
        ki: opt(params, 8).unwrap_or(0.0),
        km: opt(params, 9).unwrap_or(1.0),
        vmmax: opt(params, 10).unwrap_or(99.0),
        vmmin: opt(params, 11).unwrap_or(-99.0),
        xm: opt(params, 12).unwrap_or(0.0),
    })
}

/// CBUFR — Buffer-Frequency-Regulated BESS.
/// PSS/E format: KF TF TP P_BASE P_MIN P_MAX E_CAP
fn build_cbufr(line: usize, params: &[f64]) -> Result<CbufrParams, DyrError> {
    check_params("CBUFR", params, 3, line)?;
    Ok(CbufrParams {
        kf: p(params, 0),
        tf: p(params, 1),
        tp: p(params, 2),
        p_base: opt(params, 3).unwrap_or(100.0),
        p_min: opt(params, 4).unwrap_or(-1.0),
        p_max: opt(params, 5).unwrap_or(1.0),
        e_cap: opt(params, 6).unwrap_or(4.0),
        soc_init: opt(params, 7).unwrap_or(0.5),
    })
}

/// CBUFD — Buffer-Frequency-Dependent BESS.
/// PSS/E format: KF TF TP TQ P_BASE P_MIN P_MAX Q_BASE Q_MIN Q_MAX E_CAP
fn build_cbufd(line: usize, params: &[f64]) -> Result<CbufdParams, DyrError> {
    check_params("CBUFD", params, 3, line)?;
    Ok(CbufdParams {
        kf: p(params, 0),
        tf: p(params, 1),
        tp: p(params, 2),
        tq: opt(params, 3).unwrap_or(0.05),
        p_base: opt(params, 4).unwrap_or(100.0),
        p_min: opt(params, 5).unwrap_or(-1.0),
        p_max: opt(params, 6).unwrap_or(1.0),
        q_base: opt(params, 7).unwrap_or(100.0),
        q_min: opt(params, 8).unwrap_or(-0.5),
        q_max: opt(params, 9).unwrap_or(0.5),
        e_cap: opt(params, 10).unwrap_or(4.0),
        soc_init: opt(params, 11).unwrap_or(0.5),
    })
}

// ---------------------------------------------------------------------------
// Phase 22: Solar PV builder functions
// ---------------------------------------------------------------------------

fn build_pvgu1(line: usize, params: &[f64]) -> Result<Pvgu1Params, DyrError> {
    check_params("PVGU1", params, 14, line)?;
    Ok(Pvgu1Params {
        lvplsw: p(params, 0),
        rrpwr: p(params, 1),
        brkpt: p(params, 2),
        zerox: p(params, 3),
        lvpl1: p(params, 4),
        volim: p(params, 5),
        lvpnt1: p(params, 6),
        lvpnt0: p(params, 7),
        iolim: p(params, 8),
        tfltr: p(params, 9),
        khv: p(params, 10),
        iqrmax: p(params, 11),
        iqrmin: p(params, 12),
        accel: p(params, 13),
        vsmax: opt(params, 14).unwrap_or(1.2),
        mbase: opt(params, 15).unwrap_or(100.0),
    })
}

fn build_pveu1(line: usize, params: &[f64]) -> Result<Pveu1Params, DyrError> {
    check_params("PVEU1", params, 14, line)?;
    Ok(Pveu1Params {
        tiq: p(params, 0),
        dflag: p(params, 1),
        vref0: p(params, 2),
        tv: p(params, 3),
        dbd: p(params, 4),
        kqv: p(params, 5),
        iqhl: p(params, 6),
        iqll: p(params, 7),
        pmax: p(params, 8),
        pmin: p(params, 9),
        qmax: p(params, 10),
        qmin: p(params, 11),
        vmax: p(params, 12),
        vmin: p(params, 13),
        tpord: opt(params, 14).unwrap_or(0.02),
        mbase: opt(params, 15).unwrap_or(100.0),
    })
}

fn build_pvdg(line: usize, params: &[f64]) -> Result<PvdgParams, DyrError> {
    check_params("PVDG", params, 9, line)?;
    Ok(PvdgParams {
        tp: p(params, 0),
        tq: p(params, 1),
        vtrip1: p(params, 2),
        vtrip2: p(params, 3),
        vtrip3: p(params, 4),
        ftrip1: p(params, 5),
        ftrip2: p(params, 6),
        pmax: p(params, 7),
        qmax: p(params, 8),
        qmin: opt(params, 9).unwrap_or(-0.4),
        mbase: opt(params, 10).unwrap_or(100.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 23: Exciter builder functions
// ---------------------------------------------------------------------------

fn build_ieeet2(line: usize, params: &[f64]) -> Result<Ieeet2Params, DyrError> {
    check_params("IEEET2", params, 13, line)?;
    Ok(Ieeet2Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        ke: p(params, 5),
        te: p(params, 6),
        e1: p(params, 7),
        se1: p(params, 8),
        e2: p(params, 9),
        se2: p(params, 10),
        kf: p(params, 11),
        tf: p(params, 12),
    })
}

fn build_exac2(line: usize, params: &[f64]) -> Result<Exac2Params, DyrError> {
    check_params("EXAC2", params, 18, line)?;
    Ok(Exac2Params {
        tr: p(params, 0),
        tb: p(params, 1),
        tc: p(params, 2),
        ka: p(params, 3),
        ta: p(params, 4),
        vamax: p(params, 5),
        vamin: p(params, 6),
        te: p(params, 7),
        kf: p(params, 8),
        tf: p(params, 9),
        ke: p(params, 10),
        e1: p(params, 11),
        se1: p(params, 12),
        e2: p(params, 13),
        se2: p(params, 14),
        kc: p(params, 15),
        kd: p(params, 16),
        kh: p(params, 17),
    })
}

fn build_exac3(line: usize, params: &[f64]) -> Result<Exac3Params, DyrError> {
    check_params("EXAC3", params, 16, line)?;
    Ok(Exac3Params {
        tr: p(params, 0),
        kc: p(params, 1),
        ki: p(params, 2),
        vmin: p(params, 3),
        vmax: p(params, 4),
        ke: p(params, 5),
        te: p(params, 6),
        kf: p(params, 7),
        tf: p(params, 8),
        e1: p(params, 9),
        se1: p(params, 10),
        e2: p(params, 11),
        se2: p(params, 12),
        ka: p(params, 13),
        ta: p(params, 14),
        efdn: p(params, 15),
    })
}

fn build_esac3a(line: usize, params: &[f64]) -> Result<Esac3aParams, DyrError> {
    check_params("ESAC3A", params, 21, line)?;
    Ok(Esac3aParams {
        tr: p(params, 0),
        tb: p(params, 1),
        tc: p(params, 2),
        ka: p(params, 3),
        ta: p(params, 4),
        vamax: p(params, 5),
        vamin: p(params, 6),
        te: p(params, 7),
        ke: p(params, 8),
        kf1: p(params, 9),
        tf: p(params, 10),
        e1: p(params, 11),
        se1: p(params, 12),
        e2: p(params, 13),
        se2: p(params, 14),
        kc: p(params, 15),
        kd: p(params, 16),
        ki: p(params, 17),
        efdn: p(params, 18),
        kn: p(params, 19),
        vfemax: p(params, 20),
    })
}

fn build_esst8c(line: usize, params: &[f64]) -> Result<Esst8cParams, DyrError> {
    check_params("ESST8C", params, 12, line)?;
    Ok(Esst8cParams {
        tr: p(params, 0),
        kpr: p(params, 1),
        kir: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        ka: p(params, 5),
        ta: p(params, 6),
        kc: p(params, 7),
        vbmax: p(params, 8),
        xl: p(params, 9),
        kf: p(params, 10),
        tf: p(params, 11),
    })
}

fn build_esst9b(line: usize, params: &[f64]) -> Result<Esst9bParams, DyrError> {
    check_params("ESST9B", params, 13, line)?;
    Ok(Esst9bParams {
        tr: p(params, 0),
        kpa: p(params, 1),
        kia: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        ka: p(params, 5),
        ta: p(params, 6),
        vbmax: p(params, 7),
        kc: p(params, 8),
        t1: p(params, 9),
        t2: p(params, 10),
        t3: p(params, 11),
        t4: p(params, 12),
    })
}

fn build_esst10c(line: usize, params: &[f64]) -> Result<Esst10cParams, DyrError> {
    check_params("ESST10C", params, 13, line)?;
    Ok(Esst10cParams {
        tr: p(params, 0),
        kpa: p(params, 1),
        kia: p(params, 2),
        kpb: p(params, 3),
        kib: p(params, 4),
        vrmax: p(params, 5),
        vrmin: p(params, 6),
        ka: p(params, 7),
        ta: p(params, 8),
        vbmax: p(params, 9),
        kc: p(params, 10),
        t1: p(params, 11),
        t2: p(params, 12),
    })
}

fn build_esdc3a(line: usize, params: &[f64]) -> Result<Esdc3aParams, DyrError> {
    check_params("ESDC3A", params, 15, line)?;
    Ok(Esdc3aParams {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        te: p(params, 5),
        ke: p(params, 6),
        e1: p(params, 7),
        se1: p(params, 8),
        e2: p(params, 9),
        se2: p(params, 10),
        kp: p(params, 11),
        ki: p(params, 12),
        kf: p(params, 13),
        tf: p(params, 14),
    })
}

// ---------------------------------------------------------------------------
// Wave 33: EXDC3 builder function
// ---------------------------------------------------------------------------

/// EXDC3 — PSS/E non-continuously-acting (relay-type) DC exciter.
/// PSS/E format: TR KV TSTALL TCON TB TC VRMAX VRMIN VEFF TLIM VLIM KE TE
fn build_exdc3(line: usize, params: &[f64]) -> Result<Exdc3Params, DyrError> {
    check_params("EXDC3", params, 4, line)?; // minimum 4 required (TR KV TSTALL TCON)
    Ok(Exdc3Params {
        tr: p(params, 0),
        kv: p(params, 1),
        tstall: p(params, 2),
        tcon: p(params, 3),
        tb: opt(params, 4).unwrap_or(0.0),
        tc: opt(params, 5).unwrap_or(0.0),
        vrmax: opt(params, 6).unwrap_or(1.0),
        vrmin: opt(params, 7).unwrap_or(-1.0),
        veff: opt(params, 8).unwrap_or(0.0),
        tlim: opt(params, 9).unwrap_or(0.0),
        vlim: opt(params, 10).unwrap_or(0.0),
        ke: opt(params, 11).unwrap_or(1.0),
        te: opt(params, 12).unwrap_or(0.5),
    })
}

// Wave 32: EXDC1 + ESST2A builder functions
// ---------------------------------------------------------------------------

/// EXDC1 — IEEE Type DC1A rotating-machine exciter (legacy 13-param form).
/// PSS/E format: TR KA TA VRMAX VRMIN KE TE KF TF E1 SE1 E2 SE2
fn build_exdc1(line: usize, params: &[f64]) -> Result<Exdc1Params, DyrError> {
    check_params("EXDC1", params, 9, line)?;
    Ok(Exdc1Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        ke: p(params, 5),
        te: p(params, 6),
        kf: p(params, 7),
        tf: p(params, 8),
        e1: opt(params, 9).unwrap_or(3.1),
        se1: opt(params, 10).unwrap_or(0.0),
        e2: opt(params, 11).unwrap_or(2.3),
        se2: opt(params, 12).unwrap_or(0.0),
    })
}

/// ESST2A — IEEE 421.5-2016 Type ST2A static exciter.
/// PSS/E format: TR KA TA TB TC KE TE KF TF VRMAX VRMIN EFD1 SE1 EFD2 SE2 KC KP KI [TP]
fn build_esst2a(line: usize, params: &[f64]) -> Result<Esst2aParams, DyrError> {
    check_params("ESST2A", params, 9, line)?;
    Ok(Esst2aParams {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        tb: opt(params, 3).unwrap_or(0.0),
        tc: opt(params, 4).unwrap_or(0.0),
        ke: p(params, 5),
        te: p(params, 6),
        kf: p(params, 7),
        tf: p(params, 8),
        vrmax: opt(params, 9).unwrap_or(5.0),
        vrmin: opt(params, 10).unwrap_or(-5.0),
        e1: opt(params, 11).unwrap_or(3.1),
        se1: opt(params, 12).unwrap_or(0.0),
        e2: opt(params, 13).unwrap_or(2.3),
        se2: opt(params, 14).unwrap_or(0.0),
        kc: opt(params, 15).unwrap_or(0.0),
        kp: opt(params, 16).unwrap_or(1.0),
        ki: opt(params, 17).unwrap_or(0.0),
        tp: opt(params, 18).unwrap_or(0.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 24: PSS variant builder functions
// ---------------------------------------------------------------------------

fn build_pss2c(line: usize, params: &[f64]) -> Result<Pss2cParams, DyrError> {
    check_params("PSS2C", params, 20, line)?;
    Ok(Pss2cParams {
        m1: p(params, 0),
        t6: p(params, 1),
        m2: p(params, 2),
        t7: p(params, 3),
        tw1: p(params, 4),
        tw2: p(params, 5),
        tw3: p(params, 6),
        tw4: p(params, 7),
        t1: p(params, 8),
        t2: p(params, 9),
        t3: p(params, 10),
        t4: p(params, 11),
        t8: p(params, 12),
        t9: p(params, 13),
        n: opt(params, 14).unwrap_or(1.0) as i32,
        ks1: p(params, 15),
        ks2: p(params, 16),
        ks3: p(params, 17),
        vstmax: p(params, 18),
        vstmin: p(params, 19),
    })
}

fn build_pss5(line: usize, params: &[f64]) -> Result<Pss5Params, DyrError> {
    check_params("PSS5", params, 14, line)?;
    Ok(Pss5Params {
        kl: p(params, 0),
        km: p(params, 1),
        kh: p(params, 2),
        tw1: p(params, 3),
        tw2: p(params, 4),
        tw3: p(params, 5),
        t1: p(params, 6),
        t2: p(params, 7),
        t3: p(params, 8),
        t4: p(params, 9),
        t5: p(params, 10),
        t6: p(params, 11),
        vstmax: p(params, 12),
        vstmin: p(params, 13),
    })
}

fn build_pss6c(line: usize, params: &[f64]) -> Result<Pss6cParams, DyrError> {
    check_params("PSS6C", params, 18, line)?;
    Ok(Pss6cParams {
        kl: p(params, 0),
        km: p(params, 1),
        kh: p(params, 2),
        kl2: p(params, 3),
        km2: p(params, 4),
        kh2: p(params, 5),
        tw1: p(params, 6),
        tw2: p(params, 7),
        tw3: p(params, 8),
        tw4: p(params, 9),
        tw5: p(params, 10),
        tw6: p(params, 11),
        t1: p(params, 12),
        t2: p(params, 13),
        t3: p(params, 14),
        t4: p(params, 15),
        vstmax: p(params, 16),
        vstmin: p(params, 17),
    })
}

fn build_psssb(line: usize, params: &[f64]) -> Result<PsssbParams, DyrError> {
    check_params("PSSSB", params, 10, line)?;
    Ok(PsssbParams {
        ks: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        t4: p(params, 4),
        t5: p(params, 5),
        t6: p(params, 6),
        tw: p(params, 7),
        vstmax: p(params, 8),
        vstmin: p(params, 9),
    })
}

fn build_stab4(line: usize, params: &[f64]) -> Result<Stab4Params, DyrError> {
    check_params("STAB4", params, 10, line)?;
    Ok(Stab4Params {
        ks: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        t4: p(params, 4),
        t5: p(params, 5),
        t6: p(params, 6),
        t7: p(params, 7),
        t8: p(params, 8),
        hlim: p(params, 9),
    })
}

fn build_stab5(line: usize, params: &[f64]) -> Result<Stab5Params, DyrError> {
    check_params("STAB5", params, 12, line)?;
    Ok(Stab5Params {
        ks: p(params, 0),
        t1: p(params, 1),
        t2: p(params, 2),
        t3: p(params, 3),
        t4: p(params, 4),
        t5: p(params, 5),
        t6: p(params, 6),
        t7: p(params, 7),
        t8: p(params, 8),
        t9: p(params, 9),
        t10: p(params, 10),
        hlim: p(params, 11),
    })
}

// ---------------------------------------------------------------------------
// Phase 25: Governor variant builders
// ---------------------------------------------------------------------------

/// GGOV2 — GE GGOV1 variant 2 with supplemental load reference input (37 fields).
fn build_ggov2(line: usize, params: &[f64]) -> Result<Ggov2Params, DyrError> {
    check_params("GGOV2", params, 19, line)?;
    Ok(Ggov2Params {
        r: p(params, 0),
        rselect: p(params, 1),
        tpelec: p(params, 2),
        maxerr: p(params, 3),
        minerr: p(params, 4),
        kpgov: p(params, 5),
        kigov: p(params, 6),
        kdgov: p(params, 7),
        tdgov: p(params, 8),
        vmax: p(params, 9),
        vmin: p(params, 10),
        tact: p(params, 11),
        kturb: p(params, 12),
        wfnl: p(params, 13),
        tb: p(params, 14),
        tc: p(params, 15),
        flag: p(params, 16),
        teng: p(params, 17),
        tfload: p(params, 18),
        kpload: opt(params, 19).unwrap_or(1.0),
        kiload: opt(params, 20).unwrap_or(0.5),
        ldref: opt(params, 21).unwrap_or(1.0),
        dm: opt(params, 22).unwrap_or(0.0),
        ropen: opt(params, 23).unwrap_or(0.1),
        rclose: opt(params, 24).unwrap_or(-0.1),
        kimw: opt(params, 25).unwrap_or(0.0),
        pmwset: opt(params, 26).unwrap_or(0.0),
        aset: opt(params, 27).unwrap_or(0.0),
        ka: opt(params, 28).unwrap_or(0.0),
        ta: opt(params, 29).unwrap_or(0.0),
        db: opt(params, 30).unwrap_or(0.0),
        tsa: opt(params, 31).unwrap_or(0.0),
        tsb: opt(params, 32).unwrap_or(0.0),
        rup: opt(params, 33).unwrap_or(99.0),
        rdown: opt(params, 34).unwrap_or(-99.0),
        pmax: opt(params, 35).unwrap_or(1.0),
        pmin: opt(params, 36).unwrap_or(0.0),
    })
}

/// GGOV3 — GE GGOV1 variant 3 with washout filter on speed signal (38 fields).
fn build_ggov3(line: usize, params: &[f64]) -> Result<Ggov3Params, DyrError> {
    check_params("GGOV3", params, 19, line)?;
    Ok(Ggov3Params {
        r: p(params, 0),
        rselect: p(params, 1),
        tpelec: p(params, 2),
        maxerr: p(params, 3),
        minerr: p(params, 4),
        kpgov: p(params, 5),
        kigov: p(params, 6),
        kdgov: p(params, 7),
        tdgov: p(params, 8),
        vmax: p(params, 9),
        vmin: p(params, 10),
        tact: p(params, 11),
        kturb: p(params, 12),
        wfnl: p(params, 13),
        tb: p(params, 14),
        tc: p(params, 15),
        flag: p(params, 16),
        teng: p(params, 17),
        tfload: p(params, 18),
        kpload: opt(params, 19).unwrap_or(1.0),
        kiload: opt(params, 20).unwrap_or(0.5),
        ldref: opt(params, 21).unwrap_or(1.0),
        dm: opt(params, 22).unwrap_or(0.0),
        ropen: opt(params, 23).unwrap_or(0.1),
        rclose: opt(params, 24).unwrap_or(-0.1),
        kimw: opt(params, 25).unwrap_or(0.0),
        pmwset: opt(params, 26).unwrap_or(0.0),
        aset: opt(params, 27).unwrap_or(0.0),
        ka: opt(params, 28).unwrap_or(0.0),
        ta: opt(params, 29).unwrap_or(0.0),
        db: opt(params, 30).unwrap_or(0.0),
        tsa: opt(params, 31).unwrap_or(0.0),
        tsb: opt(params, 32).unwrap_or(0.0),
        tw: opt(params, 33).unwrap_or(0.5),
        rup: opt(params, 34).unwrap_or(99.0),
        rdown: opt(params, 35).unwrap_or(-99.0),
        pmax: opt(params, 36).unwrap_or(1.0),
        pmin: opt(params, 37).unwrap_or(0.0),
    })
}

/// WPIDHY — Woodward PID Hydro Governor (15 fields).
fn build_wpidhy(line: usize, params: &[f64]) -> Result<WpidhyParams, DyrError> {
    check_params("WPIDHY", params, 9, line)?;
    Ok(WpidhyParams {
        gatmax: p(params, 0),
        gatmin: p(params, 1),
        reg: p(params, 2),
        kp: p(params, 3),
        ki: p(params, 4),
        kd: p(params, 5),
        ta: p(params, 6),
        tb: p(params, 7),
        tw: p(params, 8),
        at: opt(params, 9).unwrap_or(1.0),
        dturb: opt(params, 10).unwrap_or(0.0),
        gmax: opt(params, 11).unwrap_or(1.0),
        gmin: opt(params, 12).unwrap_or(0.0),
        pmax: opt(params, 13).unwrap_or(1.0),
        pmin: opt(params, 14).unwrap_or(0.0),
    })
}

/// H6B — Six-State Hydro Governor Variant B (10 fields).
fn build_h6b(line: usize, params: &[f64]) -> Result<H6bParams, DyrError> {
    check_params("H6B", params, 6, line)?;
    Ok(H6bParams {
        tg: p(params, 0),
        tp: p(params, 1),
        uo: p(params, 2),
        uc: p(params, 3),
        pmax: p(params, 4),
        pmin: p(params, 5),
        beta: opt(params, 6).unwrap_or(1.0),
        tw: opt(params, 7).unwrap_or(1.0),
        dbinf: opt(params, 8).unwrap_or(0.0),
        dbsup: opt(params, 9).unwrap_or(0.0),
    })
}

/// WSHYDD — WSHYGP with speed deadband (10 fields).
fn build_wshydd(line: usize, params: &[f64]) -> Result<WshyddParams, DyrError> {
    check_params("WSHYDD", params, 4, line)?;
    Ok(WshyddParams {
        r: p(params, 0),
        tf: p(params, 1),
        tg: p(params, 2),
        tw: p(params, 3),
        db: opt(params, 4).unwrap_or(0.0),
        kd: opt(params, 5).unwrap_or(0.0),
        pmax: opt(params, 6).unwrap_or(1.0),
        pmin: opt(params, 7).unwrap_or(0.0),
        kp: opt(params, 8).unwrap_or(1.0),
        ki: opt(params, 9).unwrap_or(0.5),
    })
}

// ---------------------------------------------------------------------------
// Phase 26: HVDC/FACTS advanced builders
// ---------------------------------------------------------------------------

/// HVDCPLU1 — PSS/E LCC HVDC two-terminal (LCC firing-angle physics).
///
/// DYR format (PSS/E convention):
/// `'HVDCPLU1' IBUS1 IBUS2 / setvl vschd mbase xcr xci rdc td tr alpha_min alpha_max gamma_min kp_id ki_id t_ramp pmax pmin /`
///
/// Fields (0-indexed after the slash):
///   0  setvl      — scheduled DC power (pu on mbase)
///   1  vschd      — scheduled DC voltage (pu on system base)
///   2  mbase      — MVA base of the HVDC link
///   3  xcr        — commutation reactance at rectifier (pu)
///   4  xci        — commutation reactance at inverter (pu)
///   5  rdc        — DC line resistance (pu)
///   6  td         — DC circuit time constant (s)
///   7  tr         — measurement/control filter time constant (s)
///   8  alpha_min  — minimum firing angle (rad)
///   9  alpha_max  — maximum firing angle (rad)
///  10  gamma_min  — minimum extinction angle (rad)
///  11  kp_id      — CC control proportional gain
///  12  ki_id      — CC control integral gain
///  13  t_ramp     — power order ramp time constant (s)
///  14  pmax       — maximum power (pu)
///  15  pmin       — minimum power (pu, usually 0)
fn build_hvdcplu1(line: usize, params: &[f64]) -> Result<HvdcPlu1Params, DyrError> {
    check_params("HVDCPLU1", params, 1, line)?;
    Ok(HvdcPlu1Params {
        setvl: p(params, 0),
        vschd: opt(params, 1).unwrap_or(1.0),
        mbase: opt(params, 2).unwrap_or(100.0),
        xcr: opt(params, 3).unwrap_or(0.065),
        xci: opt(params, 4).unwrap_or(0.065),
        rdc: opt(params, 5).unwrap_or(0.01),
        td: opt(params, 6).unwrap_or(0.05),
        tr: opt(params, 7).unwrap_or(0.02),
        alpha_min: opt(params, 8).unwrap_or(5_f64.to_radians()),
        alpha_max: opt(params, 9).unwrap_or(50_f64.to_radians()),
        gamma_min: opt(params, 10).unwrap_or(17_f64.to_radians()),
        kp_id: opt(params, 11).unwrap_or(0.5),
        ki_id: opt(params, 12).unwrap_or(10.0),
        t_ramp: opt(params, 13).unwrap_or(0.5),
        pmax: opt(params, 14).unwrap_or(1.0),
        pmin: opt(params, 15).unwrap_or(0.0),
        rectifier_bus: 0, // populated from DYR bus field (primary bus = IBUS)
        inverter_bus: 0,  // populated from DYR second bus field if present
    })
}

/// CSVGN6 — SVC Variant 6 with Auxiliary Inputs (12 fields).
fn build_csvgn6(line: usize, params: &[f64]) -> Result<Csvgn6Params, DyrError> {
    check_params("CSVGN6", params, 6, line)?;
    Ok(Csvgn6Params {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        t4: p(params, 3),
        t5: p(params, 4),
        k: p(params, 5),
        k_aux: opt(params, 6).unwrap_or(0.0),
        t_aux: opt(params, 7).unwrap_or(0.0),
        vmax: opt(params, 8).unwrap_or(1.1),
        vmin: opt(params, 9).unwrap_or(0.9),
        bmax: opt(params, 10).unwrap_or(2.0),
        bmin: opt(params, 11).unwrap_or(-2.0),
    })
}

/// STCON1 — STATCOM with Inner Current Control (10 fields).
fn build_stcon1(line: usize, params: &[f64]) -> Result<Stcon1Params, DyrError> {
    check_params("STCON1", params, 5, line)?;
    Ok(Stcon1Params {
        tr: p(params, 0),
        kp: p(params, 1),
        ki: p(params, 2),
        kp_i: p(params, 3),
        ki_i: p(params, 4),
        vmax: opt(params, 5).unwrap_or(1.1),
        vmin: opt(params, 6).unwrap_or(0.9),
        iqmax: opt(params, 7).unwrap_or(1.0),
        iqmin: opt(params, 8).unwrap_or(-1.0),
        mbase: opt(params, 9).unwrap_or(100.0),
    })
}

/// GCSC — Gate-Controlled Series Compensator (6 fields).
fn build_gcsc(line: usize, params: &[f64]) -> Result<GcscParams, DyrError> {
    check_params("GCSC", params, 3, line)?;
    Ok(GcscParams {
        tr: p(params, 0),
        kp: p(params, 1),
        ki: p(params, 2),
        xmax: opt(params, 3).unwrap_or(0.5),
        xmin: opt(params, 4).unwrap_or(0.0),
        mbase: opt(params, 5).unwrap_or(100.0),
    })
}

/// SSSC — Static Synchronous Series Compensator (8 fields).
fn build_sssc(line: usize, params: &[f64]) -> Result<SsscParams, DyrError> {
    check_params("SSSC", params, 4, line)?;
    Ok(SsscParams {
        tr: p(params, 0),
        kp: p(params, 1),
        ki: p(params, 2),
        kp_i: p(params, 3),
        ki_i: opt(params, 4).unwrap_or(0.5),
        vqmax: opt(params, 5).unwrap_or(0.2),
        vqmin: opt(params, 6).unwrap_or(-0.2),
        mbase: opt(params, 7).unwrap_or(100.0),
    })
}

/// UPFC — Unified Power Flow Controller (12 fields).
fn build_upfc(line: usize, params: &[f64]) -> Result<UpfcParams, DyrError> {
    check_params("UPFC", params, 5, line)?;
    Ok(UpfcParams {
        tr: p(params, 0),
        kp_p: p(params, 1),
        ki_p: p(params, 2),
        kp_q: p(params, 3),
        ki_q: p(params, 4),
        kp_v: opt(params, 5).unwrap_or(1.0),
        ki_v: opt(params, 6).unwrap_or(0.5),
        pmax: opt(params, 7).unwrap_or(1.0),
        pmin: opt(params, 8).unwrap_or(-1.0),
        qmax: opt(params, 9).unwrap_or(0.5),
        qmin: opt(params, 10).unwrap_or(-0.5),
        mbase: opt(params, 11).unwrap_or(100.0),
    })
}

/// CDC3T — Three-Terminal LCC HVDC (10 fields).
fn build_cdc3t(line: usize, params: &[f64]) -> Result<Cdc3tParams, DyrError> {
    check_params("CDC3T", params, 5, line)?;
    Ok(Cdc3tParams {
        tr: p(params, 0),
        kp1: p(params, 1),
        ki1: p(params, 2),
        kp2: p(params, 3),
        ki2: p(params, 4),
        kp3: opt(params, 5).unwrap_or(1.0),
        ki3: opt(params, 6).unwrap_or(0.5),
        pmax: opt(params, 7).unwrap_or(1.0),
        pmin: opt(params, 8).unwrap_or(-1.0),
        mbase: opt(params, 9).unwrap_or(100.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 27: Generator/Load/Protection builders
// ---------------------------------------------------------------------------

/// REGCO1 — Grid-following converter generator (12 fields).
fn build_regco1(line: usize, params: &[f64]) -> Result<Regco1Params, DyrError> {
    check_params("REGCO1", params, 5, line)?;
    Ok(Regco1Params {
        tr: p(params, 0),
        kp_v: p(params, 1),
        ki_v: p(params, 2),
        kp_i: p(params, 3),
        ki_i: p(params, 4),
        vmax: opt(params, 5).unwrap_or(1.1),
        vmin: opt(params, 6).unwrap_or(0.9),
        iqmax: opt(params, 7).unwrap_or(1.1),
        iqmin: opt(params, 8).unwrap_or(-1.1),
        pmax: opt(params, 9).unwrap_or(1.0),
        pmin: opt(params, 10).unwrap_or(0.0),
        mbase: opt(params, 11).unwrap_or(100.0),
    })
}

/// GENSAL3 — Third-order salient-pole synchronous generator (9 fields).
fn build_gensal3(line: usize, params: &[f64]) -> Result<Gensal3Params, DyrError> {
    check_params("GENSAL3", params, 6, line)?;
    Ok(Gensal3Params {
        td0_prime: p(params, 0),
        h: p(params, 1),
        d: p(params, 2),
        xd: p(params, 3),
        xq: p(params, 4),
        xd_prime: p(params, 5),
        xl: opt(params, 6).unwrap_or(0.06),
        s1: opt(params, 7).unwrap_or(0.0),
        s12: opt(params, 8).unwrap_or(0.0),
    })
}

/// LCFB1 — Load compensator with frequency bias (6 fields).
fn build_lcfb1(line: usize, params: &[f64]) -> Result<Lcfb1Params, DyrError> {
    check_params("LCFB1", params, 3, line)?;
    Ok(Lcfb1Params {
        tc: p(params, 0),
        tb: p(params, 1),
        kf: p(params, 2),
        pmax: opt(params, 3).unwrap_or(1.0),
        pmin: opt(params, 4).unwrap_or(-1.0),
        mbase: opt(params, 5).unwrap_or(100.0),
    })
}

/// LDFRAL — Dynamic load frequency regulation (7 fields).
fn build_ldfral(line: usize, params: &[f64]) -> Result<LdfralParams, DyrError> {
    check_params("LDFRAL", params, 3, line)?;
    Ok(LdfralParams {
        tc: p(params, 0),
        tb: p(params, 1),
        kf: p(params, 2),
        kp: opt(params, 3).unwrap_or(1.0),
        pmax: opt(params, 4).unwrap_or(1.0),
        pmin: opt(params, 5).unwrap_or(-1.0),
        mbase: opt(params, 6).unwrap_or(100.0),
    })
}

/// FRQTPLT — Frequency relay trip (4 fields).
fn build_frqtplt(line: usize, params: &[f64]) -> Result<FrqtpltParams, DyrError> {
    check_params("FRQTPLT", params, 2, line)?;
    Ok(FrqtpltParams {
        tf: p(params, 0),
        fmin: p(params, 1),
        fmax: opt(params, 2).unwrap_or(65.0),
        p_trip: opt(params, 3).unwrap_or(1.0),
    })
}

/// LVSHBL — Low-voltage shunt block (3 fields).
fn build_lvshbl(line: usize, params: &[f64]) -> Result<LvshblParams, DyrError> {
    check_params("LVSHBL", params, 2, line)?;
    Ok(LvshblParams {
        tv: p(params, 0),
        vmin: p(params, 1),
        p_block: opt(params, 2).unwrap_or(1.0),
    })
}

/// UVLS1 — Under-Voltage Load Shedding relay (6 fields).
fn build_uvls1(line: usize, params: &[f64]) -> Result<Uvls1Params, DyrError> {
    check_params("UVLS1", params, 4, line)?;
    Ok(Uvls1Params {
        tv: p(params, 0),
        vmin: p(params, 1),
        t_delay: p(params, 2),
        p_shed: p(params, 3),
        v_reconnect: opt(params, 4).unwrap_or(0.0),
        t_reconnect: opt(params, 5).unwrap_or(5.0),
    })
}

// ---------------------------------------------------------------------------
// Wave 35: new model builders
// ---------------------------------------------------------------------------

/// HYGOV4 — Hydro Governor with Surge Tank (5 states, 14 params).
/// PSS/E params: TR TF DTURB HDAM TW QNL AT DG GMAX GMIN TS KS PMAX PMIN
fn build_hygov4(line: usize, params: &[f64]) -> Result<Hygov4Params, DyrError> {
    check_params("HYGOV4", params, 10, line)?;
    Ok(Hygov4Params {
        tr: p(params, 0),
        tf: p(params, 1),
        dturb: p(params, 2),
        hdam: p(params, 3),
        tw: p(params, 4),
        qnl: p(params, 5),
        at: p(params, 6),
        dg: p(params, 7),
        gmax: p(params, 8),
        gmin: p(params, 9),
        ts: opt(params, 10).unwrap_or(5.0),
        ks: opt(params, 11).unwrap_or(0.5),
        pmax: opt(params, 12).unwrap_or(1.0),
        pmin: opt(params, 13).unwrap_or(0.0),
    })
}

/// WEHGOV — WECC Enhanced Hydro Governor (4 states, 14 params).
/// PSS/E params: R TR TF TG TW AT DTURB QNL GMAX GMIN DBD1 DBD2 PMAX PMIN
fn build_wehgov(line: usize, params: &[f64]) -> Result<WehgovParams, DyrError> {
    check_params("WEHGOV", params, 10, line)?;
    Ok(WehgovParams {
        r: p(params, 0),
        tr: p(params, 1),
        tf: p(params, 2),
        tg: p(params, 3),
        tw: p(params, 4),
        at: p(params, 5),
        dturb: p(params, 6),
        qnl: p(params, 7),
        gmax: p(params, 8),
        gmin: p(params, 9),
        dbd1: opt(params, 10).unwrap_or(-0.0006),
        dbd2: opt(params, 11).unwrap_or(0.0006),
        pmax: opt(params, 12).unwrap_or(1.0),
        pmin: opt(params, 13).unwrap_or(0.0),
    })
}

/// IEEEG3 — IEEE Type G3 Hydro Governor (3 states, 10 params).
/// PSS/E params: TG TP UO UC PMAX PMIN TW AT DTURB QNL
fn build_ieeeg3(line: usize, params: &[f64]) -> Result<Ieeeg3Params, DyrError> {
    check_params("IEEEG3", params, 8, line)?;
    Ok(Ieeeg3Params {
        tg: p(params, 0),
        tp: p(params, 1),
        uo: p(params, 2),
        uc: p(params, 3),
        pmax: p(params, 4),
        pmin: p(params, 5),
        tw: p(params, 6),
        at: p(params, 7),
        dturb: opt(params, 8).unwrap_or(0.0),
        qnl: opt(params, 9).unwrap_or(0.0),
    })
}

/// IEEEG4 — IEEE Type G4 Hydro Governor (3 states, 10 params).
/// PSS/E params: T1 T2 T3 KI PMAX PMIN TW AT DTURB QNL
fn build_ieeeg4(line: usize, params: &[f64]) -> Result<Ieeeg4Params, DyrError> {
    check_params("IEEEG4", params, 6, line)?;
    Ok(Ieeeg4Params {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        ki: p(params, 3),
        pmax: p(params, 4),
        pmin: p(params, 5),
        tw: opt(params, 6).unwrap_or(1.0),
        at: opt(params, 7).unwrap_or(1.0),
        dturb: opt(params, 8).unwrap_or(0.0),
        qnl: opt(params, 9).unwrap_or(0.0),
    })
}

/// ESAC7C — IEEE 421.5-2016 AC7C exciter (same param count as ESAC7B variant).
/// PSS/E params: TR KPR KIR KDR TDR VRMAX VRMIN KA TA KP KL TE KE VFEMAX VEMIN E1 SE1 E2 SE2
fn build_esac7c(line: usize, params: &[f64]) -> Result<Esac7cParams, DyrError> {
    check_params("ESAC7C", params, 10, line)?;
    Ok(Esac7cParams {
        tr: p(params, 0),
        kpr: p(params, 1),
        kir: p(params, 2),
        kdr: opt(params, 3).unwrap_or(0.0),
        tdr: opt(params, 4).unwrap_or(0.01),
        vrmax: opt(params, 5).unwrap_or(5.0),
        vrmin: opt(params, 6).unwrap_or(-5.0),
        ka: opt(params, 7).unwrap_or(1.0),
        ta: opt(params, 8).unwrap_or(0.01),
        kp: opt(params, 9).unwrap_or(1.0),
        kl: opt(params, 10).unwrap_or(0.0),
        te: opt(params, 11).unwrap_or(0.8),
        ke: opt(params, 12).unwrap_or(1.0),
        vfemax: opt(params, 13).unwrap_or(99.0),
        vemin: opt(params, 14).unwrap_or(0.0),
        e1: opt(params, 15).unwrap_or(3.1),
        se1: opt(params, 16).unwrap_or(0.44),
        e2: opt(params, 17).unwrap_or(4.7),
        se2: opt(params, 18).unwrap_or(0.86),
    })
}

/// ESDC4C — IEEE 421.5-2016 DC4C exciter params.
/// PSS/E params: TR KA TA VRMAX VRMIN KE TE KF TF E1 SE1 E2 SE2 [KPR KIR KDR TDR]
fn build_esdc4c(line: usize, params: &[f64]) -> Result<Esdc4cParams, DyrError> {
    check_params("ESDC4C", params, 8, line)?;
    Ok(Esdc4cParams {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        vrmax: p(params, 3),
        vrmin: p(params, 4),
        ke: p(params, 5),
        te: p(params, 6),
        kf: p(params, 7),
        tf: p(params, 8),
        e1: opt(params, 9).unwrap_or(2.8),
        se1: opt(params, 10).unwrap_or(0.065),
        e2: opt(params, 11).unwrap_or(3.73),
        se2: opt(params, 12).unwrap_or(0.278),
        kpr: opt(params, 13).unwrap_or(0.0),
        kir: opt(params, 14).unwrap_or(0.0),
        kdr: opt(params, 15).unwrap_or(0.0),
        tdr: opt(params, 16).unwrap_or(0.01),
    })
}

/// PSS7C — IEEE 421.5-2016 multi-band PSS.
///
/// Legacy DYR params: `KSS TW1 TW2 T1 T2 T3 T4 VSMAX VSMIN`
/// The per-band gains (kl/ki/kh) default to 0; `effective_bands()` will map
/// legacy kss to the intermediate band at runtime.
fn build_pss7c(line: usize, params: &[f64]) -> Result<Pss7cParams, DyrError> {
    check_params("PSS7C", params, 6, line)?;
    Ok(Pss7cParams {
        kss: p(params, 0),
        tw1: p(params, 1),
        tw2: opt(params, 2).unwrap_or(p(params, 1)),
        t1: p(params, 3),
        t2: p(params, 4),
        t3: opt(params, 5).unwrap_or(p(params, 3)),
        t4: opt(params, 6).unwrap_or(p(params, 4)),
        vsmax: opt(params, 7).unwrap_or(0.1),
        vsmin: opt(params, 8).unwrap_or(-0.1),
        // Per-band params at positions 9-22 (multi-band extension).
        kl: opt(params, 9).unwrap_or(0.0),
        tw_l: opt(params, 10).unwrap_or(10.0),
        t1_l: opt(params, 11).unwrap_or(0.0),
        t2_l: opt(params, 12).unwrap_or(0.04),
        ki: opt(params, 13).unwrap_or(0.0),
        tw_i: opt(params, 14).unwrap_or(10.0),
        t1_i: opt(params, 15).unwrap_or(0.0),
        t2_i: opt(params, 16).unwrap_or(0.04),
        kh: opt(params, 17).unwrap_or(0.0),
        tw_h: opt(params, 18).unwrap_or(10.0),
        t1_h: opt(params, 19).unwrap_or(0.0),
        t2_h: opt(params, 20).unwrap_or(0.04),
        vstmax: opt(params, 21).unwrap_or(0.1),
        vstmin: opt(params, 22).unwrap_or(-0.1),
    })
}

/// VTGTPAT / VTGDCAT — Voltage-Time Generator Protection Trip.
/// PSS/E params: TV VTRIP VRESET
fn build_vtgtpat(line: usize, params: &[f64]) -> Result<VtgtpatParams, DyrError> {
    check_params("VTGTPAT", params, 2, line)?;
    Ok(VtgtpatParams {
        tv: p(params, 0),
        vtrip: p(params, 1),
        vreset: opt(params, 2).unwrap_or(0.92),
    })
}

/// FRQTPAT / FRQDCAT — Frequency-Time Generator Protection Trip.
/// PSS/E params: TF FTRIP_HI FTRIP_LO FRESET
fn build_frqtpat(line: usize, params: &[f64]) -> Result<FrqtpatParams, DyrError> {
    check_params("FRQTPAT", params, 2, line)?;
    Ok(FrqtpatParams {
        tf: p(params, 0),
        ftrip_hi: p(params, 1),
        ftrip_lo: opt(params, 2).unwrap_or(0.95),
        freset: opt(params, 3).unwrap_or(1.0),
    })
}

// ---------------------------------------------------------------------------
// Phase 28 build functions
// ---------------------------------------------------------------------------

/// REPCGFM_C1 — GFM plant Volt/Var controller.
/// Params: KpV KiV Vmax Vmin KpQ KiQ Qmax Qmin Tlag Fdroop Dbd1 Dbd2
fn build_repcgfmc1(line: usize, params: &[f64]) -> Result<Repcgfmc1Params, DyrError> {
    check_params("REPCGFM_C1", params, 8, line)?;
    Ok(Repcgfmc1Params {
        kp_v: p(params, 0),
        ki_v: p(params, 1),
        vmax: p(params, 2),
        vmin: p(params, 3),
        kp_q: p(params, 4),
        ki_q: p(params, 5),
        qmax: p(params, 6),
        qmin: p(params, 7),
        tlag: opt(params, 8).unwrap_or(0.02),
        fdroop: opt(params, 9).unwrap_or(0.05),
        dbd1: opt(params, 10).unwrap_or(0.0),
        dbd2: opt(params, 11).unwrap_or(0.0),
    })
}

/// DERP — DER with Protection.
/// Params: Xeq Trf Imax Trv Tpll Flow Fhigh VLow VHigh Trip Treconnect
fn build_derp(line: usize, params: &[f64]) -> Result<DerpParams, DyrError> {
    // PSS/E DERP params: Xeq Trf Imax Trv FLow FHigh [VLow VHigh Trip Treconnect Tpll]
    check_params("DERP", params, 4, line)?;
    Ok(DerpParams {
        x_eq: p(params, 0),
        trf: p(params, 1),
        imax: p(params, 2),
        trv: p(params, 3),
        flow: p(params, 4),
        fhigh: p(params, 5),
        vlow: opt(params, 6).unwrap_or(0.5),
        vhigh: opt(params, 7).unwrap_or(1.2),
        trip: opt(params, 8).unwrap_or(0.16),
        treconnect: opt(params, 9).unwrap_or(300.0),
        tpll: opt(params, 10).unwrap_or(0.02),
    })
}

/// REGFM_D1 — WECC Sep-2025 hybrid GFM/GFL converter.
/// Params: Rrv Lrv Kpv Kiv Kpg Kig Kdroop Kvir Kfir Imax Dpf Dqf Xeq Mbase
fn build_regfmd1(line: usize, params: &[f64]) -> Result<Regfmd1Params, DyrError> {
    check_params("REGFM_D1", params, 10, line)?;
    Ok(Regfmd1Params {
        rrv: p(params, 0),
        lrv: p(params, 1),
        kpv: p(params, 2),
        kiv: p(params, 3),
        kpg: p(params, 4),
        kig: p(params, 5),
        kdroop: p(params, 6),
        kvir: p(params, 7),
        kfir: p(params, 8),
        imax: p(params, 9),
        dpf: opt(params, 10).unwrap_or(0.0),
        dqf: opt(params, 11).unwrap_or(0.0),
        x_eq: opt(params, 12).unwrap_or(0.02),
        mbase: opt(params, 13).unwrap_or(100.0),
        tpll: opt(params, 14).unwrap_or(0.02),
        tv: opt(params, 15).unwrap_or(0.02),
    })
}

/// WTDTA1 — Wind turbine two-mass drive-train.
/// Params: H Dshaft Kshaft D2
fn build_wtdta1(line: usize, params: &[f64]) -> Result<Wtdta1Params, DyrError> {
    check_params("WTDTA1", params, 3, line)?;
    Ok(Wtdta1Params {
        h: p(params, 0),
        dshaft: p(params, 1),
        kshaft: p(params, 2),
        d2: opt(params, 3).unwrap_or(0.0),
    })
}

/// WTARA1 — Wind turbine aerodynamic aggregation.
/// Params: Ka Ta Km Tm Pmax Pmin
fn build_wtara1(line: usize, params: &[f64]) -> Result<Wtara1Params, DyrError> {
    check_params("WTARA1", params, 4, line)?;
    Ok(Wtara1Params {
        ka: p(params, 0),
        ta: p(params, 1),
        km: p(params, 2),
        tm: p(params, 3),
        pmax: opt(params, 4).unwrap_or(1.5),
        pmin: opt(params, 5).unwrap_or(0.0),
    })
}

/// WTAERO — Full aerodynamic Cp(λ,β) wind turbine model.
/// Params: Rho R_rotor Gear_ratio V_wind_base Mbase_MW [H_rotor K_shaft D_shaft]
fn build_wtaero(line: usize, params: &[f64]) -> Result<WtaeroParams, DyrError> {
    check_params("WTAERO", params, 5, line)?;
    Ok(WtaeroParams {
        rho: p(params, 0),
        r_rotor: p(params, 1),
        gear_ratio: p(params, 2),
        cp_table: CpTable::nrel_5mw(), // default NREL 5MW table
        v_wind_base: p(params, 3),
        mbase_mw: p(params, 4),
        h_rotor: if params.len() > 5 {
            Some(p(params, 5))
        } else {
            None
        },
        k_shaft: if params.len() > 6 {
            Some(p(params, 6))
        } else {
            None
        },
        d_shaft: if params.len() > 7 {
            Some(p(params, 7))
        } else {
            None
        },
    })
}

/// WTPTA1 — Wind turbine pitch angle control.
/// Params: Kpp Kip Theta_max Theta_min Rate_max Rate_min Te [KpPitch]
fn build_wtpta1(line: usize, params: &[f64]) -> Result<Wtpta1Params, DyrError> {
    check_params("WTPTA1", params, 6, line)?;
    Ok(Wtpta1Params {
        kpp: p(params, 0),
        kip: p(params, 1),
        theta_max: p(params, 2),
        theta_min: p(params, 3),
        rate_max: p(params, 4),
        rate_min: p(params, 5),
        te: opt(params, 6).unwrap_or(0.1),
        kp_pitch: opt(params, 7).unwrap_or(0.0),
    })
}

// ---------------------------------------------------------------------------
// Wave 34: new model builders
// ---------------------------------------------------------------------------

/// IEESGO — IEEE Standard Governor (5-state steam turbine).
/// PSS/E params: T1 T2 T3 T4 T5 T6 K1 K2 K3 Pmax Pmin
fn build_ieesgo(line: usize, params: &[f64]) -> Result<IeesgoParams, DyrError> {
    check_params("IEESGO", params, 10, line)?;
    Ok(IeesgoParams {
        t1: p(params, 0),
        t2: p(params, 1),
        t3: p(params, 2),
        t4: p(params, 3),
        t5: p(params, 4),
        t6: p(params, 5),
        k1: p(params, 6),
        k2: p(params, 7),
        k3: p(params, 8),
        pmax: p(params, 9),
        pmin: p(params, 10),
    })
}

/// WTTQA1 — WECC Type 2 Wind Torque Controller (3-state).
/// PSS/E params: Kp Ki Tp Pmax Pmin [Tflag Twref Temax Temin P1 Sp1 P2 Sp2 P3 Sp3 P4 Sp4]
fn build_wttqa1(line: usize, params: &[f64]) -> Result<Wttqa1Params, DyrError> {
    check_params("WTTQA1", params, 4, line)?;
    let spl = [
        (
            opt(params, 9).unwrap_or(0.2),
            opt(params, 10).unwrap_or(0.58),
        ),
        (
            opt(params, 11).unwrap_or(0.4),
            opt(params, 12).unwrap_or(0.72),
        ),
        (
            opt(params, 13).unwrap_or(0.6),
            opt(params, 14).unwrap_or(0.86),
        ),
        (
            opt(params, 15).unwrap_or(0.8),
            opt(params, 16).unwrap_or(1.0),
        ),
    ];
    Ok(Wttqa1Params {
        kp: p(params, 0),
        ki: p(params, 1),
        tp: p(params, 2),
        pmax: p(params, 3),
        pmin: p(params, 4),
        tflag: opt(params, 5).map(|v| v as i32).unwrap_or(0),
        twref: opt(params, 6).unwrap_or(0.02),
        temax: opt(params, 7).unwrap_or(1.2),
        temin: opt(params, 8).unwrap_or(0.0),
        spl,
    })
}

/// CIM6 — 6th-order induction motor.
/// PSS/E params: RA XS XM XR1 XR2 RR1 RR2 [H E1 S1 E2 S2 MBASE TQ0P XQP]
fn build_cim6(line: usize, params: &[f64]) -> Result<Cim6Params, DyrError> {
    check_params("CIM6", params, 7, line)?;
    Ok(Cim6Params {
        ra: p(params, 0),
        xs: p(params, 1),
        xm: p(params, 2),
        xr1: p(params, 3),
        xr2: p(params, 4),
        rr1: p(params, 5),
        rr2: p(params, 6),
        h: opt(params, 7).unwrap_or(0.5),
        e1: opt(params, 8).unwrap_or(1.0),
        s1: opt(params, 9).unwrap_or(0.02),
        e2: opt(params, 10).unwrap_or(1.2),
        s2: opt(params, 11).unwrap_or(0.1),
        mbase: opt(params, 12).unwrap_or(100.0),
        tq0p: opt(params, 13).unwrap_or(0.1),
        xq_prime: opt(params, 14).unwrap_or(0.2),
    })
}

/// EXTL — External Load (simplified composite load model).
/// PSS/E params: Tp Tq Kpv Kqv Kpf Kqf [mbase lfac]
fn build_extl(line: usize, params: &[f64]) -> Result<ExtlParams, DyrError> {
    check_params("EXTL", params, 5, line)?;
    Ok(ExtlParams {
        tp: p(params, 0),
        tq: p(params, 1),
        kpv: p(params, 2),
        kqv: p(params, 3),
        kpf: p(params, 4),
        kqf: opt(params, 5).unwrap_or(0.0),
        mbase: opt(params, 6).unwrap_or(100.0),
        lfac: opt(params, 7).unwrap_or(1.0),
    })
}

/// SVSMO1 — WECC Generic SVC voltage regulator (1-state).
/// PSS/E params: Tr K Ta Bmin Bmax
fn build_svsmo1(line: usize, params: &[f64]) -> Result<Svsmo1Params, DyrError> {
    check_params("SVSMO1", params, 4, line)?;
    Ok(Svsmo1Params {
        tr: p(params, 0),
        k: p(params, 1),
        ta: p(params, 2),
        b_min: p(params, 3),
        b_max: p(params, 4),
    })
}

/// SVSMO2 — WECC Generic STATCOM (1-state).
/// PSS/E params: Tr K Ta IqMin IqMax
fn build_svsmo2(line: usize, params: &[f64]) -> Result<Svsmo2Params, DyrError> {
    check_params("SVSMO2", params, 4, line)?;
    Ok(Svsmo2Params {
        tr: p(params, 0),
        k: p(params, 1),
        ta: p(params, 2),
        iq_min: p(params, 3),
        iq_max: p(params, 4),
    })
}

/// SVSMO3 — WECC Advanced SVC (2-state: b_svc + vr).
/// PSS/E params: Tr Ka Ta Tb Bmin Bmax
fn build_svsmo3(line: usize, params: &[f64]) -> Result<Svsmo3Params, DyrError> {
    check_params("SVSMO3", params, 5, line)?;
    Ok(Svsmo3Params {
        tr: p(params, 0),
        ka: p(params, 1),
        ta: p(params, 2),
        tb: p(params, 3),
        b_min: p(params, 4),
        b_max: p(params, 5),
    })
}

// ---------------------------------------------------------------------------
// Wave 36: new builder functions
// ---------------------------------------------------------------------------

/// GOVCT1 / GOVCT2 — Combined cycle turbine governor (5 states).
/// PSS/E params: R T1 VMAX VMIN T2 T3 K1 K2 K3 T4 T5 T6 K7 K8 PMAX PMIN [TD]
fn build_govct1(line: usize, params: &[f64]) -> Result<Govct1Params, DyrError> {
    check_params("GOVCT1", params, 14, line)?;
    Ok(Govct1Params {
        r: p(params, 0),
        t1: p(params, 1),
        vmax: p(params, 2),
        vmin: p(params, 3),
        t2: p(params, 4),
        t3: p(params, 5),
        k1: p(params, 6),
        k2: p(params, 7),
        k3: p(params, 8),
        t4: p(params, 9),
        t5: p(params, 10),
        t6: p(params, 11),
        k7: p(params, 12),
        k8: p(params, 13),
        pmax: opt(params, 14).unwrap_or(1.0),
        pmin: opt(params, 15).unwrap_or(0.0),
        td: opt(params, 16).unwrap_or(0.0),
    })
}

/// GOVCT2 — Two-shaft combined cycle turbine governor (7 states).
/// PSS/E params: R T1 VMAX VMIN T2 T3 K1 K2 K3 T4 T5 T6 K7 K8 PMAX PMIN [TD T_HRSG K_ST T_ST]
fn build_govct2(line: usize, params: &[f64]) -> Result<Govct2Params, DyrError> {
    check_params("GOVCT2", params, 14, line)?;
    Ok(Govct2Params {
        r: p(params, 0),
        t1: p(params, 1),
        vmax: p(params, 2),
        vmin: p(params, 3),
        t2: p(params, 4),
        t3: p(params, 5),
        k1: p(params, 6),
        k2: p(params, 7),
        k3: p(params, 8),
        t4: p(params, 9),
        t5: p(params, 10),
        t6: p(params, 11),
        k7: p(params, 12),
        k8: p(params, 13),
        pmax: opt(params, 14).unwrap_or(1.0),
        pmin: opt(params, 15).unwrap_or(0.0),
        td: opt(params, 16).unwrap_or(0.0),
        t_hrsg: opt(params, 17).unwrap_or(60.0),
        k_st: opt(params, 18).unwrap_or(0.4),
        t_st: opt(params, 19).unwrap_or(10.0),
    })
}

/// TGOV3 / TGOV4 — Two-reheat steam governor (3 states).
/// PSS/E params: R T1 VMAX VMIN T2 T3 DT KD
fn build_tgov3(line: usize, params: &[f64]) -> Result<Tgov3Params, DyrError> {
    check_params("TGOV3", params, 6, line)?;
    Ok(Tgov3Params {
        r: p(params, 0),
        t1: p(params, 1),
        vmax: p(params, 2),
        vmin: p(params, 3),
        t2: p(params, 4),
        t3: p(params, 5),
        dt: opt(params, 6).unwrap_or(0.0),
        kd: opt(params, 7).unwrap_or(0.0),
    })
}

/// WT1G1 / WT2G1 — Type 1/2 induction machine wind generator.
///
/// PSS/E params: `H D RA X_EQ IMAX`
///
/// X_EQ is the transient reactance X' = Xs + Xm·Xr/(Xm+Xr).
/// Full IM circuit parameters are derived from X_EQ using standard defaults.
fn build_wt1g1(line: usize, params: &[f64]) -> Result<Wt1g1Params, DyrError> {
    check_params("WT1G1", params, 3, line)?;
    let x_eq = opt(params, 3).unwrap_or(0.02);
    // Decompose X_EQ into full IM parameters using standard defaults.
    let xs = 0.1 * x_eq;
    let xm = 3.0;
    let xr = xs;
    let rr = 0.01;
    Ok(Wt1g1Params {
        h: p(params, 0),
        d: p(params, 1),
        ra: p(params, 2),
        x_eq,
        imax: opt(params, 4).unwrap_or(1.2),
        xs,
        xm,
        xr,
        rr,
    })
}

/// WT2E1 — Type 2 wind electrical controller.
/// PSS/E params: KP KI PMAX PMIN TE
fn build_wt2e1(line: usize, params: &[f64]) -> Result<Wt2e1Params, DyrError> {
    check_params("WT2E1", params, 3, line)?;
    Ok(Wt2e1Params {
        kp: p(params, 0),
        ki: p(params, 1),
        pmax: p(params, 2),
        pmin: p(params, 3),
        te: opt(params, 4).unwrap_or(0.05),
    })
}

/// DISTR1 — Distance relay.
/// PSS/E params: Z1 Z2 T1 T2 MBASE LFAC [Z3 T3 ANGLE BRANCH_FROM BRANCH_TO BRANCH_R BRANCH_X TF]
fn build_distr1(line: usize, params: &[f64]) -> Result<Distr1Params, DyrError> {
    check_params("DISTR1", params, 4, line)?;
    let z1 = p(params, 0);
    let z2 = p(params, 1);
    Ok(Distr1Params {
        z1,
        z2,
        z3: opt(params, 6).unwrap_or(z2 * 1.5),
        t1: p(params, 2),
        t2: p(params, 3),
        t3: opt(params, 7).unwrap_or(1.0),
        reach_angle_deg: opt(params, 8).unwrap_or(80.0),
        mbase: opt(params, 4).unwrap_or(100.0),
        lfac: opt(params, 5).unwrap_or(1.0),
        branch_from: opt(params, 9).unwrap_or(0.0) as u32,
        branch_to: opt(params, 10).unwrap_or(0.0) as u32,
        branch_r: opt(params, 11).unwrap_or(0.0),
        branch_x: opt(params, 12).unwrap_or(0.0),
        tf: opt(params, 13).unwrap_or(0.02),
    })
}

/// BFR50 — Breaker failure relay (ANSI 50BF).
/// DYR params: T_BFR  I_SUP  BRANCH_IDX
fn build_bfr50(line: usize, params: &[f64]) -> Result<Bfr50Params, DyrError> {
    check_params("BFR50", params, 1, line)?;
    Ok(Bfr50Params {
        t_bfr: p(params, 0),
        i_sup: opt(params, 1).unwrap_or(0.1),
        branch_idx: opt(params, 2).unwrap_or(0.0) as usize,
    })
}

// ---------------------------------------------------------------------------
// Wave 7 (B10): Additional protection relay builder functions
// ---------------------------------------------------------------------------

/// 87T — Transformer differential relay.
/// DYR params: SLOPE1 SLOPE2 I_PICKUP HARMONIC_RESTRAINT FROM_BUS TO_BUS CKT TURNS_RATIO [TF]
fn build_trans_diff_87(line: usize, params: &[f64]) -> Result<TransDiff87Params, DyrError> {
    check_params("87T", params, 7, line)?;
    Ok(TransDiff87Params {
        slope1: p(params, 0),
        slope2: p(params, 1),
        i_pickup: p(params, 2),
        harmonic_restraint: p(params, 3),
        from_bus: p(params, 4) as u32,
        to_bus: p(params, 5) as u32,
        circuit: String::new(), // circuit parsed from machine_id
        turns_ratio: p(params, 6),
        tf: opt(params, 7).unwrap_or(0.01),
    })
}

/// 87L — Line differential relay.
/// DYR params: SLOPE1 SLOPE2 I_PICKUP FROM_BUS TO_BUS CKT [TF]
fn build_line_diff_87l(line: usize, params: &[f64]) -> Result<LineDiff87lParams, DyrError> {
    check_params("87L", params, 5, line)?;
    Ok(LineDiff87lParams {
        slope1: p(params, 0),
        slope2: p(params, 1),
        i_pickup: p(params, 2),
        from_bus: p(params, 3) as u32,
        to_bus: p(params, 4) as u32,
        circuit: String::new(),
        tf: opt(params, 5).unwrap_or(0.01),
    })
}

/// 79 — Automatic recloser.
/// DYR params: DEAD1 DEAD2 DEAD3 MAX_ATTEMPTS FROM_BUS TO_BUS CKT RESET_TIME
fn build_recloser_79(line: usize, params: &[f64]) -> Result<Recloser79Params, DyrError> {
    check_params("79", params, 6, line)?;
    Ok(Recloser79Params {
        dead_time_1: p(params, 0),
        dead_time_2: p(params, 1),
        dead_time_3: p(params, 2),
        max_attempts: p(params, 3) as u32,
        from_bus: p(params, 4) as u32,
        to_bus: p(params, 5) as u32,
        circuit: String::new(),
        reset_time: opt(params, 6).unwrap_or(5.0),
    })
}

// ---------------------------------------------------------------------------
// Wave 37: OEL/UEL limiter builder functions
// ---------------------------------------------------------------------------

/// ESAC9C — IEEE 421.5-2016 AC9C exciter (same structure as ESAC7B, lenient param count).
/// PSS/E params: TR KPA KIA VRH VRL KPF VFH TF TE KE [E1 SE1 E2 SE2 KD KC KL]
fn build_esac9c(line: usize, params: &[f64]) -> Result<Esac7bParams, DyrError> {
    check_params("ESAC9C", params, 6, line)?;
    Ok(Esac7bParams {
        tr: p(params, 0),
        kpa: p(params, 1),
        kia: p(params, 2),
        vrh: opt(params, 3).unwrap_or(7.0),
        vrl: opt(params, 4).unwrap_or(-7.0),
        kpf: opt(params, 5).unwrap_or(0.0),
        vfh: opt(params, 6).unwrap_or(99.0),
        tf: opt(params, 7).unwrap_or(1.0),
        te: opt(params, 8).unwrap_or(0.8),
        ke: opt(params, 9).unwrap_or(1.0),
        e1: opt(params, 10).unwrap_or(3.1),
        se1: opt(params, 11).unwrap_or(0.065),
        e2: opt(params, 12).unwrap_or(4.67),
        se2: opt(params, 13).unwrap_or(0.278),
        kd: opt(params, 14).unwrap_or(0.0),
        kc: opt(params, 15).unwrap_or(0.0),
        kl: opt(params, 16).unwrap_or(0.0),
    })
}

/// OEL1B — Over-Excitation Limiter Type 1B.
/// PSS/E params: IFDMAX IFDLIM VRMAX VAMIN KRAMP TFF
fn build_oel1b(line: usize, params: &[f64]) -> Result<Oel1bParams, DyrError> {
    check_params("OEL1B", params, 4, line)?;
    Ok(Oel1bParams {
        ifdmax: p(params, 0),
        ifdlim: opt(params, 1).unwrap_or(1.5),
        vrmax: opt(params, 2).unwrap_or(7.0),
        vamin: opt(params, 3).unwrap_or(-5.0),
        kramp: opt(params, 4).unwrap_or(0.05),
        tff: opt(params, 5).unwrap_or(0.1),
    })
}

/// OEL2C / OEL3C / OEL4C / OEL5C — Over-Excitation Limiter C-series.
/// PSS/E params: IFDMAX T_OEL VAMIN VRMAX K_OEL
fn build_oel2c(line: usize, params: &[f64]) -> Result<Oel2cParams, DyrError> {
    check_params("OEL2C", params, 2, line)?;
    Ok(Oel2cParams {
        ifdmax: p(params, 0),
        t_oel: p(params, 1),
        vamin: opt(params, 2).unwrap_or(-5.0),
        vrmax: opt(params, 3).unwrap_or(7.0),
        k_oel: opt(params, 4).unwrap_or(1.0),
    })
}

/// SCL1C — Stator Current Limiter Type 1C.
/// PSS/E params: IRATED KR TR VCLMAX VCLMIN
fn build_scl1c(line: usize, params: &[f64]) -> Result<Scl1cParams, DyrError> {
    check_params("SCL1C", params, 2, line)?;
    Ok(Scl1cParams {
        irated: p(params, 0),
        kr: opt(params, 1).unwrap_or(1.0),
        tr: opt(params, 2).unwrap_or(0.05),
        vclmax: opt(params, 3).unwrap_or(0.0),
        vclmin: opt(params, 4).unwrap_or(-5.0),
    })
}

/// UEL1 — Under-Excitation Limiter Type 1.
/// PSS/E params: KUL TU1 VUCMAX VUCMIN KUR
fn build_uel1(line: usize, params: &[f64]) -> Result<Uel1Params, DyrError> {
    check_params("UEL1", params, 2, line)?;
    Ok(Uel1Params {
        kul: p(params, 0),
        tu1: p(params, 1),
        vucmax: opt(params, 2).unwrap_or(0.2),
        vucmin: opt(params, 3).unwrap_or(-0.2),
        kur: opt(params, 4).unwrap_or(1.0),
    })
}

/// UEL2C — Under-Excitation Limiter Type 2C (P-Q plane).
/// PSS/E params: KUL TU1 TU2 TU3 TU4 VUIMAX VUIMIN P0 Q0
fn build_uel2c(line: usize, params: &[f64]) -> Result<Uel2cParams, DyrError> {
    check_params("UEL2C", params, 2, line)?;
    Ok(Uel2cParams {
        kul: p(params, 0),
        tu1: p(params, 1),
        tu2: opt(params, 2).unwrap_or(0.5),
        tu3: opt(params, 3).unwrap_or(0.0),
        tu4: opt(params, 4).unwrap_or(0.0),
        vuimax: opt(params, 5).unwrap_or(0.5),
        vuimin: opt(params, 6).unwrap_or(-0.5),
        p0: opt(params, 7).unwrap_or(0.0),
        q0: opt(params, 8).unwrap_or(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal inline DYR with GENROU + EXST1 + TGOV1 + GENCLS.
    const MINIMAL_DYR: &str = r#"
      1 'GENROU' 1  8.0 0.03 0.4 0.05 6.5 0.0 1.8 1.7 0.3 0.55 0.25 0.06 0.0 0.0 /
      1 'EXST1'  1  0.02 99.0 -99.0 0.0 0.02 50.0 0.02 9999.0 -9999.0 0.0 0.01 1.0 /
      1 'TGOV1'  1  0.05 0.49 33.0 0.4 2.1 7.0 0.0 /
      2 'GENCLS' 1  3.0 0.0 /
"#;

    #[test]
    fn test_parse_minimal_dyr_string() {
        let dm = parse_str(MINIMAL_DYR).expect("parse failed");
        assert_eq!(
            dm.n_generators(),
            2,
            "expected 2 generators (GENROU + GENCLS)"
        );
        assert_eq!(dm.n_exciters(), 1, "expected 1 exciter (EXST1)");
        assert_eq!(dm.n_governors(), 1, "expected 1 governor (TGOV1)");
        assert_eq!(dm.n_pss(), 0);
        assert_eq!(dm.unknown_records.len(), 0);

        // Verify GENROU H = 6.5
        let gdyn = dm.find_generator(1, "1").expect("generator 1 not found");
        if let GeneratorModel::Genrou(p) = &gdyn.model {
            assert!(
                (p.h - 6.5).abs() < 1e-9,
                "GENROU H expected 6.5, got {}",
                p.h
            );
            assert!(
                (p.xd - 1.8).abs() < 1e-9,
                "GENROU Xd expected 1.8, got {}",
                p.xd
            );
        } else {
            panic!("expected Genrou model");
        }

        // Verify GENCLS H = 3.0
        let gdyn2 = dm.find_generator(2, "1").expect("generator 2 not found");
        if let GeneratorModel::Gencls(p) = &gdyn2.model {
            assert!((p.h - 3.0).abs() < 1e-9);
        } else {
            panic!("expected Gencls model");
        }
    }

    #[test]
    fn test_multiline_record() {
        let dyr = r#"
      1 'GENROU' 1
          8.0000      0.03000
          0.40000      0.05000
          6.5000       0.0000       1.8000
          1.7000       0.30000
          0.55000      0.25000      0.06000
          0.0000       0.0000   /
"#;
        let dm = parse_str(dyr).expect("parse failed");
        assert_eq!(dm.n_generators(), 1);
        let gdyn = dm.find_generator(1, "1").unwrap();
        if let GeneratorModel::Genrou(p) = &gdyn.model {
            assert!((p.h - 6.5).abs() < 1e-9, "H = {}", p.h);
            assert!((p.td0_prime - 8.0).abs() < 1e-9);
        } else {
            panic!("expected Genrou");
        }
    }

    #[test]
    fn test_comment_stripping() {
        // Note: `/` must appear before any `!` comment (inline comment strips rest of line).
        let dyr = r#"
@ this is a full-line comment
      1 'GENCLS' 1  6.5  0.0 / ! trailing inline comment
@ another comment
      2 'GENCLS' 1  3.0  0.0 /
"#;
        let dm = parse_str(dyr).expect("parse failed");
        assert_eq!(dm.n_generators(), 2);
        let g1 = dm.find_generator(1, "1").unwrap();
        if let GeneratorModel::Gencls(p) = &g1.model {
            assert!((p.h - 6.5).abs() < 1e-9);
        }
        let g2 = dm.find_generator(2, "1").unwrap();
        if let GeneratorModel::Gencls(p) = &g2.model {
            assert!((p.h - 3.0).abs() < 1e-9);
        }
    }

    #[test]
    fn test_unknown_model_graceful() {
        let dyr = r#"
      1 'GENROU' 1  8.0 0.03 0.4 0.05 6.5 0.0 1.8 1.7 0.3 0.55 0.25 0.06 0.0 0.0 /
      1 'MYFUTUREMODEL' 1  1.0 2.0 3.0 /
"#;
        let dm = parse_str(dyr).expect("parse failed");
        assert_eq!(dm.n_generators(), 1);
        assert_eq!(
            dm.unknown_records.len(),
            1,
            "unknown model should go to unknown_records"
        );
        assert_eq!(dm.unknown_records[0].model_name, "MYFUTUREMODEL");
        assert_eq!(dm.unknown_records[0].bus, 1);
    }

    #[test]
    fn test_parse_kundur_dyr_file() {
        // Skip if test file is not present.
        let path = std::path::Path::new("../../tests/data/raw/kundur_andes.dyr");
        if !path.exists() {
            eprintln!("SKIP: kundur_andes.dyr not found at {:?}", path);
            return;
        }

        let dm = parse_file(path).expect("parse kundur failed");

        assert_eq!(dm.n_generators(), 4, "expected 4 GENROU generators");
        assert!(
            dm.n_exciters() >= 4,
            "expected >= 4 exciters, got {}",
            dm.n_exciters()
        );

        // Verify bus 1, machine "1" → GENROU with H = 6.5
        let gdyn = dm
            .find_generator(1, "1")
            .expect("generator bus=1 id=1 not found");
        if let GeneratorModel::Genrou(p) = &gdyn.model {
            assert!(
                (p.h - 6.5).abs() < 1e-6,
                "GENROU H expected 6.5, got {}",
                p.h
            );
        } else {
            panic!("expected Genrou model for bus 1");
        }
    }

    #[test]
    fn test_parse_wecc_dyr_file() {
        let path = std::path::Path::new("../../tests/data/raw/wecc_andes.dyr");
        if !path.exists() {
            eprintln!("SKIP: wecc_andes.dyr not found at {:?}", path);
            return;
        }

        let dm = parse_file(path).expect("parse wecc failed");

        assert!(
            dm.n_generators() >= 20,
            "expected >= 20 generators, got {}",
            dm.n_generators()
        );
        assert!(
            dm.n_exciters() >= 10,
            "expected >= 10 exciters, got {}",
            dm.n_exciters()
        );

        // Verify at least one GENROU and one IEEEG1 governor
        let has_genrou = dm
            .generators
            .iter()
            .any(|g| matches!(g.model, GeneratorModel::Genrou(_)));
        assert!(has_genrou, "expected at least one GENROU");

        let has_ieeeg1 = dm
            .governors
            .iter()
            .any(|g| matches!(g.model, GovernorModel::Ieeeg1(_)));
        assert!(has_ieeeg1, "expected at least one IEEEG1");
    }
}
