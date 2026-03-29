// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Virtual energy bids for day-ahead market clearing.
//!
//! A **virtual bid** (also called an "inc/dec bid" or "convergence bid") is a
//! financial position that injects or withdraws virtual MW at a bus to influence
//! LMPs.  They are a day-ahead market construct — RTOs (ERCOT, PJM, ISO-NE) use
//! them for convergence bidding between day-ahead and real-time markets.
//!
//! # Economics
//!
//! - **Inc (increment) bid**: Injects MW at a bus, competing against physical
//!   generators.  Clears when the bus LMP ≥ bid price; drives LMP down at that bus.
//! - **Dec (decrement) bid**: Withdraws MW at a bus, competing against physical
//!   loads.  Clears when the bus LMP ≤ bid price; drives LMP up at that bus.
//!
//! # LP formulation
//!
//! Each in-service bid adds one variable `v_k ∈ [0, mw_limit / base_mva]`.
//!
//! **Objective** (minimise total production cost + virtual bid cost):
//! - Inc bid: `+price_per_mwh * base_mva * v_k` (paying for energy)
//! - Dec bid: `-price_per_mwh * base_mva * v_k` (receiving payment)
//!
//! **Power balance at bus** (LP constraint matrix coefficient for variable `v_k`):
//! - Inc bid: `-1.0` — injects power at the bus (same sign as a generator in the
//!   B-theta formulation: `B·θ - Σ Pg + Σ Pd = 0`).
//! - Dec bid: `+1.0` — withdraws power from the bus (same sign as a load).
//!
//! Uneconomic bids clear at zero naturally: the LP will not award an Inc bid at
//! a price above the equilibrium LMP, nor a Dec bid at a price below it.

use serde::{Deserialize, Serialize};

/// Direction of a virtual bid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualBidDirection {
    /// Increment (virtual injection): adds MW supply at this bus.
    Inc,
    /// Decrement (virtual withdrawal): adds MW demand at this bus.
    Dec,
}

/// A virtual energy bid for day-ahead market clearing.
///
/// Virtual bids participate in DC-OPF / SCED / SCUC as purely financial
/// positions: they affect LMPs but do not represent physical generation or load.
/// They are a day-ahead instrument — do not use in real-time dispatch.
///
/// Each bid targets exactly one dispatch period (hour).  A trader bidding the
/// same price/quantity at the same location for hours 14–18 submits five
/// separate positions, matching real ISO practice (ERCOT, PJM, ISO-NE).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualBid {
    /// Market-layer position ID that originated this bid.
    ///
    /// When a single position maps to multiple buses (e.g. via a settlement
    /// location), each constituent bus-level bid carries the same position_id
    /// so that cleared MW can be aggregated back to the originating position.
    pub position_id: String,
    /// External bus number where the virtual injection/withdrawal occurs.
    pub bus: u32,
    /// Target dispatch period (0-indexed hour).
    #[serde(default)]
    pub period: usize,
    /// Maximum cleared MW (≥ 0).  The LP variable is bounded `[0, mw_limit/base]`.
    pub mw_limit: f64,
    /// Offer/bid price ($/MWh).
    ///
    /// - Inc bid: offer price — cleared if bus LMP ≥ price (cheaper than LMP).
    /// - Dec bid: bid price  — cleared if bus LMP ≤ price (more expensive than LMP).
    pub price_per_mwh: f64,
    /// Inc (virtual injection) or Dec (virtual withdrawal).
    pub direction: VirtualBidDirection,
    /// Whether this bid participates in the current solve.
    pub in_service: bool,
}

/// Result for a single virtual bid after market clearing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualBidResult {
    /// Market-layer position ID that originated this bid.
    pub position_id: String,
    /// External bus number.
    pub bus: u32,
    /// Bid direction.
    pub direction: VirtualBidDirection,
    /// Cleared MW (≥ 0).  Zero means the bid was not awarded.
    pub cleared_mw: f64,
    /// Submitted offer/bid price ($/MWh).
    pub price_per_mwh: f64,
    /// Bus LMP at the optimal solution ($/MWh).
    pub lmp: f64,
}
