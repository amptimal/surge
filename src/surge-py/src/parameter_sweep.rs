// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Parameter sweep helper for running multiple power flow scenarios in parallel.

use std::collections::HashMap;
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use rayon::prelude::*;

use pyo3::exceptions::PyRuntimeError;

use crate::exceptions::extract_panic_msg;
use crate::network::Network;
use crate::solutions::AcPfResult;
use crate::utils::dict_to_dataframe_with_index;

// ---------------------------------------------------------------------------
// Modification enum — pure Rust representation of network edits
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Modification {
    ScaleLoad {
        factor: f64,
        area: Option<u32>,
    },
    ScaleGeneration {
        factor: f64,
        area: Option<u32>,
    },
    SetBusLoad {
        bus: u32,
        pd_mw: f64,
        qd_mvar: f64,
    },
    SetGeneratorPg {
        bus: u32,
        p_mw: f64,
        machine_id: String,
    },
    SetGeneratorInService {
        bus: u32,
        in_service: bool,
        machine_id: String,
    },
    SetBranchInService {
        from_bus: u32,
        to_bus: u32,
        in_service: bool,
        circuit: String,
    },
    SetBranchTap {
        from_bus: u32,
        to_bus: u32,
        tap: f64,
        circuit: String,
    },
    SetBusVoltage {
        bus: u32,
        vm_pu: f64,
        va_deg: f64,
    },
    SetGeneratorSetpoint {
        bus: u32,
        vs_pu: f64,
        machine_id: String,
    },
}

// ---------------------------------------------------------------------------
// Parsed scenario — pure Rust
// ---------------------------------------------------------------------------

struct ParsedScenario {
    name: String,
    modifications: Vec<Modification>,
}

// ---------------------------------------------------------------------------
// Internal result — pure Rust, no Python references
// ---------------------------------------------------------------------------

