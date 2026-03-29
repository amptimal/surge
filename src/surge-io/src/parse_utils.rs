// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared parsing utilities for PSS/E, EPC, RAWX, and DYR parsers.

use std::collections::HashMap;

use surge_network::network::Network;
use surge_network::network::SwitchedShunt;
use thiserror::Error;

/// Remove surrounding single-quotes and trim whitespace from a token.
///
/// PSS/E and DYR files quote string fields (bus names, device IDs, model names)
/// with single quotes. This strips them for clean storage.
pub(crate) fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        s[1..s.len() - 1].trim().to_string()
    } else {
        s.to_string()
    }
}

/// A raw load record before aggregation into bus Pd/Qd.
pub(crate) struct RawLoad {
    pub bus: u32,
    pub id: String,
    pub status: i32,
    pub owner: Option<u32>,
    pub pl: f64,
    pub ql: f64,
    /// Whether this load conforms to system-wide scaling (PSS/E SCALE field).
    /// `true` = conforming (default), `false` = non-conforming.
    pub conforming: bool,
    // ZIP load fractions (sum to 1.0 for each of P and Q).
    pub zip_p_impedance_frac: f64,
    pub zip_p_current_frac: f64,
    pub zip_p_power_frac: f64,
    pub zip_q_impedance_frac: f64,
    pub zip_q_current_frac: f64,
    pub zip_q_power_frac: f64,
}

/// A raw shunt record before aggregation into bus Gs/Bs.
pub(crate) struct RawShunt {
    pub bus: u32,
    pub status: i32,
    pub gl: f64,
    pub bl: f64,
}

