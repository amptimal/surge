// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Explicit fixed P/Q injections tied to physical equipment.

use serde::{Deserialize, Serialize};

/// High-level classification for fixed bus injections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PowerInjectionKind {
    /// Boundary or external-network injection.
    Boundary,
    /// Equivalent-network injection.
    Equivalent,
    /// Converter-backed AC injection.
    Converter,
    /// Reactive compensation injection that is not voltage-regulating.
    Compensator,
    /// Fallback classification when no narrower category fits.
    #[default]
    Other,
}

/// A fixed P/Q injection at a bus.
///
/// Positive real/reactive values mean injection into the network. Bus demand is
/// derived by subtracting these injections from the hosting bus aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerInjection {
    /// Bus number where the equipment is connected.
    pub bus: u32,
    /// Stable equipment identifier.
    pub id: String,
    /// Injection classification.
    pub kind: PowerInjectionKind,
    /// Real power injection in MW. Positive = inject into the network.
    pub active_power_injection_mw: f64,
    /// Reactive power injection in MVAr. Positive = inject into the network.
    pub reactive_power_injection_mvar: f64,
    /// In-service status.
    pub in_service: bool,
}

impl Default for PowerInjection {
    fn default() -> Self {
        Self {
            bus: 0,
            id: String::new(),
            kind: PowerInjectionKind::Other,
            active_power_injection_mw: 0.0,
            reactive_power_injection_mvar: 0.0,
            in_service: true,
        }
    }
}

impl PowerInjection {
    /// Construct a fixed injection with positive values meaning net injection.
    pub fn new(
        bus: u32,
        active_power_injection_mw: f64,
        reactive_power_injection_mvar: f64,
    ) -> Self {
        Self {
            bus,
            active_power_injection_mw,
            reactive_power_injection_mvar,
            ..Default::default()
        }
    }
}
