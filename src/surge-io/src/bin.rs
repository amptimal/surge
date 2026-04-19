// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Native binary format for Surge network serialization.
//!
//! The binary container is intentionally optimized for full-model native IO:
//!
//! - a compact fixed header and framed sections
//! - direct packed sections for the highest-volume entities
//! - typed extra sections for the rest of the Surge model
//!
//! This keeps the format whole-model for PF, OPF, topology, market, and
//! dynamics-adjacent workflows while avoiding the JSON-like object tree that
//! previously dominated `surge-bin` load/save costs.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use surge_network::dynamics::{CoreLossModel, CoreType, TransformerSaturation};
use surge_network::market::{
    AmbientConditions, CombinedCyclePlant, CostCurve, DispatchableLoad, EmissionPolicy,
    EmissionRates, EnergyOffer, MarketRules, OutageEntry, PumpedHydroUnit, QualificationMap,
    ReserveOffer, ReserveZone,
};
use surge_network::network::asset::AssetCatalog;
use surge_network::network::boundary::BoundaryData;
use surge_network::network::breaker::BreakerRating;
use surge_network::network::flowgate::OperatingNomogram;
use surge_network::network::grounding::GroundingEntry;
use surge_network::network::impedance_correction::ImpedanceCorrectionTable;
use surge_network::network::induction_machine::InductionMachine;
use surge_network::network::market_data::MarketData;
use surge_network::network::measurement::CimMeasurement;
use surge_network::network::model::{GeoPoint, MutualCoupling, PhaseImpedanceEntry};
use surge_network::network::multi_section_line::MultiSectionLineGroup;
use surge_network::network::net_ops::NetworkOperationsData;
use surge_network::network::op_limits::OperationalLimits;
use surge_network::network::protection::ProtectionData;
use surge_network::network::scheduled_area_transfer::ScheduledAreaTransfer;
use surge_network::network::{
    AreaSchedule, Branch, BranchConditionalRatings, BranchOpfControl, BranchType, Bus, BusType,
    FactsDevice, FixedShunt, Flowgate, FuelSupply, GenType, Generator, GeneratorTechnology,
    HarmonicData, HvdcModel, Interface, LineData, Load, LoadClass, LoadConnection, Network,
    NodeBreakerTopology, OltcSpec, Owner, OwnershipEntry, ParSpec, PhaseMode, PowerInjection,
    Region, SeriesCompData, StorageParams, SwitchedShunt, SwitchedShuntOpf, TapMode,
    TransformerConnection, TransformerData, WindingConnection, ZeroSeqData,
};
use thiserror::Error;

pub const SURGE_BIN_FORMAT: &str = "surge-bin";
pub const SURGE_BIN_SCHEMA_VERSION: &str = "0.1.0";

const MAGIC: &[u8; 8] = b"SRGBIN02";
const BINARY_REVISION: u16 = 2;
const HEADER_LEN: usize = 8 + 2 + 2 + 4 + 8 + 4;
const SECTION_HEADER_LEN: usize = 2 + 2 + 4 + 8;

const SECTION_NETWORK_HEADER: u16 = 1;
const SECTION_STRINGS: u16 = 2;
const SECTION_BUSES: u16 = 3;
const SECTION_BUS_EXTRAS: u16 = 4;
const SECTION_BRANCHES: u16 = 5;
const SECTION_BRANCH_EXTRAS: u16 = 6;
const SECTION_GENERATORS: u16 = 7;
const SECTION_GENERATOR_EXTRAS: u16 = 8;
const SECTION_LOADS: u16 = 9;
const SECTION_NETWORK_EXTRAS: u16 = 10;
const SECTION_LOAD_EXTRAS: u16 = 11;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("CBOR encode error: {0}")]
    Encode(#[from] ciborium::ser::Error<std::io::Error>),

    #[error("CBOR decode error: {0}")]
    Decode(#[from] ciborium::de::Error<std::io::Error>),

    #[error("invalid binary document: {0}")]
    InvalidDocument(String),
}

#[derive(Clone)]
struct Section {
    id: u16,
    count: u32,
    payload: Vec<u8>,
}

#[derive(Clone, Copy)]
struct SectionView<'a> {
    count: u32,
    payload: &'a [u8],
}

#[derive(Default)]
struct StringTableBuilder {
    ids: HashMap<String, u32>,
    values: Vec<String>,
}

impl StringTableBuilder {
    fn new() -> Self {
        let mut ids = HashMap::new();
        ids.insert(String::new(), 0);
        Self {
            ids,
            values: vec![String::new()],
        }
    }

    fn intern(&mut self, value: &str) -> u32 {
        if let Some(id) = self.ids.get(value) {
            return *id;
        }
        let id = self.values.len() as u32;
        let owned = value.to_string();
        self.ids.insert(owned.clone(), id);
        self.values.push(owned);
        id
    }

    fn freeze(self) -> StringTable {
        StringTable {
            values: self.values,
        }
    }
}

struct StringTable {
    values: Vec<String>,
}

