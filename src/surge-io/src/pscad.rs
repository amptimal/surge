// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSCAD .pscx format reader.
//!
//! PSCAD project files (`.pscx`) are XML-based archives used by Manitoba
//! Hydro's PSCAD electromagnetic transient (EMT) simulator.  This module
//! provides a best-effort reader that extracts network topology and initial
//! conditions for EMT-to-phasor model migration workflows.
//!
//! ## Supported element types
//! | PSCAD type         | Surge mapping           |
//! |--------------------|-------------------------|
//! | `BUS`              | `Bus` (PQ, base kV)     |
//! | `TLine`            | `Branch` (line R/X/B)   |
//! | `Transformer`      | `Branch` (transformer)  |
//! | `3Phase_Source`    | `Generator` + slack bus |
//!
//! ## File format
//! PSCX is a ZIP-compressed archive containing one or more `.pscx` XML
//! master files.  Because decompression requires an archive library not
//! currently in the workspace dependencies, this reader accepts the raw XML
//! content as a `&str`.  Callers can extract the XML from the archive with
//! any ZIP library and pass it here.
//!
//! The expected XML structure is:
//! ```xml
//! <project name="IEEE13" frequency="60">
//!   <components>
//!     <component type="BUS" name="BUS_A">
//!       <param name="BaseKV">13.8</param>
//!     </component>
//!     <component type="TLine" name="LINE_AB">
//!       <param name="R1">0.01</param>
//!       <param name="X1">0.1</param>
//!       <param name="from">BUS_A</param>
//!       <param name="to">BUS_B</param>
//!     </component>
//!     <component type="3Phase_Source" name="SOURCE1">
//!       <param name="bus">BUS_A</param>
//!       <param name="V">13.8</param>
//!       <param name="P">100.0</param>
//!     </component>
//!   </components>
//! </project>
//! ```

use std::collections::HashMap;

