// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Raw CIM object store
// ---------------------------------------------------------------------------

/// A CIM attribute value: plain text or cross-reference to another mRID.
#[derive(Debug, Clone)]
pub(crate) enum CimVal {
    Text(String),
    Ref(String),
}

impl CimVal {
    pub(crate) fn as_text(&self) -> Option<&str> {
        if let CimVal::Text(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub(crate) fn as_ref(&self) -> Option<&str> {
        if let CimVal::Ref(s) = self {
            Some(s)
        } else {
            None
        }
    }
}

/// A single CIM object, keyed by its mRID.
#[derive(Debug, Clone)]
pub(crate) struct CimObj {
    pub(crate) class: String,
    pub(crate) attrs: HashMap<String, CimVal>,
}

impl CimObj {
    pub(crate) fn new(class: &str) -> Self {
        CimObj {
            class: class.to_string(),
            attrs: HashMap::new(),
        }
    }
    pub(crate) fn get_text(&self, key: &str) -> Option<&str> {
        self.attrs.get(key)?.as_text()
    }
    pub(crate) fn get_ref(&self, key: &str) -> Option<&str> {
        self.attrs.get(key)?.as_ref()
    }
    pub(crate) fn parse_f64(&self, key: &str) -> Option<f64> {
        self.get_text(key)?.parse().ok()
    }
}

pub(crate) type ObjMap = HashMap<String, CimObj>;

/// CIM-03: hard cap on the number of CIM objects that may be loaded from a single
/// set of profile files.  Prevents >1 GB heap allocations from adversarial inputs.
/// 5 million objects covers the largest known real-world CGMES models (~500k objects)
/// with a 10× safety margin while still catching runaway files.
pub(crate) const MAX_CIM_OBJECTS: usize = 5_000_000;

/// SynchronousMachine mRID → `(bus_number, machine_id)` lookup table (DY profile use).
pub(crate) type SmBusMap = HashMap<String, (u32, String)>;
