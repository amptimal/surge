// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared Python input coercions for public binding APIs.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyAny;
use pyo3::{Borrowed, FromPyObject};

#[derive(Clone, Debug)]
pub(crate) struct PyCircuitId(String);

impl PyCircuitId {
    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

impl<'a, 'py> FromPyObject<'a, 'py> for PyCircuitId {
    type Error = PyErr;

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        if let Ok(circuit) = obj.extract::<String>() {
            return Ok(Self(circuit));
        }
        if let Ok(circuit) = obj.extract::<i64>() {
            return Ok(Self(circuit.to_string()));
        }
        Err(PyValueError::new_err("circuit must be a string or integer"))
    }
}

#[derive(Clone, Debug)]
pub struct PyBranchKey {
    pub from_bus: u32,
    pub to_bus: u32,
    pub circuit: String,
}

impl<'a, 'py> FromPyObject<'a, 'py> for PyBranchKey {
    type Error = PyErr;

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        let (from_bus, to_bus, circuit): (u32, u32, PyCircuitId) = obj.extract()?;
        Ok(Self {
            from_bus,
            to_bus,
            circuit: circuit.into_string(),
        })
    }
}

impl From<PyBranchKey> for (u32, u32, String) {
    fn from(value: PyBranchKey) -> Self {
        (value.from_bus, value.to_bus, value.circuit)
    }
}
