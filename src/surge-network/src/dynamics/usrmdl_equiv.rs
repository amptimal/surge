// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! USRMDL equivalence table — maps common proprietary dynamic model names
//! to their standard PSS/E library equivalents supported by Surge.
//!
//! Sources: public WECC/ERCOT/NERC model documentation, GE PSLF↔PSS/E
//! mapping guides, and vendor release notes.

/// A single equivalence mapping: (proprietary_name, suggested_equivalent, notes).
pub struct Equivalence {
    /// Proprietary or non-standard model name (uppercase).
    pub source: &'static str,
    /// Suggested standard PSS/E library model supported by Surge.
    pub suggested: &'static str,
    /// Category of the model.
    pub category: ModelCategory,
    /// Human-readable notes about the mapping.
    pub notes: &'static str,
}

/// Category of a dynamic model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCategory {
    Generator,
    Exciter,
    Governor,
    Pss,
    Load,
    Facts,
    Ibr,
    Bess,
    Relay,
    Unknown,
}

impl ModelCategory {
    /// Short display label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Generator => "Generator",
            Self::Exciter => "Exciter",
            Self::Governor => "Governor",
            Self::Pss => "PSS",
            Self::Load => "Load",
            Self::Facts => "FACTS",
            Self::Ibr => "IBR",
            Self::Bess => "BESS",
            Self::Relay => "Relay",
            Self::Unknown => "Unknown",
        }
    }
}

/// Static equivalence table — common proprietary/non-standard model names
/// and their closest standard PSS/E library equivalents in Surge.
///
/// This is not exhaustive; it covers the most frequently encountered models
/// in North American RTO planning cases.
pub static EQUIVALENCE_TABLE: &[Equivalence] = &[
    // ── GE PSLF generator models ──────────────────────────────────────
    Equivalence {
        source: "GENCC",
        suggested: "GENROU",
        category: ModelCategory::Generator,
        notes: "GE PSLF combined-cycle generator → round rotor",
    },
    // ── GE PSLF exciter models ────────────────────────────────────────
    Equivalence {
        source: "EXWTGE",
        suggested: "EXST1",
        category: ModelCategory::Exciter,
        notes: "GE wind turbine exciter → static exciter",
    },
    Equivalence {
        source: "EXBAS",
        suggested: "SEXS",
        category: ModelCategory::Exciter,
        notes: "GE basic exciter → simplified excitation system",
    },
    Equivalence {
        source: "EXNLC",
        suggested: "EXAC1",
        category: ModelCategory::Exciter,
        notes: "GE non-linear AC exciter → IEEE AC1A",
    },
    Equivalence {
        source: "ESDCA",
        suggested: "ESDC2A",
        category: ModelCategory::Exciter,
        notes: "GE DC exciter variant → IEEE DC2A",
    },
    // ── GE PSLF governor models ───────────────────────────────────────
    Equivalence {
        source: "GAST_GE",
        suggested: "GAST",
        category: ModelCategory::Governor,
        notes: "GE PSLF GAST variant → standard GAST",
    },
    Equivalence {
        source: "GGOV1_GE",
        suggested: "GGOV1",
        category: ModelCategory::Governor,
        notes: "GE PSLF GGOV1 variant → standard GGOV1",
    },
    Equivalence {
        source: "PIDGOV_GE",
        suggested: "PIDGOV",
        category: ModelCategory::Governor,
        notes: "GE PSLF PID governor → standard PIDGOV",
    },
    Equivalence {
        source: "IEEEG1_GE",
        suggested: "IEEEG1",
        category: ModelCategory::Governor,
        notes: "GE PSLF IEEE G1 variant → standard IEEEG1",
    },
    Equivalence {
        source: "HYGOV_GE",
        suggested: "HYGOV",
        category: ModelCategory::Governor,
        notes: "GE PSLF hydro governor → standard HYGOV",
    },
    // ── Siemens PTI proprietary models ────────────────────────────────
    Equivalence {
        source: "WT3G",
        suggested: "WT3G2U",
        category: ModelCategory::Ibr,
        notes: "Siemens Type 3 wind → WECC generic WT3G2U",
    },
    Equivalence {
        source: "WT4G",
        suggested: "WT4G1",
        category: ModelCategory::Ibr,
        notes: "Siemens Type 4 wind → WECC generic WT4G1",
    },
    Equivalence {
        source: "CBEST2",
        suggested: "CBEST",
        category: ModelCategory::Bess,
        notes: "BESS variant → standard CBEST",
    },
    // ── WECC second-generation renewable models ───────────────────────
    Equivalence {
        source: "REGCA1",
        suggested: "REGCA",
        category: ModelCategory::Ibr,
        notes: "WECC REGCA variant → standard REGCA",
    },
    Equivalence {
        source: "REECA1",
        suggested: "REECA",
        category: ModelCategory::Ibr,
        notes: "WECC REECA variant → standard REECA",
    },
    Equivalence {
        source: "REPCA1",
        suggested: "REPCA",
        category: ModelCategory::Ibr,
        notes: "WECC REPCA variant → standard REPCA",
    },
    // ── ABB / Hitachi HVDC/FACTS ──────────────────────────────────────
    Equivalence {
        source: "HVDC_ABB",
        suggested: "VSCDCT",
        category: ModelCategory::Facts,
        notes: "ABB HVDC Light model → generic VSC-HVDC",
    },
    Equivalence {
        source: "SVC_ABB",
        suggested: "SVSMO1",
        category: ModelCategory::Facts,
        notes: "ABB SVC model → generic SVC",
    },
    Equivalence {
        source: "STATCOM_ABB",
        suggested: "SVSMO3",
        category: ModelCategory::Facts,
        notes: "ABB STATCOM model → generic STATCOM",
    },
    // ── Common USRMDL names in ERCOT/SPP/MISO cases ──────────────────
    Equivalence {
        source: "GEWTG1",
        suggested: "WT4G1",
        category: ModelCategory::Ibr,
        notes: "GE wind turbine generator → WECC Type 4",
    },
    Equivalence {
        source: "GEWTG2",
        suggested: "WT4G2",
        category: ModelCategory::Ibr,
        notes: "GE wind turbine generator v2 → WECC Type 4 v2",
    },
    Equivalence {
        source: "VESTAS_WTG",
        suggested: "WT4G1",
        category: ModelCategory::Ibr,
        notes: "Vestas wind model → WECC Type 4 generic",
    },
    Equivalence {
        source: "SGWIND",
        suggested: "WT4G1",
        category: ModelCategory::Ibr,
        notes: "Siemens-Gamesa wind → WECC Type 4 generic",
    },
    Equivalence {
        source: "GESPV",
        suggested: "PVD1",
        category: ModelCategory::Ibr,
        notes: "GE solar PV model → distributed PV",
    },
    Equivalence {
        source: "GEPVG",
        suggested: "PVDU1",
        category: ModelCategory::Ibr,
        notes: "GE PV generator → distributed PV with undervoltage",
    },
    // ── Load models ───────────────────────────────────────────────────
    Equivalence {
        source: "LDELEC",
        suggested: "CLOD",
        category: ModelCategory::Load,
        notes: "Legacy distribution load → composite load",
    },
    Equivalence {
        source: "IEEL",
        suggested: "IEELAR",
        category: ModelCategory::Load,
        notes: "IEEE electronic load → IEELAR aggregated load",
    },
];

