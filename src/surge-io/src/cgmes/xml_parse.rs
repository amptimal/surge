// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use super::error::CgmesError;
use super::types::{CimObj, CimVal, MAX_CIM_OBJECTS, ObjMap};

// ---------------------------------------------------------------------------
// Stage 1 — RDF/XML → raw object store
// ---------------------------------------------------------------------------

/// Stream a CGMES XML file into the shared object map.
/// Objects in `content` overwrite same-mRID attributes already in `objects`.
pub(crate) fn collect_objects(content: &str, objects: &mut ObjMap) -> Result<(), CgmesError> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    // Namespace prefixes detected from the root element's xmlns: declarations
    let mut cim_pfx = String::from("cim");
    let mut rdf_pfx = String::from("rdf");
    // Precomputed derived strings (initialized from defaults, updated once after
    // the root element reveals the actual namespace prefixes).
    let mut cim_colon = String::from("cim:");
    let mut res_key = String::from("rdf:resource");
    let mut id_key = String::from("rdf:ID");
    let mut about_key = String::from("rdf:about");

    // Parsing state
    let mut current_id: Option<String> = None;
    let mut current_attr_key: Option<String> = None; // pending key for text capture
    let mut depth: usize = 0;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            // ----------------------------------------------------------------
            // Opening tags (and self-closing with Event::Empty)
            // ----------------------------------------------------------------
            Ok(Event::Start(ref e)) => {
                depth += 1;
                let raw = str_from_bytes(e.name().as_ref());

                if depth == 1 {
                    // Root rdf:RDF — sniff namespace prefixes
                    for attr in e.attributes().flatten() {
                        let k = str_from_bytes(attr.key.as_ref());
                        let v = str_from_bytes(&attr.value);
                        if let Some(pfx) = k.strip_prefix("xmlns:") {
                            if is_cim_ns(&v) {
                                cim_pfx = pfx.to_string();
                            } else if is_rdf_ns(&v) {
                                rdf_pfx = pfx.to_string();
                            }
                        }
                    }
                    // Recompute derived strings now that we know the actual prefixes.
                    // This is done exactly once per file; all subsequent element handling
                    // uses these pre-built strings instead of calling format!() per element.
                    cim_colon = format!("{}:", cim_pfx);
                    res_key = format!("{}:resource", rdf_pfx);
                    id_key = format!("{}:ID", rdf_pfx);
                    about_key = format!("{}:about", rdf_pfx);
                    continue;
                }

                if depth == 2 {
                    // Top-level CIM object opening tag
                    if let Some(cls) = raw.strip_prefix(cim_colon.as_str()).map(|s| s.to_string()) {
                        let mrid = extract_mrid(e.attributes(), &id_key, &about_key);
                        if let Some(id) = mrid {
                            // CIM-03: enforce object count limit before inserting new entries.
                            if !objects.contains_key(&id) && objects.len() >= MAX_CIM_OBJECTS {
                                return Err(CgmesError::TooManyObjects(format!(
                                    "exceeded {MAX_CIM_OBJECTS} CIM objects while parsing; \
                                     file may be malformed or adversarial"
                                )));
                            }
                            let obj = objects
                                .entry(id.clone())
                                .or_insert_with(|| CimObj::new(&cls));
                            obj.class = cls;
                            current_id = Some(id);
                        }
                    }
                    continue;
                }

                if depth == 3 {
                    // Attribute child element (text value or rdf:resource ref)
                    if let Some(ref parent_id) = current_id {
                        let local = raw
                            .strip_prefix(cim_colon.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or(raw.clone());
                        let attr_key = simplify_attr_key(&local);
                        let mut ref_val: Option<String> = None;
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == res_key.as_bytes() {
                                let v = str_from_bytes(&attr.value);
                                ref_val = Some(strip_hash_prefix(v));
                            }
                        }
                        if let Some(r) = ref_val {
                            if let Some(obj) = objects.get_mut(parent_id) {
                                obj.attrs.insert(attr_key, CimVal::Ref(r));
                            }
                            current_attr_key = None;
                        } else {
                            current_attr_key = Some(attr_key);
                        }
                    }
                    continue;
                }
            }

            Ok(Event::Empty(ref e)) => {
                // NOTE: Event::Empty does NOT increment depth.
                // depth==1 → top-level empty CIM object (XML nesting depth 2)
                // depth==2 → attribute child with rdf:resource (XML nesting depth 3)
                let raw = str_from_bytes(e.name().as_ref());

                if depth == 1 {
                    if let Some(cls) = raw.strip_prefix(cim_colon.as_str()).map(|s| s.to_string()) {
                        let mrid = extract_mrid(e.attributes(), &id_key, &about_key);
                        if let Some(id) = mrid {
                            // CIM-03: enforce object count limit before inserting new entries.
                            if !objects.contains_key(&id) && objects.len() >= MAX_CIM_OBJECTS {
                                return Err(CgmesError::TooManyObjects(format!(
                                    "exceeded {MAX_CIM_OBJECTS} CIM objects while parsing; \
                                     file may be malformed or adversarial"
                                )));
                            }
                            let obj = objects.entry(id).or_insert_with(|| CimObj::new(&cls));
                            obj.class = cls;
                        }
                    }
                    continue;
                }

                if depth == 2 {
                    if let Some(ref parent_id) = current_id {
                        let local = raw
                            .strip_prefix(cim_colon.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or(raw.clone());
                        let attr_key = simplify_attr_key(&local);
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == res_key.as_bytes() {
                                let v = str_from_bytes(&attr.value);
                                let r = strip_hash_prefix(v);
                                if let Some(obj) = objects.get_mut(parent_id) {
                                    obj.attrs.insert(attr_key.clone(), CimVal::Ref(r));
                                }
                            }
                        }
                    }
                    continue;
                }
            }

            Ok(Event::Text(ref e)) => {
                if depth == 3
                    && let (Some(ref parent_id), Some(ref key)) =
                        (current_id.clone(), current_attr_key.clone())
                {
                    let text = e.unescape().unwrap_or_default().trim().to_string();
                    if !text.is_empty()
                        && let Some(obj) = objects.get_mut(parent_id)
                    {
                        obj.attrs.insert(key.clone(), CimVal::Text(text));
                    }
                }
            }

            Ok(Event::End(_)) => {
                if depth == 3 {
                    current_attr_key = None;
                }
                if depth == 2 {
                    current_id = None;
                }
                depth = depth.saturating_sub(1);
            }

            Ok(Event::Eof) => break,
            Err(e) => return Err(CgmesError::Xml(e)),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// XML helpers
