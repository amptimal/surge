// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical [`AcScedSetup`] combinator over primitives.
//!
//! Wires pre-computed adapter-side primitives (classification, bandable
//! subset, ramp limits, warm-start mappings, commitment augmentation)
//! into the canonical [`AcScedSetup`] config bag. Adapters compute the
//! primitives from their own problem/context types; this module has no
//! format-specific coupling.

use std::collections::{HashMap, HashSet};

use surge_dispatch::ResourceCommitmentSchedule;

use crate::ac_reconcile::{AcScedSetup, AcWarmStartConfig, ProducerDispatchPinning, RampLimits};
use crate::heuristics::ResourceClassification;

/// Reserve product ID sets consumed by the bandable-subset pinning
/// (active-reserve headroom shrink) and the AC SCED market filter
/// (reactive-only retention).
///
/// Adapters populate these with their format's product IDs
/// (e.g. `reg_up` / `syn` / `ramp_up_on`). The canonical machinery
/// only needs the sets.
#[derive(Debug, Clone, Default)]
pub struct ReserveProductIdSets {
    /// Up-direction active reserve products (shrink upper band on
    /// bandable producers).
    pub up_active: HashSet<String>,
    /// Down-direction active reserve products (shrink lower band).
    pub down_active: HashSet<String>,
    /// Reactive reserve products that survive the AC SCED market
    /// filter. Everything else is dropped.
    pub reactive: HashSet<String>,
}

/// Band configuration for the bandable-producer dispatch pin.
///
/// A bandable producer's per-period bounds become
/// `[target − band, target + band]` where `band = clamp(|target| ×
/// fraction, floor, cap)`.
#[derive(Debug, Clone)]
pub struct DispatchPinningBands {
    pub band_fraction: f64,
    pub band_floor_mw: f64,
    pub band_cap_mw: f64,
    /// When true, active-reserve awards shrink the band so the AC
    /// pass can't re-use headroom already committed to reserves.
    pub apply_reserve_shrink: bool,
}

impl DispatchPinningBands {
    /// Canonical `±5 %, 1 MW floor, 1 GW cap, reserve-aware` preset.
    pub fn default_band_reserve_aware() -> Self {
        Self {
            band_fraction: 0.05,
            band_floor_mw: 1.0,
            band_cap_mw: 1.0e9,
            apply_reserve_shrink: true,
        }
    }
}

/// Inputs to [`build_ac_sced_setup`]. Adapters compute each field from
/// their own problem/context types and hand the bundle to the builder.
#[derive(Debug, Clone)]
pub struct AcScedSetupInputs {
    /// Stage ID whose solved solution drives the AC SCED enrichment
    /// (typically the DC SCUC stage).
    pub source_stage_id: String,
    /// Canonical resource classification (producers / producer_statics
    /// / consumer blocks). Partitioned inside the builder.
    pub classification: ResourceClassification,
    /// The bandable-producer subset — everything else classified as
    /// producer is tight-pinned to the source stage's dispatch.
    pub bandable_producer_resource_ids: HashSet<String>,
    /// Reserve product ID sets for the bandable pin and market filter.
    pub reserve_product_ids: ReserveProductIdSets,
    /// Per-producer ramp envelope (MW/hr), used to intersect the band
    /// with the inter-period ramp envelope.
    pub producer_ramp_limits_mw_per_hr: HashMap<String, RampLimits>,
    /// Bus UID → bus number map for the AC warm start.
    pub bus_uid_to_number: HashMap<String, u32>,
    /// Per-consumer Q/P ratio fallback for the AC warm start.
    pub consumer_q_to_p_ratios: HashMap<String, f64>,
    /// Extra per-resource commitment schedules merged onto the stage's
    /// pinned commitment (voltage-support must-runs, etc.).
    pub commitment_augmentation: Vec<ResourceCommitmentSchedule>,
    /// Band configuration for the bandable subset.
    pub pinning_bands: DispatchPinningBands,
}

/// Assemble the canonical [`AcScedSetup`] from the adapter-supplied
/// primitives. Anchor IDs and Q locks / fixes start empty — callers
/// that need them populate the returned value.
pub fn build_ac_sced_setup(inputs: AcScedSetupInputs) -> AcScedSetup {
    let (producer_ids, producer_static_ids, consumer_block_ids) = inputs.classification.partition();
    let consumer_blocks_by_uid = inputs.classification.consumer_blocks_by_uid;

    let dispatch_pinning = ProducerDispatchPinning {
        producer_resource_ids: producer_ids.clone(),
        producer_static_resource_ids: producer_static_ids.clone(),
        bandable_producer_resource_ids: inputs.bandable_producer_resource_ids,
        band_fraction: inputs.pinning_bands.band_fraction,
        band_floor_mw: inputs.pinning_bands.band_floor_mw,
        band_cap_mw: inputs.pinning_bands.band_cap_mw,
        up_reserve_product_ids: inputs.reserve_product_ids.up_active,
        down_reserve_product_ids: inputs.reserve_product_ids.down_active,
        apply_reserve_shrink: inputs.pinning_bands.apply_reserve_shrink,
        ramp_limits_mw_per_hr: inputs.producer_ramp_limits_mw_per_hr,
        relax_pmin: false,
        relax_pmin_for_resources: HashSet::new(),
        anchor_resource_ids: HashSet::new(),
    };

    let warm_start = AcWarmStartConfig {
        bus_uid_to_number: inputs.bus_uid_to_number,
        producer_resource_ids: producer_ids.union(&producer_static_ids).cloned().collect(),
        dispatchable_load_resource_ids: consumer_block_ids,
        consumer_block_resource_ids_by_uid: consumer_blocks_by_uid,
        consumer_q_to_p_ratio_by_uid: inputs.consumer_q_to_p_ratios,
    };

    AcScedSetup {
        source_stage_id: inputs.source_stage_id,
        reactive_reserve_product_ids: Some(inputs.reserve_product_ids.reactive),
        commitment_augmentation: inputs.commitment_augmentation,
        dispatch_pinning: Some(dispatch_pinning),
        warm_start: Some(warm_start),
        generator_q_locks: HashSet::new(),
        generator_q_fixes: HashMap::new(),
    }
}
