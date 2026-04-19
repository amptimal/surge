// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical resource classification and producer-selection heuristics.
//!
//! Market formulations (GO C3 today; ERCOT, MISO, research variants
//! next) all share a small set of selection heuristics that operate on
//! physical generator capability plus a device classification layer.
//! This module hosts those heuristics as pure functions over a
//! canonical [`ResourceClassification`]. Adapters translate their
//! format-specific device enum into [`MarketDeviceKind`] once, then
//! reuse the canonical selectors unchanged across stages.
//!
//! The heuristics themselves — bandable producer subset, wide-Q anchor
//! producers, reactive-support pin candidates, PV promotion — are not
//! GO-C3-specific; they encode general power-system intuitions about
//! which generators carry reactive-corner imbalance and which should
//! be voltage-regulating. Magic numbers (min P/Q ranges, BFS hop
//! radius, Q-target factor) are captured in typed criteria structs
//! that each adapter fills in from its own preset.

use std::collections::{HashMap, HashSet};

use surge_network::Network;

/// Canonical market device classification. Adapters map their format-
/// specific enum (e.g. `GoC3DeviceKind`) into this once when they build
/// their [`ResourceClassification`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarketDeviceKind {
    /// Dispatchable producer — active power output varies within
    /// `[p_min, p_max]` across the horizon.
    Producer,
    /// Static / must-run producer — output set by a fixed profile
    /// (renewables, synthetic terminal Q gens). Pinned to `[0, 0]` on
    /// AC SCED; its P is modeled elsewhere (e.g. as a load injection).
    ProducerStatic,
    /// Dispatchable (price-responsive) consumer.
    Consumer,
}

/// The minimum classification information canonical heuristics need
/// beyond `Network` itself. Adapters construct this once from their
/// format-specific context and then reuse it across selection calls.
#[derive(Debug, Clone, Default)]
pub struct ResourceClassification {
    /// Resource ID → [`MarketDeviceKind`].
    pub kind_by_resource_id: HashMap<String, MarketDeviceKind>,
    /// Slack-bus numbers (matches `Generator::bus`).
    pub slack_bus_numbers: HashSet<u32>,
    /// Consumer UID → its dispatchable block resource IDs, for formats
    /// that decompose consumers into blocks (GO C3). Empty for formats
    /// that don't.
    pub consumer_blocks_by_uid: HashMap<String, Vec<String>>,
}

impl ResourceClassification {
    /// Partition into `(producers, producer_statics, consumer_blocks)`.
    /// Consumer block IDs flatten from `consumer_blocks_by_uid` — the
    /// raw consumer UIDs in `kind_by_resource_id` are not the
    /// dispatchable block IDs.
    pub fn partition(&self) -> (HashSet<String>, HashSet<String>, HashSet<String>) {
        let mut producers = HashSet::new();
        let mut producer_statics = HashSet::new();
        let mut consumer_blocks = HashSet::new();
        for (rid, kind) in &self.kind_by_resource_id {
            match kind {
                MarketDeviceKind::Producer => {
                    producers.insert(rid.clone());
                }
                MarketDeviceKind::ProducerStatic => {
                    producer_statics.insert(rid.clone());
                }
                MarketDeviceKind::Consumer => {}
            }
        }
        for block_ids in self.consumer_blocks_by_uid.values() {
            for id in block_ids {
                consumer_blocks.insert(id.clone());
            }
        }
        (producers, producer_statics, consumer_blocks)
    }
}

/// Selection criteria for the bandable-producer subset used by
/// [`select_bandable_producers`].
#[derive(Debug, Clone)]
pub struct BandableCriteria {
    /// Add every producer on a slack bus unconditionally (no P/Q
    /// filters applied to these).
    pub include_slack_producers: bool,
    /// After slack producers, add up to `max_additional` non-slack
    /// producers ranked by `|Q range|` desc, then `P range` desc.
    pub max_additional: usize,
    /// Non-slack filter: include only producers with `pmax − pmin >
    /// min_p_range_mw` (strict `>` to match historical behaviour).
    pub min_p_range_mw: f64,
    /// Non-slack filter: include only producers with `|qmax − qmin| >
    /// min_q_range_mvar` (strict `>`).
    pub min_q_range_mvar: f64,
}