/// Look up a suggested standard equivalent for a proprietary model name.
///
/// Returns `None` if no mapping is known. Matching is case-insensitive.
pub fn suggest_equivalent(model_name: &str) -> Option<&'static Equivalence> {
    let upper = model_name.to_uppercase();
    EQUIVALENCE_TABLE.iter().find(|e| e.source == upper)
}

/// Guess the category of an unknown model based on common naming conventions.
///
/// This uses prefix/suffix heuristics — not a definitive classification.
pub fn guess_category(model_name: &str) -> ModelCategory {
    let upper = model_name.to_uppercase();

    // Check equivalence table first
    if let Some(eq) = suggest_equivalent(&upper) {
        return eq.category;
    }

    // Heuristic prefix/suffix matching — use specific prefixes to avoid
    // false positives (e.g. "EX*" matches EXAMPLE, "REG*" matches REGULATOR).
    if upper.starts_with("GEN") || upper.contains("ROTOR") || upper.contains("MACHINE") {
        ModelCategory::Generator
    } else if upper.starts_with("EXDC")
        || upper.starts_with("EXST")
        || upper.starts_with("EXAC")
        || upper.starts_with("EXWT")
        || upper.starts_with("ESDC")
        || upper.starts_with("ESST")
        || upper.starts_with("ESAC")
        || upper.starts_with("AC8B")
        || upper.contains("EXCIT")
    {
        ModelCategory::Exciter
    } else if upper.starts_with("IEEEST")
        || upper.starts_with("PSS")
        || upper.starts_with("STAB")
        || upper.contains("STABIL")
    {
        // IEEEST is a PSS model — must be checked before the governor IEEEG/IEEES block.
        ModelCategory::Pss
    } else if upper.contains("GOV")
        || upper.starts_with("TGOV")
        || upper.starts_with("HYGOV")
        || upper.starts_with("GGOV")
        || upper.starts_with("IEEEG")
        || upper.starts_with("IEEES")
        || upper.contains("TURB")
    {
        ModelCategory::Governor
    } else if upper.starts_with("WT")
        || upper.starts_with("REGC")
        || upper.starts_with("REGF")
        || upper.starts_with("REGCO")
        || upper.starts_with("REEC")
        || upper.starts_with("REPC")
        || upper.starts_with("PVD")
        || upper.starts_with("PVDU")
        || upper.starts_with("PVGU")
        || upper.contains("WIND")
        || upper.contains("SOLAR")
    {
        ModelCategory::Ibr
    } else if upper.starts_with("CBEST")
        || upper.starts_with("CBUF")
        || upper.starts_with("CHA")
        || upper.contains("BESS")
        || upper.contains("BATT")
    {
        ModelCategory::Bess
    } else if upper.starts_with("SVC")
        || upper.starts_with("STATCOM")
        || upper.starts_with("SVSMO")
        || upper.contains("HVDC")
        || upper.starts_with("MMC")
        || upper.starts_with("CDC")
        || upper.starts_with("TCSC")
    {
        ModelCategory::Facts
    } else if upper.contains("LOAD")
        || upper.starts_with("CMPLD")
        || upper.starts_with("CLOD")
        || upper.starts_with("MOTOR")
    {
        ModelCategory::Load
    } else if upper.contains("RELAY")
        || upper.contains("PROT")
        || upper.starts_with("VTGT")
        || upper.starts_with("FRQT")
    {
        ModelCategory::Relay
    } else {
        ModelCategory::Unknown
    }
}

