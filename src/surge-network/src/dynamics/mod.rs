// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Transient stability dynamic model parameter definitions.

pub mod models;
pub mod saturation;
pub mod shaft;
pub mod usrmdl_equiv;

pub use models::*;
pub use saturation::{
    ConverterCommutationModel, CoreLossModel, CoreType, PolynomialSaturation, SaturationCurve,
    SaturationPoint, TransformerSaturation, TwoSlopeSaturation,
};
pub use shaft::{
    SegmentTorqueSource, ShaftCoupling, ShaftModel, ShaftSegment, ieee_fbm_shaft_model,
};
pub use usrmdl_equiv::{
    EQUIVALENCE_TABLE, Equivalence, ModelCategory, equivalence_table, guess_category,
    suggest_equivalent,
};
