// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python-facing HVDC view for the canonical `Network.hvdc` namespace.

use pyo3::prelude::*;

use crate::network::Network;
use crate::rich_objects;

#[pyclass(name = "Hvdc", unsendable, skip_from_py_object)]
pub struct HvdcView {
    pub(crate) parent: Py<Network>,
}

#[pymethods]
impl HvdcView {
    #[getter]
    fn is_empty(&self, py: Python<'_>) -> bool {
        let parent = self.parent.bind(py).borrow();
        parent.inner.hvdc.is_empty()
    }

    #[getter]
    fn has_links(&self, py: Python<'_>) -> bool {
        let parent = self.parent.bind(py).borrow();
        parent.inner.hvdc.has_point_to_point_links()
    }

    #[getter]
    fn has_explicit_dc_topology(&self, py: Python<'_>) -> bool {
        let parent = self.parent.bind(py).borrow();
        parent.inner.hvdc.has_explicit_dc_topology()
    }

    #[getter]
    fn links(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let parent = self.parent.bind(py).borrow();
        let hvdc = &parent.inner.hvdc;
        let mut links = Vec::with_capacity(hvdc.links.len());
        for link in &hvdc.links {
            match link {
                surge_network::network::HvdcLink::Lcc(link) => {
                    let obj = Py::new(py, rich_objects::DcLine::from_core(link))?;
                    links.push(obj.into_bound(py).into_any().unbind());
                }
                surge_network::network::HvdcLink::Vsc(link) => {
                    let obj = Py::new(py, rich_objects::VscDcLine::from_core(link))?;
                    links.push(obj.into_bound(py).into_any().unbind());
                }
            }
        }
        Ok(links)
    }

    #[getter]
    fn dc_grids(&self, py: Python<'_>) -> Vec<rich_objects::DcGrid> {
        let parent = self.parent.bind(py).borrow();
        parent
            .inner
            .hvdc
            .dc_grids
            .iter()
            .map(rich_objects::DcGrid::from_core)
            .collect()
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        let parent = self.parent.bind(py).borrow();
        let hvdc = &parent.inner.hvdc;
        format!(
            "Hvdc(links={}, dc_grids={})",
            hvdc.links.len(),
            hvdc.dc_grids.len()
        )
    }
}
