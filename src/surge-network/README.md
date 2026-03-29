# surge-network

Canonical power-system domain model for Surge.

This crate defines the shared `Network` type and the equipment records used
across importers, topology processing, steady-state solvers, optimization,
dispatch, transfer studies, contingency analysis, and market simulation.

Use `surge-network` when you need the common data model without pulling in a
specific solver surface.