// ---------------------------------------------------------------------------

pub(crate) fn str_from_bytes(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn is_cim_ns(ns: &str) -> bool {
    // Match standard IEC CIM class namespaces (CIM16, CIM17, CIM100, etc.)
    // Exclude ENTSO-E SchemaExtension (/CIM/SchemaExtension) and
    // ModelDescription (TC57/61970-552/ModelDescription) which are NOT class definition URIs.
    ns.contains("CIM-schema-cim") || (ns.contains("/CIM") && !ns.contains("SchemaExtension"))
}

fn is_rdf_ns(ns: &str) -> bool {
    ns.contains("rdf-syntax") || ns.contains("1999/02/22-rdf")
}

/// Strip leading `#` from rdf:resource / rdf:ID attribute values.
pub(crate) fn strip_hash_prefix(s: String) -> String {
    if let Some(stripped) = s.strip_prefix('#') {
        stripped.to_string()
    } else {
        s
    }
}

/// Extract mRID from rdf:ID or rdf:about attribute.
///
/// Takes precomputed `id_key`/`about_key` strings (e.g. "rdf:ID", "rdf:about") to
/// avoid re-allocating them on every call.
fn extract_mrid(
    attrs: quick_xml::events::attributes::Attributes,
    id_key: &str,
    about_key: &str,
) -> Option<String> {
    let id_bytes = id_key.as_bytes();
    let about_bytes = about_key.as_bytes();
    let mut mrid = None;
    for attr in attrs.flatten() {
        let k = attr.key.as_ref();
        if k == id_bytes {
            let v = str_from_bytes(&attr.value);
            mrid = Some(strip_hash_prefix(v));
            break;
        }
        if k == about_bytes && mrid.is_none() {
            let v = str_from_bytes(&attr.value);
            mrid = Some(strip_hash_prefix(v));
        }
    }
    mrid
}

/// Reduce "ClassName.AttrName" → "AttrName" (strip everything up to and
/// including the last dot). This collapses parent-class attr names like
/// `RotatingMachine.p` → `p`, `ConductingEquipment.BaseVoltage` → `BaseVoltage`.
fn simplify_attr_key(raw: &str) -> String {
    raw.rsplit('.').next().unwrap_or(raw).to_string()
}