struct InternalSweepResult {
    name: String,
    solution: Option<(surge_solution::PfSolution, Arc<surge_network::Network>)>,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Apply a modification to a mutable Network
// ---------------------------------------------------------------------------

fn apply_modification(
    net: &mut surge_network::Network,
    modification: &Modification,
) -> Result<(), String> {
    match modification {
        Modification::ScaleLoad { factor, area } => {
            let area_map: HashMap<u32, u32> =
                net.buses.iter().map(|b| (b.number, b.area)).collect();
            for load in net.loads.iter_mut() {
                if area.is_none() || area_map.get(&load.bus).copied() == *area {
                    load.active_power_demand_mw *= factor;
                    load.reactive_power_demand_mvar *= factor;
                }
            }
        }
        Modification::ScaleGeneration { factor, area } => {
            let area_map: HashMap<u32, u32> =
                net.buses.iter().map(|b| (b.number, b.area)).collect();
            for g in net.generators.iter_mut() {
                if !g.in_service {
                    continue;
                }
                if area.is_none() || area_map.get(&g.bus).copied() == *area {
                    g.p *= factor;
                }
            }
        }
        Modification::SetBusLoad {
            bus,
            pd_mw,
            qd_mvar,
        } => {
            if !net.buses.iter().any(|b| b.number == *bus) {
                return Err(format!("Bus {bus} not found"));
            }
            // Update load demand via Load objects.
            if let Some(load) = net.loads.iter_mut().find(|l| l.bus == *bus) {
                load.active_power_demand_mw = *pd_mw;
                load.reactive_power_demand_mvar = *qd_mvar;
            } else {
                net.loads.push(surge_network::network::Load {
                    bus: *bus,
                    id: "1".to_string(),
                    in_service: true,
                    conforming: true,
                    active_power_demand_mw: *pd_mw,
                    reactive_power_demand_mvar: *qd_mvar,
                    ..Default::default()
                });
            }
        }
        Modification::SetGeneratorPg {
            bus,
            p_mw,
            machine_id,
        } => {
            let g = net
                .generators
                .iter_mut()
                .find(|g| gen_matches(g, *bus, machine_id))
                .ok_or_else(|| {
                    format!("Generator at bus {bus} machine_id '{machine_id}' not found")
                })?;
            g.p = *p_mw;
        }
        Modification::SetGeneratorInService {
            bus,
            in_service,
            machine_id,
        } => {
            let g = net
                .generators
                .iter_mut()
                .find(|g| gen_matches(g, *bus, machine_id))
                .ok_or_else(|| {
                    format!("Generator at bus {bus} machine_id '{machine_id}' not found")
                })?;
            g.in_service = *in_service;
        }
        Modification::SetBranchInService {
            from_bus,
            to_bus,
            in_service,
            circuit,
        } => {
            let br = net
                .branches
                .iter_mut()
                .find(|br| {
                    br.from_bus == *from_bus && br.to_bus == *to_bus && br.circuit == *circuit
                })
                .ok_or_else(|| format!("Branch {from_bus}-{to_bus} circuit {circuit} not found"))?;
            br.in_service = *in_service;
        }
        Modification::SetBranchTap {
            from_bus,
            to_bus,
            tap,
            circuit,
        } => {
            let br = net
                .branches
                .iter_mut()
                .find(|br| {
                    br.from_bus == *from_bus && br.to_bus == *to_bus && br.circuit == *circuit
                })
                .ok_or_else(|| format!("Branch {from_bus}-{to_bus} circuit {circuit} not found"))?;
            br.tap = *tap;
        }
        Modification::SetBusVoltage { bus, vm_pu, va_deg } => {
            let b = net
                .buses
                .iter_mut()
                .find(|b| b.number == *bus)
                .ok_or_else(|| format!("Bus {bus} not found"))?;
            b.voltage_magnitude_pu = *vm_pu;
            b.voltage_angle_rad = va_deg.to_radians();
        }
        Modification::SetGeneratorSetpoint {
            bus,
            vs_pu,
            machine_id,
        } => {
            let g = net
                .generators
                .iter_mut()
                .find(|g| gen_matches(g, *bus, machine_id))
                .ok_or_else(|| {
                    format!("Generator at bus {bus} machine_id '{machine_id}' not found")
                })?;
            g.voltage_setpoint_pu = *vs_pu;
        }
    }
    Ok(())
}

/// Match a generator by (bus, machine_id). None machine_id treated as "1".
fn gen_matches(g: &surge_network::network::Generator, bus: u32, machine_id: &str) -> bool {
    g.bus == bus
        && match &g.machine_id {
            None => machine_id == "1",
            Some(id) => id.as_str() == machine_id,
        }
}

// ---------------------------------------------------------------------------
// Solve a single scenario in pure Rust
// ---------------------------------------------------------------------------

fn solve_scenario(
    base_net: &surge_network::Network,
    scenario: &ParsedScenario,
    solver: &str,
) -> InternalSweepResult {
    // Clone the network for this scenario
    let mut net = base_net.clone();

    // Apply modifications
    for m in &scenario.modifications {
        if let Err(e) = apply_modification(&mut net, m) {
            return InternalSweepResult {
                name: scenario.name.clone(),
                solution: None,
                error: Some(format!("Modification failed: {e}")),
            };
        }
    }

    // Validate
    if let Err(e) = net.validate() {
        return InternalSweepResult {
            name: scenario.name.clone(),
            solution: None,
            error: Some(format!("Network validation failed: {e}")),
        };
    }

    let net_arc = Arc::new(net);

    // Solve
    match solver {
        "acpf" => {
            let opts = surge_ac::AcPfOptions::default();
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_ac::solve_ac_pf_kernel(&net_arc, &opts)
            })) {
                Ok(Ok(sol)) => InternalSweepResult {
                    name: scenario.name.clone(),
                    solution: Some((sol, net_arc)),
                    error: None,
                },
                Ok(Err(e)) => InternalSweepResult {
                    name: scenario.name.clone(),
                    solution: None,
                    error: Some(format!("NR solver error: {e}")),
                },
                Err(panic) => InternalSweepResult {
                    name: scenario.name.clone(),
                    solution: None,
                    error: Some(format!("NR solver panicked: {}", extract_panic_msg(panic))),
                },
            }
        }
        "dcpf" => {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                surge_dc::solve_dc(&net_arc)
            })) {
                Ok(Ok(dc_result)) => {
                    let sol = surge_dc::to_pf_solution(&dc_result, &net_arc);
                    InternalSweepResult {
                        name: scenario.name.clone(),
                        solution: Some((sol, net_arc)),
                        error: None,
                    }
                }
                Ok(Err(e)) => InternalSweepResult {
                    name: scenario.name.clone(),
                    solution: None,
                    error: Some(format!("DC solver error: {e}")),
                },
                Err(panic) => InternalSweepResult {
                    name: scenario.name.clone(),
                    solution: None,
                    error: Some(format!("DC solver panicked: {}", extract_panic_msg(panic))),
                },
            }
        }
        "fdpf" => {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                solve_fdpf_internal(&net_arc)
            })) {
                Ok(Ok(sol)) => InternalSweepResult {
                    name: scenario.name.clone(),
                    solution: Some((sol, net_arc)),
                    error: None,
                },
                Ok(Err(e)) => InternalSweepResult {
                    name: scenario.name.clone(),
                    solution: None,
                    error: Some(format!("FDPF solver error: {e}")),
                },
                Err(panic) => InternalSweepResult {
                    name: scenario.name.clone(),
                    solution: None,
                    error: Some(format!(
                        "FDPF solver panicked: {}",
                        extract_panic_msg(panic)
                    )),
                },
            }
        }
        _ => InternalSweepResult {
            name: scenario.name.clone(),
            solution: None,
            error: Some(format!(
                "Unknown solver '{solver}'. Use 'acpf', 'dcpf', or 'fdpf'."
            )),
        },
    }
}

