# Python Result Objects

This page documents the fields and methods on the result objects returned by
Surge's Python analysis functions.

## AcPfResult

Returned by `surge.solve_ac_pf()` and `surge.powerflow.solve_ac_pf()`.

### Properties

| Property | Type | Description |
|---|---|---|
| `converged` | bool | Whether the solver converged |
| `status` | str | `"Converged"`, `"MaxIterations"`, `"Diverged"`, or `"Unsolved"` |
| `iterations` | int | Newton-Raphson iteration count |
| `max_mismatch` | float | Final maximum power mismatch (p.u.) |
| `solve_time_secs` | float | Wall-clock solve time in seconds |
| `vm` | ndarray | Bus voltage magnitudes (p.u.) |
| `va_rad` | ndarray | Bus voltage angles (radians) |
| `va_deg` | ndarray | Bus voltage angles (degrees) |
| `p_inject_mw` | ndarray | Net active power injection per bus (MW) |
| `q_inject_mvar` | ndarray | Net reactive power injection per bus (MVAr) |
| `gen_q_mvar` | ndarray | Reactive power output per generator (MVAr) |
| `q_limited_buses` | list[int] | Bus numbers where PV-to-PQ switching occurred |
| `n_q_limit_switches` | int | Total PV/PQ bus type switches |
| `island_ids` | list[int] | Per-bus island assignment (when island detection enabled) |
| `n_islands` | int | Number of electrical islands detected |
| `area_interchange` | dict or None | Area interchange results (when enforce_interchange enabled) |
| `convergence_history` | ndarray | Nx2 array of [iteration, mismatch] (when recorded) |
| `branch_apparent_power` | ndarray | Branch apparent power flows (MVA) |
| `branch_loading_pct` | ndarray | Branch loading as percentage of Rate A (requires attached network) |

### Methods

| Method | Return | Description |
|---|---|---|
| `get_buses()` | list | All buses with solved voltages and injections |
| `bus(number)` | object | Single bus result by bus number |
| `get_branches()` | list | All branches with solved power flows |
| `get_generators()` | list | All generators with solved reactive output |
| `violated_buses(vmin, vmax)` | list | Buses with voltage outside [vmin, vmax] |
| `overloaded_branches(threshold_pct)` | list | Branches above loading threshold |
| `to_dataframe()` | DataFrame | Bus results: bus_id, vm_pu, va_deg, p_mw, q_mvar |
| `to_json()` / `from_json()` | str / result | JSON serialization round-trip |
| `to_dict()` | dict | Plain Python dictionary of all results |

## DcPfResult

Returned by `surge.solve_dc_pf()` and `surge.powerflow.solve_dc_pf()`.

### Properties

| Property | Type | Description |
|---|---|---|
| `va_rad` | ndarray | Bus voltage angles (radians) |
| `va_deg` | ndarray | Bus voltage angles (degrees) |
| `branch_p_mw` | ndarray | Branch active power flows (MW) |
| `slack_p_mw` | float | Slack bus real power injection (MW) |
| `solve_time_secs` | float | Solve time in seconds |
| `total_generation_mw` | float | Total system generation after slack balancing |
| `bus_p_inject_mw` | ndarray | Net real power injection per bus (MW) |
| `bus_numbers` | list[int] | Bus numbers in bus order |
| `branch_from` | list[int] | From-bus numbers in branch order |
| `branch_to` | list[int] | To-bus numbers in branch order |
| `branch_circuit` | list[str] | Circuit identifiers in branch order |

### Methods

| Method | Return | Description |
|---|---|---|
| `to_dataframe()` | DataFrame | Bus results: bus_id, va_rad, va_deg |
| `branch_dataframe()` | DataFrame | Branch results: from_bus, to_bus, circuit, p_mw |

## DcOpfResult

Returned by `surge.solve_dc_opf()`.

Wraps the base `OpfResult` and adds DC-OPF-specific fields. All `OpfResult`
properties are accessible directly on the `DcOpfResult` object.

### Additional Properties

| Property | Type | Description |
|---|---|---|
| `hvdc_dispatch_mw` | list | Optimal HVDC P_dc setpoints (MW) |
| `hvdc_shadow_prices` | list | HVDC capacity shadow prices ($/MWh) |
| `generator_limit_violations` | list | (gen_index, violation_mw) pairs |
| `feasible` | bool | Whether all hard constraints are satisfied |

## AcOpfResult

Returned by `surge.solve_ac_opf()`. Wraps `OpfResult` with the same
delegation pattern as `DcOpfResult`.

## OpfResult (Base)

The shared OPF solution surface used by DC-OPF, AC-OPF, and SCOPF results.

### Properties

