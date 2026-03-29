// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use serde::{Deserialize, Serialize};

/// Voltage droop control record (PSS/E v36).
///
/// Defines a voltage droop relationship between a regulating device and a
/// regulated bus. The device adjusts reactive output to hold the regulated bus
/// voltage within \[vmin, vmax\] with slope `vdrp` (pu V / pu Q).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoltageDroopControl {
    /// Bus number of the controlling device.
    pub bus: u32,
    /// Machine/device identifier.
    pub device_id: String,
    /// Device type: 1=generator, 2=two-terminal DC converter,
    /// 3=VSC DC converter, 4=FACTS device.
    pub device_type: u32,
    /// Bus number where voltage is regulated.
    pub regulated_bus: u32,
    /// Voltage droop slope (pu).
    pub vdrp: f64,
    /// Maximum voltage at regulated bus (pu).
    pub vmax: f64,
    /// Minimum voltage at regulated bus (pu).
    pub vmin: f64,
}