/// Internal FDPF solve that returns a AcPfResult (pure Rust, no Python).
fn solve_fdpf_internal(net: &surge_network::Network) -> Result<surge_solution::PfSolution, String> {
    let opts = surge_ac::FdpfOptions {
        tolerance: 1e-6,
        max_iterations: 100,
        flat_start: false,
        ..Default::default()
    };
    surge_ac::solve_fdpf(net, &opts)
}

// ---------------------------------------------------------------------------
// Parse Python arguments into Rust types
// ---------------------------------------------------------------------------

fn parse_modification(_py: Python<'_>, item: &Bound<'_, PyAny>) -> PyResult<Modification> {
    let tuple: &Bound<'_, PyTuple> = item.cast::<PyTuple>()?;
    let method: String = tuple.get_item(0)?.extract()?;

    match method.as_str() {
        "scale_load" => {
            let factor: f64 = tuple.get_item(1)?.extract()?;
            let area: Option<u32> = if tuple.len() > 2 {
                tuple.get_item(2)?.extract().ok()
            } else {
                None
            };
            Ok(Modification::ScaleLoad { factor, area })
        }
        "scale_generation" => {
            let factor: f64 = tuple.get_item(1)?.extract()?;
            let area: Option<u32> = if tuple.len() > 2 {
                tuple.get_item(2)?.extract().ok()
            } else {
                None
            };
            Ok(Modification::ScaleGeneration { factor, area })
        }
        "set_bus_load" => {
            let bus: u32 = tuple.get_item(1)?.extract()?;
            let pd_mw: f64 = tuple.get_item(2)?.extract()?;
            let qd_mvar: f64 = if tuple.len() > 3 {
                tuple.get_item(3)?.extract()?
            } else {
                0.0
            };
            Ok(Modification::SetBusLoad {
                bus,
                pd_mw,
                qd_mvar,
            })
        }
        "set_generator_p" => {
            let bus: u32 = tuple.get_item(1)?.extract()?;
            let p_mw: f64 = tuple.get_item(2)?.extract()?;
            let machine_id: String = if tuple.len() > 3 {
                tuple.get_item(3)?.extract()?
            } else {
                "1".to_string()
            };
            Ok(Modification::SetGeneratorPg {
                bus,
                p_mw,
                machine_id,
            })
        }
        "set_generator_in_service" => {
            let bus: u32 = tuple.get_item(1)?.extract()?;
            let in_service: bool = tuple.get_item(2)?.extract()?;
            let machine_id: String = if tuple.len() > 3 {
                tuple.get_item(3)?.extract()?
            } else {
                "1".to_string()
            };
            Ok(Modification::SetGeneratorInService {
                bus,
                in_service,
                machine_id,
            })
        }
        "set_branch_in_service" => {
            let from_bus: u32 = tuple.get_item(1)?.extract()?;
            let to_bus: u32 = tuple.get_item(2)?.extract()?;
            let in_service: bool = tuple.get_item(3)?.extract()?;
            let circuit: String = if tuple.len() > 4 {
                let v = tuple.get_item(4)?;
                if let Ok(n) = v.extract::<i64>() {
                    n.to_string()
                } else {
                    v.extract::<String>().unwrap_or_else(|_| "1".to_string())
                }
            } else {
                "1".to_string()
            };
            Ok(Modification::SetBranchInService {
                from_bus,
                to_bus,
                in_service,
                circuit,
            })
        }
        "set_branch_tap" => {
            let from_bus: u32 = tuple.get_item(1)?.extract()?;
            let to_bus: u32 = tuple.get_item(2)?.extract()?;
            let tap: f64 = tuple.get_item(3)?.extract()?;
            let circuit: String = if tuple.len() > 4 {
                let v = tuple.get_item(4)?;
                if let Ok(n) = v.extract::<i64>() {
                    n.to_string()
                } else {
                    v.extract::<String>().unwrap_or_else(|_| "1".to_string())
                }
            } else {
                "1".to_string()
            };
            Ok(Modification::SetBranchTap {
                from_bus,
                to_bus,
                tap,
                circuit,
            })
        }
        "set_bus_voltage" => {
            let bus: u32 = tuple.get_item(1)?.extract()?;
            let vm_pu: f64 = tuple.get_item(2)?.extract()?;
            let va_deg: f64 = if tuple.len() > 3 {
                tuple.get_item(3)?.extract()?
            } else {
                0.0
            };
            Ok(Modification::SetBusVoltage { bus, vm_pu, va_deg })
        }
        "set_generator_setpoint" => {
            let bus: u32 = tuple.get_item(1)?.extract()?;
            let vs_pu: f64 = tuple.get_item(2)?.extract()?;
            let machine_id: String = if tuple.len() > 3 {
                tuple.get_item(3)?.extract()?
            } else {
                "1".to_string()
            };
            Ok(Modification::SetGeneratorSetpoint {
                bus,
                vs_pu,
                machine_id,
            })
        }
        _ => Err(PyValueError::new_err(format!(
            "Unknown modification method '{method}'. Supported: scale_load, scale_generation, \
             set_bus_load, set_generator_p, set_generator_in_service, set_branch_in_service, \
             set_branch_tap, set_bus_voltage, set_generator_setpoint"
        ))),
    }
}