| Property | Type | Description |
|---|---|---|
| `total_cost` | float | Optimal generation cost ($/hr) |
| `opf_type` | str | Formulation: `"dc_opf"`, `"ac_opf"`, `"dc_scopf"`, `"ac_scopf"` |
| `solve_time_secs` | float | Wall-clock solve time |
| `iterations` | int | Solver iterations |
| `gen_p_mw` | ndarray | Generator active power dispatch (MW) |
| `gen_q_mvar` | ndarray | Generator reactive dispatch (MVAr); AC-OPF only |
| `gen_bus_numbers` | list[int] | Bus number per generator |
| `gen_ids` | list[str] | Generator IDs |
| `lmp` | ndarray | Locational marginal prices ($/MWh) |
| `lmp_energy` | ndarray | Energy component of LMP ($/MWh) |
| `lmp_congestion` | ndarray | Congestion component of LMP ($/MWh) |
| `lmp_loss` | ndarray | Loss component of LMP ($/MWh) |
| `lmp_reactive` | ndarray | Reactive LMP ($/MVAr-h); AC-OPF only |
| `vm` | ndarray | Bus voltage magnitudes (p.u.); all 1.0 for DC-OPF |
| `va_rad` | ndarray | Bus voltage angles (radians) |
| `total_load_mw` | float | Total system load (MW) |
| `total_generation_mw` | float | Total generation (MW) |
| `total_losses_mw` | float | Total system losses (MW); zero for DC-OPF |
| `branch_pf_mw` | ndarray | From-end active power flow per branch (MW) |
| `branch_pt_mw` | ndarray | To-end active power flow per branch (MW) |
| `branch_qf_mvar` | ndarray | From-end reactive flow (MVAr); AC-OPF only |
| `branch_qt_mvar` | ndarray | To-end reactive flow (MVAr); AC-OPF only |
| `branch_loading_pct` | ndarray | Branch loading as % of Rate A |
| `branch_shadow_prices` | ndarray | Branch constraint shadow prices ($/MWh) |
| `binding_branch_indices` | list[int] | Indices of branches with active constraints |
| `mu_pg_min` / `mu_pg_max` | ndarray | Generator P bound duals ($/MWh) |
| `mu_qg_min` / `mu_qg_max` | ndarray | Generator Q bound duals; AC only |
| `mu_vm_min` / `mu_vm_max` | ndarray | Voltage bound duals; AC only |

## ScopfResult

Returned by `surge.solve_scopf()`.

### Properties

| Property | Type | Description |
|---|---|---|
| `base_opf` | OpfResult | Base-case OPF solution |
| `converged` | bool | Whether constraint generation converged |
| `iterations` | int | Constraint generation iterations |
| `formulation` | str | `"dc"` or `"ac"` |
| `mode` | str | `"preventive"` or `"corrective"` |
| `total_contingencies_evaluated` | int | Contingencies checked |
| `binding_contingencies` | list | Contingencies with non-zero shadow prices |
| `lmp_contingency_congestion` | list | Per-bus contingency congestion component ($/MWh) |
| `solve_time_secs` | float | Wall-clock time |

## ContingencyAnalysis

Returned by `surge.analyze_n1_branch()`, `surge.analyze_n1_generator()`,
`surge.analyze_n2_branch()`, and `surge.analyze_contingencies()`.

### Properties

| Property | Type | Description |
|---|---|---|
| `n_contingencies` | int | Total contingencies analyzed |
| `n_screened_out` | int | Contingencies filtered by screening |
| `n_ac_solved` | int | Contingencies solved with full AC NR |
| `n_converged` | int | AC-solved contingencies that converged |
| `n_with_violations` | int | Contingencies with at least one violation |
| `n_violations` | int | Total violations across all contingencies |
| `n_voltage_critical` | int | Contingencies classified as voltage-critical |
| `solve_time_secs` | float | Wall-clock analysis time |

### Methods

| Method | Return | Description |
|---|---|---|
| `to_dataframe()` | DataFrame | Per-contingency summary with violations |
| `results_dataframe()` | DataFrame | Per-contingency: id, converged, n_violations, max_loading |
| `violations_dataframe()` | DataFrame | Flat violation table across all contingencies |
| `voltage_critical_df()` | DataFrame | Voltage-critical contingencies sorted by L-index |
| `post_contingency_vm(id)` | ndarray or None | Post-contingency voltages for a specific contingency |
| `post_contingency_va(id)` | ndarray or None | Post-contingency angles for a specific contingency |

## HvdcResult

Returned by `surge.solve_hvdc()`.

### Properties

| Property | Type | Description |
|---|---|---|
| `converged` | bool | Convergence status |
| `iterations` | int | Solver iterations |
| `method` | str | Solution method used |
| `total_converter_loss_mw` | float | Total converter losses (MW) |
| `total_dc_network_loss_mw` | float | Total DC network losses (MW) |
| `total_loss_mw` | float | Total HVDC losses (MW) |
| `stations` | list | Converter station solutions |
| `dc_buses` | list | DC bus voltage solutions |

Each `HvdcStationSolution` contains: `name`, `technology`, `ac_bus`, `dc_bus`,
`p_ac_mw`, `q_ac_mvar`, `p_dc_mw`, `v_dc_pu`, `converter_loss_mw`,
`converged`, and optionally `lcc_detail` (with `alpha_deg`, `gamma_deg`,
`i_dc_pu`, `power_factor`).

## NercAtcResult

Returned by `surge.transfer.compute_nerc_atc()`.

### Properties

| Property | Type | Description |
|---|---|---|
| `atc_mw` | float | Available Transfer Capability (MW) |
| `ttc_mw` | float | Total Transfer Capability (MW) |
| `trm_mw` | float | Transmission Reliability Margin (MW) |
| `cbm_mw` | float | Capacity Benefit Margin (MW) |
| `etc_mw` | float | Existing Transmission Commitments (MW) |
| `binding_branch` | int or None | Index of the branch limiting transfer |
| `binding_contingency` | int or None | Index of the contingency causing the limit |
| `transfer_ptdf` | list[float] | Per-monitored-branch sensitivity to the transfer |
| `reactive_margin_warning` | bool | True if generators approach reactive limits |
