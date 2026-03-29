// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python-facing `Network` pyclass — wraps `Arc<surge_network::Network>`.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use surge_network::network::BusType;

use crate::exceptions::{NetworkError, extract_panic_msg, to_network_pyerr, to_pyerr};
use crate::hvdc::HvdcView;
use crate::matrices::{JacobianResult, YBusResult};
use crate::rich_objects;
use crate::solutions::AcPfResult;
use crate::topology::NodeBreakerTopologyView;
use crate::utils::dict_to_dataframe_with_index;

// ---------------------------------------------------------------------------
// Network wrapper
// ---------------------------------------------------------------------------

/// A power system network (buses, branches, generators, loads).
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct Network {
    pub(crate) inner: Arc<surge_network::Network>,
    /// OLTC tap controls registered via `add_oltc_control()`.
    pub(crate) oltc_controls: Vec<surge_network::network::discrete_control::OltcControl>,
    /// Switched shunt controls registered via `add_switched_shunt()`.
    pub(crate) switched_shunts: Vec<surge_network::network::SwitchedShunt>,
}

#[pymethods]
impl Network {
    /// Create an empty network.
    ///
    /// Parameters
    /// ----------
    /// name : str, optional
    ///     Network name (default ``""``).
    /// base_mva : float, optional
    ///     System base MVA (default ``100.0``).
    /// freq_hz : float, optional
    ///     Nominal frequency in Hz (default ``60.0``).
    #[new]
    #[pyo3(signature = (name="", base_mva=100.0, freq_hz=60.0))]
    fn py_new(name: &str, base_mva: f64, freq_hz: f64) -> Self {
        let mut net = surge_network::Network::new(name);
        net.base_mva = base_mva;
        net.freq_hz = freq_hz;
        Network {
            inner: Arc::new(net),
            oltc_controls: Vec::new(),
            switched_shunts: Vec::new(),
        }
    }

    /// Network name.
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    /// Set the network name.
    #[setter]
    fn set_name(&mut self, name: &str) {
        Arc::make_mut(&mut self.inner).name = name.to_string();
    }

    /// System base MVA.
    #[getter]
    fn base_mva(&self) -> f64 {
        self.inner.base_mva
    }

    /// Set system base MVA.
    #[setter]
    fn set_base_mva(&mut self, base_mva: f64) {
        Arc::make_mut(&mut self.inner).base_mva = base_mva;
    }

    /// Nominal system frequency (Hz).
    #[getter]
    fn freq_hz(&self) -> f64 {
        self.inner.freq_hz
    }

    /// Set nominal system frequency (Hz).
    #[setter]
    fn set_freq_hz(&mut self, freq_hz: f64) {
        Arc::make_mut(&mut self.inner).freq_hz = freq_hz;
    }

    /// Validate the network model (bus/branch/generator consistency).
    ///
    /// Raises ``ValueError`` with a descriptive message if validation fails.
    pub(crate) fn validate(&self) -> PyResult<()> {
        self.inner.validate().map_err(to_network_pyerr)
    }