fn parse_scenarios(py: Python<'_>, scenarios: &Bound<'_, PyAny>) -> PyResult<Vec<ParsedScenario>> {
    let list: Vec<Bound<'_, PyAny>> = scenarios.extract()?;
    let mut parsed = Vec::with_capacity(list.len());

    for item in &list {
        let tuple: &Bound<'_, PyTuple> = item.cast::<PyTuple>()?;
        if tuple.len() < 2 {
            return Err(PyValueError::new_err(
                "Each scenario must be a tuple of (name: str, modifications: list)",
            ));
        }
        let name: String = tuple.get_item(0)?.extract()?;
        let mods_list: Vec<Bound<'_, PyAny>> = tuple.get_item(1)?.extract()?;

        let mut modifications = Vec::with_capacity(mods_list.len());
        for mod_item in &mods_list {
            modifications.push(parse_modification(py, mod_item)?);
        }

        parsed.push(ParsedScenario {
            name,
            modifications,
        });
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// Python-visible result types
// ---------------------------------------------------------------------------

/// A single scenario result from a parameter sweep.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct SweepResult {
    name_str: String,
    converged_flag: bool,
    pf_solution: Option<(surge_solution::PfSolution, Arc<surge_network::Network>)>,
    error_str: Option<String>,
}

#[pymethods]
impl SweepResult {
    /// Scenario name.
    #[getter]
    fn name(&self) -> &str {
        &self.name_str
    }

    /// Whether the solver converged.
    #[getter]
    fn converged(&self) -> bool {
        self.converged_flag
    }

    /// The power flow solution, or None if the solve failed.
    #[getter]
    fn solution(&self) -> Option<AcPfResult> {
        self.pf_solution.as_ref().map(|(sol, net)| AcPfResult {
            inner: sol.clone(),
            net: Some(Arc::clone(net)),
        })
    }

    /// Error message if the solve failed, or None.
    #[getter]
    fn error(&self) -> Option<&str> {
        self.error_str.as_deref()
    }

    fn __repr__(&self) -> String {
        if let Some(ref sol) = self.pf_solution {
            format!(
                "SweepResult(name='{}', converged={}, iterations={})",
                self.name_str, self.converged_flag, sol.0.iterations
            )
        } else {
            format!(
                "SweepResult(name='{}', converged=False, error='{}')",
                self.name_str,
                self.error_str.as_deref().unwrap_or("unknown")
            )
        }
    }
}

/// Results from a parameter sweep (collection of scenario results).
#[pyclass]
pub struct SweepResults {
    items: Vec<SweepResult>,
}

#[pymethods]
impl SweepResults {
    /// List of all scenario results.
    #[getter]
    fn results(&self) -> Vec<SweepResult> {
        self.items.clone()
    }

    /// Summary DataFrame (or dict) with columns: name, converged, iterations,
    /// max_vm, min_vm, total_losses_mw, solve_time_secs.
    fn to_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let names: Vec<String> = self.items.iter().map(|r| r.name_str.clone()).collect();
        let converged: Vec<bool> = self.items.iter().map(|r| r.converged_flag).collect();
        let iterations: Vec<u32> = self
            .items
            .iter()
            .map(|r| {
                r.pf_solution
                    .as_ref()
                    .map(|(s, _)| s.iterations)
                    .unwrap_or(0)
            })
            .collect();
        let max_vm: Vec<f64> = self
            .items
            .iter()
            .map(|r| {
                r.pf_solution
                    .as_ref()
                    .map(|(s, _)| {
                        s.voltage_magnitude_pu
                            .iter()
                            .cloned()
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                    .unwrap_or(f64::NAN)
            })
            .collect();
        let min_vm: Vec<f64> = self
            .items
            .iter()
            .map(|r| {
                r.pf_solution
                    .as_ref()
                    .map(|(s, _)| {
                        s.voltage_magnitude_pu
                            .iter()
                            .cloned()
                            .fold(f64::INFINITY, f64::min)
                    })
                    .unwrap_or(f64::NAN)
            })
            .collect();
        let total_losses_mw: Vec<f64> = self
            .items
            .iter()
            .map(|r| {
                r.pf_solution
                    .as_ref()
                    .map(|(s, net)| s.active_power_injection_pu.iter().sum::<f64>() * net.base_mva)
                    .unwrap_or(f64::NAN)
            })
            .collect();
        let solve_time_secs: Vec<f64> = self
            .items
            .iter()
            .map(|r| {
                r.pf_solution
                    .as_ref()
                    .map(|(s, _)| s.solve_time_secs)
                    .unwrap_or(f64::NAN)
            })
            .collect();

        let dict = PyDict::new(py);
        dict.set_item("name", names)?;
        dict.set_item("converged", converged)?;
        dict.set_item("iterations", iterations)?;
        dict.set_item("max_vm", max_vm)?;
        dict.set_item("min_vm", min_vm)?;
        dict.set_item("total_losses_mw", total_losses_mw)?;
        dict.set_item("solve_time_secs", solve_time_secs)?;
        dict_to_dataframe_with_index(py, dict, &["name"])
    }

    fn __len__(&self) -> usize {
        self.items.len()
    }

    fn __getitem__(&self, idx: isize) -> PyResult<SweepResult> {
        let len = self.items.len() as isize;
        let actual = if idx < 0 { len + idx } else { idx };
        if actual < 0 || actual >= len {
            return Err(pyo3::exceptions::PyIndexError::new_err(format!(
                "index {idx} out of range for SweepResults of length {len}"
            )));
        }
        Ok(self.items[actual as usize].clone())
    }

    fn __repr__(&self) -> String {
        let n_converged = self.items.iter().filter(|r| r.converged_flag).count();
        format!(
            "SweepResults({} scenarios, {} converged)",
            self.items.len(),
            n_converged
        )
    }
}

// ---------------------------------------------------------------------------
// Top-level function
// ---------------------------------------------------------------------------

/// Run a parameter sweep: solve multiple power flow scenarios in parallel.
///
/// Each scenario clones the base network, applies a list of modifications,
/// and solves using the specified solver.  Scenarios execute in parallel
/// via Rust threads (rayon), with the Python GIL released.
///
/// Args:
///     network:   Base power system network (not modified).
///     scenarios: List of ``(name, modifications)`` tuples.  Each modification
///                is a tuple ``(method_name, *args)`` — see :pep:`parameter_sweep`
///                for supported methods.
///     solver:    ``"acpf"``, ``"dcpf"``, or ``"fdpf"``.
///
/// Returns:
///     SweepResults collection with per-scenario solutions.
#[pyfunction]
#[pyo3(signature = (network, scenarios, solver="acpf", on_progress=None))]
pub fn parameter_sweep(
    py: Python<'_>,
    network: &Network,
    scenarios: &Bound<'_, PyAny>,
    solver: &str,
    on_progress: Option<Py<PyAny>>,
) -> PyResult<SweepResults> {
    // Validate solver name early
    if !["acpf", "dcpf", "fdpf"].contains(&solver) {
        return Err(PyValueError::new_err(format!(
            "Unknown solver '{solver}'. Supported: 'acpf', 'dcpf', 'fdpf'"
        )));
    }

    // Parse all Python arguments into pure Rust BEFORE releasing the GIL
    let parsed = parse_scenarios(py, scenarios)?;

    if parsed.is_empty() {
        return Ok(SweepResults { items: Vec::new() });
    }

    // Validate the base network once
    network.validate()?;

    let base_net = Arc::clone(&network.inner);
    let solver_str = solver.to_string();

    // Build a thread-safe progress callback if requested.
    let progress_cb: Option<Arc<dyn Fn(usize, usize) + Send + Sync>> = on_progress.map(|cb| {
        let arc_cb = Arc::new(cb);
        let total = parsed.len();
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let f: Arc<dyn Fn(usize, usize) + Send + Sync> =
            Arc::new(move |_done: usize, _total: usize| {
                let done = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let _ = Python::attach(|py| arc_cb.call1(py, (done, total)));
            });
        f
    });

    // Release GIL and run all scenarios in parallel via a scoped rayon pool.
    // A fresh pool is built per call so that set_max_threads() takes effect
    // even after the first invocation (unlike build_global which is one-shot).
    let pool = crate::utils::make_thread_pool()?;
    let internal_results: Vec<InternalSweepResult> = py
        .detach(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pool.install(|| {
                    parsed
                        .par_iter()
                        .map(|scenario| {
                            let result = solve_scenario(&base_net, scenario, &solver_str);
                            if let Some(ref cb) = progress_cb {
                                cb(0, 0); // counter is tracked internally by the closure
                            }
                            result
                        })
                        .collect()
                })
            }))
        })
        .map_err(|e| {
            PyRuntimeError::new_err(format!(
                "parameter_sweep panicked: {}",
                extract_panic_msg(e)
            ))
        })?;

    // Convert internal results to Python-visible SweepResult objects
    let items: Vec<SweepResult> = internal_results
        .into_iter()
        .map(|ir| {
            let converged = ir
                .solution
                .as_ref()
                .map(|(s, _)| s.status == surge_solution::SolveStatus::Converged)
                .unwrap_or(false);
            SweepResult {
                name_str: ir.name,
                converged_flag: converged,
                pf_solution: ir.solution,
                error_str: ir.error,
            }
        })
        .collect();

    Ok(SweepResults { items })
}