/// Select the bandable-producer subset for AC SCED dispatch pinning.
///
/// Produces a deterministic set: slack-bus producers first (when
/// enabled), then non-slack producers ranked by `(q_range desc,
/// p_range desc, resource_id asc)` up to
/// [`BandableCriteria::max_additional`]. Everything outside the set is
/// tight-pinned at the source stage's dispatch target.
pub fn select_bandable_producers(
    network: &Network,
    cls: &ResourceClassification,
    criteria: &BandableCriteria,
) -> HashSet<String> {
    let mut bandable: HashSet<String> = HashSet::new();

    if criteria.include_slack_producers {
        for generator in network.generators.iter() {
            let rid = generator.id.trim();
            if rid.is_empty() {
                continue;
            }
            if !matches!(
                cls.kind_by_resource_id.get(rid),
                Some(MarketDeviceKind::Producer)
            ) {
                continue;
            }
            if cls.slack_bus_numbers.contains(&generator.bus) {
                bandable.insert(rid.to_string());
            }
        }
    }

    if criteria.max_additional == 0 {
        return bandable;
    }

    let mut candidates: Vec<(f64, f64, String)> = Vec::new();
    for generator in network.generators.iter() {
        let rid = generator.id.trim();
        if rid.is_empty() || bandable.contains(rid) {
            continue;
        }
        if !matches!(
            cls.kind_by_resource_id.get(rid),
            Some(MarketDeviceKind::Producer)
        ) {
            continue;
        }
        let p_range = generator.pmax - generator.pmin;
        if p_range <= criteria.min_p_range_mw {
            continue;
        }
        let q_range = (generator.qmax - generator.qmin).abs();
        if q_range <= criteria.min_q_range_mvar {
            continue;
        }
        candidates.push((q_range, p_range, rid.to_string()));
    }

    candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.2.cmp(&b.2))
    });

    for (_, _, rid) in candidates.into_iter().take(criteria.max_additional) {
        bandable.insert(rid);
    }
    bandable
}

/// Selection criteria for wide-Q anchor producers used by
/// [`select_widest_q_anchors`].
#[derive(Debug, Clone)]
pub struct WideQAnchorCriteria {
    /// Maximum number of anchors to return. `0` returns an empty
    /// vector unconditionally.
    pub max_n: usize,
    /// Filter: `pmax − pmin >= min_p_range_mw` (anchors need
    /// meaningful real-power flexibility, not just reactive).
    pub min_p_range_mw: f64,
    /// When true, slack-bus producers are not eligible (they are
    /// typically bandable already).
    pub exclude_slack: bool,
}

/// Select up to `max_n` wide-Q anchor producer IDs for the last-ditch
/// AC-SCED retry rung. Anchors bypass band narrowing and keep their
/// full physical `[P_min, P_max]` envelope so they can absorb
/// imbalance when the default and wide-band attempts both failed.
///
/// Ranking: `(|q_range| desc, p_range desc, resource_id asc)`. Filter
/// also requires `|q_range| > 0` (pure-real machines aren't useful
/// anchors).
pub fn select_widest_q_anchors(
    network: &Network,
    cls: &ResourceClassification,
    criteria: &WideQAnchorCriteria,
) -> Vec<String> {
    if criteria.max_n == 0 {
        return Vec::new();
    }
    let mut candidates: Vec<(f64, f64, String)> = Vec::new();
    for generator in network.generators.iter() {
        let rid = generator.id.trim();
        if rid.is_empty() {
            continue;
        }
        if !matches!(
            cls.kind_by_resource_id.get(rid),
            Some(MarketDeviceKind::Producer)
        ) {
            continue;
        }
        if criteria.exclude_slack && cls.slack_bus_numbers.contains(&generator.bus) {
            continue;
        }
        let p_range = generator.pmax - generator.pmin;
        if p_range < criteria.min_p_range_mw {
            continue;
        }
        let q_range = (generator.qmax - generator.qmin).abs();
        if q_range <= 0.0 {
            continue;
        }
        candidates.push((q_range, p_range, rid.to_string()));
    }
    candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.2.cmp(&b.2))
    });
    candidates
        .into_iter()
        .take(criteria.max_n)
        .map(|(_, _, rid)| rid)
        .collect()
}

/// Selection criteria for the reactive-support pin heuristic used by
/// [`select_reactive_support_pin_generators`].
///
/// Scores each eligible non-slack producer by
/// `q_range × (local_load + 1)²` where `local_load` sums consumer
/// demand on buses within [`Self::load_weight_hops`] branch hops of
/// the generator's bus. Greedy accumulation picks highest-scoring
/// generators until cumulative `|q_range|` meets the target
/// (`factor × peak_system_load_mw`).
#[derive(Debug, Clone)]
pub struct ReactiveSupportPinCriteria {
    /// Multiplier on `peak_system_load_mw` used to derive the
    /// cumulative Q target (MVAr). `0.0` disables selection.
    pub factor: f64,
    /// Filter: `|qmax − qmin| >= min_q_range_mvar`.
    pub min_q_range_mvar: f64,
    /// Filter: `pmax − pmin >= min_p_range_mw`.
    pub min_p_range_mw: f64,
    /// BFS radius for the local-load sum.
    pub load_weight_hops: u32,
}