/// A raw switched-shunt record with full PSS/E control metadata.
///
/// Used by `apply_switched_shunts()` to distinguish between fixed shunts
/// (MODSW=0, baked into `bus.shunt_susceptance_mvar`) and controlled shunts (MODSW!=0, kept as
/// discrete `SwitchedShunt` objects for the NR outer-loop controller).
#[derive(Debug)]
pub(crate) struct RawSwitchedShunt {
    /// Bus number (PSS/E external numbering).
    pub bus: u32,
    /// Control mode: 0=fixed, 1=discrete admittance, 2=reactive power, 3+=other.
    pub modsw: i32,
    /// Status: 1=in-service, 0=out-of-service.
    pub stat: i32,
    /// Upper voltage limit (pu). Shunt switches out a step when `vm > vswhi`.
    pub vswhi: f64,
    /// Lower voltage limit (pu). Shunt switches in a step when `vm < vswlo`.
    pub vswlo: f64,
    /// Regulated bus number. 0 = regulate the host bus.
    pub swrem: u32,
    /// Current operating susceptance in Mvar (positive = capacitive).
    pub binit: f64,
    /// Up to 8 (n_steps, b_per_step_mvar) blocks.
    /// Positive `b_per_step_mvar` = capacitor bank; negative = reactor bank.
    pub blocks: Vec<(i32, f64)>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ApplyError {
    #[error("load references missing bus {bus}")]
    MissingLoadBus { bus: u32 },
}

/// Accumulate load records into bus Pd/Qd and populate `network.loads`.
///
/// Only in-service loads (status == 1) are applied.
pub(crate) fn apply_loads(network: &mut Network, loads: &[RawLoad]) -> Result<(), ApplyError> {
    use surge_network::network::Load;

    let bus_map: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    for load in loads {
        if load.status != 1 {
            continue;
        }
        if !bus_map.contains_key(&load.bus) {
            return Err(ApplyError::MissingLoadBus { bus: load.bus });
        }
        network.loads.push(Load {
            bus: load.bus,
            active_power_demand_mw: load.pl,
            reactive_power_demand_mvar: load.ql,
            in_service: load.status == 1,
            conforming: load.conforming,
            id: load.id.clone(),
            zip_p_impedance_frac: load.zip_p_impedance_frac,
            zip_p_current_frac: load.zip_p_current_frac,
            zip_p_power_frac: load.zip_p_power_frac,
            zip_q_impedance_frac: load.zip_q_impedance_frac,
            zip_q_current_frac: load.zip_q_current_frac,
            zip_q_power_frac: load.zip_q_power_frac,
            owners: load
                .owner
                .map(|owner| {
                    vec![surge_network::network::OwnershipEntry {
                        owner,
                        fraction: 1.0,
                    }]
                })
                .unwrap_or_default(),
            ..Load::new(0, 0.0, 0.0)
        });
    }

    Ok(())
}

/// Accumulate shunt records into bus Gs/Bs.
///
/// Only in-service shunts (status == 1) are applied.
pub(crate) fn apply_shunts(network: &mut Network, shunts: &[RawShunt]) {
    let bus_map: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    for shunt in shunts {
        if shunt.status != 1 {
            continue;
        }
        if let Some(&idx) = bus_map.get(&shunt.bus) {
            network.buses[idx].shunt_conductance_mw += shunt.gl;
            network.buses[idx].shunt_susceptance_mvar += shunt.bl;
        }
    }
}

/// Apply switched-shunt records to the network.
///
/// **Fixed shunts** (`MODSW = 0` or `STAT != 1`): BINIT is accumulated into
/// `bus.shunt_susceptance_mvar` as fixed susceptance, identical to the previous behaviour.
///
/// **Controlled shunts** (`MODSW != 0` and `STAT == 1`): BINIT is NOT added to
/// `bus.shunt_susceptance_mvar`. Instead, each non-zero (N, B) block becomes a discrete
/// [`SwitchedShunt`] entry on `network.controls.switched_shunts`. The NR outer control
/// loop then dispatches the shunt to hold the regulated bus voltage within the
/// `[vswlo, vswhi]` band.
///
/// `n_active_steps` for each block is initialised by greedily allocating BINIT
/// across the blocks so that the initial injection equals BINIT.
pub(crate) fn apply_switched_shunts(
    network: &mut Network,
    shunts: &[RawSwitchedShunt],
    base_mva: f64,
) {
    let bus_map: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();
    let mut ordinal_by_bus: HashMap<u32, usize> = HashMap::new();

    for shunt in shunts {
        let Some(&bus_idx) = bus_map.get(&shunt.bus) else {
            continue;
        };

        // Out-of-service or fixed: treat as a plain fixed shunt at BINIT.
        if shunt.stat != 1 || shunt.modsw == 0 {
            if shunt.stat == 1 {
                network.buses[bus_idx].shunt_susceptance_mvar += shunt.binit;
            }
            continue;
        }

        // Controlled shunt — build discrete SwitchedShunt objects, one per block.
        // v_target is the midpoint of the [vswlo, vswhi] band.
        let v_target = (shunt.vswhi + shunt.vswlo) / 2.0;
        // Dead-band is the full [vswlo, vswhi] width, minimum 0.02 pu.
        let v_band = (shunt.vswhi - shunt.vswlo).abs().max(0.02);

        // Regulated bus: SWREM = 0 means self-regulation.
        let bus_regulated = if shunt.swrem != 0 {
            if bus_map.contains_key(&shunt.swrem) {
                shunt.swrem
            } else {
                shunt.bus
            }
        } else {
            shunt.bus
        };
        let next_ordinal = ordinal_by_bus.entry(shunt.bus).or_insert(0);

        // Greedy allocation of BINIT across blocks to initialise n_active_steps.
        let mut binit_remaining = shunt.binit; // Mvar

        for &(ni, bi) in &shunt.blocks {
            if ni <= 0 || bi.abs() < 1e-9 {
                continue;
            }

            // Compute initial active steps from remaining BINIT.
            let n_active_steps = if bi > 0.0 {
                // Capacitor block — steps are positive.
                let active = (binit_remaining / bi).round() as i32;
                active.clamp(0, ni)
            } else {
                // Reactor block — steps are stored as negative (convention: ≤ 0).
                // binit_remaining / bi is positive (both negative), so negate before clamping.
                let n_steps = (binit_remaining / bi).round() as i32;
                (-n_steps).clamp(-ni, 0)
            };

            // Subtract the Mvar contribution using |n_active_steps| × bi so the sign
            // is correct for both capacitor (bi > 0) and reactor (bi < 0) conventions.
            binit_remaining -= n_active_steps.unsigned_abs() as f64 * bi;
            *next_ordinal += 1;

            network.controls.switched_shunts.push(SwitchedShunt {
                id: format!("switched_shunt_{}_{}", shunt.bus, *next_ordinal),
                bus: shunt.bus,
                bus_regulated,
                b_step: bi.abs() / base_mva,
                n_steps_cap: if bi > 0.0 { ni } else { 0 },
                n_steps_react: if bi < 0.0 { ni } else { 0 },
                v_target,
                v_band,
                n_active_steps,
            });
        }

        // If no blocks were specified but BINIT != 0, create a single-block
        // approximation so BINIT is represented in the discrete model.
        if shunt.blocks.iter().all(|&(n, b)| n <= 0 || b.abs() < 1e-9) && shunt.binit.abs() > 1e-9 {
            // Represent as one step of size |BINIT|.
            let bi = shunt.binit;
            *next_ordinal += 1;
            network.controls.switched_shunts.push(SwitchedShunt {
                id: format!("switched_shunt_{}_{}", shunt.bus, *next_ordinal),
                bus: shunt.bus,
                bus_regulated,
                b_step: bi.abs() / base_mva,
                n_steps_cap: if bi > 0.0 { 1 } else { 0 },
                n_steps_react: if bi < 0.0 { 1 } else { 0 },
                v_target,
                v_band,
                n_active_steps: if bi > 0.0 { 1 } else { -1 },
            });
        }
    }
}

/// Fix vmin/vmax values that are in kV rather than per-unit, zero, or mis-ordered.
///
/// Detects voltage limits stored in kV (older PSS/E files) by checking if either
/// exceeds 10.0/100.0, and applies safe defaults (0.9/1.1 p.u.). Also guards
/// against zero or negative values and swaps if vmin > vmax.
pub(crate) fn sanitize_voltage_limits(network: &mut Network) {
    for bus in &mut network.buses {
        if bus.voltage_min_pu > 10.0 || bus.voltage_max_pu > 100.0 {
            tracing::warn!(
                "bus {}: vmin={:.2}, vmax={:.2} appear to be in kV, not p.u.; \
                 resetting to 0.9/1.1 p.u.",
                bus.number,
                bus.voltage_min_pu,
                bus.voltage_max_pu
            );
            bus.voltage_min_pu = 0.9;
            bus.voltage_max_pu = 1.1;
        }
        if bus.voltage_min_pu <= 0.0 {
            bus.voltage_min_pu = 0.9;
        }
        if bus.voltage_max_pu <= 0.0 {
            bus.voltage_max_pu = 1.1;
        }
        if bus.voltage_min_pu > bus.voltage_max_pu {
            std::mem::swap(&mut bus.voltage_min_pu, &mut bus.voltage_max_pu);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Bus, BusType};

    /// Build a minimal 1-bus network for shunt parsing tests.
    fn one_bus_network(bus_num: u32) -> Network {
        let mut net = Network::new("test");
        net.base_mva = 100.0;
        net.buses = vec![Bus::new(bus_num, BusType::Slack, 100.0)];
        net
    }

    // -------------------------------------------------------------------------
    // apply_switched_shunts: fixed shunts (MODSW=0)
    // -------------------------------------------------------------------------

    #[test]
    fn fixed_shunt_baked_into_bus_bs() {
        let mut net = one_bus_network(5);
        let shunts = vec![RawSwitchedShunt {
            bus: 5,
            modsw: 0,
            stat: 1,
            vswhi: 1.1,
            vswlo: 0.9,
            swrem: 0,
            binit: 50.0, // 50 Mvar capacitor
            blocks: vec![],
        }];
        apply_switched_shunts(&mut net, &shunts, 100.0);
        // Fixed shunt baked into bus.shunt_susceptance_mvar; switched_shunts stays empty.
        assert!(
            (net.buses[0].shunt_susceptance_mvar - 50.0).abs() < 1e-9,
            "fixed shunt BINIT must be in bus.shunt_susceptance_mvar"
        );
        assert!(
            net.controls.switched_shunts.is_empty(),
            "no discrete SwitchedShunt objects for a fixed shunt"
        );
    }

    #[test]
    fn out_of_service_shunt_ignored() {
        let mut net = one_bus_network(5);
        let shunts = vec![RawSwitchedShunt {
            bus: 5,
            modsw: 1, // controlled, but out of service
            stat: 0,
            vswhi: 1.1,
            vswlo: 0.9,
            swrem: 0,
            binit: 50.0,
            blocks: vec![(1, 50.0)],
        }];
        apply_switched_shunts(&mut net, &shunts, 100.0);
        assert!(
            (net.buses[0].shunt_susceptance_mvar).abs() < 1e-9,
            "OOS shunt must not affect bus.shunt_susceptance_mvar"
        );
        assert!(net.controls.switched_shunts.is_empty());
    }

    // -------------------------------------------------------------------------
    // apply_switched_shunts: controlled shunts (MODSW != 0)
    // -------------------------------------------------------------------------

    #[test]
    fn controlled_shunt_creates_switched_shunt_not_in_bus_bs() {
        let mut net = one_bus_network(5);
        // 4 steps × 50 Mvar each = 200 Mvar total cap; BINIT = 150 Mvar (3 steps in)
        let shunts = vec![RawSwitchedShunt {
            bus: 5,
            modsw: 1,
            stat: 1,
            vswhi: 1.05,
            vswlo: 0.95,
            swrem: 0,
            binit: 150.0,
            blocks: vec![(4, 50.0)],
        }];
        apply_switched_shunts(&mut net, &shunts, 100.0);

        // BINIT must NOT be in bus.shunt_susceptance_mvar for a controlled shunt.
        assert!(
            net.buses[0].shunt_susceptance_mvar.abs() < 1e-9,
            "controlled shunt BINIT must not be baked into bus.shunt_susceptance_mvar"
        );

        // One discrete SwitchedShunt object must be created.
        assert_eq!(net.controls.switched_shunts.len(), 1);
        let ss = &net.controls.switched_shunts[0];
        assert_eq!(ss.bus, 5);
        assert_eq!(ss.bus_regulated, 5);
        assert_eq!(ss.n_steps_cap, 4);
        assert_eq!(ss.n_steps_react, 0);
        assert!(
            (ss.b_step - 50.0 / 100.0).abs() < 1e-9,
            "b_step = 50 Mvar / 100 MVA = 0.5 pu"
        );
        // Initial steps: 150 Mvar / 50 Mvar_per_step = 3 steps.
        assert_eq!(ss.n_active_steps, 3);
        // Voltage band midpoint = (1.05 + 0.95) / 2 = 1.0.
        assert!((ss.v_target - 1.0).abs() < 1e-9);
        // Voltage band width = 1.05 - 0.95 = 0.10.
        assert!((ss.v_band - 0.10).abs() < 1e-9);
    }

    #[test]
    fn controlled_shunt_multi_block() {
        let mut net = one_bus_network(1);
        // Two blocks: 2 × 100 Mvar cap + 4 × 50 Mvar cap.
        // BINIT = 300 Mvar → block1 fully on (2 steps), block2 has 2 of 4 steps.
        let shunts = vec![RawSwitchedShunt {
            bus: 1,
            modsw: 1,
            stat: 1,
            vswhi: 1.05,
            vswlo: 0.95,
            swrem: 0,
            binit: 300.0,
            blocks: vec![(2, 100.0), (4, 50.0)],
        }];
        apply_switched_shunts(&mut net, &shunts, 100.0);

        assert!(net.buses[0].shunt_susceptance_mvar.abs() < 1e-9);
        assert_eq!(net.controls.switched_shunts.len(), 2);

        // Block 1: 2 steps × 100 Mvar → fully allocated (200 Mvar of 300).
        let b1 = &net.controls.switched_shunts[0];
        assert_eq!(b1.n_steps_cap, 2);
        assert!((b1.b_step - 1.0).abs() < 1e-9); // 100/100 = 1.0 pu
        assert_eq!(b1.n_active_steps, 2); // fully on

        // Block 2: 4 steps × 50 Mvar → 2 steps active (100 Mvar of remaining 100).
        let b2 = &net.controls.switched_shunts[1];
        assert_eq!(b2.n_steps_cap, 4);
        assert!((b2.b_step - 0.5).abs() < 1e-9); // 50/100 = 0.5 pu
        assert_eq!(b2.n_active_steps, 2);
    }

    #[test]
    fn controlled_shunt_no_blocks_uses_binit_as_single_step() {
        let mut net = one_bus_network(3);
        let shunts = vec![RawSwitchedShunt {
            bus: 3,
            modsw: 1,
            stat: 1,
            vswhi: 1.05,
            vswlo: 0.95,
            swrem: 0,
            binit: 75.0,
            blocks: vec![], // no block data provided
        }];
        apply_switched_shunts(&mut net, &shunts, 100.0);

        // Should create a single-step approximation.
        assert_eq!(net.controls.switched_shunts.len(), 1);
        let ss = &net.controls.switched_shunts[0];
        assert_eq!(ss.n_steps_cap, 1);
        assert_eq!(ss.n_active_steps, 1);
        assert!((ss.b_step - 0.75).abs() < 1e-9); // 75/100 = 0.75 pu
    }

    #[test]
    fn reactor_block_creates_react_steps() {
        let mut net = one_bus_network(2);
        // Reactor: 3 steps × -100 Mvar each; BINIT = -200 Mvar (2 steps in).
        let shunts = vec![RawSwitchedShunt {
            bus: 2,
            modsw: 1,
            stat: 1,
            vswhi: 1.05,
            vswlo: 0.95,
            swrem: 0,
            binit: -200.0,
            blocks: vec![(3, -100.0)],
        }];
        apply_switched_shunts(&mut net, &shunts, 100.0);

        assert_eq!(net.controls.switched_shunts.len(), 1);
        let ss = &net.controls.switched_shunts[0];
        assert_eq!(ss.n_steps_cap, 0);
        assert_eq!(ss.n_steps_react, 3);
        assert!((ss.b_step - 1.0).abs() < 1e-9); // |−100| / 100 = 1.0 pu
        assert_eq!(ss.n_active_steps, -2); // 2 reactor steps in
    }

    #[test]
    fn apply_loads_rejects_missing_bus_reference() {
        let mut net = one_bus_network(1);
        let loads = vec![RawLoad {
            bus: 99,
            id: String::new(),
            status: 1,
            owner: None,
            pl: 10.0,
            ql: 5.0,
            conforming: true,
            zip_p_impedance_frac: 0.0,
            zip_p_current_frac: 0.0,
            zip_p_power_frac: 1.0,
            zip_q_impedance_frac: 0.0,
            zip_q_current_frac: 0.0,
            zip_q_power_frac: 1.0,
        }];

        let err = apply_loads(&mut net, &loads).expect_err("missing bus should be rejected");
        assert_eq!(err, ApplyError::MissingLoadBus { bus: 99 });
        assert!(net.loads.is_empty());
    }
}
