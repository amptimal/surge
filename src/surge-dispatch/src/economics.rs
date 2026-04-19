// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Objective-ledger helpers for dispatch reporting.

use std::collections::HashMap;

use surge_solution::{
    ObjectiveBucket, ObjectiveQuantityUnit, ObjectiveSubjectKind, ObjectiveTerm, ObjectiveTermKind,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ObjectiveAggregateKey {
    bucket: ObjectiveBucket,
    kind: ObjectiveTermKind,
    subject_kind: ObjectiveSubjectKind,
    subject_id: String,
    component_id: String,
    quantity_unit: Option<ObjectiveQuantityUnit>,
}

pub(crate) const SYSTEM_SUBJECT_ID: &str = "system";

pub(crate) fn push_term(terms: &mut Vec<ObjectiveTerm>, term: ObjectiveTerm) {
    if term.dollars.abs() > 1e-9 || term.quantity.unwrap_or(0.0).abs() > 1e-9 {
        terms.push(term);
    }
}

pub(crate) fn make_term(
    component_id: impl Into<String>,
    bucket: ObjectiveBucket,
    kind: ObjectiveTermKind,
    subject_kind: ObjectiveSubjectKind,
    subject_id: impl Into<String>,
    dollars: f64,
    quantity: Option<f64>,
    quantity_unit: Option<ObjectiveQuantityUnit>,
    unit_rate: Option<f64>,
) -> ObjectiveTerm {
    ObjectiveTerm {
        component_id: component_id.into(),
        bucket,
        kind,
        subject_kind,
        subject_id: subject_id.into(),
        dollars,
        quantity,
        quantity_unit,
        unit_rate,
    }
}

pub(crate) fn sum_terms(terms: &[ObjectiveTerm]) -> f64 {
    terms.iter().map(|term| term.dollars).sum()
}

pub(crate) fn aggregate_terms(terms: &[ObjectiveTerm]) -> Vec<ObjectiveTerm> {
    #[derive(Debug, Clone)]
    struct AggregateState {
        term: ObjectiveTerm,
        rate_mismatch: bool,
    }

    let mut grouped: HashMap<ObjectiveAggregateKey, AggregateState> = HashMap::new();
    for term in terms {
        let key = ObjectiveAggregateKey {
            bucket: term.bucket,
            kind: term.kind,
            subject_kind: term.subject_kind,
            subject_id: term.subject_id.clone(),
            component_id: term.component_id.clone(),
            quantity_unit: term.quantity_unit,
        };
        match grouped.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(AggregateState {
                    term: term.clone(),
                    rate_mismatch: false,
                });
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let state = entry.get_mut();
                state.term.dollars += term.dollars;
                match (state.term.quantity, term.quantity) {
                    (Some(existing), Some(current)) => {
                        state.term.quantity = Some(existing + current)
                    }
                    (None, Some(current)) => state.term.quantity = Some(current),
                    _ => {}
                }
                if state.term.unit_rate != term.unit_rate {
                    state.rate_mismatch = true;
                }
            }
        }
    }

    let mut aggregated: Vec<ObjectiveTerm> = grouped
        .into_values()
        .map(|mut state| {
            if state.rate_mismatch {
                state.term.unit_rate = None;
            }
            state.term
        })
        .collect();
    aggregated.sort_by(|lhs, rhs| {
        (
            lhs.subject_kind as u8,
            lhs.subject_id.as_str(),
            lhs.kind as u8,
            lhs.component_id.as_str(),
        )
            .cmp(&(
                rhs.subject_kind as u8,
                rhs.subject_id.as_str(),
                rhs.kind as u8,
                rhs.component_id.as_str(),
            ))
    });
    aggregated
}

pub(crate) fn filter_terms_for_subject(
    terms: &[ObjectiveTerm],
    subject_kind: ObjectiveSubjectKind,
    subject_id: &str,
) -> Vec<ObjectiveTerm> {
    terms
        .iter()
        .filter(|term| term.subject_kind == subject_kind && term.subject_id == subject_id)
        .cloned()
        .collect()
}

pub(crate) fn resource_energy_cost(terms: &[ObjectiveTerm]) -> f64 {
    terms
        .iter()
        .filter(|term| term.bucket == ObjectiveBucket::Energy)
        .map(|term| term.dollars)
        .sum()
}

pub(crate) fn resource_no_load_cost(terms: &[ObjectiveTerm]) -> f64 {
    terms
        .iter()
        .filter(|term| term.bucket == ObjectiveBucket::NoLoad)
        .map(|term| term.dollars)
        .sum()
}

pub(crate) fn resource_startup_cost(terms: &[ObjectiveTerm]) -> f64 {
    terms
        .iter()
        .filter(|term| term.bucket == ObjectiveBucket::Startup)
        .map(|term| term.dollars)
        .sum()
}

pub(crate) fn resource_shutdown_cost(terms: &[ObjectiveTerm]) -> f64 {
    terms
        .iter()
        .filter(|term| term.bucket == ObjectiveBucket::Shutdown)
        .map(|term| term.dollars)
        .sum()
}

pub(crate) fn resource_reserve_costs(terms: &[ObjectiveTerm]) -> HashMap<String, f64> {
    let mut costs = HashMap::new();
    for term in terms.iter().filter(|term| {
        matches!(
            term.kind,
            ObjectiveTermKind::ReserveProcurement | ObjectiveTermKind::ReactiveReserveProcurement
        )
    }) {
        costs
            .entry(term.component_id.clone())
            .and_modify(|value| *value += term.dollars)
            .or_insert(term.dollars);
    }
    costs
}

pub(crate) fn reserve_shortfall_cost(terms: &[ObjectiveTerm], requirement_id: &str) -> f64 {
    terms
        .iter()
        .filter(|term| {
            term.subject_kind == ObjectiveSubjectKind::ReserveRequirement
                && term.subject_id == requirement_id
                && matches!(
                    term.kind,
                    ObjectiveTermKind::ReserveShortfall
                        | ObjectiveTermKind::ReactiveReserveShortfall
                )
        })
        .map(|term| term.dollars)
        .sum()
}

pub(crate) fn bucket_total(terms: &[ObjectiveTerm], bucket: ObjectiveBucket) -> f64 {
    terms
        .iter()
        .filter(|term| term.bucket == bucket)
        .map(|term| term.dollars)
        .sum()
}
