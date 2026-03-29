// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! IEC 62325 Energy Market Data types — market participants, time series,
//! bids/offers, energy schedules, and transmission capacity allocations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Market participant identity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketParticipant {
    pub mrid: String,
    pub name: String,
    pub role: Option<String>,
}

/// A single time-series point (one interval).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketPoint {
    pub position: u32,
    pub quantity: Option<f64>,
    pub price: Option<f64>,
    pub secondary_quantity: Option<f64>,
}

/// A period within a time series.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketPeriod {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub resolution: Option<String>,
    pub points: Vec<MarketPoint>,
}

/// A complete time series from IEC 62325.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketTimeSeries {
    pub mrid: String,
    pub business_type: Option<String>,
    pub in_domain: Option<String>,
    pub out_domain: Option<String>,
    pub registered_resource: Option<String>,
    pub quantity_unit: Option<String>,
    pub currency: Option<String>,
    pub curve_type: Option<String>,
    pub periods: Vec<MarketPeriod>,
}

/// Energy schedule for a generator or area.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnergySchedule {
    pub resource_mrid: String,
    /// `(timestamp, mw_value)` pairs.
    pub schedule: Vec<(DateTime<Utc>, f64)>,
}

/// Bid/offer curve.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BidOffer {
    pub resource_mrid: String,
    pub business_type: Option<String>,
    /// `(quantity_mw, price)` segments.
    pub segments: Vec<(f64, f64)>,
    pub period_start: Option<DateTime<Utc>>,
}

/// Transmission capacity allocation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransmissionAllocation {
    pub from_domain: String,
    pub to_domain: String,
    /// `(timestamp, mw_value)` pairs.
    pub allocations: Vec<(DateTime<Utc>, f64)>,
}

/// Complete market data container parsed from IEC 62325 ESMP XML.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketData {
    pub document_mrid: Option<String>,
    pub document_type: Option<String>,
    pub sender: Option<MarketParticipant>,
    pub receiver: Option<MarketParticipant>,
    pub participants: Vec<MarketParticipant>,
    pub time_series: Vec<MarketTimeSeries>,
    pub energy_schedules: Vec<EnergySchedule>,
    pub bid_offers: Vec<BidOffer>,
    pub transmission_allocations: Vec<TransmissionAllocation>,
}

impl MarketData {
    /// Returns `true` when no market data has been populated.
    pub fn is_empty(&self) -> bool {
        self.time_series.is_empty()
            && self.participants.is_empty()
            && self.energy_schedules.is_empty()
            && self.bid_offers.is_empty()
            && self.transmission_allocations.is_empty()
    }
}