/// Return the full equivalence table.
pub fn equivalence_table() -> &'static [Equivalence] {
    EQUIVALENCE_TABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suggest_equivalent_found() {
        let eq = suggest_equivalent("GEWTG1").unwrap();
        assert_eq!(eq.suggested, "WT4G1");
        assert_eq!(eq.category, ModelCategory::Ibr);
    }

    #[test]
    fn test_suggest_equivalent_case_insensitive() {
        assert!(suggest_equivalent("gewtg1").is_some());
        assert!(suggest_equivalent("Gewtg1").is_some());
    }

    #[test]
    fn test_suggest_equivalent_not_found() {
        assert!(suggest_equivalent("TOTALLY_UNKNOWN_MODEL_XYZ").is_none());
    }

    #[test]
    fn test_guess_category_from_table() {
        assert_eq!(guess_category("GEWTG1"), ModelCategory::Ibr);
    }

    #[test]
    fn test_guess_category_heuristic() {
        assert_eq!(guess_category("GENXYZ"), ModelCategory::Generator);
        assert_eq!(guess_category("EXDC3A"), ModelCategory::Exciter);
        assert_eq!(guess_category("EXST5"), ModelCategory::Exciter);
        assert_eq!(guess_category("EXAC9"), ModelCategory::Exciter);
        assert_eq!(guess_category("TGOV99"), ModelCategory::Governor);
        assert_eq!(guess_category("HYGOV5"), ModelCategory::Governor);
        assert_eq!(guess_category("PSSBAR"), ModelCategory::Pss);
        assert_eq!(guess_category("IEEEST"), ModelCategory::Pss);
        assert_eq!(guess_category("WTFOO"), ModelCategory::Ibr);
        assert_eq!(guess_category("PVDU2"), ModelCategory::Ibr);
        assert_eq!(guess_category("REGCA2"), ModelCategory::Ibr);
        assert_eq!(guess_category("REGFM_X"), ModelCategory::Ibr);
        assert_eq!(guess_category("REGCO2"), ModelCategory::Ibr);
        assert_eq!(guess_category("CBEST3"), ModelCategory::Bess);
        assert_eq!(guess_category("CBUFD"), ModelCategory::Bess);
        assert_eq!(guess_category("CHAAUT"), ModelCategory::Bess);
        assert_eq!(guess_category("SVCFOO"), ModelCategory::Facts);
        assert_eq!(guess_category("SVSMO5"), ModelCategory::Facts);
        assert_eq!(guess_category("MOTORX"), ModelCategory::Load);
        // Previously false-positive prefixes should now be Unknown
        assert_eq!(guess_category("ACLINE"), ModelCategory::Unknown);
        assert_eq!(guess_category("STATION1"), ModelCategory::Unknown);
        assert_eq!(guess_category("XYZABC"), ModelCategory::Unknown);
    }

    #[test]
    fn test_guess_category_false_positive_regressions() {
        // Verify that previously-broad prefixes no longer cause false positives.
        // EX* no longer matches non-exciter models:
        assert_eq!(guess_category("EXPORT_MODULE"), ModelCategory::Unknown);
        // REG* no longer matches non-IBR models:
        assert_eq!(guess_category("REGULATOR"), ModelCategory::Unknown);
        // CB* no longer matches non-BESS models (but RELAY substring → Relay):
        assert_eq!(guess_category("CB_RELAY"), ModelCategory::Relay);
        // Pure CB prefix without BESS/BATT/known suffix → Unknown:
        assert_eq!(guess_category("CB_SWITCH"), ModelCategory::Unknown);
        // RELAY_DEV correctly classified as Relay:
        assert_eq!(guess_category("RELAY_DEV"), ModelCategory::Relay);
        // GOV substring in any name is legitimately a governor hint:
        assert_eq!(guess_category("EXAMPLE_GOV"), ModelCategory::Governor);
    }

    #[test]
    fn test_equivalence_table_not_empty() {
        assert!(!equivalence_table().is_empty());
    }
}