/// Select producer resource IDs to must-run on the AC SCED stage for
/// reactive support. Returns resource IDs sorted ascending.
///
/// The caller supplies pre-computed peak-load inputs so this selector
/// has no coupling to any specific time-series format. Typical
/// adapter wiring:
///
/// * `peak_load_by_bus_mw` — for each bus, the max across periods of
///   summed consumer `p_ub` (multiplied into MW).
/// * `peak_system_load_mw` — max across periods of system-wide summed
///   consumer load (the horizon's peak hour).
pub fn select_reactive_support_pin_generators(
    network: &Network,
    cls: &ResourceClassification,
    peak_load_by_bus_mw: &HashMap<u32, f64>,
    peak_system_load_mw: f64,
    criteria: &ReactiveSupportPinCriteria,
) -> Vec<String> {
    if criteria.factor <= 0.0 {
        return Vec::new();
    }
    let target_q = criteria.factor * peak_system_load_mw;

    let mut adj: HashMap<u32, HashSet<u32>> = HashMap::new();
    for branch in network.branches.iter() {
        if !branch.in_service {
            continue;
        }
        adj.entry(branch.from_bus)
            .or_default()
            .insert(branch.to_bus);
        adj.entry(branch.to_bus)
            .or_default()
            .insert(branch.from_bus);
    }

    let load_within_hops = |start_bus: u32, max_hops: u32| -> f64 {
        let mut visited = HashSet::new();
        visited.insert(start_bus);
        let mut frontier = vec![start_bus];
        let mut total = peak_load_by_bus_mw.get(&start_bus).copied().unwrap_or(0.0);
        for _ in 0..max_hops {
            let mut next = Vec::new();
            for &b in &frontier {
                if let Some(neighbors) = adj.get(&b) {
                    for &n in neighbors {
                        if visited.insert(n) {
                            next.push(n);
                            total += peak_load_by_bus_mw.get(&n).copied().unwrap_or(0.0);
                        }
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        total
    };

    let mut candidates: Vec<(f64, f64, String)> = Vec::new(); // (score, q_range, rid)
    for generator in network.generators.iter() {
        let rid = generator.id.trim();
        if rid.is_empty() {
            continue;
        }
        if !matches!(
            cls.kind_by_resource_id.get(rid),
            Some(MarketDeviceKind::Producer)
        ) {
            continue;
        }
        if cls.slack_bus_numbers.contains(&generator.bus) {
            continue;
        }
        let q_range = (generator.qmax - generator.qmin).abs();
        if q_range < criteria.min_q_range_mvar {
            continue;
        }
        let p_range = generator.pmax - generator.pmin;
        if p_range < criteria.min_p_range_mw {
            continue;
        }
        let local_load = load_within_hops(generator.bus, criteria.load_weight_hops);
        let score = q_range * (local_load + 1.0).powi(2);
        candidates.push((score, q_range, rid.to_string()));
    }

    candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.2.cmp(&b.2))
    });

    let mut selected: Vec<String> = Vec::new();
    let mut cum_q = 0.0;
    for (_, q_range, rid) in candidates {
        if cum_q >= target_q {
            break;
        }
        cum_q += q_range;
        selected.push(rid);
    }
    selected.sort();
    selected
}

/// Promote every generator with nontrivial reactive capability to
/// voltage-regulating (PV) status. Pure [`Network`] mutation — no
/// adapter context required.
///
/// The canonical AC SCED stage needs a broad set of voltage-regulating
/// generators for convergence; many source formats flag only a
/// narrow "preferred" subset as regulating, which is too tight on
/// scenarios with reactive-heavy demand patterns.
pub fn promote_q_capable_generators_to_pv(network: &mut Network) {
    for generator in network.generators.iter_mut() {
        let q_range = (generator.qmax - generator.qmin).abs();
        if q_range <= 1e-9 {
            continue;
        }
        generator.voltage_regulated = true;
        if generator.reg_bus.is_none() {
            generator.reg_bus = Some(generator.bus);
        }
    }
}
