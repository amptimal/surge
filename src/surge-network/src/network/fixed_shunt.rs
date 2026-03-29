// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Fixed shunt equipment model.

use serde::{Deserialize, Serialize};

/// Classification of fixed shunt equipment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ShuntType {
    /// Shunt capacitor bank (positive susceptance).
    #[default]
    Capacitor,
    /// Shunt reactor (negative susceptance).
    Reactor,
    /// Harmonic filter (capacitive at fundamental frequency).
    HarmonicFilter,
}

/// A fixed shunt device at a bus.
///
/// Preserves equipment identity that is otherwise lost when baked into Bus.shunt_susceptance_mvar.
/// PSS/E "FIXED SHUNT DATA" section populates these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedShunt {
    /// Bus number.
    pub bus: u32,
    /// Shunt identifier.
    pub id: String,
    /// Equipment classification.
    pub shunt_type: ShuntType,
    /// Conductance (MW at V = 1.0 pu).
    pub g_mw: f64,
    /// Susceptance (MVAr at V = 1.0 pu). Positive = cap, negative = reactor.
    pub b_mvar: f64,
    /// In-service status.
    pub in_service: bool,
    /// Rated voltage (kV).
    pub rated_kv: Option<f64>,
    /// Rated reactive power (MVAr).
    pub rated_mvar: Option<f64>,
}
