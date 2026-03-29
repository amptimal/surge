#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Build the market30 case from the IEEE 30-bus system.

Creates a modified 30-bus network with a diverse resource fleet designed
to exercise every participant type in dispatch and market workflows:
  - Coal, Gas CC, Gas CT (x2), Nuclear generators
  - Wind and solar renewables
  - BESS (battery storage) and pumped-hydro storage
  - Curtailable, interruptible, and elastic demand response
  - VSC-HVDC tie between areas 2 and 3
  - Flowgate, interface, and tight branch ratings

Run:
    source .venv/bin/activate
    python examples/cases/market30/build.py
"""

from pathlib import Path

import surge


def build() -> surge.Network:
    net = surge.case30()
    net.name = "market30"

    # ── Remove original 6 generators ──────────────────────────────────
    for g in list(net.generators):
        net.remove_generator(g.id)

    # ── Add designed fleet ────────────────────────────────────────────
    # Generator fleet: (bus, machine_id, pg, pmax, pmin, cost_coeffs)
    fleet = [
        (1,  "G1",      160.0, 200.0, 50.0, [0.002, 18.0, 400.0]),  # Coal
        (2,  "CC1_GT",   80.0, 110.0, 25.0, [0.003, 24.0, 150.0]),  # CC gas turbine
        (2,  "CC1_ST",   35.0,  60.0, 10.0, [0.002, 16.0, 120.0]),  # CC steam section
        (13, "G3",        0.0,  80.0, 10.0, [0.005, 55.0, 100.0]),  # Gas CT
        (22, "G4",       95.0, 100.0, 50.0, [0.0,    8.0, 600.0]),  # Nuclear
        (23, "G5",        0.0,  60.0,  5.0, [0.006, 65.0,  80.0]),  # Gas CT2
        (15, "W1",       36.0, 120.0,  0.0, [0.0,    0.0,   0.0]),  # Wind
        (25, "S1",        0.0,  80.0,  0.0, [0.0,    0.0,   0.0]),  # Solar
        (10, "B1",        0.0,  50.0, -50.0, [0.0,   0.5,   0.0]),  # Battery storage
        (27, "PH",        0.0, 100.0, -80.0, [0.0,   1.0,   0.0]),  # Pumped hydro
    ]
    for bus, mid, pg, pmax, pmin, coeffs in fleet:
        net.add_generator(bus=bus, pg_mw=pg, pmax_mw=pmax, pmin_mw=pmin,
                          machine_id=mid)

    # Set costs using generator id (format: gen_{bus}_{seq})
    for g in net.generators:
        key = (g.bus, g.machine_id)
        cost_map = {(b, m): c for b, m, _, _, _, c in fleet}
        if key in cost_map:
            net.set_generator_cost(g.id, cost_map[key])

    # ── Set generator attributes ──────────────────────────────────────
    gen_attrs = {
        # (bus, mid): (fuel, commit, ramp_up, ramp_dn, min_up, min_dn,
        #              quick_start, co2_rate)
        (1, "G1"):      ("coal", "Market", 2.0, 2.0, 8.0, 8.0, False, 0.95),
        (2, "CC1_GT"):  ("gas", "Market", 6.0, 6.0, 2.0, 2.0, False, 0.42),
        (2, "CC1_ST"):  ("gas", "Market", 4.0, 4.0, 2.0, 2.0, False, 0.18),
        (13, "G3"):     ("gas", "Market", 10.0, 10.0, 1.0, 1.0, True, 0.55),
        (22, "G4"):     ("nuclear", "MustRun", 0.5, 0.5, 168.0, 72.0, False, 0.0),
        (23, "G5"):     ("gas", "Market", 10.0, 10.0, 1.0, 1.0, True, 0.60),
        (15, "W1"):     ("wind", "Market", 120.0, 120.0, None, None, False, 0.0),
        (25, "S1"):     ("solar", "Market", 80.0, 80.0, None, None, False, 0.0),
        (10, "B1"):     (None, "Market", 50.0, 50.0, None, None, False, 0.0),
        (27, "PH"):     (None, "Market", 20.0, 20.0, 2.0, 2.0, False, 0.0),
    }
    for g in net.generators:
        key = (g.bus, g.machine_id)
        if key not in gen_attrs:
            continue
        fuel, commit, rup, rdn, mup, mdn, qs, co2 = gen_attrs[key]
        if fuel is not None:
            g.fuel_type = fuel
        g.commitment_status = commit
        g.ramp_up_curve = [(0.0, rup)]
        g.ramp_down_curve = [(0.0, rdn)]
        if mup is not None:
            g.min_up_time_hr = mup
        if mdn is not None:
            g.min_down_time_hr = mdn
        g.quick_start = qs
        g.co2_rate_t_per_mwh = co2
        net.update_generator_object(g)

    # ── Set reserve offers and qualifications on generators ───────────
    reserve_specs = {
        (1, "G1"):      [("spin", 40.0, 5.0), ("reg_up", 10.0, 8.0),
                         ("reg_dn", 10.0, 8.0)],
        (13, "G3"):     [("spin", 40.0, 6.0), ("nspin", 80.0, 2.0)],
        (23, "G5"):     [("nspin", 60.0, 3.0)],
        (10, "B1"):     [("spin", 25.0, 3.0), ("reg_up", 25.0, 5.0),
                         ("reg_dn", 25.0, 5.0)],
        (27, "PH"):     [("spin", 30.0, 4.0), ("reg_up", 15.0, 6.0),
                         ("reg_dn", 15.0, 6.0)],
    }
    qual_specs = {
        (1, "G1"):      {"spin": True, "reg_up": True, "reg_dn": True,
                         "nspin": False},
        (13, "G3"):     {"spin": True, "reg_up": False, "reg_dn": False,
                         "nspin": True},
        (22, "G4"):     {"spin": False, "reg_up": False, "reg_dn": False,
                         "nspin": False},
        (23, "G5"):     {"spin": False, "reg_up": False, "reg_dn": False,
                         "nspin": True},
        (15, "W1"):     {"spin": False, "reg_up": False, "reg_dn": False,
                         "nspin": False},
        (25, "S1"):     {"spin": False, "reg_up": False, "reg_dn": False,
                         "nspin": False},
        (10, "B1"):     {"spin": True, "reg_up": True, "reg_dn": True,
                         "nspin": True},
        (27, "PH"):     {"spin": True, "reg_up": True, "reg_dn": True,
                         "nspin": True},
    }
    for g in net.generators:
        key = (g.bus, g.machine_id)
        if key in reserve_specs:
            net.set_generator_reserve_offers(g.id, reserve_specs[key])
        if key in qual_specs:
            net.set_generator_qualifications(g.id, qual_specs[key])

    # ── Native battery storage and pumped hydro ───────────────────────
    # Find generator indices for B1 and PH backing generators
    b1_idx = next(i for i, g in enumerate(net.generators)
                  if g.bus == 10 and g.machine_id == "B1")
    ph_idx = next(i for i, g in enumerate(net.generators)
                  if g.bus == 27 and g.machine_id == "PH")

    # B1: true 4-hour battery on a generator-backed storage model.
    bess = next(g for g in net.generators if g.bus == 10 and g.machine_id == "B1")
    bess.storage = surge.StorageParams(
        200.0,
        efficiency=0.92,
        soc_initial_mwh=110.0,
        soc_min_mwh=15.0,
        soc_max_mwh=190.0,
        variable_cost_per_mwh=1.0,
        degradation_cost_per_mwh=2.0,
        dispatch_mode="cost_minimization",
        chemistry="LFP",
    )
    bess.storage.max_c_rate_charge = 0.25
    bess.storage.max_c_rate_discharge = 0.25
    net.update_generator_object(bess)

    ph_gen = next(g for g in net.generators if g.bus == 27 and g.machine_id == "PH")
    ph_gen.storage = surge.StorageParams(
        800.0,
        efficiency=0.89,
        soc_initial_mwh=500.0,
        soc_min_mwh=80.0,
        soc_max_mwh=760.0,
        variable_cost_per_mwh=1.0,
        degradation_cost_per_mwh=0.5,
        dispatch_mode="cost_minimization",
    )
    ph_gen.storage.max_c_rate_charge = 0.10
    ph_gen.storage.max_c_rate_discharge = 0.125
    net.update_generator_object(ph_gen)

    # PH1: Pumped hydro — full constraints
    ph = surge.PumpedHydroUnit("PH1_Hydro", ph_idx, capacity_mwh=800.0)
    ph.soc_initial_mwh = 500.0
    ph.soc_min_mwh = 80.0
    ph.soc_max_mwh = 760.0
    ph.efficiency_generate = 0.90
    ph.efficiency_pump = 0.88
    ph.pump_mw_max = 80.0
    ph.pump_mw_min = 20.0
    ph.min_release_mw = 10.0
    ph.ramp_rate_mw_per_min = 20.0
    ph.startup_time_gen_min = 5.0
    ph.startup_time_pump_min = 10.0
    ph.startup_cost = 500.0
    net.add_pumped_hydro_unit_object(ph)

    # ── Combined-cycle plant ──────────────────────────────────────────
    cc_gt_idx = next(i for i, g in enumerate(net.generators)
                     if g.bus == 2 and g.machine_id == "CC1_GT")
    cc_st_idx = next(i for i, g in enumerate(net.generators)
                     if g.bus == 2 and g.machine_id == "CC1_ST")
    cc_plant = surge.CombinedCyclePlant(
        "CC1",
        configs=[
            surge.CombinedCycleConfig(
                "GT_ONLY",
                [cc_gt_idx],
                p_min_mw=25.0,
                p_max_mw=110.0,
                min_up_time_hr=2.0,
                min_down_time_hr=2.0,
            ),
            surge.CombinedCycleConfig(
                "CC_FULL",
                [cc_gt_idx, cc_st_idx],
                p_min_mw=45.0,
                p_max_mw=170.0,
                min_up_time_hr=4.0,
                min_down_time_hr=3.0,
            ),
        ],
        transitions=[
            surge.CombinedCycleTransition(
                "GT_ONLY",
                "CC_FULL",
                transition_time_min=60.0,
                transition_cost=900.0,
                online_transition=True,
            ),
            surge.CombinedCycleTransition(
                "CC_FULL",
                "GT_ONLY",
                transition_time_min=45.0,
                transition_cost=600.0,
                online_transition=True,
            ),
        ],
        active_config="GT_ONLY",
        hours_in_config=6.0,
    )
    net.add_combined_cycle_plant_object(cc_plant)

    # ── HVDC tie: bus 12 (area 2) ↔ bus 30 (area 3) ──────────────────
    hvdc = surge.VscHvdcLink(
        name="HVDC_12_30",
        converter1_bus=12,
        converter2_bus=30,
        p_mw=40.0,
    )
    net.add_vsc_dc_line_object(hvdc)

    # ── Dispatchable loads (demand response) ──────────────────────────
    # DR1: Curtailable load at bus 7 — 30 MW at $100/MWh
    net.add_dispatchable_load(bus=7, p_sched_mw=30.0, cost_per_mwh=100.0,
                              archetype="Curtailable")
    # DR2: Interruptible load at bus 21 — 20 MW at $200/MWh
    net.add_dispatchable_load(bus=21, p_sched_mw=20.0, cost_per_mwh=200.0,
                              archetype="Interruptible")
    # DL1: Elastic (price-responsive) load at bus 30 — 15 MW at $80/MWh
    net.add_dispatchable_load(bus=30, p_sched_mw=15.0, cost_per_mwh=80.0,
                              archetype="Curtailable")

    # ── Flowgate ──────────────────────────────────────────────────────
    net.add_flowgate(
        "North-South",
        monitored_branches=[(6, 10, "1")],
        monitored_coefficients=[1.0],
        limit_mw=50.0,
        contingency_branch=(4, 12, "1"),
    )

    # ── Interface ─────────────────────────────────────────────────────
    net.add_interface(
        "Area1-Area2",
        branches=[(6, 9, "1"), (6, 10, "1"), (4, 12, "1")],
        coefficients=[1.0, 1.0, 1.0],
        limit_forward_mw=180.0,
        limit_reverse_mw=120.0,
    )

    # ── Adjust branch ratings ────────────────────────────────────────
    # Base case30 has many 16 MVA branches. Scale all ratings up so
    # the network can handle ~340 MW load while still producing
    # meaningful congestion on the flowgate and interface.
    for i in range(net.n_branches):
        rate = net.branch_rate_a[i]
        if rate > 0:
            fb = net.branch_from[i]
            tb = net.branch_to[i]
            ckt = net.branch_circuit[i]
            # Scale up ratings for feasibility; the flowgate (50 MW) still binds
            net.set_branch_rating(fb, tb, rate * 4.0, int(ckt))

    # ── Scale loads ───────────────────────────────────────────────────
    # Base case30 has ~189 MW total demand. Scale to ~340 MW so that
    # the fleet (~760 MW thermal+renewable Pmax) produces meaningful
    # commitment decisions and congestion.
    net.scale_loads(1.6)

    return net


def main():
    net = build()
    out = Path(__file__).parent / "market30.surge.json.zst"
    surge.save(net, str(out))

    # Verification
    net2 = surge.load(str(out))
    battery_generators = sum(
        1 for g in net2.generators
        if g.storage is not None and g.storage.chemistry is not None
    )
    print(f"market30: {net2.n_buses} buses, {net2.n_branches} branches, "
          f"{len(net2.generators)} generators, "
          f"{battery_generators} battery generators, "
          f"{len(net2.pumped_hydro_units)} pumped-hydro units, "
          f"{len(net2.combined_cycle_plants)} combined-cycle plants, "
          f"{len(net2.dispatchable_loads)} dispatchable loads")
    print(f"Saved to {out}")


if __name__ == "__main__":
    main()
