// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Protection equipment data types (IEC 61970-302 Protection package).
//!
//! Stores relay settings, auto-reclose sequences, and synchrocheck parameters
//! parsed from CGMES protection profile data. These are informational/metadata
//! types — they do not affect power flow computation but are preserved for
//! downstream protection coordination tools (e.g., surge-fault TCC analysis).

use serde::{Deserialize, Serialize};

/// Overcurrent relay settings (IEC 61970-302 CurrentRelay).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CurrentRelaySettings {
    /// CIM mRID.
    pub mrid: String,
    /// Relay name.
    pub name: String,
    /// Phase pickup current (A).
    pub phase_pickup_a: Option<f64>,
    /// Ground pickup current (A).
    pub ground_pickup_a: Option<f64>,
    /// Negative-sequence pickup (A).
    pub neg_seq_pickup_a: Option<f64>,
    /// Phase time dial (s).
    pub phase_time_dial_s: Option<f64>,
    /// Ground time dial (s).
    pub ground_time_dial_s: Option<f64>,
    /// Neg-seq time dial (s).
    pub neg_seq_time_dial_s: Option<f64>,
    /// True = inverse-time characteristic, false = definite-time.
    pub inverse_time: bool,
    /// Directional element.
    pub directional: bool,
    /// Bus where CTs are located.
    pub bus: Option<u32>,
    /// Protected switch/breaker mRID.
    pub protected_switch_mrid: Option<String>,
}

/// Distance relay settings (IEC 61970-302 DistanceRelay / zones).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DistanceRelaySettings {
    /// CIM mRID.
    pub mrid: String,
    /// Relay name.
    pub name: String,
    /// Forward reach (Ohm).
    pub forward_reach_ohm: Option<f64>,
    /// Forward blind reach (Ohm).
    pub forward_blind_ohm: Option<f64>,
    /// Backward/reverse reach (Ohm).
    pub backward_reach_ohm: Option<f64>,
    /// Backward blind reach (Ohm).
    pub backward_blind_ohm: Option<f64>,
    /// MHO characteristic angle (degrees).
    pub mho_angle_deg: Option<f64>,
    /// Z0/Z1 compensation ratio.
    pub zero_seq_rx_ratio: Option<f64>,
    /// Zero-sequence forward reach (Ohm).
    pub zero_seq_reach_ohm: Option<f64>,
    /// Bus where CTs/VTs are located.
    pub bus: Option<u32>,
    /// Protected switch mRID.
    pub protected_switch_mrid: Option<String>,
}

/// A single auto-reclose shot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecloseShot {
    /// Shot number (1-based).
    pub step: u32,
    /// Reclose delay (s).
    pub delay_s: f64,
}

/// Auto-reclose sequence for a breaker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecloseSequenceData {
    /// Protected switch/breaker mRID.
    pub protected_switch_mrid: String,
    /// Ordered reclose shots.
    pub shots: Vec<RecloseShot>,
}

/// Synchrocheck relay settings (IEC 61970-302 SynchrocheckRelay).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SynchrocheckSettings {
    /// CIM mRID.
    pub mrid: String,
    /// Relay name.
    pub name: String,
    /// Maximum angle difference (degrees).
    pub max_angle_diff_deg: Option<f64>,
    /// Maximum frequency difference (Hz).
    pub max_freq_diff_hz: Option<f64>,
    /// Maximum voltage magnitude difference (pu).
    pub max_volt_diff_pu: Option<f64>,
    /// Bus where relay is located.
    pub bus: Option<u32>,
    /// Protected switch mRID.
    pub protected_switch_mrid: Option<String>,
}

/// Container for all protection equipment data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProtectionData {
    /// Overcurrent relays.
    pub current_relays: Vec<CurrentRelaySettings>,
    /// Distance/impedance relays.
    pub distance_relays: Vec<DistanceRelaySettings>,
    /// Auto-reclose sequences.
    pub reclose_sequences: Vec<RecloseSequenceData>,
    /// Synchrocheck relays.
    pub synchrocheck_relays: Vec<SynchrocheckSettings>,
}

impl ProtectionData {
    /// Returns true if no protection equipment has been loaded.
    pub fn is_empty(&self) -> bool {
        self.current_relays.is_empty()
            && self.distance_relays.is_empty()
            && self.reclose_sequences.is_empty()
            && self.synchrocheck_relays.is_empty()
    }
}