impl StringTable {
    fn resolve(&self, id: u32) -> Result<&str, Error> {
        self.values
            .get(id as usize)
            .map(String::as_str)
            .ok_or_else(|| Error::InvalidDocument(format!("invalid string table index {id}")))
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn is_empty(&self) -> bool {
        self.pos == self.bytes.len()
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| Error::InvalidDocument("binary document length overflow".to_string()))?;
        if end > self.bytes.len() {
            return Err(Error::InvalidDocument(format!(
                "truncated binary payload: need {len} bytes, have {}",
                self.remaining()
            )));
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let mut bytes = [0u8; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn f64(&mut self) -> Result<f64, Error> {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(f64::from_le_bytes(bytes))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedBusExtra {
    index: u32,
    extra: BusExtra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BusExtra {
    latitude: Option<f64>,
    longitude: Option<f64>,
    freq_hz: Option<f64>,
    ambient: Option<AmbientConditions>,
    reserve_zone: Option<String>,
    owners: Vec<OwnershipEntry>,
}

impl BusExtra {
    fn from_bus(bus: &Bus) -> Option<Self> {
        let extra = Self {
            latitude: bus.latitude,
            longitude: bus.longitude,
            freq_hz: bus.freq_hz,
            ambient: bus.ambient.clone(),
            reserve_zone: bus.reserve_zone.clone(),
            owners: bus.owners.clone(),
        };
        (!extra.is_empty()).then_some(extra)
    }

    fn is_empty(&self) -> bool {
        self.latitude.is_none()
            && self.longitude.is_none()
            && self.freq_hz.is_none()
            && self.ambient.is_none()
            && self.reserve_zone.is_none()
            && self.owners.is_empty()
    }

    fn apply(self, bus: &mut Bus) {
        bus.latitude = self.latitude;
        bus.longitude = self.longitude;
        bus.freq_hz = self.freq_hz;
        bus.ambient = self.ambient;
        bus.reserve_zone = self.reserve_zone;
        bus.owners = self.owners;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedBranchExtra {
    index: u32,
    extra: BranchExtra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BranchExtra {
    r0: Option<f64>,
    x0: Option<f64>,
    b0: Option<f64>,
    zn: Option<(f64, f64)>,
    tab: Option<u32>,
    oil_temp_limit_c: Option<f64>,
    winding_temp_limit_c: Option<f64>,
    impedance_limit_ohm: Option<f64>,
    saturation: Option<TransformerSaturation>,
    core_type: Option<CoreType>,
    core_loss_model: Option<CoreLossModel>,
    length_km: Option<f64>,
    line_type: Option<surge_network::network::LineType>,
    conductor: Option<String>,
    n_bundles: Option<u32>,
    winding_rated_kv: Option<f64>,
    winding_rated_mva: Option<f64>,
    parent_transformer_id: Option<String>,
    winding_number: Option<u8>,
    winding_connection: Option<WindingConnection>,
    zn_winding: Option<(f64, f64)>,
    bypass_current_ka: Option<f64>,
    rated_mvar_series: Option<f64>,
    ambient: Option<AmbientConditions>,
    owners: Vec<OwnershipEntry>,
}

impl BranchExtra {
    fn from_branch(branch: &Branch) -> Option<Self> {
        let zs = branch.zero_seq.as_ref();
        let td = branch.transformer_data.as_ref();
        let hd = branch.harmonic.as_ref();
        let ld = branch.line_data.as_ref();
        let sc = branch.series_comp.as_ref();
        let extra = Self {
            r0: zs.map(|z| z.r0),
            x0: zs.map(|z| z.x0),
            b0: zs.map(|z| z.b0),
            zn: zs.and_then(|z| z.zn).map(|value| (value.re, value.im)),
            tab: branch.tab,
            oil_temp_limit_c: td.and_then(|t| t.oil_temp_limit_c),
            winding_temp_limit_c: td.and_then(|t| t.winding_temp_limit_c),
            impedance_limit_ohm: td.and_then(|t| t.impedance_limit_ohm),
            saturation: hd.and_then(|h| h.saturation.clone()),
            core_type: hd.and_then(|h| h.core_type),
            core_loss_model: hd.and_then(|h| h.core_loss_model),
            length_km: ld.and_then(|l| l.length_km),
            line_type: ld.and_then(|l| l.line_type),
            conductor: ld.and_then(|l| l.conductor.clone()),
            n_bundles: ld.and_then(|l| l.n_bundles),
            winding_rated_kv: td.and_then(|t| t.winding_rated_kv),
            winding_rated_mva: td.and_then(|t| t.winding_rated_mva),
            parent_transformer_id: td.and_then(|t| t.parent_transformer_id.clone()),
            winding_number: td.and_then(|t| t.winding_number),
            winding_connection: td.and_then(|t| t.winding_connection),
            zn_winding: td
                .and_then(|t| t.zn_winding)
                .map(|value| (value.re, value.im)),
            bypass_current_ka: sc.and_then(|s| s.bypass_current_ka),
            rated_mvar_series: sc.and_then(|s| s.rated_mvar_series),
            ambient: branch.ambient.clone(),
            owners: branch.owners.clone(),
        };
        (!extra.is_empty()).then_some(extra)
    }

    fn is_empty(&self) -> bool {
        self.r0.is_none()
            && self.x0.is_none()
            && self.b0.is_none()
            && self.zn.is_none()
            && self.tab.is_none()
            && self.oil_temp_limit_c.is_none()
            && self.winding_temp_limit_c.is_none()
            && self.impedance_limit_ohm.is_none()
            && self.saturation.is_none()
            && self.core_type.is_none()
            && self.core_loss_model.is_none()
            && self.length_km.is_none()
            && self.line_type.is_none()
            && self.conductor.is_none()
            && self.n_bundles.is_none()
            && self.winding_rated_kv.is_none()
            && self.winding_rated_mva.is_none()
            && self.parent_transformer_id.is_none()
            && self.winding_number.is_none()
            && self.winding_connection.is_none()
            && self.zn_winding.is_none()
            && self.bypass_current_ka.is_none()
            && self.rated_mvar_series.is_none()
            && self.ambient.is_none()
            && self.owners.is_empty()
    }

    fn apply(self, branch: &mut Branch) {
        // Zero-sequence data
        if self.r0.is_some() || self.x0.is_some() || self.b0.is_some() || self.zn.is_some() {
            let zs = branch.zero_seq.get_or_insert_with(ZeroSeqData::default);
            if let Some(r0) = self.r0 {
                zs.r0 = r0;
            }
            if let Some(x0) = self.x0 {
                zs.x0 = x0;
            }
            if let Some(b0) = self.b0 {
                zs.b0 = b0;
            }
            zs.zn = self.zn.map(|(re, im)| num_complex::Complex64::new(re, im));
        }
        branch.tab = self.tab;
        // Transformer data
        if self.oil_temp_limit_c.is_some()
            || self.winding_temp_limit_c.is_some()
            || self.impedance_limit_ohm.is_some()
            || self.winding_rated_kv.is_some()
            || self.winding_rated_mva.is_some()
            || self.parent_transformer_id.is_some()
            || self.winding_number.is_some()
            || self.winding_connection.is_some()
            || self.zn_winding.is_some()
        {
            let td = branch
                .transformer_data
                .get_or_insert_with(TransformerData::default);
            td.oil_temp_limit_c = self.oil_temp_limit_c;
            td.winding_temp_limit_c = self.winding_temp_limit_c;
            td.impedance_limit_ohm = self.impedance_limit_ohm;
            td.winding_rated_kv = self.winding_rated_kv;
            td.winding_rated_mva = self.winding_rated_mva;
            td.parent_transformer_id = self.parent_transformer_id;
            td.winding_number = self.winding_number;
            td.winding_connection = self.winding_connection;
            td.zn_winding = self
                .zn_winding
                .map(|(re, im)| num_complex::Complex64::new(re, im));
        }
        // Harmonic data
        if self.saturation.is_some() || self.core_type.is_some() || self.core_loss_model.is_some() {
            let hd = branch.harmonic.get_or_insert_with(HarmonicData::default);
            hd.saturation = self.saturation;
            hd.core_type = self.core_type;
            hd.core_loss_model = self.core_loss_model;
        }
        // Line data
        if self.length_km.is_some()
            || self.line_type.is_some()
            || self.conductor.is_some()
            || self.n_bundles.is_some()
        {
            let ld = branch.line_data.get_or_insert_with(LineData::default);
            ld.length_km = self.length_km;
            ld.line_type = self.line_type;
            ld.conductor = self.conductor;
            ld.n_bundles = self.n_bundles;
        }
        // Series comp data
        if self.bypass_current_ka.is_some() || self.rated_mvar_series.is_some() {
            let sc = branch
                .series_comp
                .get_or_insert_with(SeriesCompData::default);
            sc.bypass_current_ka = self.bypass_current_ka;
            sc.rated_mvar_series = self.rated_mvar_series;
        }
        branch.ambient = self.ambient;
        branch.owners = self.owners;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedGeneratorExtra {
    index: u32,
    extra: GeneratorExtra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeneratorExtra {
    technology: Option<GeneratorTechnology>,
    source_technology_code: Option<String>,
    xs: Option<f64>,
    x2_pu: Option<f64>,
    r2_pu: Option<f64>,
    x0_pu: Option<f64>,
    r0_pu: Option<f64>,
    zn: Option<(f64, f64)>,
    s_rated_mva: Option<f64>,
    p_available_mw: Option<f64>,
    p_ecomin: Option<f64>,
    p_ecomax: Option<f64>,
    p_emergency_min: Option<f64>,
    p_emergency_max: Option<f64>,
    p_reg_min: Option<f64>,
    p_reg_max: Option<f64>,
    min_up_time_hr: Option<f64>,
    min_down_time_hr: Option<f64>,
    max_up_time_hr: Option<f64>,
    min_run_at_pmin_hr: Option<f64>,
    max_starts_per_day: Option<u32>,
    max_starts_per_week: Option<u32>,
    max_energy_mwh_per_day: Option<f64>,
    shutdown_ramp_mw_per_min: Option<f64>,
    startup_ramp_mw_per_min: Option<f64>,
    forbidden_zones: Vec<(f64, f64)>,
    ramp_up_curve: Vec<(f64, f64)>,
    ramp_down_curve: Vec<(f64, f64)>,
    emergency_ramp_up_curve: Vec<(f64, f64)>,
    emergency_ramp_down_curve: Vec<(f64, f64)>,
    reg_ramp_up_curve: Vec<(f64, f64)>,
    reg_ramp_down_curve: Vec<(f64, f64)>,
    primary_fuel: Option<FuelSupply>,
    backup_fuel: Option<FuelSupply>,
    fuel_switch_time_min: Option<f64>,
    fuel_type: Option<String>,
    heat_rate_btu_mwh: Option<f64>,
    energy_offer: Option<EnergyOffer>,
    reserve_offers: Vec<ReserveOffer>,
    qualifications: QualificationMap,
    forced_outage_rate: Option<f64>,
    pq_curve: Vec<(f64, f64, f64)>,
    storage: Option<StorageParams>,
    owners: Vec<OwnershipEntry>,
}

impl GeneratorExtra {
    fn from_generator(generator: &Generator) -> Option<Self> {
        let fd = generator.fault_data.as_ref();
        let inv = generator.inverter.as_ref();
        let cm = generator.commitment.as_ref();
        let rp = generator.ramping.as_ref();
        let fl = generator.fuel.as_ref();
        let mk = generator.market.as_ref();
        let rc = generator.reactive_capability.as_ref();
        let extra = Self {
            technology: generator.technology,
            source_technology_code: generator.source_technology_code.clone(),
            xs: fd.and_then(|f| f.xs),
            x2_pu: fd.and_then(|f| f.x2_pu),
            r2_pu: fd.and_then(|f| f.r2_pu),
            x0_pu: fd.and_then(|f| f.x0_pu),
            r0_pu: fd.and_then(|f| f.r0_pu),
            zn: fd.and_then(|f| f.zn).map(|value| (value.re, value.im)),
            s_rated_mva: inv.and_then(|i| i.s_rated_mva),
            p_available_mw: inv.and_then(|i| i.p_available_mw),
            p_ecomin: cm.and_then(|c| c.p_ecomin),
            p_ecomax: cm.and_then(|c| c.p_ecomax),
            p_emergency_min: cm.and_then(|c| c.p_emergency_min),
            p_emergency_max: cm.and_then(|c| c.p_emergency_max),
            p_reg_min: cm.and_then(|c| c.p_reg_min),
            p_reg_max: cm.and_then(|c| c.p_reg_max),
            min_up_time_hr: cm.and_then(|c| c.min_up_time_hr),
            min_down_time_hr: cm.and_then(|c| c.min_down_time_hr),
            max_up_time_hr: cm.and_then(|c| c.max_up_time_hr),
            min_run_at_pmin_hr: cm.and_then(|c| c.min_run_at_pmin_hr),
            max_starts_per_day: cm.and_then(|c| c.max_starts_per_day),
            max_starts_per_week: cm.and_then(|c| c.max_starts_per_week),
            max_energy_mwh_per_day: cm.and_then(|c| c.max_energy_mwh_per_day),
            shutdown_ramp_mw_per_min: cm.and_then(|c| c.shutdown_ramp_mw_per_min),
            startup_ramp_mw_per_min: cm.and_then(|c| c.startup_ramp_mw_per_min),
            forbidden_zones: cm.map(|c| c.forbidden_zones.clone()).unwrap_or_default(),
            ramp_up_curve: rp.map(|r| r.ramp_up_curve.clone()).unwrap_or_default(),
            ramp_down_curve: rp.map(|r| r.ramp_down_curve.clone()).unwrap_or_default(),
            emergency_ramp_up_curve: rp
                .map(|r| r.emergency_ramp_up_curve.clone())
                .unwrap_or_default(),
            emergency_ramp_down_curve: rp
                .map(|r| r.emergency_ramp_down_curve.clone())
                .unwrap_or_default(),
            reg_ramp_up_curve: rp.map(|r| r.reg_ramp_up_curve.clone()).unwrap_or_default(),
            reg_ramp_down_curve: rp
                .map(|r| r.reg_ramp_down_curve.clone())
                .unwrap_or_default(),
            primary_fuel: fl.and_then(|f| f.primary_fuel.clone()),
            backup_fuel: fl.and_then(|f| f.backup_fuel.clone()),
            fuel_switch_time_min: fl.and_then(|f| f.fuel_switch_time_min),
            fuel_type: fl.and_then(|f| f.fuel_type.clone()),
            heat_rate_btu_mwh: fl.and_then(|f| f.heat_rate_btu_mwh),
            energy_offer: mk.and_then(|m| m.energy_offer.clone()),
            reserve_offers: mk.map(|m| m.reserve_offers.clone()).unwrap_or_default(),
            qualifications: mk.map(|m| m.qualifications.clone()).unwrap_or_default(),
            forced_outage_rate: generator.forced_outage_rate,
            pq_curve: rc.map(|r| r.pq_curve.clone()).unwrap_or_default(),
            storage: generator.storage.clone(),
            owners: generator.owners.clone(),
        };
        (!extra.is_empty()).then_some(extra)
    }

    fn is_empty(&self) -> bool {
        self.xs.is_none()
            && self.technology.is_none()
            && self.source_technology_code.is_none()
            && self.x2_pu.is_none()
            && self.r2_pu.is_none()
            && self.x0_pu.is_none()
            && self.r0_pu.is_none()
            && self.zn.is_none()
            && self.s_rated_mva.is_none()
            && self.p_available_mw.is_none()
            && self.p_ecomin.is_none()
            && self.p_ecomax.is_none()
            && self.p_emergency_min.is_none()
            && self.p_emergency_max.is_none()
            && self.p_reg_min.is_none()
            && self.p_reg_max.is_none()
            && self.min_up_time_hr.is_none()
            && self.min_down_time_hr.is_none()
            && self.max_up_time_hr.is_none()
            && self.min_run_at_pmin_hr.is_none()
            && self.max_starts_per_day.is_none()
            && self.max_starts_per_week.is_none()
            && self.max_energy_mwh_per_day.is_none()
            && self.shutdown_ramp_mw_per_min.is_none()
            && self.startup_ramp_mw_per_min.is_none()
            && self.forbidden_zones.is_empty()
            && self.ramp_up_curve.is_empty()
            && self.ramp_down_curve.is_empty()
            && self.emergency_ramp_up_curve.is_empty()
            && self.emergency_ramp_down_curve.is_empty()
            && self.reg_ramp_up_curve.is_empty()
            && self.reg_ramp_down_curve.is_empty()
            && self.primary_fuel.is_none()
            && self.backup_fuel.is_none()
            && self.fuel_switch_time_min.is_none()
            && self.fuel_type.is_none()
            && self.heat_rate_btu_mwh.is_none()
            && self.energy_offer.is_none()
            && self.reserve_offers.is_empty()
            && self.qualifications.is_empty()
            && self.forced_outage_rate.is_none()
            && self.pq_curve.is_empty()
            && self.storage.is_none()
            && self.owners.is_empty()
    }

    fn apply(self, generator: &mut Generator) {
        generator.technology = self.technology;
        generator.source_technology_code = self.source_technology_code;
        // Fault data
        if self.xs.is_some()
            || self.x2_pu.is_some()
            || self.r2_pu.is_some()
            || self.x0_pu.is_some()
            || self.r0_pu.is_some()
            || self.zn.is_some()
        {
            let fd = generator.fault_data.get_or_insert_with(Default::default);
            fd.xs = self.xs;
            fd.x2_pu = self.x2_pu;
            fd.r2_pu = self.r2_pu;
            fd.x0_pu = self.x0_pu;
            fd.r0_pu = self.r0_pu;
            fd.zn = self.zn.map(|(re, im)| num_complex::Complex64::new(re, im));
        }
        // Inverter
        if self.s_rated_mva.is_some() || self.p_available_mw.is_some() {
            let inv = generator.inverter.get_or_insert_with(Default::default);
            inv.s_rated_mva = self.s_rated_mva;
            inv.p_available_mw = self.p_available_mw;
        }
        // Commitment
        if self.p_ecomin.is_some()
            || self.p_ecomax.is_some()
            || self.p_emergency_min.is_some()
            || self.p_emergency_max.is_some()
            || self.p_reg_min.is_some()
            || self.p_reg_max.is_some()
            || self.min_up_time_hr.is_some()
            || self.min_down_time_hr.is_some()
            || self.max_up_time_hr.is_some()
            || self.min_run_at_pmin_hr.is_some()
            || self.max_starts_per_day.is_some()
            || self.max_starts_per_week.is_some()
            || self.max_energy_mwh_per_day.is_some()
            || self.shutdown_ramp_mw_per_min.is_some()
            || self.startup_ramp_mw_per_min.is_some()
            || !self.forbidden_zones.is_empty()
        {
            let cm = generator.commitment.get_or_insert_with(Default::default);
            cm.p_ecomin = self.p_ecomin;
            cm.p_ecomax = self.p_ecomax;
            cm.p_emergency_min = self.p_emergency_min;
            cm.p_emergency_max = self.p_emergency_max;
            cm.p_reg_min = self.p_reg_min;
            cm.p_reg_max = self.p_reg_max;
            cm.min_up_time_hr = self.min_up_time_hr;
            cm.min_down_time_hr = self.min_down_time_hr;
            cm.max_up_time_hr = self.max_up_time_hr;
            cm.min_run_at_pmin_hr = self.min_run_at_pmin_hr;
            cm.max_starts_per_day = self.max_starts_per_day;
            cm.max_starts_per_week = self.max_starts_per_week;
            cm.max_energy_mwh_per_day = self.max_energy_mwh_per_day;
            cm.shutdown_ramp_mw_per_min = self.shutdown_ramp_mw_per_min;
            cm.startup_ramp_mw_per_min = self.startup_ramp_mw_per_min;
            cm.forbidden_zones = self.forbidden_zones;
        }
        // Ramping
        if !self.ramp_up_curve.is_empty()
            || !self.ramp_down_curve.is_empty()
            || !self.emergency_ramp_up_curve.is_empty()
            || !self.emergency_ramp_down_curve.is_empty()
            || !self.reg_ramp_up_curve.is_empty()
            || !self.reg_ramp_down_curve.is_empty()
        {
            let rp = generator.ramping.get_or_insert_with(Default::default);
            rp.ramp_up_curve = self.ramp_up_curve;
            rp.ramp_down_curve = self.ramp_down_curve;
            rp.emergency_ramp_up_curve = self.emergency_ramp_up_curve;
            rp.emergency_ramp_down_curve = self.emergency_ramp_down_curve;
            rp.reg_ramp_up_curve = self.reg_ramp_up_curve;
            rp.reg_ramp_down_curve = self.reg_ramp_down_curve;
        }
        // Fuel
        if self.primary_fuel.is_some()
            || self.backup_fuel.is_some()
            || self.fuel_switch_time_min.is_some()
            || self.fuel_type.is_some()
            || self.heat_rate_btu_mwh.is_some()
        {
            let fl = generator.fuel.get_or_insert_with(Default::default);
            fl.primary_fuel = self.primary_fuel;
            fl.backup_fuel = self.backup_fuel;
            fl.fuel_switch_time_min = self.fuel_switch_time_min;
            fl.fuel_type = self.fuel_type;
            fl.heat_rate_btu_mwh = self.heat_rate_btu_mwh;
        }
        // Market
        if self.energy_offer.is_some()
            || !self.reserve_offers.is_empty()
            || !self.qualifications.is_empty()
        {
            let mk = generator.market.get_or_insert_with(Default::default);
            mk.energy_offer = self.energy_offer;
            mk.reserve_offers = self.reserve_offers;
            mk.qualifications = self.qualifications;
        }
        // Reactive capability (pq_curve)
        if !self.pq_curve.is_empty() {
            generator
                .reactive_capability
                .get_or_insert_with(Default::default)
                .pq_curve = self.pq_curve;
        }
        generator.forced_outage_rate = self.forced_outage_rate;
        generator.storage = self.storage;
        generator.owners = self.owners;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedLoadExtra {
    index: u32,
    extra: LoadExtra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoadExtra {
    owners: Vec<OwnershipEntry>,
}

impl LoadExtra {
    fn from_load(load: &Load) -> Option<Self> {
        let extra = Self {
            owners: load.owners.clone(),
        };
        (!extra.is_empty()).then_some(extra)
    }

    fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }

    fn apply(self, load: &mut Load) {
        load.owners = self.owners;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::type_complexity)]
struct NetworkExtras {
    dispatchable_loads: Vec<DispatchableLoad>,
    switched_shunts: Vec<SwitchedShunt>,
    switched_shunts_opf: Vec<SwitchedShuntOpf>,
    oltc_specs: Vec<OltcSpec>,
    par_specs: Vec<ParSpec>,
    hvdc: HvdcModel,
    area_schedules: Vec<AreaSchedule>,
    facts_devices: Vec<FactsDevice>,
    regions: Vec<Region>,
    owners: Vec<Owner>,
    scheduled_area_transfers: Vec<ScheduledAreaTransfer>,
    impedance_corrections: Vec<ImpedanceCorrectionTable>,
    multi_section_line_groups: Vec<MultiSectionLineGroup>,
    per_length_phase_impedances: HashMap<String, Vec<PhaseImpedanceEntry>>,
    mutual_couplings: Vec<MutualCoupling>,
    grounding_impedances: Vec<GroundingEntry>,
    geo_locations: HashMap<String, Vec<GeoPoint>>,
    interfaces: Vec<Interface>,
    flowgates: Vec<Flowgate>,
    nomograms: Vec<OperatingNomogram>,
    topology: Option<NodeBreakerTopology>,
    induction_machines: Vec<InductionMachine>,
    conditional_limits: BranchConditionalRatings,
    pumped_hydro_units: Vec<PumpedHydroUnit>,
    breaker_ratings: Vec<BreakerRating>,
    fixed_shunts: Vec<FixedShunt>,
    power_injections: Vec<PowerInjection>,
    combined_cycle_plants: Vec<CombinedCyclePlant>,
    outage_schedule: Vec<OutageEntry>,
    reserve_zones: Vec<ReserveZone>,
    ambient: Option<AmbientConditions>,
    emission_policy: Option<EmissionPolicy>,
    measurements: Vec<CimMeasurement>,
    asset_catalog: AssetCatalog,
    operational_limits: OperationalLimits,
    market_rules: Option<MarketRules>,
    boundary_data: BoundaryData,
    protection_data: ProtectionData,
    market_data: MarketData,
    network_operations: NetworkOperationsData,
}

impl NetworkExtras {
    fn from_network(network: &Network) -> Self {
        Self {
            dispatchable_loads: network.market_data.dispatchable_loads.clone(),
            switched_shunts: network.controls.switched_shunts.clone(),
            switched_shunts_opf: network.controls.switched_shunts_opf.clone(),
            oltc_specs: network.controls.oltc_specs.clone(),
            par_specs: network.controls.par_specs.clone(),
            hvdc: network.hvdc.clone(),
            area_schedules: network.area_schedules.clone(),
            facts_devices: network.facts_devices.clone(),
            regions: network.metadata.regions.clone(),
            owners: network.metadata.owners.clone(),
            scheduled_area_transfers: network.metadata.scheduled_area_transfers.clone(),
            impedance_corrections: network.metadata.impedance_corrections.clone(),
            multi_section_line_groups: network.metadata.multi_section_line_groups.clone(),
            per_length_phase_impedances: network.cim.per_length_phase_impedances.clone(),
            mutual_couplings: network.cim.mutual_couplings.clone(),
            grounding_impedances: network.cim.grounding_impedances.clone(),
            geo_locations: network.cim.geo_locations.clone(),
            interfaces: network.interfaces.clone(),
            flowgates: network.flowgates.clone(),
            nomograms: network.nomograms.clone(),
            topology: network.topology.clone(),
            induction_machines: network.induction_machines.clone(),
            conditional_limits: network.conditional_limits.clone(),
            pumped_hydro_units: network.market_data.pumped_hydro_units.clone(),
            breaker_ratings: network.breaker_ratings.clone(),
            fixed_shunts: network.fixed_shunts.clone(),
            power_injections: network.power_injections.clone(),
            combined_cycle_plants: network.market_data.combined_cycle_plants.clone(),
            outage_schedule: network.market_data.outage_schedule.clone(),
            reserve_zones: network.market_data.reserve_zones.clone(),
            ambient: network.market_data.ambient.clone(),
            emission_policy: network.market_data.emission_policy.clone(),
            measurements: network.cim.measurements.clone(),
            asset_catalog: network.cim.asset_catalog.clone(),
            operational_limits: network.cim.operational_limits.clone(),
            market_rules: network.market_data.market_rules.clone(),
            boundary_data: network.cim.boundary_data.clone(),
            protection_data: network.cim.protection_data.clone(),
            market_data: network.cim.market_data.clone(),
            network_operations: network.cim.network_operations.clone(),
        }
    }

    fn is_empty(&self) -> bool {
        self.dispatchable_loads.is_empty()
            && self.switched_shunts.is_empty()
            && self.switched_shunts_opf.is_empty()
            && self.oltc_specs.is_empty()
            && self.par_specs.is_empty()
            && self.hvdc.is_empty()
            && self.area_schedules.is_empty()
            && self.facts_devices.is_empty()
            && self.regions.is_empty()
            && self.owners.is_empty()
            && self.scheduled_area_transfers.is_empty()
            && self.impedance_corrections.is_empty()
            && self.multi_section_line_groups.is_empty()
            && self.per_length_phase_impedances.is_empty()
            && self.mutual_couplings.is_empty()
            && self.grounding_impedances.is_empty()
            && self.geo_locations.is_empty()
            && self.interfaces.is_empty()
            && self.flowgates.is_empty()
            && self.nomograms.is_empty()
            && self.topology.is_none()
            && self.induction_machines.is_empty()
            && self.conditional_limits.is_empty()
            && self.pumped_hydro_units.is_empty()
            && self.breaker_ratings.is_empty()
            && self.fixed_shunts.is_empty()
            && self.power_injections.is_empty()
            && self.combined_cycle_plants.is_empty()
            && self.outage_schedule.is_empty()
            && self.reserve_zones.is_empty()
            && self.ambient.is_none()
            && self.emission_policy.is_none()
            && self.measurements.is_empty()
            && self.asset_catalog.is_empty()
            && self.operational_limits.is_empty()
            && self.market_rules.is_none()
            && self.boundary_data.is_empty()
            && self.protection_data.is_empty()
            && self.market_data.is_empty()
            && self.network_operations.is_empty()
    }

    fn apply(self, network: &mut Network) {
        network.market_data.dispatchable_loads = self.dispatchable_loads;
        network.controls.switched_shunts = self.switched_shunts;
        network.controls.switched_shunts_opf = self.switched_shunts_opf;
        network.controls.oltc_specs = self.oltc_specs;
        network.controls.par_specs = self.par_specs;
        network.hvdc = self.hvdc;
        network.area_schedules = self.area_schedules;
        network.facts_devices = self.facts_devices;
        network.metadata.regions = self.regions;
        network.metadata.owners = self.owners;
        network.metadata.scheduled_area_transfers = self.scheduled_area_transfers;
        network.metadata.impedance_corrections = self.impedance_corrections;
        network.metadata.multi_section_line_groups = self.multi_section_line_groups;
        network.cim.per_length_phase_impedances = self.per_length_phase_impedances;
        network.cim.mutual_couplings = self.mutual_couplings;
        network.cim.grounding_impedances = self.grounding_impedances;
        network.cim.geo_locations = self.geo_locations;
        network.interfaces = self.interfaces;
        network.flowgates = self.flowgates;
        network.nomograms = self.nomograms;
        network.topology = self.topology;
        network.induction_machines = self.induction_machines;
        network.conditional_limits = self.conditional_limits;
        network.market_data.pumped_hydro_units = self.pumped_hydro_units;
        network.breaker_ratings = self.breaker_ratings;
        network.fixed_shunts = self.fixed_shunts;
        network.power_injections = self.power_injections;
        network.market_data.combined_cycle_plants = self.combined_cycle_plants;
        network.market_data.outage_schedule = self.outage_schedule;
        network.market_data.reserve_zones = self.reserve_zones;
        network.market_data.ambient = self.ambient;
        network.market_data.emission_policy = self.emission_policy;
        network.cim.measurements = self.measurements;
        network.cim.asset_catalog = self.asset_catalog;
        network.cim.operational_limits = self.operational_limits;
        network.market_data.market_rules = self.market_rules;
        network.cim.boundary_data = self.boundary_data;
        network.cim.protection_data = self.protection_data;
        network.cim.market_data = self.market_data;
        network.cim.network_operations = self.network_operations;
    }
}

/// Load a binary network file from disk.
pub fn load(path: impl AsRef<Path>) -> Result<Network, Error> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    loads(&bytes)
}

/// Load a binary network from in-memory bytes.
pub fn loads(bytes: &[u8]) -> Result<Network, Error> {
    decode_document(bytes)
}

/// Save a network to a binary file.
pub fn save(network: &Network, path: impl AsRef<Path>) -> Result<(), Error> {
    let bytes = dumps(network)?;
    let mut file = std::fs::File::create(path)?;
    file.write_all(&bytes)?;
    Ok(())
}

/// Serialize a network to binary bytes.
pub fn dumps(network: &Network) -> Result<Vec<u8>, Error> {
    encode_document(network)
}

fn encode_document(network: &Network) -> Result<Vec<u8>, Error> {
    let mut strings = StringTableBuilder::new();

    let network_header = encode_network_header(network, &mut strings);
    let bus_section = encode_buses(&network.buses, &mut strings);
    let bus_extras = encode_bus_extras(&network.buses)?;
    let branch_section = encode_branches(&network.branches, &mut strings);
    let branch_extras = encode_branch_extras(&network.branches)?;
    let generator_section = encode_generators(&network.generators, &mut strings);
    let generator_extras = encode_generator_extras(&network.generators)?;
    let load_section = encode_loads(&network.loads, &mut strings);
    let load_extras = encode_load_extras(&network.loads)?;
    let string_section = encode_strings(strings.freeze());
    let network_extras = encode_network_extras(network)?;

    let mut sections = vec![
        Section {
            id: SECTION_NETWORK_HEADER,
            count: 1,
            payload: network_header,
        },
        string_section,
        Section {
            id: SECTION_BUSES,
            count: network.buses.len() as u32,
            payload: bus_section,
        },
        Section {
            id: SECTION_BRANCHES,
            count: network.branches.len() as u32,
            payload: branch_section,
        },
        Section {
            id: SECTION_GENERATORS,
            count: network.generators.len() as u32,
            payload: generator_section,
        },
        Section {
            id: SECTION_LOADS,
            count: network.loads.len() as u32,
            payload: load_section,
        },
    ];

    if let Some(payload) = bus_extras {
        sections.push(Section {
            id: SECTION_BUS_EXTRAS,
            count: payload.1,
            payload: payload.0,
        });
    }
    if let Some(payload) = branch_extras {
        sections.push(Section {
            id: SECTION_BRANCH_EXTRAS,
            count: payload.1,
            payload: payload.0,
        });
    }
    if let Some(payload) = generator_extras {
        sections.push(Section {
            id: SECTION_GENERATOR_EXTRAS,
            count: payload.1,
            payload: payload.0,
        });
    }
    if let Some(payload) = load_extras {
        sections.push(Section {
            id: SECTION_LOAD_EXTRAS,
            count: payload.1,
            payload: payload.0,
        });
    }
    if let Some(payload) = network_extras {
        sections.push(Section {
            id: SECTION_NETWORK_EXTRAS,
            count: 1,
            payload,
        });
    }

    sections.sort_by_key(|section| section.id);

    let mut body = Vec::new();
    for section in &sections {
        write_u16(&mut body, section.id);
        write_u16(&mut body, 0);
        write_u32(&mut body, section.count);
        write_u64(&mut body, section.payload.len() as u64);
        body.extend_from_slice(&section.payload);
    }

    let checksum = crc32fast::hash(&body);
    let mut output = Vec::with_capacity(HEADER_LEN + body.len());
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&BINARY_REVISION.to_le_bytes());
    output.extend_from_slice(&0u16.to_le_bytes());
    output.extend_from_slice(&(sections.len() as u32).to_le_bytes());
    output.extend_from_slice(&(body.len() as u64).to_le_bytes());
    output.extend_from_slice(&checksum.to_le_bytes());
    output.extend_from_slice(&body);
    Ok(output)
}

fn decode_document(bytes: &[u8]) -> Result<Network, Error> {
    if bytes.len() < HEADER_LEN {
        return Err(Error::InvalidDocument(
            "binary document is shorter than the minimum header".to_string(),
        ));
    }

    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(Error::InvalidDocument(
            "invalid binary magic header".to_string(),
        ));
    }

    let mut reader = Reader::new(bytes);
    let _magic = reader.take(MAGIC.len())?;
    let revision = reader.u16()?;
    if revision != BINARY_REVISION {
        return Err(Error::InvalidDocument(format!(
            "unsupported binary revision {revision}"
        )));
    }
    let _flags = reader.u16()?;
    let section_count = reader.u32()? as usize;
    let body_len = reader.u64()? as usize;
    let checksum = reader.u32()?;
    let body = reader.take(body_len)?;
    if !reader.is_empty() {
        return Err(Error::InvalidDocument(
            "binary document has trailing bytes after body".to_string(),
        ));
    }
    if crc32fast::hash(body) != checksum {
        return Err(Error::InvalidDocument(
            "binary payload checksum mismatch".to_string(),
        ));
    }

    let mut body_reader = Reader::new(body);
    let mut sections = HashMap::new();
    for _ in 0..section_count {
        if body_reader.remaining() < SECTION_HEADER_LEN {
            return Err(Error::InvalidDocument(
                "truncated section header".to_string(),
            ));
        }
        let id = body_reader.u16()?;
        let _section_flags = body_reader.u16()?;
        let count = body_reader.u32()?;
        let len = body_reader.u64()? as usize;
        let payload = body_reader.take(len)?;
        if sections
            .insert(id, SectionView { count, payload })
            .is_some()
        {
            return Err(Error::InvalidDocument(format!("duplicate section id {id}")));
        }
    }

    if !body_reader.is_empty() {
        return Err(Error::InvalidDocument(
            "binary body contains trailing bytes after declared sections".to_string(),
        ));
    }

    let strings = decode_strings(require_section(&sections, SECTION_STRINGS)?)?;
    let mut network = decode_network_header(
        require_section(&sections, SECTION_NETWORK_HEADER)?,
        &strings,
    )?;
    network.buses = decode_buses(require_section(&sections, SECTION_BUSES)?, &strings)?;
    network.branches = decode_branches(require_section(&sections, SECTION_BRANCHES)?, &strings)?;
    network.generators =
        decode_generators(require_section(&sections, SECTION_GENERATORS)?, &strings)?;
    network.loads = decode_loads(require_section(&sections, SECTION_LOADS)?, &strings)?;

    if let Some(section) = sections.get(&SECTION_BUS_EXTRAS) {
        apply_bus_extras(section, &mut network)?;
    }
    if let Some(section) = sections.get(&SECTION_BRANCH_EXTRAS) {
        apply_branch_extras(section, &mut network)?;
    }
    if let Some(section) = sections.get(&SECTION_GENERATOR_EXTRAS) {
        apply_generator_extras(section, &mut network)?;
    }
    if let Some(section) = sections.get(&SECTION_LOAD_EXTRAS) {
        apply_load_extras(section, &mut network)?;
    }
    if let Some(section) = sections.get(&SECTION_NETWORK_EXTRAS) {
        decode_cbor::<NetworkExtras>(section.payload)?.apply(&mut network);
    }

    Ok(network)
}

fn require_section<'a>(
    sections: &'a HashMap<u16, SectionView<'a>>,
    id: u16,
) -> Result<SectionView<'a>, Error> {
    sections
        .get(&id)
        .copied()
        .ok_or_else(|| Error::InvalidDocument(format!("missing required section id {id}")))
}

fn encode_strings(table: StringTable) -> Section {
    let mut payload = Vec::new();
    for value in &table.values {
        write_bytes(&mut payload, value.as_bytes());
    }
    Section {
        id: SECTION_STRINGS,
        count: table.values.len() as u32,
        payload,
    }
}

fn decode_strings(section: SectionView<'_>) -> Result<StringTable, Error> {
    let mut reader = Reader::new(section.payload);
    let mut values = Vec::with_capacity(section.count as usize);
    for _ in 0..section.count {
        let bytes = read_bytes(&mut reader)?;
        let value = String::from_utf8(bytes)
            .map_err(|error| Error::InvalidDocument(format!("invalid UTF-8 string: {error}")))?;
        values.push(value);
    }
    if !reader.is_empty() {
        return Err(Error::InvalidDocument(
            "string table payload has trailing bytes".to_string(),
        ));
    }
    Ok(StringTable { values })
}

fn encode_network_header(network: &Network, strings: &mut StringTableBuilder) -> Vec<u8> {
    let mut payload = Vec::new();
    write_u32(&mut payload, strings.intern(&network.name));
    write_f64(&mut payload, network.base_mva);
    write_f64(&mut payload, network.freq_hz);
    payload
}

fn decode_network_header(
    section: SectionView<'_>,
    strings: &StringTable,
) -> Result<Network, Error> {
    let mut reader = Reader::new(section.payload);
    let name_id = reader.u32()?;
    let base_mva = reader.f64()?;
    let freq_hz = reader.f64()?;
    if !reader.is_empty() {
        return Err(Error::InvalidDocument(
            "network header payload has trailing bytes".to_string(),
        ));
    }
    Ok(Network {
        name: strings.resolve(name_id)?.to_string(),
        base_mva,
        freq_hz,
        ..Network::default()
    })
}

fn encode_buses(buses: &[Bus], strings: &mut StringTableBuilder) -> Vec<u8> {
    let mut payload = Vec::new();
    for bus in buses {
        write_u32(&mut payload, bus.number);
        write_u32(&mut payload, strings.intern(&bus.name));
        write_u8(&mut payload, encode_bus_type(bus.bus_type));
        write_u32(&mut payload, bus.area);
        write_u32(&mut payload, bus.zone);
        write_u32(&mut payload, bus.island_id);
        write_f64(&mut payload, 0.0); // legacy: active_power_demand_mw (now on Load objects)
        write_f64(&mut payload, 0.0); // legacy: reactive_power_demand_mvar (now on Load objects)
        write_f64(&mut payload, bus.shunt_conductance_mw);
        write_f64(&mut payload, bus.shunt_susceptance_mvar);
        write_f64(&mut payload, bus.voltage_magnitude_pu);
        write_f64(&mut payload, bus.voltage_angle_rad);
        write_f64(&mut payload, bus.base_kv);
        write_f64(&mut payload, bus.voltage_max_pu);
        write_f64(&mut payload, bus.voltage_min_pu);
    }
    payload
}

fn decode_buses(section: SectionView<'_>, strings: &StringTable) -> Result<Vec<Bus>, Error> {
    let mut reader = Reader::new(section.payload);
    let mut buses = Vec::with_capacity(section.count as usize);
    for _ in 0..section.count {
        let bus = Bus {
            number: reader.u32()?,
            name: strings.resolve(reader.u32()?)?.to_string(),
            bus_type: decode_bus_type(reader.u8()?)?,
            area: reader.u32()?,
            zone: reader.u32()?,
            island_id: reader.u32()?,
            shunt_conductance_mw: {
                let _legacy_pd = reader.f64()?; // legacy: active_power_demand_mw
                let _legacy_qd = reader.f64()?; // legacy: reactive_power_demand_mvar
                reader.f64()?
            },
            shunt_susceptance_mvar: reader.f64()?,
            voltage_magnitude_pu: reader.f64()?,
            voltage_angle_rad: reader.f64()?,
            base_kv: reader.f64()?,
            voltage_max_pu: reader.f64()?,
            voltage_min_pu: reader.f64()?,
            ..Bus::default()
        };
        buses.push(bus);
    }
    if !reader.is_empty() {
        return Err(Error::InvalidDocument(
            "bus section payload has trailing bytes".to_string(),
        ));
    }
    Ok(buses)
}

fn encode_bus_extras(buses: &[Bus]) -> Result<Option<(Vec<u8>, u32)>, Error> {
    let extras: Vec<_> = buses
        .iter()
        .enumerate()
        .filter_map(|(index, bus)| {
            BusExtra::from_bus(bus).map(|extra| IndexedBusExtra {
                index: index as u32,
                extra,
            })
        })
        .collect();
    if extras.is_empty() {
        return Ok(None);
    }
    Ok(Some((encode_cbor(&extras)?, extras.len() as u32)))
}

fn apply_bus_extras(section: &SectionView<'_>, network: &mut Network) -> Result<(), Error> {
    let extras: Vec<IndexedBusExtra> = decode_cbor(section.payload)?;
    for extra in extras {
        let bus = network.buses.get_mut(extra.index as usize).ok_or_else(|| {
            Error::InvalidDocument(format!("bus extra index {} out of range", extra.index))
        })?;
        extra.extra.apply(bus);
    }
    Ok(())
}

fn encode_branches(branches: &[Branch], strings: &mut StringTableBuilder) -> Vec<u8> {
    let mut payload = Vec::new();
    for branch in branches {
        let zs = branch.zero_seq.as_ref();
        let oc = branch.opf_control.as_ref();
        let ld = branch.line_data.as_ref();
        let hd = branch.harmonic.as_ref();
        let td = branch.transformer_data.as_ref();
        let sc = branch.series_comp.as_ref();

        let mut flags = 0u8;
        if branch.in_service {
            flags |= 1 << 0;
        }
        if zs.is_some_and(|z| z.delta_connected) {
            flags |= 1 << 1;
        }
        if sc.is_some_and(|s| s.bypassed) {
            flags |= 1 << 2;
        }
        if branch.angle_diff_min_rad.is_some() {
            flags |= 1 << 3;
        }
        if branch.angle_diff_max_rad.is_some() {
            flags |= 1 << 4;
        }

        write_u32(&mut payload, branch.from_bus);
        write_u32(&mut payload, branch.to_bus);
        write_u32(&mut payload, strings.intern(&branch.circuit));
        write_u8(&mut payload, flags);
        write_u8(
            &mut payload,
            encode_transformer_connection(td.map(|t| t.transformer_connection).unwrap_or_default()),
        );
        write_u8(
            &mut payload,
            encode_tap_mode(oc.map(|c| c.tap_mode).unwrap_or_default()),
        );
        write_u8(
            &mut payload,
            encode_phase_mode(oc.map(|c| c.phase_mode).unwrap_or_default()),
        );
        write_u8(&mut payload, encode_branch_type(branch.branch_type));
        write_f64(&mut payload, branch.r);
        write_f64(&mut payload, branch.x);
        write_f64(&mut payload, branch.b);
        write_f64(&mut payload, branch.rating_a_mva);
        write_f64(&mut payload, branch.rating_b_mva);
        write_f64(&mut payload, branch.rating_c_mva);
        write_f64(&mut payload, branch.tap);
        write_f64(&mut payload, branch.phase_shift_rad);
        write_f64(&mut payload, hd.map(|h| h.skin_effect_alpha).unwrap_or(0.0));
        write_f64(&mut payload, branch.g_pi);
        write_f64(&mut payload, branch.g_mag);
        write_f64(&mut payload, branch.b_mag);
        write_f64(&mut payload, zs.map(|z| z.gi0).unwrap_or(0.0));
        write_f64(&mut payload, zs.map(|z| z.bi0).unwrap_or(0.0));
        write_f64(&mut payload, zs.map(|z| z.gj0).unwrap_or(0.0));
        write_f64(&mut payload, zs.map(|z| z.bj0).unwrap_or(0.0));
        write_f64(
            &mut payload,
            oc.map(|c| c.tap_min)
                .unwrap_or(BranchOpfControl::default().tap_min),
        );
        write_f64(
            &mut payload,
            oc.map(|c| c.tap_max)
                .unwrap_or(BranchOpfControl::default().tap_max),
        );
        write_f64(&mut payload, oc.map(|c| c.tap_step).unwrap_or(0.0));
        write_f64(
            &mut payload,
            oc.map(|c| c.phase_min_rad)
                .unwrap_or(BranchOpfControl::default().phase_min_rad),
        );
        write_f64(
            &mut payload,
            oc.map(|c| c.phase_max_rad)
                .unwrap_or(BranchOpfControl::default().phase_max_rad),
        );
        write_f64(&mut payload, oc.map(|c| c.phase_step_rad).unwrap_or(0.0));
        write_f64(&mut payload, ld.map(|l| l.r_temp_coeff).unwrap_or(0.0));
        write_f64(&mut payload, ld.map(|l| l.r_ref_temp_c).unwrap_or(20.0));
        if let Some(value) = branch.angle_diff_min_rad {
            write_f64(&mut payload, value);
        }
        if let Some(value) = branch.angle_diff_max_rad {
            write_f64(&mut payload, value);
        }
    }
    payload
}

fn decode_branches(section: SectionView<'_>, strings: &StringTable) -> Result<Vec<Branch>, Error> {
    let mut reader = Reader::new(section.payload);
    let mut branches = Vec::with_capacity(section.count as usize);
    for _ in 0..section.count {
        let from_bus = reader.u32()?;
        let to_bus = reader.u32()?;
        let circuit = strings.resolve(reader.u32()?)?.to_string();
        let flags = reader.u8()?;
        let in_service = (flags & (1 << 0)) != 0;
        let delta_connected = (flags & (1 << 1)) != 0;
        let bypassed = (flags & (1 << 2)) != 0;
        let has_angle_min = (flags & (1 << 3)) != 0;
        let has_angle_max = (flags & (1 << 4)) != 0;

        let transformer_connection = decode_transformer_connection(reader.u8()?)?;
        let tap_mode = decode_tap_mode(reader.u8()?)?;
        let phase_mode = decode_phase_mode(reader.u8()?)?;
        let branch_type = decode_branch_type(reader.u8()?)?;
        let r = reader.f64()?;
        let x = reader.f64()?;
        let b_val = reader.f64()?;
        let rating_a_mva = reader.f64()?;
        let rating_b_mva = reader.f64()?;
        let rating_c_mva = reader.f64()?;
        let tap = reader.f64()?;
        let phase_shift_rad = reader.f64()?;
        let skin_effect_alpha = reader.f64()?;
        let g_pi = reader.f64()?;
        let g_mag = reader.f64()?;
        let b_mag = reader.f64()?;
        let gi0 = reader.f64()?;
        let bi0 = reader.f64()?;
        let gj0 = reader.f64()?;
        let bj0 = reader.f64()?;
        let tap_min = reader.f64()?;
        let tap_max = reader.f64()?;
        let tap_step = reader.f64()?;
        let phase_min_rad = reader.f64()?;
        let phase_max_rad = reader.f64()?;
        let phase_step_rad = reader.f64()?;
        let r_temp_coeff = reader.f64()?;
        let r_ref_temp_c = reader.f64()?;

        // Reconstruct optional sub-structs only when they contain non-default data.
        let has_zero_seq = delta_connected
            || gi0.abs() > 0.0
            || bi0.abs() > 0.0
            || gj0.abs() > 0.0
            || bj0.abs() > 0.0;
        let zero_seq = if has_zero_seq {
            Some(ZeroSeqData {
                r0: 0.0,
                x0: 0.0,
                b0: 0.0,
                zn: None,
                gi0,
                bi0,
                gj0,
                bj0,
                delta_connected,
            })
        } else {
            None
        };

        let opf_default = BranchOpfControl::default();
        let has_opf = tap_mode != TapMode::Fixed
            || phase_mode != PhaseMode::Fixed
            || (tap_min - opf_default.tap_min).abs() > 1e-12
            || (tap_max - opf_default.tap_max).abs() > 1e-12
            || tap_step.abs() > 1e-12
            || (phase_min_rad - opf_default.phase_min_rad).abs() > 1e-12
            || (phase_max_rad - opf_default.phase_max_rad).abs() > 1e-12
            || phase_step_rad.abs() > 1e-12;
        let opf_control = if has_opf {
            Some(BranchOpfControl {
                tap_mode,
                tap_min,
                tap_max,
                tap_step,
                phase_mode,
                phase_min_rad,
                phase_max_rad,
                phase_step_rad,
            })
        } else {
            None
        };

        let has_transformer_data =
            !matches!(transformer_connection, TransformerConnection::WyeGWyeG);
        let transformer_data = if has_transformer_data {
            Some(TransformerData {
                transformer_connection,
                ..TransformerData::default()
            })
        } else {
            None
        };

        let has_line_data = r_temp_coeff.abs() > 1e-12 || (r_ref_temp_c - 20.0).abs() > 1e-12;
        let line_data = if has_line_data {
            Some(LineData {
                r_temp_coeff,
                r_ref_temp_c,
                ..LineData::default()
            })
        } else {
            None
        };

        let has_harmonic = skin_effect_alpha.abs() > 1e-12;
        let harmonic = if has_harmonic {
            Some(HarmonicData {
                skin_effect_alpha,
                ..HarmonicData::default()
            })
        } else {
            None
        };

        let bypassed_val = bypassed;
        let series_comp = if bypassed_val {
            Some(SeriesCompData {
                bypassed: true,
                ..SeriesCompData::default()
            })
        } else {
            None
        };

        let mut branch = Branch {
            from_bus,
            to_bus,
            circuit,
            in_service,
            branch_type,
            r,
            x,
            b: b_val,
            rating_a_mva,
            rating_b_mva,
            rating_c_mva,
            tap,
            phase_shift_rad,
            g_pi,
            g_mag,
            b_mag,
            opf_control,
            transformer_data,
            line_data,
            zero_seq,
            harmonic,
            series_comp,
            ..Branch::default()
        };
        if has_angle_min {
            branch.angle_diff_min_rad = Some(reader.f64()?);
        }
        if has_angle_max {
            branch.angle_diff_max_rad = Some(reader.f64()?);
        }
        branches.push(branch);
    }
    if !reader.is_empty() {
        return Err(Error::InvalidDocument(
            "branch section payload has trailing bytes".to_string(),
        ));
    }
    Ok(branches)
}

fn encode_branch_extras(branches: &[Branch]) -> Result<Option<(Vec<u8>, u32)>, Error> {
    let extras: Vec<_> = branches
        .iter()
        .enumerate()
        .filter_map(|(index, branch)| {
            BranchExtra::from_branch(branch).map(|extra| IndexedBranchExtra {
                index: index as u32,
                extra,
            })
        })
        .collect();
    if extras.is_empty() {
        return Ok(None);
    }
    Ok(Some((encode_cbor(&extras)?, extras.len() as u32)))
}

fn apply_branch_extras(section: &SectionView<'_>, network: &mut Network) -> Result<(), Error> {
    let extras: Vec<IndexedBranchExtra> = decode_cbor(section.payload)?;
    for extra in extras {
        let branch = network
            .branches
            .get_mut(extra.index as usize)
            .ok_or_else(|| {
                Error::InvalidDocument(format!("branch extra index {} out of range", extra.index))
            })?;
        extra.extra.apply(branch);
    }
    Ok(())
}

fn encode_generators(generators: &[Generator], strings: &mut StringTableBuilder) -> Vec<u8> {
    let mut payload = Vec::new();
    for generator in generators {
        let mut flags = 0u16;
        if generator.voltage_regulated {
            flags |= 1 << 0;
        }
        if generator.in_service {
            flags |= 1 << 1;
        }
        if generator.inverter.as_ref().is_some_and(|i| i.curtailable) {
            flags |= 1 << 2;
        }
        if generator.inverter.as_ref().is_some_and(|i| i.grid_forming) {
            flags |= 1 << 3;
        }
        if generator.fuel.as_ref().is_some_and(|f| f.on_backup_fuel) {
            flags |= 1 << 4;
        }
        if generator.quick_start {
            flags |= 1 << 5;
        }
        if generator.pfr_eligible {
            flags |= 1 << 6;
        }
        if generator.reg_bus.is_some() {
            flags |= 1 << 7;
        }
        if generator.agc_participation_factor.is_some() {
            flags |= 1 << 8;
        }
        let rc = generator.reactive_capability.as_ref();
        if rc.and_then(|r| r.pc1).is_some() {
            flags |= 1 << 9;
        }
        if rc.and_then(|r| r.pc2).is_some() {
            flags |= 1 << 10;
        }
        if rc.and_then(|r| r.qc1min).is_some() {
            flags |= 1 << 11;
        }
        if rc.and_then(|r| r.qc1max).is_some() {
            flags |= 1 << 12;
        }
        if rc.and_then(|r| r.qc2min).is_some() {
            flags |= 1 << 13;
        }
        if rc.and_then(|r| r.qc2max).is_some() {
            flags |= 1 << 14;
        }
        if generator.h_inertia_s.is_some() {
            flags |= 1 << 15;
        }

        write_u32(&mut payload, strings.intern(&generator.id));
        write_u32(&mut payload, generator.bus);
        write_bool(&mut payload, generator.machine_id.is_some());
        if let Some(machine_id) = &generator.machine_id {
            write_u32(&mut payload, strings.intern(machine_id));
        }
        write_u16(&mut payload, flags);
        write_u8(&mut payload, encode_gen_type(generator.gen_type));
        write_u8(
            &mut payload,
            encode_commitment_status(
                generator
                    .commitment
                    .as_ref()
                    .map(|c| c.status)
                    .unwrap_or(surge_network::network::CommitmentStatus::Market),
            ),
        );
        write_f64(&mut payload, generator.p);
        write_f64(&mut payload, generator.q);
        write_f64(&mut payload, generator.qmax);
        write_f64(&mut payload, generator.qmin);
        write_f64(&mut payload, generator.voltage_setpoint_pu);
        write_f64(&mut payload, generator.machine_base_mva);
        write_f64(&mut payload, generator.pmax);
        write_f64(&mut payload, generator.pmin);
        write_f64(
            &mut payload,
            generator
                .inverter
                .as_ref()
                .map_or(0.0, |i| i.inverter_loss_a_mw),
        );
        write_f64(
            &mut payload,
            generator
                .inverter
                .as_ref()
                .map_or(0.0, |i| i.inverter_loss_b_pu),
        );
        write_f64(
            &mut payload,
            generator
                .commitment
                .as_ref()
                .map_or(0.0, |c| c.hours_online),
        );
        write_f64(
            &mut payload,
            generator
                .commitment
                .as_ref()
                .map_or(0.0, |c| c.hours_offline),
        );
        write_cost_curve(&mut payload, &generator.cost);
        {
            let default_rates = EmissionRates::default();
            let rates = generator
                .fuel
                .as_ref()
                .map_or(&default_rates, |f| &f.emission_rates);
            write_emission_rates(&mut payload, rates);
        }
        if let Some(value) = generator.reg_bus {
            write_u32(&mut payload, value);
        }
        if let Some(value) = generator.agc_participation_factor {
            write_f64(&mut payload, value);
        }
        if let Some(value) = rc.and_then(|r| r.pc1) {
            write_f64(&mut payload, value);
        }
        if let Some(value) = rc.and_then(|r| r.pc2) {
            write_f64(&mut payload, value);
        }
        if let Some(value) = rc.and_then(|r| r.qc1min) {
            write_f64(&mut payload, value);
        }
        if let Some(value) = rc.and_then(|r| r.qc1max) {
            write_f64(&mut payload, value);
        }
        if let Some(value) = rc.and_then(|r| r.qc2min) {
            write_f64(&mut payload, value);
        }
        if let Some(value) = rc.and_then(|r| r.qc2max) {
            write_f64(&mut payload, value);
        }
        if let Some(value) = generator.h_inertia_s {
            write_f64(&mut payload, value);
        }
    }
    payload
}

fn decode_generators(
    section: SectionView<'_>,
    strings: &StringTable,
) -> Result<Vec<Generator>, Error> {
    let mut reader = Reader::new(section.payload);
    let mut generators = Vec::with_capacity(section.count as usize);
    for _ in 0..section.count {
        let id = strings.resolve(reader.u32()?)?.to_string();
        let bus = reader.u32()?;
        let has_machine_id = read_bool(&mut reader)?;
        let machine_id = if has_machine_id {
            Some(strings.resolve(reader.u32()?)?.to_string())
        } else {
            None
        };
        let flags = reader.u16()?;
        let gen_type = decode_gen_type(reader.u8()?)?;
        let commitment_status = decode_commitment_status(reader.u8()?)?;
        let pg = reader.f64()?;
        let qg = reader.f64()?;
        let qmax = reader.f64()?;
        let qmin = reader.f64()?;
        let voltage_setpoint_pu = reader.f64()?;
        let machine_base_mva = reader.f64()?;
        let pmax = reader.f64()?;
        let pmin = reader.f64()?;
        let inverter_loss_a_mw = reader.f64()?;
        let inverter_loss_b_pu = reader.f64()?;
        let hours_online = reader.f64()?;
        let hours_offline = reader.f64()?;
        let cost = read_cost_curve(&mut reader)?;
        let emission_rates = read_emission_rates(&mut reader)?;
        let reg_bus = (flags & (1 << 7) != 0).then(|| reader.u32()).transpose()?;
        let agc_participation_factor = (flags & (1 << 8) != 0).then(|| reader.f64()).transpose()?;
        let pc1 = (flags & (1 << 9) != 0).then(|| reader.f64()).transpose()?;
        let pc2 = (flags & (1 << 10) != 0).then(|| reader.f64()).transpose()?;
        let qc1min = (flags & (1 << 11) != 0).then(|| reader.f64()).transpose()?;
        let qc1max = (flags & (1 << 12) != 0).then(|| reader.f64()).transpose()?;
        let qc2min = (flags & (1 << 13) != 0).then(|| reader.f64()).transpose()?;
        let qc2max = (flags & (1 << 14) != 0).then(|| reader.f64()).transpose()?;
        let h_inertia_s = (flags & (1 << 15) != 0).then(|| reader.f64()).transpose()?;

        // Build optional sub-structs from decoded binary fields.
        let has_inverter = inverter_loss_a_mw.abs() > 1e-20
            || inverter_loss_b_pu.abs() > 1e-20
            || (flags & (1 << 2)) != 0
            || (flags & (1 << 3)) != 0;
        let inverter = if has_inverter {
            Some(surge_network::network::InverterParams {
                curtailable: (flags & (1 << 2)) != 0,
                grid_forming: (flags & (1 << 3)) != 0,
                inverter_loss_a_mw,
                inverter_loss_b_pu,
                ..Default::default()
            })
        } else {
            None
        };
        let has_commitment = commitment_status != surge_network::network::CommitmentStatus::Market
            || hours_online.abs() > 1e-20
            || hours_offline.abs() > 1e-20;
        let commitment = if has_commitment {
            Some(surge_network::network::CommitmentParams {
                status: commitment_status,
                hours_online,
                hours_offline,
                ..Default::default()
            })
        } else {
            None
        };
        let has_fuel = (flags & (1 << 4)) != 0
            || emission_rates.co2.abs() > 1e-20
            || emission_rates.nox.abs() > 1e-20
            || emission_rates.so2.abs() > 1e-20
            || emission_rates.pm25.abs() > 1e-20;
        let fuel = if has_fuel {
            Some(surge_network::network::FuelParams {
                on_backup_fuel: (flags & (1 << 4)) != 0,
                emission_rates,
                ..Default::default()
            })
        } else {
            None
        };
        let has_rc = pc1.is_some()
            || pc2.is_some()
            || qc1min.is_some()
            || qc1max.is_some()
            || qc2min.is_some()
            || qc2max.is_some();
        let reactive_capability = if has_rc {
            Some(surge_network::network::ReactiveCapability {
                pc1,
                pc2,
                qc1min,
                qc1max,
                qc2min,
                qc2max,
                ..Default::default()
            })
        } else {
            None
        };
        let generator = Generator {
            id,
            bus,
            machine_id,
            voltage_regulated: (flags & (1 << 0)) != 0,
            in_service: (flags & (1 << 1)) != 0,
            quick_start: (flags & (1 << 5)) != 0,
            pfr_eligible: (flags & (1 << 6)) != 0,
            gen_type,
            commitment,
            inverter,
            fuel,
            reactive_capability,
            p: pg,
            q: qg,
            qmax,
            qmin,
            voltage_setpoint_pu,
            machine_base_mva,
            pmax,
            pmin,
            cost,
            reg_bus,
            agc_participation_factor,
            h_inertia_s,
            ..Generator::default()
        };
        generators.push(generator);
    }
    if !reader.is_empty() {
        return Err(Error::InvalidDocument(
            "generator section payload has trailing bytes".to_string(),
        ));
    }
    Ok(generators)
}

fn encode_generator_extras(generators: &[Generator]) -> Result<Option<(Vec<u8>, u32)>, Error> {
    let extras: Vec<_> = generators
        .iter()
        .enumerate()
        .filter_map(|(index, generator)| {
            GeneratorExtra::from_generator(generator).map(|extra| IndexedGeneratorExtra {
                index: index as u32,
                extra,
            })
        })
        .collect();
    if extras.is_empty() {
        return Ok(None);
    }
    Ok(Some((encode_cbor(&extras)?, extras.len() as u32)))
}

fn apply_generator_extras(section: &SectionView<'_>, network: &mut Network) -> Result<(), Error> {
    let extras: Vec<IndexedGeneratorExtra> = decode_cbor(section.payload)?;
    for extra in extras {
        let generator = network
            .generators
            .get_mut(extra.index as usize)
            .ok_or_else(|| {
                Error::InvalidDocument(format!(
                    "generator extra index {} out of range",
                    extra.index
                ))
            })?;
        extra.extra.apply(generator);
    }
    Ok(())
}

fn encode_load_extras(loads: &[Load]) -> Result<Option<(Vec<u8>, u32)>, Error> {
    let extras: Vec<_> = loads
        .iter()
        .enumerate()
        .filter_map(|(index, load)| {
            LoadExtra::from_load(load).map(|extra| IndexedLoadExtra {
                index: index as u32,
                extra,
            })
        })
        .collect();
    if extras.is_empty() {
        return Ok(None);
    }
    Ok(Some((encode_cbor(&extras)?, extras.len() as u32)))
}

fn apply_load_extras(section: &SectionView<'_>, network: &mut Network) -> Result<(), Error> {
    let extras: Vec<IndexedLoadExtra> = decode_cbor(section.payload)?;
    for extra in extras {
        let load = network.loads.get_mut(extra.index as usize).ok_or_else(|| {
            Error::InvalidDocument(format!("load extra index {} out of range", extra.index))
        })?;
        extra.extra.apply(load);
    }
    Ok(())
}

fn encode_loads(loads: &[Load], strings: &mut StringTableBuilder) -> Vec<u8> {
    let mut payload = Vec::new();
    for load in loads {
        let mut flags = 0u8;
        if load.in_service {
            flags |= 1 << 0;
        }
        if load.conforming {
            flags |= 1 << 1;
        }
        if load.load_class.is_some() {
            flags |= 1 << 2;
        }
        if load.shedding_priority.is_some() {
            flags |= 1 << 3;
        }
        write_u32(&mut payload, load.bus);
        write_u32(&mut payload, strings.intern(&load.id));
        write_u8(&mut payload, flags);
        write_u8(&mut payload, encode_load_connection(load.connection));
        write_f64(&mut payload, load.active_power_demand_mw);
        write_f64(&mut payload, load.reactive_power_demand_mvar);
        write_f64(&mut payload, load.zip_p_impedance_frac);
        write_f64(&mut payload, load.zip_p_current_frac);
        write_f64(&mut payload, load.zip_p_power_frac);
        write_f64(&mut payload, load.zip_q_impedance_frac);
        write_f64(&mut payload, load.zip_q_current_frac);
        write_f64(&mut payload, load.zip_q_power_frac);
        write_f64(&mut payload, load.freq_sensitivity_p_pct_per_hz);
        write_f64(&mut payload, load.freq_sensitivity_q_pct_per_hz);
        write_f64(&mut payload, load.frac_motor_a);
        write_f64(&mut payload, load.frac_motor_b);
        write_f64(&mut payload, load.frac_motor_c);
        write_f64(&mut payload, load.frac_motor_d);
        write_f64(&mut payload, load.frac_electronic);
        write_f64(&mut payload, load.frac_static);
        if let Some(value) = load.load_class {
            write_u8(&mut payload, encode_load_class(value));
        }
        if let Some(value) = load.shedding_priority {
            write_u32(&mut payload, value);
        }
    }
    payload
}

fn decode_loads(section: SectionView<'_>, strings: &StringTable) -> Result<Vec<Load>, Error> {
    let mut reader = Reader::new(section.payload);
    let mut loads = Vec::with_capacity(section.count as usize);
    for _ in 0..section.count {
        let bus = reader.u32()?;
        let id = strings.resolve(reader.u32()?)?.to_string();
        let flags = reader.u8()?;
        let connection = decode_load_connection(reader.u8()?)?;
        let has_load_class = (flags & (1 << 2)) != 0;
        let has_shedding_priority = (flags & (1 << 3)) != 0;
        let load = Load {
            bus,
            id,
            in_service: (flags & (1 << 0)) != 0,
            conforming: (flags & (1 << 1)) != 0,
            connection,
            active_power_demand_mw: reader.f64()?,
            reactive_power_demand_mvar: reader.f64()?,
            zip_p_impedance_frac: reader.f64()?,
            zip_p_current_frac: reader.f64()?,
            zip_p_power_frac: reader.f64()?,
            zip_q_impedance_frac: reader.f64()?,
            zip_q_current_frac: reader.f64()?,
            zip_q_power_frac: reader.f64()?,
            freq_sensitivity_p_pct_per_hz: reader.f64()?,
            freq_sensitivity_q_pct_per_hz: reader.f64()?,
            frac_motor_a: reader.f64()?,
            frac_motor_b: reader.f64()?,
            frac_motor_c: reader.f64()?,
            frac_motor_d: reader.f64()?,
            frac_electronic: reader.f64()?,
            frac_static: reader.f64()?,
            load_class: if has_load_class {
                Some(decode_load_class(reader.u8()?)?)
            } else {
                None
            },
            shedding_priority: if has_shedding_priority {
                Some(reader.u32()?)
            } else {
                None
            },
            owners: Vec::new(),
        };
        loads.push(load);
    }
    if !reader.is_empty() {
        return Err(Error::InvalidDocument(
            "load section payload has trailing bytes".to_string(),
        ));
    }
    Ok(loads)
}

fn encode_network_extras(network: &Network) -> Result<Option<Vec<u8>>, Error> {
    let extras = NetworkExtras::from_network(network);
    if extras.is_empty() {
        return Ok(None);
    }
    Ok(Some(encode_cbor(&extras)?))
}

fn encode_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    let mut payload = Vec::new();
    ciborium::into_writer(value, &mut payload)?;
    Ok(payload)
}

fn decode_cbor<T: DeserializeOwned>(payload: &[u8]) -> Result<T, Error> {
    Ok(ciborium::from_reader(payload)?)
}

fn write_cost_curve(payload: &mut Vec<u8>, cost: &Option<CostCurve>) {
    match cost {
        None => write_u8(payload, 0),
        Some(CostCurve::Polynomial {
            startup,
            shutdown,
            coeffs,
        }) => {
            write_u8(payload, 1);
            write_f64(payload, *startup);
            write_f64(payload, *shutdown);
            write_u32(payload, coeffs.len() as u32);
            for value in coeffs {
                write_f64(payload, *value);
            }
        }
        Some(CostCurve::PiecewiseLinear {
            startup,
            shutdown,
            points,
        }) => {
            write_u8(payload, 2);
            write_f64(payload, *startup);
            write_f64(payload, *shutdown);
            write_u32(payload, points.len() as u32);
            for (x, y) in points {
                write_f64(payload, *x);
                write_f64(payload, *y);
            }
        }
    }
}

fn read_cost_curve(reader: &mut Reader<'_>) -> Result<Option<CostCurve>, Error> {
    Ok(match reader.u8()? {
        0 => None,
        1 => {
            let startup = reader.f64()?;
            let shutdown = reader.f64()?;
            let len = reader.u32()? as usize;
            let mut coeffs = Vec::with_capacity(len);
            for _ in 0..len {
                coeffs.push(reader.f64()?);
            }
            Some(CostCurve::Polynomial {
                startup,
                shutdown,
                coeffs,
            })
        }
        2 => {
            let startup = reader.f64()?;
            let shutdown = reader.f64()?;
            let len = reader.u32()? as usize;
            let mut points = Vec::with_capacity(len);
            for _ in 0..len {
                points.push((reader.f64()?, reader.f64()?));
            }
            Some(CostCurve::PiecewiseLinear {
                startup,
                shutdown,
                points,
            })
        }
        tag => {
            return Err(Error::InvalidDocument(format!(
                "unsupported generator cost curve tag {tag}"
            )));
        }
    })
}

fn write_emission_rates(payload: &mut Vec<u8>, emission_rates: &EmissionRates) {
    write_f64(payload, emission_rates.co2);
    write_f64(payload, emission_rates.nox);
    write_f64(payload, emission_rates.so2);
    write_f64(payload, emission_rates.pm25);
}

fn read_emission_rates(reader: &mut Reader<'_>) -> Result<EmissionRates, Error> {
    Ok(EmissionRates {
        co2: reader.f64()?,
        nox: reader.f64()?,
        so2: reader.f64()?,
        pm25: reader.f64()?,
    })
}

fn write_u8(payload: &mut Vec<u8>, value: u8) {
    payload.push(value);
}

fn write_u16(payload: &mut Vec<u8>, value: u16) {
    payload.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(payload: &mut Vec<u8>, value: u32) {
    payload.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(payload: &mut Vec<u8>, value: u64) {
    payload.extend_from_slice(&value.to_le_bytes());
}

fn write_f64(payload: &mut Vec<u8>, value: f64) {
    payload.extend_from_slice(&value.to_le_bytes());
}

fn write_bool(payload: &mut Vec<u8>, value: bool) {
    write_u8(payload, u8::from(value));
}

fn read_bool(reader: &mut Reader<'_>) -> Result<bool, Error> {
    Ok(match reader.u8()? {
        0 => false,
        1 => true,
        value => {
            return Err(Error::InvalidDocument(format!(
                "invalid encoded boolean value {value}"
            )));
        }
    })
}

fn write_bytes(payload: &mut Vec<u8>, bytes: &[u8]) {
    write_u32(payload, bytes.len() as u32);
    payload.extend_from_slice(bytes);
}

fn read_bytes(reader: &mut Reader<'_>) -> Result<Vec<u8>, Error> {
    let len = reader.u32()? as usize;
    Ok(reader.take(len)?.to_vec())
}

fn encode_bus_type(value: BusType) -> u8 {
    match value {
        BusType::PQ => 1,
        BusType::PV => 2,
        BusType::Slack => 3,
        BusType::Isolated => 4,
    }
}

fn decode_bus_type(value: u8) -> Result<BusType, Error> {
    match value {
        1 => Ok(BusType::PQ),
        2 => Ok(BusType::PV),
        3 => Ok(BusType::Slack),
        4 => Ok(BusType::Isolated),
        _ => Err(Error::InvalidDocument(format!(
            "invalid bus type code {value}"
        ))),
    }
}

fn encode_transformer_connection(value: TransformerConnection) -> u8 {
    match value {
        TransformerConnection::WyeGWyeG => 0,
        TransformerConnection::WyeGDelta => 1,
        TransformerConnection::DeltaWyeG => 2,
        TransformerConnection::DeltaDelta => 3,
        TransformerConnection::WyeGWye => 4,
    }
}

fn decode_transformer_connection(value: u8) -> Result<TransformerConnection, Error> {
    match value {
        0 => Ok(TransformerConnection::WyeGWyeG),
        1 => Ok(TransformerConnection::WyeGDelta),
        2 => Ok(TransformerConnection::DeltaWyeG),
        3 => Ok(TransformerConnection::DeltaDelta),
        4 => Ok(TransformerConnection::WyeGWye),
        _ => Err(Error::InvalidDocument(format!(
            "invalid transformer connection code {value}"
        ))),
    }
}

fn encode_tap_mode(value: TapMode) -> u8 {
    match value {
        TapMode::Fixed => 0,
        TapMode::Continuous => 1,
    }
}

fn decode_tap_mode(value: u8) -> Result<TapMode, Error> {
    match value {
        0 => Ok(TapMode::Fixed),
        1 => Ok(TapMode::Continuous),
        _ => Err(Error::InvalidDocument(format!(
            "invalid tap mode code {value}"
        ))),
    }
}

fn encode_phase_mode(value: PhaseMode) -> u8 {
    match value {
        PhaseMode::Fixed => 0,
        PhaseMode::Continuous => 1,
    }
}

fn decode_phase_mode(value: u8) -> Result<PhaseMode, Error> {
    match value {
        0 => Ok(PhaseMode::Fixed),
        1 => Ok(PhaseMode::Continuous),
        _ => Err(Error::InvalidDocument(format!(
            "invalid phase mode code {value}"
        ))),
    }
}

fn encode_branch_type(value: BranchType) -> u8 {
    match value {
        BranchType::Line => 0,
        BranchType::Transformer => 1,
        BranchType::Transformer3W => 2,
        BranchType::SeriesCapacitor => 3,
        BranchType::ZeroImpedanceTie => 4,
    }
}

fn decode_branch_type(value: u8) -> Result<BranchType, Error> {
    match value {
        0 => Ok(BranchType::Line),
        1 => Ok(BranchType::Transformer),
        2 => Ok(BranchType::Transformer3W),
        3 => Ok(BranchType::SeriesCapacitor),
        4 => Ok(BranchType::ZeroImpedanceTie),
        _ => Err(Error::InvalidDocument(format!(
            "invalid branch type code {value}"
        ))),
    }
}

fn encode_gen_type(value: GenType) -> u8 {
    match value {
        GenType::Synchronous => 0,
        GenType::Asynchronous => 1,
        GenType::InverterBased => 2,
        GenType::Hybrid => 3,
        GenType::Unknown => 4,
    }
}

fn decode_gen_type(value: u8) -> Result<GenType, Error> {
    match value {
        0 => Ok(GenType::Synchronous),
        1 => Ok(GenType::Asynchronous),
        2 => Ok(GenType::InverterBased),
        3 => Ok(GenType::Hybrid),
        4 => Ok(GenType::Unknown),
        _ => Err(Error::InvalidDocument(format!(
            "invalid generator type code {value}"
        ))),
    }
}

fn encode_commitment_status(value: surge_network::network::CommitmentStatus) -> u8 {
    match value {
        surge_network::network::CommitmentStatus::Market => 0,
        surge_network::network::CommitmentStatus::SelfCommitted => 1,
        surge_network::network::CommitmentStatus::MustRun => 2,
        surge_network::network::CommitmentStatus::Unavailable => 3,
        surge_network::network::CommitmentStatus::EmergencyOnly => 4,
    }
}

fn decode_commitment_status(value: u8) -> Result<surge_network::network::CommitmentStatus, Error> {
    match value {
        0 => Ok(surge_network::network::CommitmentStatus::Market),
        1 => Ok(surge_network::network::CommitmentStatus::SelfCommitted),
        2 => Ok(surge_network::network::CommitmentStatus::MustRun),
        3 => Ok(surge_network::network::CommitmentStatus::Unavailable),
        4 => Ok(surge_network::network::CommitmentStatus::EmergencyOnly),
        _ => Err(Error::InvalidDocument(format!(
            "invalid commitment status code {value}"
        ))),
    }
}

fn encode_load_connection(value: LoadConnection) -> u8 {
    match value {
        LoadConnection::WyeGrounded => 0,
        LoadConnection::WyeUngrounded => 1,
        LoadConnection::Delta => 2,
    }
}

fn decode_load_connection(value: u8) -> Result<LoadConnection, Error> {
    match value {
        0 => Ok(LoadConnection::WyeGrounded),
        1 => Ok(LoadConnection::WyeUngrounded),
        2 => Ok(LoadConnection::Delta),
        _ => Err(Error::InvalidDocument(format!(
            "invalid load connection code {value}"
        ))),
    }
}

fn encode_load_class(value: LoadClass) -> u8 {
    match value {
        LoadClass::Residential => 0,
        LoadClass::Commercial => 1,
        LoadClass::Industrial => 2,
        LoadClass::Agricultural => 3,
        LoadClass::DataCenter => 4,
        LoadClass::EvCharging => 5,
        LoadClass::Other => 6,
    }
}

fn decode_load_class(value: u8) -> Result<LoadClass, Error> {
    match value {
        0 => Ok(LoadClass::Residential),
        1 => Ok(LoadClass::Commercial),
        2 => Ok(LoadClass::Industrial),
        3 => Ok(LoadClass::Agricultural),
        4 => Ok(LoadClass::DataCenter),
        5 => Ok(LoadClass::EvCharging),
        6 => Ok(LoadClass::Other),
        _ => Err(Error::InvalidDocument(format!(
            "invalid load class code {value}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::{
        Branch, Bus, GenType, Generator, GeneratorTechnology, OwnershipEntry,
    };

    fn mini_network() -> Network {
        let mut network = Network::new("test_bin");
        network.base_mva = 100.0;
        network.buses.push(Bus::new(1, BusType::Slack, 138.0));
        network.buses.push(Bus::new(2, BusType::PQ, 138.0));
        network.buses[0].latitude = Some(30.0);
        network.buses[0].reserve_zone = Some("Reserve Zone 1".to_string());
        network.buses[0].owners = vec![OwnershipEntry {
            owner: 7,
            fraction: 1.0,
        }];
        let mut generator = Generator::new(1, 100.0, 1.06);
        generator.cost = Some(CostCurve::Polynomial {
            startup: 10.0,
            shutdown: 5.0,
            coeffs: vec![0.01, 1.5, 3.0],
        });
        generator.gen_type = GenType::InverterBased;
        generator.technology = Some(GeneratorTechnology::SolarPv);
        generator.source_technology_code = Some("PV".to_string());
        generator
            .fuel
            .get_or_insert_with(Default::default)
            .fuel_type = Some("solar".to_string());
        generator.storage = Some(StorageParams::with_energy_capacity_mwh(50.0));
        generator.owners = vec![OwnershipEntry {
            owner: 11,
            fraction: 0.75,
        }];
        network.generators.push(generator);
        let mut branch = Branch::new_line(1, 2, 0.01, 0.1, 0.02);
        {
            let zs = branch.zero_seq.get_or_insert_with(ZeroSeqData::default);
            zs.r0 = 0.03;
        }
        {
            let td = branch
                .transformer_data
                .get_or_insert_with(TransformerData::default);
            td.parent_transformer_id = Some("tx1".to_string());
        }
        branch.owners = vec![OwnershipEntry {
            owner: 13,
            fraction: 0.5,
        }];
        network.branches.push(branch);
        let mut load = Load::new(2, 50.0, 20.0);
        load.owners = vec![OwnershipEntry {
            owner: 17,
            fraction: 1.0,
        }];
        network.loads.push(load);
        network.market_data.reserve_zones.push(ReserveZone {
            name: "Reserve Zone 1".to_string(),
            zonal_requirements: Vec::new(),
        });
        network
    }

    #[test]
    fn test_roundtrip_preserves_network_json_shape() {
        let network = mini_network();
        let before = crate::json::encode_network(&network).expect("encode original");
        let bytes = dumps(&network).expect("failed to dump binary");
        let parsed = loads(&bytes).expect("failed to parse binary");
        let after = crate::json::encode_network(&parsed).expect("encode parsed");
        assert_eq!(before, after);
    }

    #[test]
    fn test_file_roundtrip() {
        let network = mini_network();
        let tmp = std::env::temp_dir().join("surge_test_roundtrip.surge.bin");
        save(&network, &tmp).expect("failed to save binary");
        let parsed = load(&tmp).expect("failed to load binary");
        assert_eq!(parsed.name, network.name);
        assert_eq!(parsed.n_buses(), network.n_buses());
        assert_eq!(parsed.buses[0].owners, network.buses[0].owners);
        assert_eq!(parsed.branches[0].owners, network.branches[0].owners);
        assert_eq!(parsed.generators[0].owners, network.generators[0].owners);
        assert_eq!(parsed.loads[0].owners, network.loads[0].owners);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_corrupt_checksum_is_rejected() {
        let network = mini_network();
        let mut bytes = dumps(&network).expect("failed to dump binary");
        *bytes.last_mut().expect("binary output is non-empty") ^= 0x01;
        let result = loads(&bytes);
        assert!(result.is_err(), "corrupt payload should be rejected");
    }
}