    /// Build the bus admittance matrix (Y-bus) as a sparse CSC complex matrix.
    ///
    /// Returns a ``YBusResult`` with CSC arrays (indptr, indices, data) and
    /// a ``to_scipy()`` method to convert to ``scipy.sparse.csc_matrix``.
    ///
    /// The Y-bus is an n×n complex matrix where Y[i,j] = G[i,j] + jB[i,j].
    fn ybus(&self, py: Python<'_>) -> PyResult<YBusResult> {
        use num_complex::Complex64;

        let net = Arc::clone(&self.inner);
        let ybus = py
            .detach(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    surge_ac::matrix::ybus::build_ybus(&net)
                }))
                .map_err(|e| format!("Y-bus build failed: {}", extract_panic_msg(e)))
            })
            .map_err(to_pyerr)?;

        let n = ybus.n;
        let nnz = ybus.nnz;

        // Convert CSR (separate G/B) to CSC (complex)
        // Step 1: Count entries per column
        let mut col_count = vec![0usize; n + 1];
        for i in 0..n {
            let row = ybus.row(i);
            for &j in row.col_idx {
                col_count[j + 1] += 1;
            }
        }
        // Step 2: Prefix sum → column pointers
        for j in 0..n {
            col_count[j + 1] += col_count[j];
        }
        // Step 3: Fill CSC arrays
        let mut csc_row_idx = vec![0i64; nnz];
        let mut csc_data = vec![Complex64::new(0.0, 0.0); nnz];
        let mut offset = col_count.clone();
        for i in 0..n {
            let row = ybus.row(i);
            for (k, &j) in row.col_idx.iter().enumerate() {
                let pos = offset[j];
                csc_row_idx[pos] = i as i64;
                csc_data[pos] = Complex64::new(row.g[k], row.b[k]);
                offset[j] += 1;
            }
        }
        let col_ptr: Vec<i64> = col_count.iter().map(|&v| v as i64).collect();

        let bus_numbers: Vec<u32> = self.inner.buses.iter().map(|b| b.number).collect();

        Ok(YBusResult {
            col_ptr,
            row_idx: csc_row_idx,
            data: csc_data,
            n,
            bus_numbers_vec: bus_numbers,
        })
    }

    /// Build the power flow Jacobian at the given voltage state.
    ///
    /// The Jacobian J = [H N; M L] has dimensions (n_pvpq + n_pq) × (n_pvpq + n_pq).
    /// Rows: [ΔP for PV+PQ buses, ΔQ for PQ buses].
    /// Columns: [Δθ for PV+PQ buses, ΔVm for PQ buses].
    ///
    /// Args:
    ///     vm: Bus voltage magnitudes (p.u.), length n_buses.
    ///     va: Bus voltage angles (radians), length n_buses.
    ///
    /// Returns:
    ///     JacobianResult with sparse CSC arrays and bus classification metadata.
    fn jacobian<'py>(
        &self,
        py: Python<'py>,
        voltage_magnitude_pu: numpy::PyReadonlyArray1<'py, f64>,
        va_rad: numpy::PyReadonlyArray1<'py, f64>,
    ) -> PyResult<JacobianResult> {
        let vm_slice = voltage_magnitude_pu.as_slice()?;
        let va_slice = va_rad.as_slice()?;
        let n = self.inner.n_buses();
        if vm_slice.len() != n || va_slice.len() != n {
            return Err(PyValueError::new_err(format!(
                "vm and va_rad must have length {} (n_buses), got {} and {}",
                n,
                vm_slice.len(),
                va_slice.len()
            )));
        }
        let vm_vec: Vec<f64> = vm_slice.to_vec();
        let va_vec: Vec<f64> = va_slice.to_vec();

        let net = Arc::clone(&self.inner);
        let (col_ptrs, row_indices, values, pvpq, pq) = py
            .detach(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // Build Y-bus
                    let ybus = surge_ac::matrix::ybus::build_ybus(&net);

                    // Classify buses: pvpq = PV+PQ indices, pq = PQ indices
                    let mut pv_idx: Vec<usize> = Vec::new();
                    let mut pq_idx: Vec<usize> = Vec::new();
                    for (i, bus) in net.buses.iter().enumerate() {
                        match bus.bus_type {
                            BusType::PV => pv_idx.push(i),
                            BusType::PQ => pq_idx.push(i),
                            _ => {} // Slack excluded
                        }
                    }
                    let mut pvpq: Vec<usize> = Vec::with_capacity(pv_idx.len() + pq_idx.len());
                    pvpq.extend(&pv_idx);
                    pvpq.extend(&pq_idx);
                    pvpq.sort_unstable();

                    // Compute power injections
                    let (p_calc, q_calc) = surge_ac::matrix::mismatch::compute_power_injection(
                        &ybus, &vm_vec, &va_vec,
                    );

                    // Build Jacobian (CSC via faer)
                    let jac = surge_ac::matrix::jacobian::build_jacobian(
                        &ybus, &vm_vec, &va_vec, &p_calc, &q_calc, &pvpq, &pq_idx,
                    );

                    // Extract CSC components from faer SparseColMat
                    let jac_ref = jac.as_ref();
                    let symbolic = jac_ref.symbolic();
                    let cp: Vec<i64> = symbolic.col_ptr().iter().map(|&v| v as i64).collect();
                    let ri: Vec<i64> = symbolic.row_idx().iter().map(|&v| v as i64).collect();
                    let vals: Vec<f64> = jac_ref.val().to_vec();

                    Ok::<_, String>((cp, ri, vals, pvpq, pq_idx))
                }))
                .map_err(|e| format!("Jacobian build failed: {}", extract_panic_msg(e)))
                .and_then(|r| r)
            })
            .map_err(to_pyerr)?;

        let dim = pvpq.len() + pq.len();

        // Convert internal indices to external bus numbers
        let buses = &self.inner.buses;
        let pvpq_buses: Vec<u32> = pvpq.iter().map(|&i| buses[i].number).collect();
        let pq_buses: Vec<u32> = pq.iter().map(|&i| buses[i].number).collect();

        Ok(JacobianResult {
            col_ptr: col_ptrs,
            row_idx: row_indices,
            data: values,
            nrows: dim,
            ncols: dim,
            pvpq_buses_vec: pvpq_buses,
            pq_buses_vec: pq_buses,
        })
    }

    /// Number of buses.
    #[getter]
    fn n_buses(&self) -> usize {
        self.inner.n_buses()
    }

    /// Number of branches.
    #[getter]
    fn n_branches(&self) -> usize {
        self.inner.n_branches()
    }

    /// Number of generators.
    #[getter]
    fn n_generators(&self) -> usize {
        self.inner.generators.len()
    }

    /// Total active generation (MW).
    #[getter]
    fn total_generation_mw(&self) -> f64 {
        self.inner.total_generation_mw()
    }

    /// Total active load (MW).
    #[getter]
    fn total_load_mw(&self) -> f64 {
        self.inner.total_load_mw()
    }

    /// Bus numbers as a list.
    #[getter]
    fn bus_numbers(&self) -> Vec<u32> {
        self.inner.buses.iter().map(|b| b.number).collect()
    }

    /// Bus active power demand (MW) as a list.
    #[getter]
    fn bus_pd(&self) -> Vec<f64> {
        self.inner.bus_load_p_mw()
    }

    /// Bus voltage magnitudes (p.u.) as a list.
    #[getter]
    fn bus_vm(&self) -> Vec<f64> {
        self.inner
            .buses
            .iter()
            .map(|b| b.voltage_magnitude_pu)
            .collect()
    }

    /// Bus geographic coordinates as `(latitude, longitude)` pairs in decimal degrees (WGS84).
    /// Returns `None` for buses where coordinates are unavailable.
    /// Populated by the PSS/E, CGMES, and XIIDM parsers from coordinates embedded in the
    /// case file, or estimated by a force-directed layout algorithm when coordinates are absent.
    #[getter]
    fn bus_coordinates(&self) -> Vec<Option<(f64, f64)>> {
        self.inner
            .buses
            .iter()
            .map(|b| b.latitude.zip(b.longitude))
            .collect()
    }

    /// Generator bus numbers.
    #[getter]
    fn gen_buses(&self) -> Vec<u32> {
        self.inner.generators.iter().map(|g| g.bus).collect()
    }

    /// Canonical generator identifiers in model order.
    #[getter]
    fn generator_ids(&self) -> Vec<String> {
        self.inner
            .generators
            .iter()
            .map(|generator| generator.id.clone())
            .collect()
    }

    /// Generator active power output (MW).
    #[getter]
    fn gen_p(&self) -> Vec<f64> {
        self.inner.generators.iter().map(|g| g.p).collect()
    }

    /// Generator Pmax (MW).
    #[getter]
    fn gen_pmax(&self) -> Vec<f64> {
        self.inner.generators.iter().map(|g| g.pmax).collect()
    }

    /// Generator Pmin (MW).
    #[getter]
    fn gen_pmin(&self) -> Vec<f64> {
        self.inner.generators.iter().map(|g| g.pmin).collect()
    }

    /// Generator in-service status.
    #[getter]
    fn gen_in_service(&self) -> Vec<bool> {
        self.inner.generators.iter().map(|g| g.in_service).collect()
    }

    /// Generator machine base MVA.
    #[getter]
    fn gen_mbase(&self) -> Vec<f64> {
        self.inner
            .generators
            .iter()
            .map(|g| g.machine_base_mva)
            .collect()
    }

    /// Branch from-bus numbers.
    #[getter]
    fn branch_from(&self) -> Vec<u32> {
        self.inner.branches.iter().map(|b| b.from_bus).collect()
    }

    /// Branch to-bus numbers.
    #[getter]
    fn branch_to(&self) -> Vec<u32> {
        self.inner.branches.iter().map(|b| b.to_bus).collect()
    }

    /// Branch thermal ratings (MVA).
    #[getter]
    fn branch_rate_a(&self) -> Vec<f64> {
        self.inner.branches.iter().map(|b| b.rating_a_mva).collect()
    }

    /// Per-unit resistance of each branch (R in p.u.).
    #[getter]
    fn branch_r(&self) -> Vec<f64> {
        self.inner.branches.iter().map(|b| b.r).collect()
    }

    /// Per-unit reactance of each branch (X in p.u.).
    #[getter]
    fn branch_x(&self) -> Vec<f64> {
        self.inner.branches.iter().map(|b| b.x).collect()
    }

    // -----------------------------------------------------------------------
    // Phase 1: PSS/E data-model read-back properties
    // -----------------------------------------------------------------------

    // --- Bus arrays ---

    /// Area number for each bus (u32, in network.buses order).
    #[getter]
    fn bus_area(&self) -> Vec<u32> {
        self.inner.buses.iter().map(|b| b.area).collect()
    }

    /// Zone number for each bus (u32, in network.buses order).
    #[getter]
    fn bus_zone(&self) -> Vec<u32> {
        self.inner.buses.iter().map(|b| b.zone).collect()
    }

    /// Base voltage (kV) for each bus.
    #[getter]
    fn bus_base_kv(&self) -> Vec<f64> {
        self.inner.buses.iter().map(|b| b.base_kv).collect()
    }

    /// Name string for each bus.
    #[getter]
    fn bus_name(&self) -> Vec<String> {
        self.inner.buses.iter().map(|b| b.name.clone()).collect()
    }

    /// Bus type string for each bus: "PQ", "PV", "Slack", or "Isolated".
    #[getter]
    fn bus_type_str(&self) -> Vec<String> {
        self.inner
            .buses
            .iter()
            .map(|b| match b.bus_type {
                BusType::PQ => "PQ".to_string(),
                BusType::PV => "PV".to_string(),
                BusType::Slack => "Slack".to_string(),
                BusType::Isolated => "Isolated".to_string(),
            })
            .collect()
    }

    /// Reactive power demand (MVAr) for each bus.
    #[getter]
    fn bus_qd(&self) -> Vec<f64> {
        self.inner.bus_load_q_mvar()
    }

    /// Minimum voltage limit (p.u.) for each bus.
    #[getter]
    fn bus_vmin(&self) -> Vec<f64> {
        self.inner.buses.iter().map(|b| b.voltage_min_pu).collect()
    }

    /// Maximum voltage limit (p.u.) for each bus.
    #[getter]
    fn bus_vmax(&self) -> Vec<f64> {
        self.inner.buses.iter().map(|b| b.voltage_max_pu).collect()
    }

    /// Shunt conductance (MW at 1.0 p.u.) for each bus.
    #[getter]
    fn bus_gs(&self) -> Vec<f64> {
        self.inner
            .buses
            .iter()
            .map(|b| b.shunt_conductance_mw)
            .collect()
    }

    /// Shunt susceptance (MVAr at 1.0 p.u.) for each bus.
    #[getter]
    fn bus_bs(&self) -> Vec<f64> {
        self.inner
            .buses
            .iter()
            .map(|b| b.shunt_susceptance_mvar)
            .collect()
    }

    // --- Branch arrays ---

    /// Line charging susceptance (p.u.) for each branch.
    #[getter]
    fn branch_b(&self) -> Vec<f64> {
        self.inner.branches.iter().map(|b| b.b).collect()
    }

    /// Short-term thermal rating (MVA) for each branch.
    #[getter]
    fn branch_rate_b(&self) -> Vec<f64> {
        self.inner.branches.iter().map(|b| b.rating_b_mva).collect()
    }

    /// Emergency thermal rating (MVA) for each branch.
    #[getter]
    fn branch_rate_c(&self) -> Vec<f64> {
        self.inner.branches.iter().map(|b| b.rating_c_mva).collect()
    }

    /// In-service status for each branch.
    #[getter]
    fn branch_in_service(&self) -> Vec<bool> {
        self.inner.branches.iter().map(|b| b.in_service).collect()
    }

    /// Transformer tap ratio (1.0 for lines) for each branch.
    #[getter]
    fn branch_tap(&self) -> Vec<f64> {
        self.inner.branches.iter().map(|b| b.tap).collect()
    }

    /// Phase shift angle (degrees) for each branch.
    #[getter]
    fn branch_shift_deg(&self) -> Vec<f64> {
        self.inner
            .branches
            .iter()
            .map(|b| b.phase_shift_rad.to_degrees())
            .collect()
    }

    /// Parallel circuit identifier for each branch.
    #[getter]
    fn branch_circuit(&self) -> Vec<String> {
        self.inner
            .branches
            .iter()
            .map(|b| b.circuit.clone())
            .collect()
    }

    /// Rated MVAr of series compensation element (None if not a series-
    /// compensated branch). Positive = capacitive, negative = inductive.
    #[getter]
    fn branch_rated_mvar_series(&self) -> Vec<Option<f64>> {
        self.inner
            .branches
            .iter()
            .map(|b| b.series_comp.as_ref().and_then(|s| s.rated_mvar_series))
            .collect()
    }

    /// Whether each branch's series compensator is currently bypassed.
    #[getter]
    fn branch_bypassed(&self) -> Vec<bool> {
        self.inner
            .branches
            .iter()
            .map(|b| b.series_comp.as_ref().is_some_and(|s| s.bypassed))
            .collect()
    }

    /// Bypass current threshold (kA) for series capacitor protection.
    #[getter]
    fn branch_bypass_current_ka(&self) -> Vec<Option<f64>> {
        self.inner
            .branches
            .iter()
            .map(|b| b.series_comp.as_ref().and_then(|s| s.bypass_current_ka))
            .collect()
    }

    // --- Generator arrays ---

    /// Maximum reactive power (MVAr) for each generator.
    #[getter]
    fn gen_qmax(&self) -> Vec<f64> {
        self.inner.generators.iter().map(|g| g.qmax).collect()
    }

    /// Minimum reactive power (MVAr) for each generator.
    #[getter]
    fn gen_qmin(&self) -> Vec<f64> {
        self.inner.generators.iter().map(|g| g.qmin).collect()
    }

    /// Voltage setpoint (p.u.) for each generator.
    #[getter]
    fn gen_vs_pu(&self) -> Vec<f64> {
        self.inner
            .generators
            .iter()
            .map(|g| g.voltage_setpoint_pu)
            .collect()
    }

    /// Machine ID string for each generator (defaults to "1" if unset).
    #[getter]
    fn gen_machine_id(&self) -> Vec<String> {
        self.inner
            .generators
            .iter()
            .map(|g| g.machine_id.clone().unwrap_or_else(|| "1".to_string()))
            .collect()
    }

    /// Reactive power output (MVAr) stored in the model for each generator.
    /// This is the initial/scheduled value, NOT post-power-flow computed Qg.
    /// For post-solve per-generator Q use AcPfResult.gen_q_mvar.
    #[getter]
    fn gen_q(&self) -> Vec<f64> {
        self.inner.generators.iter().map(|g| g.q).collect()
    }

    // --- Tabular methods ---

    /// Return a pandas DataFrame of buses (or dict if pandas is not installed).
    ///
    /// Columns: bus_id, name, type, base_kv, area, zone, pd_mw, qd_mvar,
    ///          gs_mw, bs_mvar, vmin_pu, vmax_pu, vm_pu, va_deg, latitude, longitude.
    fn bus_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        let n = self.inner.buses.len();
        let mut bus_id = Vec::with_capacity(n);
        let mut name = Vec::with_capacity(n);
        let mut type_str = Vec::with_capacity(n);
        let mut base_kv = Vec::with_capacity(n);
        let mut area = Vec::with_capacity(n);
        let mut zone = Vec::with_capacity(n);
        let mut pd_mw = Vec::with_capacity(n);
        let mut qd_mvar = Vec::with_capacity(n);
        let mut gs_mw = Vec::with_capacity(n);
        let mut bs_mvar = Vec::with_capacity(n);
        let mut vmin_pu = Vec::with_capacity(n);
        let mut vmax_pu = Vec::with_capacity(n);
        let mut vm_pu = Vec::with_capacity(n);
        let mut va_deg = Vec::with_capacity(n);
        let mut lat: Vec<Option<f64>> = Vec::with_capacity(n);
        let mut lon: Vec<Option<f64>> = Vec::with_capacity(n);
        let load_p_vec = self.inner.bus_load_p_mw();
        let load_q_vec = self.inner.bus_load_q_mvar();
        for (i, b) in self.inner.buses.iter().enumerate() {
            bus_id.push(b.number);
            name.push(b.name.clone());
            type_str.push(match b.bus_type {
                BusType::PQ => "PQ",
                BusType::PV => "PV",
                BusType::Slack => "Slack",
                BusType::Isolated => "Isolated",
            });
            base_kv.push(b.base_kv);
            area.push(b.area);
            zone.push(b.zone);
            pd_mw.push(load_p_vec.get(i).copied().unwrap_or(0.0));
            qd_mvar.push(load_q_vec.get(i).copied().unwrap_or(0.0));
            gs_mw.push(b.shunt_conductance_mw);
            bs_mvar.push(b.shunt_susceptance_mvar);
            vmin_pu.push(b.voltage_min_pu);
            vmax_pu.push(b.voltage_max_pu);
            vm_pu.push(b.voltage_magnitude_pu);
            va_deg.push(b.voltage_angle_rad.to_degrees());
            lat.push(b.latitude);
            lon.push(b.longitude);
        }
        dict.set_item("bus_id", bus_id)?;
        dict.set_item("name", name)?;
        dict.set_item("type", type_str)?;
        dict.set_item("base_kv", base_kv)?;
        dict.set_item("area", area)?;
        dict.set_item("zone", zone)?;
        dict.set_item("pd_mw", pd_mw)?;
        dict.set_item("qd_mvar", qd_mvar)?;
        dict.set_item("gs_mw", gs_mw)?;
        dict.set_item("bs_mvar", bs_mvar)?;
        dict.set_item("vmin_pu", vmin_pu)?;
        dict.set_item("vmax_pu", vmax_pu)?;
        dict.set_item("vm_pu", vm_pu)?;
        dict.set_item("va_deg", va_deg)?;
        dict.set_item("latitude", lat)?;
        dict.set_item("longitude", lon)?;
        dict_to_dataframe_with_index(py, dict, &["bus_id"])
    }

    /// Return a pandas DataFrame of branches (or dict if pandas is not installed).
    ///
    /// Columns: from_bus, to_bus, circuit, r, x, b, rate_a_mva, rate_b_mva,
    ///          rate_c_mva, tap, shift_deg, in_service.
    fn branch_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        let n = self.inner.branches.len();
        let mut from_bus = Vec::with_capacity(n);
        let mut to_bus = Vec::with_capacity(n);
        let mut circuit = Vec::with_capacity(n);
        let mut r = Vec::with_capacity(n);
        let mut x = Vec::with_capacity(n);
        let mut b = Vec::with_capacity(n);
        let mut rate_a = Vec::with_capacity(n);
        let mut rate_b = Vec::with_capacity(n);
        let mut rate_c = Vec::with_capacity(n);
        let mut tap = Vec::with_capacity(n);
        let mut shift_deg = Vec::with_capacity(n);
        let mut in_service = Vec::with_capacity(n);
        for br in &self.inner.branches {
            from_bus.push(br.from_bus);
            to_bus.push(br.to_bus);
            circuit.push(br.circuit.clone());
            r.push(br.r);
            x.push(br.x);
            b.push(br.b);
            rate_a.push(br.rating_a_mva);
            rate_b.push(br.rating_b_mva);
            rate_c.push(br.rating_c_mva);
            tap.push(br.tap);
            shift_deg.push(br.phase_shift_rad.to_degrees());
            in_service.push(br.in_service);
        }
        dict.set_item("from_bus", from_bus)?;
        dict.set_item("to_bus", to_bus)?;
        dict.set_item("circuit", circuit)?;
        dict.set_item("r", r)?;
        dict.set_item("x", x)?;
        dict.set_item("b", b)?;
        dict.set_item("rate_a_mva", rate_a)?;
        dict.set_item("rate_b_mva", rate_b)?;
        dict.set_item("rate_c_mva", rate_c)?;
        dict.set_item("tap", tap)?;
        dict.set_item("shift_deg", shift_deg)?;
        dict.set_item("in_service", in_service)?;
        dict_to_dataframe_with_index(py, dict, &["from_bus", "to_bus"])
    }

    /// Return a pandas DataFrame of generators (or dict if pandas is not installed).
    ///
    /// Index: MultiIndex (bus_id, machine_id).
    /// Columns: gen_idx, p_mw, q_mvar, pmax_mw, pmin_mw,
    ///          qmax_mvar, qmin_mvar, vs_pu, in_service, fuel_type.
    fn gen_dataframe<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        let n = self.inner.generators.len();
        let mut bus_id = Vec::with_capacity(n);
        let mut machine_id = Vec::with_capacity(n);
        let mut gen_idx: Vec<usize> = Vec::with_capacity(n);
        let mut p_mw = Vec::with_capacity(n);
        let mut q_mvar = Vec::with_capacity(n);
        let mut pmax_mw = Vec::with_capacity(n);
        let mut pmin_mw = Vec::with_capacity(n);
        let mut qmax_mvar = Vec::with_capacity(n);
        let mut qmin_mvar = Vec::with_capacity(n);
        let mut vs_pu = Vec::with_capacity(n);
        let mut in_service = Vec::with_capacity(n);
        let mut fuel_type = Vec::with_capacity(n);
        for (i, g) in self.inner.generators.iter().enumerate() {
            bus_id.push(g.bus);
            machine_id.push(g.machine_id.clone().unwrap_or_else(|| "1".to_string()));
            gen_idx.push(i);
            p_mw.push(g.p);
            q_mvar.push(g.q);
            pmax_mw.push(g.pmax);
            pmin_mw.push(g.pmin);
            qmax_mvar.push(g.qmax);
            qmin_mvar.push(g.qmin);
            vs_pu.push(g.voltage_setpoint_pu);
            in_service.push(g.in_service);
            fuel_type.push(
                g.fuel
                    .as_ref()
                    .and_then(|f| f.fuel_type.clone())
                    .unwrap_or_default(),
            );
        }
        dict.set_item("bus_id", bus_id)?;
        dict.set_item("machine_id", machine_id)?;
        dict.set_item("gen_idx", gen_idx)?;
        dict.set_item("p_mw", p_mw)?;
        dict.set_item("q_mvar", q_mvar)?;
        dict.set_item("pmax_mw", pmax_mw)?;
        dict.set_item("pmin_mw", pmin_mw)?;
        dict.set_item("qmax_mvar", qmax_mvar)?;
        dict.set_item("qmin_mvar", qmin_mvar)?;
        dict.set_item("vs_pu", vs_pu)?;
        dict.set_item("in_service", in_service)?;
        dict.set_item("fuel_type", fuel_type)?;
        dict_to_dataframe_with_index(py, dict, &["bus_id", "machine_id"])
    }

    /// Detect electrically connected islands in the network.
    ///
    /// Returns the connected components of the in-service branch graph.
    /// Each element is a list of external bus numbers belonging to the same island.
    /// Isolated buses (no in-service neighbors) appear as single-element lists.
    ///
    /// Example:
    ///   ``[[1, 2, 3, 4, 5], [6, 7], [8]]`` → 3 islands
    fn islands(&self) -> Vec<Vec<u32>> {
        let bus_map = self.inner.bus_index_map();
        let info = surge_topology::islands::detect_islands(&self.inner, &bus_map);
        info.components
            .into_iter()
            .map(|component| {
                component
                    .into_iter()
                    .map(|idx| self.inner.buses[idx].number)
                    .collect()
            })
            .collect()
    }

    /// Compute area net interchange (MW) from a power flow solution.
    ///
    /// Returns a dict mapping area number → net MW export for that area.
    /// Net export = sum of from-end flows on branches crossing area boundaries
    /// (positive = exporting, negative = importing).
    fn area_schedule_mw<'py>(
        &self,
        py: Python<'py>,
        solution: &AcPfResult,
    ) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        // Build bus→area map
        let bus_area: std::collections::HashMap<u32, u32> = self
            .inner
            .buses
            .iter()
            .map(|b| (b.number, b.area))
            .collect();
        // Accumulate net export per area from cross-area branch flows
        let mut area_export: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
        let pq_flows = solution.inner.branch_pq_flows();
        for (br, (pf, _qf)) in self.inner.branches.iter().zip(pq_flows.iter()) {
            if !br.in_service {
                continue;
            }
            let area_f = bus_area.get(&br.from_bus).copied().unwrap_or(0);
            let area_t = bus_area.get(&br.to_bus).copied().unwrap_or(0);
            if area_f == area_t {
                continue; // intra-area branch
            }
            // pf is from-end flow in MW; positive = flowing from_bus→to_bus
            // from-area exports +pf, to-area imports +pf (exports -pf)
            *area_export.entry(area_f).or_insert(0.0) += pf;
            *area_export.entry(area_t).or_insert(0.0) -= pf;
        }
        for (area, export_mw) in &area_export {
            dict.set_item(area, export_mw)?;
        }
        Ok(dict)
    }

    /// Compare this network against another and return the differences.
    ///
    /// Returns a dict with keys ``'buses'``, ``'branches'``, ``'generators'``,
    /// each containing a list of change dicts with ``kind``, and per-field
    /// ``(old, new)`` tuples for modified elements. Uses the structured diff
    /// engine from ``surge_network::network::case_diff``.
    fn compare_with<'py>(&self, py: Python<'py>, other: &Network) -> PyResult<Bound<'py, PyDict>> {
        use surge_network::network::case_diff::{DiffKind, diff_networks};

        let diff = diff_networks(&self.inner, &other.inner);
        let kind_str = |k: DiffKind| match k {
            DiffKind::Added => "added",
            DiffKind::Removed => "removed",
            DiffKind::Modified => "modified",
        };
        let opt_f64 = |py: Python<'py>,
                       v: &Option<(f64, f64)>|
         -> PyResult<Option<Bound<'py, pyo3::types::PyTuple>>> {
            match v {
                Some((a, b)) => Ok(Some(pyo3::types::PyTuple::new(py, [*a, *b])?)),
                None => Ok(None),
            }
        };

        let result = PyDict::new(py);

        // Buses
        let bus_list: Vec<Py<PyAny>> = diff
            .bus_diffs
            .iter()
            .map(|bd| {
                let d = PyDict::new(py);
                d.set_item("bus_id", bd.bus_number).ok();
                d.set_item("kind", kind_str(bd.kind)).ok();
                if let Some(ref v) = bd.bus_type {
                    d.set_item("bus_type", (&v.0, &v.1)).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &bd.voltage_magnitude_pu) {
                    d.set_item("vm_pu", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &bd.voltage_angle_rad) {
                    d.set_item("va_rad", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &bd.base_kv) {
                    d.set_item("base_kv", t).ok();
                }
                d.into_any().unbind()
            })
            .collect();
        result.set_item("buses", bus_list)?;

        // Branches
        let branch_list: Vec<Py<PyAny>> = diff
            .branch_diffs
            .iter()
            .map(|bd| {
                let d = PyDict::new(py);
                d.set_item("from_bus", bd.from_bus).ok();
                d.set_item("to_bus", bd.to_bus).ok();
                d.set_item("circuit", &bd.circuit).ok();
                d.set_item("kind", kind_str(bd.kind)).ok();
                if let Ok(Some(t)) = opt_f64(py, &bd.r) {
                    d.set_item("r", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &bd.x) {
                    d.set_item("x", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &bd.b) {
                    d.set_item("b", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &bd.rating_a_mva) {
                    d.set_item("rate_a_mva", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &bd.tap) {
                    d.set_item("tap", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &bd.phase_shift_rad) {
                    d.set_item("shift_rad", t).ok();
                }
                if let Some((a, b)) = bd.in_service {
                    d.set_item("in_service", (a, b)).ok();
                }
                d.into_any().unbind()
            })
            .collect();
        result.set_item("branches", branch_list)?;

        // Generators (new — was missing from the old implementation)
        let gen_list: Vec<Py<PyAny>> = diff
            .gen_diffs
            .iter()
            .map(|gd| {
                let d = PyDict::new(py);
                d.set_item("bus", gd.bus).ok();
                d.set_item("id", &gd.id).ok();
                d.set_item("kind", kind_str(gd.kind)).ok();
                if let Ok(Some(t)) = opt_f64(py, &gd.p) {
                    d.set_item("p", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &gd.q) {
                    d.set_item("q", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &gd.pmin) {
                    d.set_item("pmin", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &gd.pmax) {
                    d.set_item("pmax", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &gd.qmin) {
                    d.set_item("qmin", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &gd.qmax) {
                    d.set_item("qmax", t).ok();
                }
                if let Some((a, b)) = gd.in_service {
                    d.set_item("in_service", (a, b)).ok();
                }
                if let Some(ref v) = gd.cost {
                    d.set_item("cost", (&v.0, &v.1)).ok();
                }
                d.into_any().unbind()
            })
            .collect();
        result.set_item("generators", gen_list)?;

        // Loads
        let load_list: Vec<Py<PyAny>> = diff
            .load_diffs
            .iter()
            .map(|ld| {
                let d = PyDict::new(py);
                d.set_item("bus", ld.bus).ok();
                d.set_item("id", &ld.id).ok();
                d.set_item("kind", kind_str(ld.kind)).ok();
                if let Ok(Some(t)) = opt_f64(py, &ld.active_power_demand_mw) {
                    d.set_item("pd_mw", t).ok();
                }
                if let Ok(Some(t)) = opt_f64(py, &ld.reactive_power_demand_mvar) {
                    d.set_item("qd_mvar", t).ok();
                }
                if let Some((a, b)) = ld.in_service {
                    d.set_item("in_service", (a, b)).ok();
                }
                d.into_any().unbind()
            })
            .collect();
        result.set_item("loads", load_list)?;

        result.set_item("summary", &diff.summary)?;
        Ok(result)
    }

    /// Return the P-Q capability curve for a generator.
    ///
    /// Returns a list of ``(p_pu, qmax_pu, qmin_pu)`` tuples from the
    /// D-curve stored in the generator model.  Returns ``[]`` if the
    /// generator has no capability curve data.
    ///
    /// Args:
    ///   id: Canonical generator identifier.
    fn generator_capability_curve(&self, id: &str) -> PyResult<Vec<(f64, f64, f64)>> {
        let r#gen = self
            .inner
            .generators
            .iter()
            .find(|g| g.id == id)
            .ok_or_else(|| NetworkError::new_err(format!("Generator id='{id}' not found")))?;
        Ok(r#gen
            .reactive_capability
            .as_ref()
            .map(|r| r.pq_curve.clone())
            .unwrap_or_default())
    }

    /// Return external bus numbers matching all specified filters.
    ///
    /// All filters are optional; omitting a filter means "no constraint".
    /// Returns external bus numbers in the same order as `network.bus_numbers`.
    ///
    /// Args:
    ///   area: Include only buses whose area number is in this list.
    ///   zone: Include only buses whose zone number is in this list.
    ///   kv_min: Include only buses with base_kv >= kv_min.
    ///   kv_max: Include only buses with base_kv <= kv_max.
    ///   bus_type: Include only buses of this type ("PQ"/"PV"/"Slack"/"Isolated").
    #[pyo3(signature = (area=None, zone=None, kv_min=None, kv_max=None, bus_type=None))]
    fn find_bus_numbers(
        &self,
        area: Option<Vec<u32>>,
        zone: Option<Vec<u32>>,
        kv_min: Option<f64>,
        kv_max: Option<f64>,
        bus_type: Option<&str>,
    ) -> Vec<u32> {
        let area_set: Option<std::collections::HashSet<u32>> =
            area.map(|v| v.into_iter().collect());
        let zone_set: Option<std::collections::HashSet<u32>> =
            zone.map(|v| v.into_iter().collect());
        self.inner
            .buses
            .iter()
            .filter(|b| {
                if let Some(ref s) = area_set
                    && !s.contains(&b.area)
                {
                    return false;
                }
                if let Some(ref s) = zone_set
                    && !s.contains(&b.zone)
                {
                    return false;
                }
                if let Some(lo) = kv_min
                    && b.base_kv < lo
                {
                    return false;
                }
                if let Some(hi) = kv_max
                    && b.base_kv > hi
                {
                    return false;
                }
                if let Some(t) = bus_type {
                    let matches = match b.bus_type {
                        BusType::PQ => t == "PQ",
                        BusType::PV => t == "PV",
                        BusType::Slack => t == "Slack",
                        BusType::Isolated => t == "Isolated",
                    };
                    if !matches {
                        return false;
                    }
                }
                true
            })
            .map(|b| b.number)
            .collect()
    }

    // ───────────────────────────────────────────────────────────────────────
    // Rich object accessors
    // ───────────────────────────────────────────────────────────────────────

    /// All buses as a list of `Bus` objects (one per bus in model order).
    ///
    /// Each `Bus` object exposes all static model fields (number, name, type,
    /// base_kv, pd_mw, qd_mvar, area, zone, vmin_pu, vmax_pu, etc.) as
    /// readable properties, plus computed properties (`is_slack`, `is_pv`, etc.).
    ///
    /// Example::
    ///
    ///     for bus in net.buses:
    ///         print(bus.number, bus.name, bus.base_kv, bus.pd_mw)
    ///
    ///     slack = net.slack_bus
    ///     high_kv = [b for b in net.buses if b.base_kv >= 345.0]
    #[getter]
    fn buses(&self) -> Vec<rich_objects::Bus> {
        rich_objects::buses_from_network(&self.inner)
    }

    /// All branches as a list of `Branch` objects (one per branch in model order).
    ///
    /// Each `Branch` exposes all static model fields (from_bus, to_bus, circuit,
    /// r_pu, x_pu, b_pu, rate_a_mva, tap, shift_deg, in_service, etc.) as
    /// readable properties, plus computed properties (`is_transformer`, `b_dc_pu`, etc.).
    ///
    /// Example::
    ///
    ///     lines = [br for br in net.branches if not br.is_transformer]
    ///     overloaded_model = [br for br in net.branches if br.pf_mw and br.pf_mw > br.rate_a_mva]
    #[getter]
    fn branches(&self) -> Vec<rich_objects::Branch> {
        rich_objects::branches_from_network(&self.inner)
    }

    /// All generators as a list of `Generator` objects (one per generator in model order).
    ///
    /// Each `Generator` exposes all static model fields (bus, machine_id, p_mw,
    /// pmax_mw, pmin_mw, qmax_mvar, qmin_mvar, fuel_type, ramp rates, ancillary
    /// service capabilities, cost curve, etc.).
    ///
    /// Example::
    ///
    ///     gas_gens = [g for g in net.generators if g.fuel_type == "gas"]
    ///     fast_ramp = [g for g in net.generators if (g.ramp_up_mw_per_min or 0) > 10]
    #[getter]
    fn generators(&self) -> Vec<rich_objects::Generator> {
        rich_objects::generators_from_network(&self.inner)
    }

    /// All loads as a list of `Load` objects (one per Load record in model order).
    ///
    /// Note: many networks store load at the bus level (bus.pd_mw / bus.qd_mvar)
    /// rather than as explicit Load records. For bus-level loads use `net.bus_pd`
    /// or `net.buses` instead.
    #[getter]
    fn loads(&self) -> Vec<rich_objects::Load> {
        rich_objects::loads_from_network(&self.inner)
    }

    /// All pumped hydro units as a list of `PumpedHydroUnit` objects.
    #[getter]
    fn pumped_hydro_units(&self) -> Vec<rich_objects::PumpedHydroUnit> {
        rich_objects::pumped_hydro_units_from_network(&self.inner)
    }

    /// All breaker ratings as a list of `BreakerRating` objects.
    #[getter]
    fn breaker_ratings(&self) -> Vec<rich_objects::BreakerRating> {
        rich_objects::breaker_ratings_from_network(&self.inner)
    }

    /// All fixed shunts as a list of `FixedShunt` objects.
    #[getter]
    fn fixed_shunts(&self) -> Vec<rich_objects::FixedShunt> {
        rich_objects::fixed_shunts_from_network(&self.inner)
    }

    /// All combined cycle plants as a list of `CombinedCyclePlant` objects.
    #[getter]
    fn combined_cycle_plants(&self) -> Vec<rich_objects::CombinedCyclePlant> {
        rich_objects::combined_cycle_plants_from_network(&self.inner)
    }

    /// Outage schedule as a list of `OutageEntry` objects.
    #[getter]
    fn outage_entries(&self) -> Vec<rich_objects::OutageEntry> {
        rich_objects::outage_entries_from_network(&self.inner)
    }

    /// Reserve zones as a list of `ReserveZone` objects.
    #[getter]
    fn reserve_zones(&self) -> Vec<rich_objects::ReserveZone> {
        rich_objects::reserve_zones_from_network(&self.inner)
    }

    /// System-wide ambient conditions as a dict, or None.
    #[getter]
    fn ambient<'py>(&self, py: Python<'py>) -> PyResult<Option<Py<PyAny>>> {
        match &self.inner.market_data.ambient {
            None => Ok(None),
            Some(a) => {
                let d = pyo3::types::PyDict::new(py);
                d.set_item("temperature_c", a.temperature_c)?;
                d.set_item("wind_speed_m_s", a.wind_speed_m_s)?;
                d.set_item("wind_angle_deg", a.wind_angle_deg)?;
                d.set_item("solar_irradiance_w_m2", a.solar_irradiance_w_m2)?;
                d.set_item("timestamp", a.timestamp.map(|t| t.to_rfc3339()))?;
                Ok(Some(d.into_any().unbind()))
            }
        }
    }

    /// System-wide emission policy as a dict, or None.
    #[getter]
    fn emission_policy<'py>(&self, py: Python<'py>) -> PyResult<Option<Py<PyAny>>> {
        match &self.inner.market_data.emission_policy {
            None => Ok(None),
            Some(ep) => {
                let d = pyo3::types::PyDict::new(py);
                d.set_item("carbon_price", ep.carbon_price)?;
                d.set_item("co2_cap", ep.co2_cap)?;
                d.set_item("nox_cap", ep.nox_cap)?;
                d.set_item("so2_cap", ep.so2_cap)?;
                d.set_item("pm25_cap", ep.pm25_cap)?;
                d.set_item("co2_allowance_price", ep.co2_allowance_price)?;
                Ok(Some(d.into_any().unbind()))
            }
        }
    }

    /// Market rules as a dict, or None.
    #[getter]
    fn market_rules<'py>(&self, py: Python<'py>) -> PyResult<Option<Py<PyAny>>> {
        match &self.inner.market_data.market_rules {
            None => Ok(None),
            Some(mr) => {
                let d = pyo3::types::PyDict::new(py);
                d.set_item("voll", mr.voll)?;
                // Reserve products
                let products: Vec<_> = mr
                    .reserve_products
                    .iter()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect();
                d.set_item("reserve_products", products)?;
                // System reserve requirements
                let sys_reqs: Vec<_> = mr
                    .system_reserve_requirements
                    .iter()
                    .map(|r| (r.product_id.clone(), r.requirement_mw))
                    .collect();
                d.set_item("system_reserve_requirements", sys_reqs)?;
                Ok(Some(d.into_any().unbind()))
            }
        }
    }

    /// Look up a bus by external bus number.
    ///
    /// Raises ``ValueError`` if the bus number is not found.
    ///
    /// Example::
    ///
    ///     bus = net.bus(118)
    ///     print(bus.name, bus.base_kv, bus.pd_mw)
    fn bus(&self, number: u32) -> PyResult<rich_objects::Bus> {
        rich_objects::find_bus(&self.inner, number)
    }

    /// Look up a branch by (from_bus, to_bus) and optional circuit number.
    ///
    /// Also checks the reversed direction (to_bus→from_bus).
    /// Raises ``ValueError`` if no matching branch is found.
    ///
    /// Example::
    ///
    ///     br = net.branch(1, 2)
    ///     print(br.r_pu, br.x_pu, br.rate_a_mva)
    #[pyo3(signature = (from_bus, to_bus, circuit=None))]
    fn branch(
        &self,
        from_bus: u32,
        to_bus: u32,
        circuit: Option<crate::input_types::PyCircuitId>,
    ) -> PyResult<rich_objects::Branch> {
        let ckt = circuit
            .map(|c| c.into_string())
            .unwrap_or_else(|| "1".to_string());
        rich_objects::find_branch(&self.inner, from_bus, to_bus, &ckt)
    }

    /// Look up a generator by canonical generator ID.
    ///
    /// Raises ``ValueError`` if the generator is not found.
    ///
    /// Example::
    ///
    ///     gen = net.generator("gen_30_1")
    ///     print(gen.p_mw, gen.pmax_mw, gen.fuel_type)
    fn generator(&self, id: &str) -> PyResult<rich_objects::Generator> {
        rich_objects::find_generator_by_id(&self.inner, id)
    }

    /// Return the 0-based internal index of the bus with external number *number*.
    ///
    /// Use this to build index-keyed inputs (SE measurements, derate profiles, etc.)
    /// from external bus numbers. Raises ``ValueError`` if not found.
    ///
    /// Example::
    ///
    ///     idx = net.bus_index(1234)
    ///     measurements.append({"type": "v_mag", "value": 1.02, "sigma": 0.01, "bus": idx})
    fn bus_index(&self, number: u32) -> PyResult<usize> {
        self.inner
            .buses
            .iter()
            .position(|b| b.number == number)
            .ok_or_else(|| PyValueError::new_err(format!("Bus {number} not found")))
    }

    /// Return the 0-based internal index of the branch (from_bus, to_bus, circuit).
    ///
    /// Also checks the reversed direction. Raises ``ValueError`` if not found.
    ///
    /// Example::
    ///
    ///     idx = net.branch_index(1, 4)
    ///     branch_derates = {idx: [1.0, 0.0, 1.0, ...]}  # outage in hour 1
    #[pyo3(signature = (from_bus, to_bus, circuit=None))]
    fn branch_index(
        &self,
        from_bus: u32,
        to_bus: u32,
        circuit: Option<crate::input_types::PyCircuitId>,
    ) -> PyResult<usize> {
        let ckt = circuit
            .map(|c| c.into_string())
            .unwrap_or_else(|| "1".to_string());
        self.inner
            .branches
            .iter()
            .position(|br| {
                (br.from_bus == from_bus && br.to_bus == to_bus && br.circuit == ckt)
                    || (br.from_bus == to_bus && br.to_bus == from_bus && br.circuit == ckt)
            })
            .ok_or_else(|| {
                PyValueError::new_err(format!("Branch {from_bus}→{to_bus} ckt={ckt} not found"))
            })
    }

    /// Return the 0-based internal index of the generator with canonical ID *id*.
    ///
    /// Raises ``ValueError`` if not found.
    ///
    /// Example::
    ///
    ///     idx = net.generator_index("gen_30_1")
    ///     gen_derates = {idx: [1.0, 0.5, 0.0, ...]}  # partial derate hour 1, outage hour 2
    fn generator_index(&self, id: &str) -> PyResult<usize> {
        self.inner
            .find_gen_index_by_id(id)
            .ok_or_else(|| PyValueError::new_err(format!("Generator id='{id}' not found")))
    }

    /// The Slack (reference) bus.
    ///
    /// Raises ``ValueError`` if no Slack bus exists in the network.
    #[getter]
    fn slack_bus(&self) -> PyResult<rich_objects::Bus> {
        rich_objects::find_slack_bus(&self.inner)
    }

    // ─── Phase E: Network collection helpers ───────────────────────────────

    /// All transformer branches (is_transformer == true).
    #[getter]
    fn transformers(&self) -> Vec<rich_objects::Branch> {
        rich_objects::branches_filtered(&self.inner, rich_objects::core_branch_is_transformer)
    }
    /// All line branches (is_transformer == false).
    #[getter]
    fn lines(&self) -> Vec<rich_objects::Branch> {
        rich_objects::branches_filtered(&self.inner, |b| {
            !rich_objects::core_branch_is_transformer(b)
        })
    }
    /// In-service generators.
    #[getter]
    fn in_service_generators(&self) -> Vec<rich_objects::Generator> {
        rich_objects::generators_filtered(&self.inner, |g| g.in_service)
    }
    /// In-service branches.
    #[getter]
    fn in_service_branches(&self) -> Vec<rich_objects::Branch> {
        rich_objects::branches_filtered(&self.inner, |b| b.in_service)
    }
    /// All generators connected to a given bus number.
    fn generators_at_bus(&self, bus: u32) -> Vec<rich_objects::Generator> {
        rich_objects::generators_filtered(&self.inner, |g| g.bus == bus)
    }
    /// All branches incident to a given bus number (from OR to end).
    fn branches_at_bus(&self, bus: u32) -> Vec<rich_objects::Branch> {
        rich_objects::branches_filtered(&self.inner, |b| b.from_bus == bus || b.to_bus == bus)
    }
    /// All loads at a given bus number.
    fn loads_at_bus(&self, bus: u32) -> Vec<rich_objects::Load> {
        rich_objects::loads_filtered(&self.inner, |l| l.bus == bus)
    }
    /// Sorted unique area numbers in the network.
    #[getter]
    fn area_numbers(&self) -> Vec<u32> {
        let mut areas: Vec<u32> = self.inner.buses.iter().map(|b| b.area).collect();
        areas.sort_unstable();
        areas.dedup();
        areas
    }
    /// Sorted unique zone numbers in the network.
    #[getter]
    fn zone_numbers(&self) -> Vec<u32> {
        let mut zones: Vec<u32> = self.inner.buses.iter().map(|b| b.zone).collect();
        zones.sort_unstable();
        zones.dedup();
        zones
    }
    /// Total reactive power load from Load objects (MVAr).
    #[getter]
    fn total_load_mvar(&self) -> f64 {
        self.inner.bus_load_q_mvar().iter().sum()
    }
    /// Total scheduled generation from in-service generators (MW).
    #[getter]
    fn total_scheduled_generation_mw(&self) -> f64 {
        self.inner
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.p)
            .sum()
    }
    /// Total scheduled reactive generation from in-service generators (MVAr).
    #[getter]
    fn total_scheduled_generation_mvar(&self) -> f64 {
        self.inner
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.q)
            .sum()
    }
    /// Generation headroom: sum of (pmax - pg) for in-service generators (MW).
    #[getter]
    fn generation_reserve_mw(&self) -> f64 {
        self.inner
            .generators
            .iter()
            .filter(|g| g.in_service)
            .map(|g| g.pmax - g.p)
            .sum()
    }
    /// Canonical HVDC namespace for point-to-point links and explicit DC topology.
    #[getter]
    fn hvdc(slf: PyRef<'_, Self>, _py: Python<'_>) -> HvdcView {
        HvdcView { parent: slf.into() }
    }
    /// All dispatchable load resources.
    #[getter]
    fn dispatchable_loads(&self) -> Vec<rich_objects::DispatchableLoad> {
        let base = self.inner.base_mva;
        self.inner
            .market_data
            .dispatchable_loads
            .iter()
            .enumerate()
            .map(|(index, dl)| {
                rich_objects::DispatchableLoad::from_core(index, dl, &self.inner.buses, base)
            })
            .collect()
    }
    /// All FACTS devices.
    #[getter]
    fn facts_devices(&self) -> Vec<rich_objects::FactsDevice> {
        self.inner
            .facts_devices
            .iter()
            .map(rich_objects::FactsDevice::from_core)
            .collect()
    }
    /// All area interchange records.
    #[getter]
    fn area_schedules(&self) -> Vec<rich_objects::AreaSchedule> {
        self.inner
            .area_schedules
            .iter()
            .map(rich_objects::AreaSchedule::from_core)
            .collect()
    }

    /// Register an On-Load Tap-Changer (OLTC) voltage control on a transformer.
    ///
    /// The transformer is identified by (from_bus, to_bus, circuit).  During
    /// ``solve_ac_pf()``, the outer OLTC loop steps the tap ratio until the
    /// regulated bus voltage is within the dead-band around ``v_target``.
    ///
    /// Args:
    ///   from_bus: External from-bus number of the transformer.
    ///   to_bus: External to-bus number of the transformer.
    ///   circuit: Circuit identifier (default 1).
    ///   v_target: Voltage target in per-unit (default 1.0).
    ///   v_band: Dead-band half-width in per-unit (default 0.01).
    ///   tap_min: Minimum tap ratio (default 0.9).
    ///   tap_max: Maximum tap ratio (default 1.1).
    ///   tap_step: Discrete tap step size (default 0.00625 = 16 steps/side).
    ///   regulated_bus: External bus to regulate (default = to_bus).
    #[pyo3(signature = (
        from_bus, to_bus, circuit = "1",
        v_target = 1.0, v_band = 0.01,
        tap_min = 0.9, tap_max = 1.1, tap_step = 0.00625,
        regulated_bus = None,
    ))]
    fn add_oltc_control(
        &mut self,
        from_bus: u32,
        to_bus: u32,
        circuit: &str,
        v_target: f64,
        v_band: f64,
        tap_min: f64,
        tap_max: f64,
        tap_step: f64,
        regulated_bus: Option<u32>,
    ) -> PyResult<()> {
        let branch_index = self
            .inner
            .find_branch_index(from_bus, to_bus, circuit)
            .ok_or_else(|| {
                NetworkError::new_err(format!(
                    "Branch ({from_bus}, {to_bus}, ckt={circuit}) not found"
                ))
            })?;
        let bus_map = self.inner.bus_index_map();
        let reg_bus_ext = regulated_bus.unwrap_or(to_bus);
        let bus_regulated = *bus_map.get(&reg_bus_ext).ok_or_else(|| {
            NetworkError::new_err(format!("Regulated bus {reg_bus_ext} not found"))
        })?;
        self.oltc_controls
            .push(surge_network::network::discrete_control::OltcControl {
                branch_index,
                bus_regulated,
                v_target,
                v_band,
                tap_min,
                tap_max,
                tap_step,
            });
        Ok(())
    }

    /// Register a switched shunt (capacitor/reactor bank) voltage control.
    ///
    /// During ``solve_ac_pf()``, the outer shunt loop steps the bank in or out
    /// until the bus voltage is within the dead-band around ``v_target``.
    ///
    /// Args:
    ///   bus: External bus number where the shunt is connected.
    ///   b_step_mvar: Susceptance per step in MVAr at 1.0 pu (positive = capacitive).
    ///   n_steps_cap: Maximum capacitor (voltage-raising) steps (default 0).
    ///   n_steps_react: Maximum reactor (voltage-lowering) steps (default 0).
    ///   v_target: Voltage target in per-unit (default 1.0).
    ///   v_band: Dead-band half-width in per-unit (default 0.02).
    #[pyo3(signature = (
        bus, b_step_mvar,
        n_steps_cap = 0, n_steps_react = 0,
        v_target = 1.0, v_band = 0.02,
    ))]
    fn add_switched_shunt(
        &mut self,
        bus: u32,
        b_step_mvar: f64,
        n_steps_cap: i32,
        n_steps_react: i32,
        v_target: f64,
        v_band: f64,
    ) -> PyResult<()> {
        if !self.inner.buses.iter().any(|b| b.number == bus) {
            return Err(NetworkError::new_err(format!("Bus {bus} not found")));
        }
        let b_step_pu = b_step_mvar / self.inner.base_mva;
        let ordinal = self
            .switched_shunts
            .iter()
            .filter(|existing| existing.bus == bus)
            .count()
            + 1;
        self.switched_shunts
            .push(surge_network::network::SwitchedShunt {
                id: format!("switched_shunt_{}_{}", bus, ordinal),
                bus,
                bus_regulated: bus, // local regulation (default)
                b_step: b_step_pu,
                n_steps_cap,
                n_steps_react,
                v_target,
                v_band,
                n_active_steps: 0,
            });
        Ok(())
    }

    /// Remove all registered OLTC and switched shunt controls.
    fn clear_discrete_controls(&mut self) {
        self.oltc_controls.clear();
        self.switched_shunts.clear();
    }

    /// Return the number of registered OLTC controls.
    #[getter]
    fn n_oltc_controls(&self) -> usize {
        self.oltc_controls.len()
    }

    /// Return the number of registered switched shunt controls.
    #[getter]
    fn n_switched_shunts(&self) -> usize {
        self.switched_shunts.len()
    }

    // -----------------------------------------------------------------------
    // Substation topology
    // -----------------------------------------------------------------------

    #[getter]
    fn topology(slf: PyRef<'_, Self>, _py: Python<'_>) -> Option<NodeBreakerTopologyView> {
        slf.inner.topology.as_ref()?;
        Some(NodeBreakerTopologyView { parent: slf.into() })
    }

    fn __repr__(&self) -> String {
        format!(
            "Network('{}', buses={}, branches={}, generators={})",
            self.inner.name,
            self.inner.n_buses(),
            self.inner.n_branches(),
            self.inner.generators.len()
        )
    }

    fn __copy__(&self) -> Self {
        self.copy()
    }

    fn __deepcopy__(&self, _memo: &Bound<'_, PyDict>) -> Self {
        self.copy()
    }

    /// Apply voltages to bus initial conditions (e.g. from state estimation).
    ///
    /// Parameters
    /// ----------
    /// vm_pu : list[float]
    ///     Voltage magnitudes in per-unit.
    /// va_deg : list[float]
    ///     Voltage angles in degrees.
    /// bus_numbers : list[int]
    ///     External bus numbers (same order as vm_pu / va_deg).
    #[pyo3(signature = (vm_pu, va_deg, bus_numbers))]
    fn apply_voltages(
        &mut self,
        vm_pu: Vec<f64>,
        va_deg: Vec<f64>,
        bus_numbers: Vec<u32>,
    ) -> PyResult<()> {
        if vm_pu.len() != va_deg.len() || vm_pu.len() != bus_numbers.len() {
            return Err(PyValueError::new_err(
                "vm_pu, va_deg, and bus_numbers must have the same length",
            ));
        }
        let net = Arc::make_mut(&mut self.inner);
        let bus_map: std::collections::HashMap<u32, usize> = net
            .buses
            .iter()
            .enumerate()
            .map(|(i, b)| (b.number, i))
            .collect();
        for ((&vm, &va_d), &bnum) in vm_pu.iter().zip(va_deg.iter()).zip(bus_numbers.iter()) {
            if let Some(&idx) = bus_map.get(&bnum) {
                net.buses[idx].voltage_magnitude_pu = vm;
                net.buses[idx].voltage_angle_rad = va_d.to_radians();
            }
        }
        Ok(())
    }
}