use surge_network::Network;
use surge_network::network::{Branch, BranchType, Bus, BusType, Generator};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum PscadError {
    #[error("XML parse error: {0}")]
    Xml(String),
    #[error("missing required attribute '{0}' on element")]
    MissingAttr(String),
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single PSCAD component extracted from the project file.
#[derive(Debug, Clone)]
pub struct PscadComponent {
    /// PSCAD element type, e.g. `"BUS"`, `"TLine"`, `"Transformer"`, `"3Phase_Source"`.
    pub comp_type: String,
    /// User-assigned name/label in the schematic.
    pub name: String,
    /// All `<param>` name→value pairs on this component.
    pub parameters: HashMap<String, String>,
    /// Names of connected nodes (from/to bus names, or `bus` param).
    pub connections: Vec<String>,
}

impl PscadComponent {
    fn get_param_f64(&self, key: &str) -> Option<f64> {
        self.parameters.get(key)?.parse::<f64>().ok()
    }

    fn get_param_str(&self, key: &str) -> Option<&str> {
        self.parameters.get(key).map(String::as_str)
    }
}

/// Parsed PSCAD project.
pub struct PscadProject {
    /// All schematic components (buses, lines, sources, transformers, …).
    pub components: Vec<PscadComponent>,
    /// Project / study name from the `<project name="…">` attribute.
    pub study_name: String,
    /// System frequency from `<project frequency="…">` (Hz).  Defaults to 60.
    pub frequency_hz: f64,
}

/// Result of converting a `PscadProject` to a Surge `Network`.
pub struct PscadConversionResult {
    /// Assembled Surge network (buses, branches, generators).
    pub network: Network,
    /// Components that could not be mapped to a Surge equivalent.
    pub unmapped_components: Vec<PscadComponent>,
    /// Non-fatal warnings encountered during conversion.
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Parser helpers
// ---------------------------------------------------------------------------

/// Extract a named attribute value from a tag line.
///
/// Handles both `attr="value"` and `attr='value'` quoting styles.
fn attr_value<'a>(line: &'a str, attr: &str) -> Option<&'a str> {
    for sep in &[format!("{attr}=\""), format!("{attr}='")] {
        if let Some(pos) = line.find(sep.as_str()) {
            let start = pos + sep.len();
            let rest = &line[start..];
            let quote_char = if sep.ends_with('"') { '"' } else { '\'' };
            if let Some(end) = rest.find(quote_char) {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

/// Extract the text content between open and close tags on the same line.
fn text_content<'a>(line: &'a str, open: &str, close: &str) -> Option<&'a str> {
    if let (Some(s), Some(e)) = (line.find(open), line.find(close)) {
        let start = s + open.len();
        if start <= e {
            return Some(line[start..e].trim());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Main parser
// ---------------------------------------------------------------------------

/// Parse a PSCAD `.pscx` XML file content and return a `PscadProject`.
///
/// The input should be raw XML text (not ZIP-compressed).
pub fn parse_pscx(xml_content: &str) -> Result<PscadProject, PscadError> {
    let mut study_name = String::from("unnamed");
    let mut frequency_hz = 60.0_f64;
    let mut components: Vec<PscadComponent> = Vec::new();

    // Parser state
    let mut current_comp: Option<PscadComponent> = None;
    let mut in_components = false;

    for line in xml_content.lines() {
        let trimmed = line.trim();

        // <project …> root element
        if trimmed.starts_with("<project") {
            if let Some(name) = attr_value(trimmed, "name") {
                study_name = name.to_string();
            }
            if let Some(freq) = attr_value(trimmed, "frequency")
                && let Ok(f) = freq.parse::<f64>()
            {
                frequency_hz = f;
            }
            continue;
        }

        // <components> section
        if trimmed == "<components>" {
            in_components = true;
            continue;
        }
        if trimmed == "</components>" {
            in_components = false;
            // Flush any open component
            if let Some(comp) = current_comp.take() {
                components.push(comp);
            }
            continue;
        }

        if !in_components {
            continue;
        }

        // Opening <component type="…" name="…">
        if trimmed.starts_with("<component") && !trimmed.starts_with("</component") {
            // Flush previous
            if let Some(comp) = current_comp.take() {
                components.push(comp);
            }

            let comp_type = attr_value(trimmed, "type")
                .map(str::to_string)
                .unwrap_or_default();
            let name = attr_value(trimmed, "name")
                .map(str::to_string)
                .unwrap_or_default();

            current_comp = Some(PscadComponent {
                comp_type,
                name,
                parameters: HashMap::new(),
                connections: Vec::new(),
            });
            continue;
        }

        // Closing </component>
        if trimmed == "</component>" {
            if let Some(mut comp) = current_comp.take() {
                // Populate connections from well-known params
                for key in &["from", "to", "bus", "node", "node1", "node2"] {
                    if let Some(val) = comp.parameters.get(*key) {
                        let v = val.clone();
                        if !comp.connections.contains(&v) {
                            comp.connections.push(v);
                        }
                    }
                }
                components.push(comp);
            }
            continue;
        }

        // <param name="KEY">VALUE</param>
        if trimmed.starts_with("<param")
            && let Some(comp) = &mut current_comp
            && let Some(param_name) = attr_value(trimmed, "name")
        {
            // Value can be in text content or in a `value` attribute
            let value = text_content(trimmed, ">", "</param>")
                .or_else(|| attr_value(trimmed, "value"))
                .unwrap_or("")
                .to_string();
            comp.parameters.insert(param_name.to_string(), value);
        }
    }

    // Flush any trailing component not closed by </component>
    if let Some(comp) = current_comp.take() {
        components.push(comp);
    }

    Ok(PscadProject {
        components,
        study_name,
        frequency_hz,
    })
}

// ---------------------------------------------------------------------------
// Network conversion
// ---------------------------------------------------------------------------

/// Convert a parsed `PscadProject` to a Surge `Network`.
///
/// Mapping rules:
/// - `BUS` → `Bus` (PQ by default; base_kv from `BaseKV` param)
/// - `TLine` → `Branch` (line; r/x/b from R1/X1/B1 params, pu on 100 MVA)
/// - `Transformer` → `Branch` (transformer; r/x from R/X params)
/// - `3Phase_Source` → `Generator` + promotes connected bus to Slack
pub fn pscad_to_network(project: &PscadProject) -> PscadConversionResult {
    let mut network = Network::new(&project.study_name);
    network.base_mva = 100.0;

    let mut unmapped: Vec<PscadComponent> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // --- First pass: collect bus names and assign internal IDs ---
    let mut bus_name_to_id: HashMap<String, u32> = HashMap::new();
    let mut next_bus_id: u32 = 1;

    // Register buses declared explicitly as BUS components
    for comp in &project.components {
        if comp.comp_type == "BUS" && !bus_name_to_id.contains_key(&comp.name) {
            bus_name_to_id.insert(comp.name.clone(), next_bus_id);
            next_bus_id += 1;
        }
    }

    // Register bus names referenced by lines/sources that were not declared
    for comp in &project.components {
        for conn in &comp.connections {
            if !bus_name_to_id.contains_key(conn) && !conn.is_empty() {
                bus_name_to_id.insert(conn.clone(), next_bus_id);
                next_bus_id += 1;
            }
        }
    }

    // Build Bus objects
    for comp in &project.components {
        if comp.comp_type == "BUS" {
            let bus_id = bus_name_to_id[&comp.name];
            let base_kv = comp.get_param_f64("BaseKV").unwrap_or(1.0);
            let bus = Bus::new(bus_id, BusType::PQ, base_kv);
            network.buses.push(bus);
        }
    }

    // Ensure buses referenced but not declared also exist
    for (name, &id) in &bus_name_to_id {
        if !network.buses.iter().any(|b| b.number == id) {
            let bus = Bus::new(id, BusType::PQ, 1.0);
            network.buses.push(bus);
            warnings.push(format!(
                "Bus '{}' (id={}) inferred from connection — no BUS component found",
                name, id
            ));
        }
    }

    // Sort buses by id for determinism
    network.buses.sort_by_key(|b| b.number);

    // --- Second pass: branches and generators ---
    let _next_branch = 0usize;

    for comp in &project.components {
        match comp.comp_type.as_str() {
            "BUS" => {
                // Already handled above
            }

            "TLine" => {
                let from_name = comp.get_param_str("from").unwrap_or("");
                let to_name = comp.get_param_str("to").unwrap_or("");
                if from_name.is_empty() || to_name.is_empty() {
                    warnings.push(format!("TLine '{}' missing from/to — skipped", comp.name));
                    continue;
                }
                let from_id = match bus_name_to_id.get(from_name) {
                    Some(&id) => id,
                    None => {
                        warnings.push(format!(
                            "TLine '{}': unknown bus '{}' — skipped",
                            comp.name, from_name
                        ));
                        continue;
                    }
                };
                let to_id = match bus_name_to_id.get(to_name) {
                    Some(&id) => id,
                    None => {
                        warnings.push(format!(
                            "TLine '{}': unknown bus '{}' — skipped",
                            comp.name, to_name
                        ));
                        continue;
                    }
                };
                let r = comp.get_param_f64("R1").unwrap_or(0.01);
                let x = comp.get_param_f64("X1").unwrap_or(0.1);
                let b = comp.get_param_f64("B1").unwrap_or(0.0);
                network
                    .branches
                    .push(Branch::new_line(from_id, to_id, r, x, b));
            }

            "Transformer" => {
                let from_name = comp
                    .get_param_str("from")
                    .or_else(|| comp.get_param_str("bus1"))
                    .unwrap_or("");
                let to_name = comp
                    .get_param_str("to")
                    .or_else(|| comp.get_param_str("bus2"))
                    .unwrap_or("");
                if from_name.is_empty() || to_name.is_empty() {
                    warnings.push(format!(
                        "Transformer '{}' missing from/to — skipped",
                        comp.name
                    ));
                    continue;
                }
                let from_id = match bus_name_to_id.get(from_name) {
                    Some(&id) => id,
                    None => {
                        warnings.push(format!(
                            "Transformer '{}': unknown bus '{}' — skipped",
                            comp.name, from_name
                        ));
                        continue;
                    }
                };
                let to_id = match bus_name_to_id.get(to_name) {
                    Some(&id) => id,
                    None => {
                        warnings.push(format!(
                            "Transformer '{}': unknown bus '{}' — skipped",
                            comp.name, to_name
                        ));
                        continue;
                    }
                };
                let r = comp.get_param_f64("R").unwrap_or(0.005);
                let x = comp.get_param_f64("X").unwrap_or(0.05);
                let tap = comp.get_param_f64("Tap").unwrap_or(1.0);
                let mut branch = Branch::new_line(from_id, to_id, r, x, 0.0);
                branch.tap = tap;
                branch.branch_type = BranchType::Transformer;
                network.branches.push(branch);
            }

            "3Phase_Source" => {
                let bus_name = comp.get_param_str("bus").unwrap_or("");
                if bus_name.is_empty() {
                    warnings.push(format!(
                        "3Phase_Source '{}' has no 'bus' param — skipped",
                        comp.name
                    ));
                    continue;
                }
                let bus_id = match bus_name_to_id.get(bus_name) {
                    Some(&id) => id,
                    None => {
                        warnings.push(format!(
                            "3Phase_Source '{}': unknown bus '{}' — skipped",
                            comp.name, bus_name
                        ));
                        continue;
                    }
                };
                // Promote connected bus to Slack
                if let Some(bus) = network.buses.iter_mut().find(|b| b.number == bus_id) {
                    bus.bus_type = BusType::Slack;
                }
                // Set generator injection
                let pg = comp.get_param_f64("P").unwrap_or(0.0) / network.base_mva;
                let vm = comp
                    .get_param_f64("V")
                    .and_then(|v| {
                        // V may be in kV; if it matches BaseKV roughly, treat as 1.0 pu
                        let base_kv = network
                            .buses
                            .iter()
                            .find(|b| b.number == bus_id)
                            .map(|b| b.base_kv)
                            .unwrap_or(1.0);
                        if base_kv > 0.0 {
                            Some(v / base_kv)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(1.0);
                let mut generator = Generator::new(bus_id, pg * network.base_mva, vm);
                generator.p = pg * network.base_mva;
                network.generators.push(generator);
            }

            _ => {
                unmapped.push(comp.clone());
            }
        }
    }

    PscadConversionResult {
        network,
        unmapped_components: unmapped,
        warnings,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_PSCX_2BUS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project name="test_2bus" frequency="60">
  <components>
    <component type="BUS" name="BUS_A">
      <param name="BaseKV">13.8</param>
    </component>
    <component type="TLine" name="LINE_AB">
      <param name="R1">0.01</param>
      <param name="X1">0.1</param>
      <param name="from">BUS_A</param>
      <param name="to">BUS_B</param>
    </component>
  </components>
</project>"#;

    const PSCX_3BUS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project name="three_bus" frequency="60">
  <components>
    <component type="BUS" name="BUS_1">
      <param name="BaseKV">138.0</param>
    </component>
    <component type="BUS" name="BUS_2">
      <param name="BaseKV">138.0</param>
    </component>
    <component type="BUS" name="BUS_3">
      <param name="BaseKV">138.0</param>
    </component>
    <component type="TLine" name="LINE_12">
      <param name="R1">0.02</param>
      <param name="X1">0.06</param>
      <param name="from">BUS_1</param>
      <param name="to">BUS_2</param>
    </component>
    <component type="3Phase_Source" name="SLACK_SRC">
      <param name="bus">BUS_1</param>
      <param name="V">138.0</param>
      <param name="P">100.0</param>
    </component>
  </components>
</project>"#;

    const PSCX_50HZ: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project name="eu_grid" frequency="50">
  <components>
  </components>
</project>"#;

    // -----------------------------------------------------------------------
    // PLAN-084 / P5-050 — test_pscx_parse_minimal
    // -----------------------------------------------------------------------

    /// Parsing the minimal 2-bus XML should yield at least 2 components
    /// (the BUS and the TLine).
    #[test]
    fn test_pscx_parse_minimal() {
        let project = parse_pscx(MINIMAL_PSCX_2BUS).expect("parse_pscx should succeed");
        assert!(
            project.components.len() >= 2,
            "Expected at least 2 components, got {}",
            project.components.len()
        );
        assert_eq!(project.study_name, "test_2bus");
        assert!(
            project.components.iter().any(|c| c.comp_type == "BUS"),
            "Expected at least one BUS component"
        );
        assert!(
            project.components.iter().any(|c| c.comp_type == "TLine"),
            "Expected at least one TLine component"
        );
    }

    // -----------------------------------------------------------------------
    // test_pscad_to_network_buses
    // -----------------------------------------------------------------------

    /// A 3-bus PSCX should produce a network with exactly 3 buses.
    #[test]
    fn test_pscad_to_network_buses() {
        let project = parse_pscx(PSCX_3BUS).expect("parse_pscx should succeed");
        let result = pscad_to_network(&project);
        assert_eq!(
            result.network.n_buses(),
            3,
            "Expected 3 buses, got {}",
            result.network.n_buses()
        );
        // Verify no critical conversion error occurred
        assert!(
            result.warnings.iter().all(|w| !w.contains("unknown bus")),
            "Unexpected 'unknown bus' warnings: {:?}",
            result.warnings
        );
    }

    // -----------------------------------------------------------------------
    // test_pscx_frequency
    // -----------------------------------------------------------------------

    /// The frequency attribute on <project> should be parsed correctly.
    #[test]
    fn test_pscx_frequency() {
        let project = parse_pscx(PSCX_50HZ).expect("parse_pscx should succeed");
        assert!(
            (project.frequency_hz - 50.0).abs() < 1e-10,
            "Expected 50 Hz, got {}",
            project.frequency_hz
        );
    }

    // -----------------------------------------------------------------------
    // Additional: source → slack promotion
    // -----------------------------------------------------------------------

    #[test]
    fn test_pscad_source_promotes_slack() {
        let project = parse_pscx(PSCX_3BUS).unwrap();
        let result = pscad_to_network(&project);
        let slack_count = result
            .network
            .buses
            .iter()
            .filter(|b| b.bus_type == BusType::Slack)
            .count();
        assert_eq!(
            slack_count, 1,
            "Expected exactly 1 slack bus, got {slack_count}"
        );
    }

    // -----------------------------------------------------------------------
    // Additional: unmapped components
    // -----------------------------------------------------------------------

    #[test]
    fn test_pscad_unmapped_components() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<project name="misc" frequency="60">
  <components>
    <component type="BUS" name="BUS_A">
      <param name="BaseKV">1.0</param>
    </component>
    <component type="SVC" name="SVC_1">
      <param name="bus">BUS_A</param>
    </component>
  </components>
</project>"#;
        let project = parse_pscx(xml).unwrap();
        let result = pscad_to_network(&project);
        assert!(
            result
                .unmapped_components
                .iter()
                .any(|c| c.comp_type == "SVC"),
            "Expected SVC in unmapped_components"
        );
    }
}
