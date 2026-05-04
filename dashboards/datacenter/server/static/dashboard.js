/* Datacenter dashboard frontend.
 *
 * Loads default scenario, lets user edit forecasts + asset specs +
 * 4-CP flags, POSTs to /api/solve, renders dispatch / P&L / SOC / AS
 * / 4-CP charts.
 */

(() => {
  "use strict";

  const STATE = {
    scenario: null,
    lastResult: null,
    activePriceTab: "lmp",
    activeLoadTab: "must_serve",
    activeRenewableTab: "solar",
  };
  // Expose STATE so the standalone editable-chart.js (loaded above
  // this IIFE) can read the scenario time axis for period-time
  // labels in tooltips + x-axis ticks.
  window.STATE = STATE;

  // ------------------------------------------------------------ DOM
  const $ = (id) => document.getElementById(id);

  // Each x_t is the start time of period t and the value y_t holds for
  // the entire [x_t, x_{t+1}) interval — interpolated lines slope
  // between period centers and obscure that semantics. We render every
  // time-series with `line.shape = 'hv'` (right-step) and append a
  // trailing boundary point at x_{N-1} + dt so the final period's flat
  // top is visible (without it, hv-step truncates the last interval).
  function extendX(x) {
    if (!x || x.length < 2) return x ? x.slice() : [];
    const last = new Date(x[x.length - 1]).getTime();
    const prev = new Date(x[x.length - 2]).getTime();
    const next = new Date(last + (last - prev)).toISOString();
    return [...x, next];
  }
  const extendY = (y) => (y && y.length ? [...y, y[y.length - 1]] : []);

  // Resample an array from (oldRes × oldLen) minutes to
  // (newRes × newLen) minutes by treating the source as a periodic
  // cycle. For each new period at minute t = i × newRes, the value
  // comes from the old period at minute (t mod oldCycleMin). This
  // gives:
  //   * Same resolution + longer horizon (24h hourly → 72h hourly):
  //     tiled — three daily copies of the profile.
  //   * Finer resolution + same horizon (24h hourly → 24h 15min):
  //     step-sampled — one old hour fills four new sub-periods.
  //   * Coarser resolution: take nearest old sample at each new
  //     period's start minute.
  // Booleans (4-CP flags) extend with `false` rather than tiling so a
  // user-flagged day doesn't auto-replicate across additional days.
  function resampleArrayPeriodic(arr, oldResMin, newResMin, newLen) {
    if (!Array.isArray(arr) || arr.length === 0) return new Array(newLen).fill(0);
    if (arr.length === newLen && oldResMin === newResMin) return arr;
    const oldLen = arr.length;
    const cycleMin = Math.max(1, oldLen * oldResMin);
    return Array.from({ length: newLen }, (_, i) => {
      const tMin = i * newResMin;
      const cyclePos = ((tMin % cycleMin) + cycleMin) % cycleMin;
      const oldIdx = Math.min(oldLen - 1, Math.floor(cyclePos / Math.max(1, oldResMin)));
      return arr[oldIdx];
    });
  }

  function resampleFlags(arr, newLen) {
    if (!Array.isArray(arr)) return arr;
    if (arr.length === newLen) return arr;
    if (newLen < arr.length) return arr.slice(0, newLen);
    return arr.concat(new Array(newLen - arr.length).fill(false));
  }

  function resizeScenarioToPeriods(newPeriods, oldResMin, newResMin) {
    const s = STATE.scenario;
    if (!s) return;
    const tile = (arr) =>
      resampleArrayPeriodic(arr, oldResMin, newResMin, newPeriods);
    s.lmp_forecast_per_mwh = tile(s.lmp_forecast_per_mwh);
    if (Array.isArray(s.natural_gas_price_per_mmbtu)) {
      s.natural_gas_price_per_mmbtu = tile(s.natural_gas_price_per_mmbtu);
    }
    if (s.site?.it_load) {
      s.site.it_load.must_serve_mw = tile(s.site.it_load.must_serve_mw);
      (s.site.it_load.tiers || []).forEach((t) => {
        if (Array.isArray(t.capacity_per_period_mw)) {
          t.capacity_per_period_mw = tile(t.capacity_per_period_mw);
        }
      });
    }
    if (Array.isArray(s.site?.solar?.capacity_factors)) {
      s.site.solar.capacity_factors = tile(s.site.solar.capacity_factors);
    }
    if (Array.isArray(s.site?.wind?.capacity_factors)) {
      s.site.wind.capacity_factors = tile(s.site.wind.capacity_factors);
    }
    if (Array.isArray(s.site?.nuclear?.availability_per_period)) {
      s.site.nuclear.availability_per_period = tile(s.site.nuclear.availability_per_period);
    }
    (s.as_products || []).forEach((ap) => {
      ap.price_forecast_per_mwh = tile(ap.price_forecast_per_mwh);
    });
    if (Array.isArray(s.four_cp?.period_flags)) {
      s.four_cp.period_flags = resampleFlags(s.four_cp.period_flags, newPeriods);
    }
  }

  // ------------------------------------------------------------ Init
  async function init() {
    bindStaticControls();
    try {
      const res = await fetch("api/default-scenario");
      STATE.scenario = await res.json();
    } catch (err) {
      showError("Failed to load default scenario: " + err);
      return;
    }
    renderControls(STATE.scenario);
    setStatus("ready");
    await runSolve();
  }

  function bindTimeAxisControls() {
    const horizonInp = $("inp-horizon-h");
    const resSel = $("sel-resolution-min");
    const periodsRead = $("readout-periods");
    if (!horizonInp || !resSel || !periodsRead) return;
    const sync = () => {
      const ta = STATE.scenario?.time_axis;
      if (!ta) return;
      horizonInp.value = String(Math.round(ta.horizon_minutes / 60));
      resSel.value = String(ta.resolution_minutes);
      periodsRead.textContent = `${ta.periods}`;
      $("periods-display").textContent = `${ta.periods} periods · ${ta.resolution_minutes} min`;
    };
    const apply = () => {
      const ta = STATE.scenario?.time_axis;
      if (!ta) return;
      const oldResMin = ta.resolution_minutes;
      const horizonH = Math.max(1, Math.min(744, Math.round(Number(horizonInp.value) || 1)));
      const resMin = Math.max(5, Math.min(60, Number(resSel.value) || 60));
      let periods = Math.floor((horizonH * 60) / resMin);
      periods = Math.max(1, Math.min(8760, periods));
      if (periods === ta.periods && resMin === oldResMin) {
        sync();
        return;
      }
      // Pass old + new resolution so resizeScenarioToPeriods can
      // treat each profile as a periodic cycle: 24h hourly → 72h
      // hourly tiles (3 daily copies) instead of stretching a single
      // cycle across the new horizon.
      resizeScenarioToPeriods(periods, oldResMin, resMin);
      ta.periods = periods;
      ta.resolution_minutes = resMin;
      ta.horizon_minutes = periods * resMin;
      sync();
      // Auto re-solve so every chart reflects the new horizon — the
      // forecasts tab re-renders its editors with the new period
      // count once the result lands.
      runSolve();
    };
    // Re-binding is idempotent: replace any prior handler so renderControls
    // can be called more than once without piling up listeners.
    horizonInp.oninput = null; horizonInp.onchange = apply;
    resSel.onchange = apply;
    sync();
  }

  function bindStaticControls() {
    $("btn-solve").addEventListener("click", runSolve);
    $("error-banner-close").addEventListener("click", () => {
      $("error-banner").classList.remove("is-open");
    });
    document.querySelectorAll(".tab-btn").forEach((btn) => {
      btn.addEventListener("click", () => activateTab(btn.dataset.tab));
    });
    const sidebarToggle = $("sidebar-toggle");
    if (sidebarToggle) {
      sidebarToggle.addEventListener("click", () => {
        $("sidebar").classList.toggle("is-collapsed");
      });
    }
    $("btn-add-tier").addEventListener("click", addTier);
  }

  function activateTab(tabName) {
    document.querySelectorAll(".tab-btn").forEach((btn) => {
      btn.classList.toggle("active", btn.dataset.tab === tabName);
    });
    document.querySelectorAll(".tab-pane").forEach((pane) => {
      pane.classList.toggle("active", pane.dataset.tab === tabName);
    });
    if (STATE.lastResult) renderResult(STATE.lastResult);
  }

  // ------------------------------------------------------------ Controls
  function renderControls(s) {
    $("periods-display").textContent = `${s.time_axis.periods} periods · ${s.time_axis.resolution_minutes} min`;
    bindTimeAxisControls();
    $("inp-poi-limit").value = s.site.poi_limit_mw;
    const must = s.site.it_load.must_serve_mw;
    $("inp-must-serve-base").value = must.length ? must[0].toFixed(1) : 0;
    $("inp-must-serve-base").addEventListener("change", (e) => {
      const v = Number(e.target.value);
      STATE.scenario.site.it_load.must_serve_mw = STATE.scenario.site.it_load.must_serve_mw.map(() => v);
    });
    $("inp-poi-limit").addEventListener("change", (e) => {
      STATE.scenario.site.poi_limit_mw = Number(e.target.value);
    });

    // Natural-gas flat price — broadcasts to every period on change.
    // The per-period chart on the forecasts tab is the source of
    // truth otherwise; this is a one-shot "set them all to X" knob
    // matching the AS-price scalar pattern.
    const gasFlat = $("inp-gas-flat");
    if (gasFlat) {
      const gasArr = STATE.scenario.natural_gas_price_per_mmbtu || [];
      gasFlat.value = gasArr.length ? Number(gasArr[0]).toFixed(2) : "";
      gasFlat.addEventListener("change", (e) => {
        const v = Math.max(0, Number(e.target.value) || 0);
        const periods = STATE.scenario?.time_axis?.periods || 0;
        STATE.scenario.natural_gas_price_per_mmbtu = new Array(periods).fill(v);
        // If the forecasts tab is currently showing this chart,
        // re-render so the user sees the new flat shape immediately.
        if (typeof window.EditableLineChart === "function") {
          renderForecastEditor({
            chartId: "chart-gas-price",
            label: "Natural gas price",
            color: "#fb923c",
            enabled: true,
            values: STATE.scenario.natural_gas_price_per_mmbtu,
            inputMin: 0, decimals: 2,
          });
        }
      });
    }

    renderTiers(s.site.it_load.tiers);

    bindBessFields(s.site.bess);
    bindRenewable("solar", s.site.solar);
    bindRenewable("wind", s.site.wind);
    renderThermal("fuel_cell", s.site.fuel_cell);
    renderThermal("gas_ct", s.site.gas_ct);
    renderThermal("diesel", s.site.diesel);
    renderThermal("nuclear", s.site.nuclear);
    renderAsList(s.as_products);
    bindFourCp(s.four_cp);
    bindPolicy(s.policy);

    // Mode selectors
    $("sel-commitment-mode").value = s.policy.commitment_mode;
    $("sel-period-coupling").value = s.policy.period_coupling;
    $("sel-lp-solver").value = s.policy.lp_solver;
    [["sel-commitment-mode", "commitment_mode"], ["sel-period-coupling", "period_coupling"], ["sel-lp-solver", "lp_solver"]].forEach(
      ([id, key]) => $(id).addEventListener("change", (e) => { STATE.scenario.policy[key] = e.target.value; }),
    );
  }

  function renderTiers(tiers) {
    const list = $("tier-list");
    list.innerHTML = "";
    tiers.forEach((tier, idx) => list.appendChild(makeTierRow(tier, idx)));
  }

  function makeTierRow(tier, idx) {
    const row = document.createElement("div");
    row.className = "tier-row";
    row.innerHTML = `
      <div class="tier-head">
        <input type="text" class="tier-id" value="${tier.tier_id}" placeholder="tier-id">
        <button class="tier-remove" type="button" aria-label="remove">×</button>
      </div>
      <label class="field-row"><span class="field-label">Capacity <em>MW</em></span><input type="number" class="tier-mw" min="0" step="10" value="${tier.capacity_mw}"></label>
      <label class="field-row"><span class="field-label">VOLL <em>$/MWh</em></span><input type="number" class="tier-voll" min="0" step="1" value="${tier.voll_per_mwh}"></label>
    `;
    row.querySelector(".tier-id").addEventListener("change", (e) => {
      STATE.scenario.site.it_load.tiers[idx].tier_id = e.target.value || `tier_${idx}`;
    });
    row.querySelector(".tier-mw").addEventListener("change", (e) => {
      const v = Number(e.target.value);
      const tier = STATE.scenario.site.it_load.tiers[idx];
      tier.capacity_mw = v;
      // Broadcast scalar to per-period array so the forecasts-tab
      // editor stays consistent with the sidebar setting until the
      // user shapes individual periods explicitly.
      const periods = STATE.scenario.time_axis.periods;
      tier.capacity_per_period_mw = new Array(periods).fill(v);
    });
    row.querySelector(".tier-voll").addEventListener("change", (e) => {
      STATE.scenario.site.it_load.tiers[idx].voll_per_mwh = Number(e.target.value);
    });
    row.querySelector(".tier-remove").addEventListener("click", () => {
      STATE.scenario.site.it_load.tiers.splice(idx, 1);
      renderTiers(STATE.scenario.site.it_load.tiers);
    });
    return row;
  }

  function addTier() {
    const periods = STATE.scenario.time_axis.periods;
    STATE.scenario.site.it_load.tiers.push({
      tier_id: `tier_${STATE.scenario.site.it_load.tiers.length}`,
      capacity_mw: 50.0,
      capacity_per_period_mw: new Array(periods).fill(50.0),
      voll_per_mwh: 25.0,
    });
    renderTiers(STATE.scenario.site.it_load.tiers);
  }

  function bindBessFields(b) {
    $("inp-bess-charge").value = b.power_charge_mw;
    $("inp-bess-discharge").value = b.power_discharge_mw;
    $("inp-bess-energy").value = b.energy_mwh;
    $("inp-bess-eff-charge").value = (b.charge_efficiency * 100).toFixed(1);
    $("inp-bess-eff-discharge").value = (b.discharge_efficiency * 100).toFixed(1);
    $("inp-bess-soc-min").value = (b.soc_min_fraction * 100).toFixed(1);
    $("inp-bess-soc-max").value = (b.soc_max_fraction * 100).toFixed(1);
    $("inp-bess-soc-init").value = b.initial_soc_mwh ?? "";
    $("inp-bess-deg").value = b.degradation_cost_per_mwh;
    $("inp-bess-cycle").value = b.daily_cycle_limit ?? "";

    const bind = (id, set) => $(id).addEventListener("change", (e) => set(e.target.value));
    bind("inp-bess-charge", (v) => { b.power_charge_mw = Number(v); });
    bind("inp-bess-discharge", (v) => { b.power_discharge_mw = Number(v); });
    bind("inp-bess-energy", (v) => { b.energy_mwh = Number(v); });
    bind("inp-bess-eff-charge", (v) => { b.charge_efficiency = Number(v) / 100; });
    bind("inp-bess-eff-discharge", (v) => { b.discharge_efficiency = Number(v) / 100; });
    bind("inp-bess-soc-min", (v) => { b.soc_min_fraction = Number(v) / 100; });
    bind("inp-bess-soc-max", (v) => { b.soc_max_fraction = Number(v) / 100; });
    bind("inp-bess-soc-init", (v) => { b.initial_soc_mwh = v === "" ? null : Number(v); });
    bind("inp-bess-deg", (v) => { b.degradation_cost_per_mwh = Number(v); });
    bind("inp-bess-cycle", (v) => { b.daily_cycle_limit = v === "" ? null : Number(v); });
  }

  function bindRenewable(kind, spec) {
    if (!spec) return;
    const idMw = `inp-${kind}-mw`;
    const idRec = `inp-${kind}-rec`;
    $(idMw).value = spec.nameplate_mw;
    $(idMw).addEventListener("change", (e) => {
      spec.nameplate_mw = Number(e.target.value);
    });
    const recEl = $(idRec);
    if (recEl) {
      recEl.value = spec.rec_value_per_mwh ?? 0;
      recEl.addEventListener("change", (e) => {
        spec.rec_value_per_mwh = Math.max(0, Number(e.target.value) || 0);
      });
    }
  }

  const THERMAL_FIELDS = [
    ["nameplate_mw", "Nameplate MW", 0, 10],
    ["pmin_mw", "Pmin MW", 0, 5],
    ["fuel_price_per_mmbtu", "Fuel $/MMBtu", 0, 0.5],
    ["heat_rate_btu_per_kwh", "Heat rate Btu/kWh", 0, 100],
    ["vom_per_mwh", "VOM $/MWh", 0, 0.5],
    ["no_load_cost_per_hr", "No-load $/hr", 0, 50],
    ["min_up_h", "Min up h", 0, 0.5],
    ["min_down_h", "Min down h", 0, 0.5],
    ["ramp_up_mw_per_min", "Ramp up MW/min", 0, 1],
    ["ramp_down_mw_per_min", "Ramp down MW/min", 0, 1],
    ["co2_tonnes_per_mwh", "CO₂ t/MWh", 0, 0.05],
  ];
  const NUCLEAR_FIELDS = [
    ["nameplate_mw", "Nameplate MW", 0, 10],
    ["marginal_cost_per_mwh", "Marginal $/MWh", 0, 0.5],
  ];

  function renderThermal(slot, spec) {
    const section = document.querySelector(`.thermal-section[data-thermal="${slot}"]`);
    if (!section) return;
    const enableEl = section.querySelector(".thermal-enable");
    enableEl.checked = !!spec?.enabled;
    enableEl.addEventListener("change", (e) => {
      if (!STATE.scenario.site[slot]) return;
      STATE.scenario.site[slot].enabled = e.target.checked;
      if (e.target.checked) section.setAttribute("open", "");
    });
    const wrap = section.querySelector(".thermal-fields");
    wrap.innerHTML = "";
    // Gas-fed thermals (fuel_cell + gas_ct) share the per-period
    // natural-gas price configured on the forecasts tab, so the
    // per-thermal scalar is hidden from the sidebar to avoid two
    // conflicting price knobs. Diesel keeps its own scalar.
    const baseList = slot === "nuclear" ? NUCLEAR_FIELDS : THERMAL_FIELDS;
    const fieldList = (slot === "fuel_cell" || slot === "gas_ct")
      ? baseList.filter(([key]) => key !== "fuel_price_per_mmbtu")
      : baseList;
    const stack = document.createElement("div");
    stack.className = "field-stack";
    fieldList.forEach(([key, label, min, step]) => {
      const value = spec?.[key];
      const row = document.createElement("label");
      row.className = "field-row";
      row.innerHTML = `<span class="field-label">${label}</span><input type="number" min="${min}" step="${step}" value="${value ?? ""}">`;
      const input = row.querySelector("input");
      input.addEventListener("change", (e) => {
        if (!STATE.scenario.site[slot]) return;
        STATE.scenario.site[slot][key] = e.target.value === "" ? null : Number(e.target.value);
      });
      stack.appendChild(row);
    });

    // Startup cost row — stored as a tier list on the back-end
    // (\\\`startup_cost_tiers\\\` = [{max_offline_hours, cost, ...}]).
    // Surface a single scalar "Startup cost $" input that maps to the
    // largest existing tier on read and replaces the whole tier list
    // with one cold-start tier on write. Fine-grained warm/cold tier
    // editing isn't exposed in the sidebar — power users can edit the
    // scenario JSON if they need it. Skip for nuclear (always-on,
    // no startup) and any thermal that explicitly opted out.
    if (slot !== "nuclear") {
      const tiers = spec?.startup_cost_tiers || [];
      const cold = tiers.length
        ? Math.max(...tiers.map((t) => Number(t.cost) || 0))
        : 0;
      const row = document.createElement("label");
      row.className = "field-row";
      row.innerHTML = `<span class="field-label">Startup cost <em>$</em></span><input type="number" min="0" step="100" value="${cold || ""}">`;
      const input = row.querySelector("input");
      input.addEventListener("change", (e) => {
        if (!STATE.scenario.site[slot]) return;
        const v = e.target.value === "" ? 0 : Math.max(0, Number(e.target.value));
        STATE.scenario.site[slot].startup_cost_tiers = v > 0
          ? [{ max_offline_hours: 24.0, cost: v }]
          : [];
      });
      stack.appendChild(row);
    }
    wrap.appendChild(stack);
  }

  function renderAsList(asProducts) {
    const list = $("as-list");
    list.innerHTML = "";
    asProducts.forEach((ap, idx) => {
      const row = document.createElement("div");
      row.className = "as-row";
      row.innerHTML = `
        <input type="checkbox" checked>
        <div class="as-name">${ap.title}</div>
        <input type="number" min="0" step="0.5" value="${ap.price_forecast_per_mwh[0] ?? 0}" title="flat price ($/MWh)">
      `;
      const [enable, , price] = row.querySelectorAll("input,div");
      const enableEl = row.querySelector('input[type="checkbox"]');
      const priceEl = row.querySelector('input[type="number"]');
      enableEl.addEventListener("change", () => {
        if (!enableEl.checked) ap.price_forecast_per_mwh = ap.price_forecast_per_mwh.map(() => 0);
      });
      priceEl.addEventListener("change", () => {
        const v = Number(priceEl.value);
        ap.price_forecast_per_mwh = ap.price_forecast_per_mwh.map(() => v);
      });
      list.appendChild(row);
    });
  }

  function bindFourCp(fc) {
    if (!fc) return;
    $("inp-4cp-enabled").checked = !!fc.enabled;
    $("inp-4cp-rate").value = fc.annual_charge_per_mw_year;
    $("inp-4cp-expected").value = fc.expected_intervals_per_year;
    $("inp-4cp-enabled").addEventListener("change", (e) => { fc.enabled = e.target.checked; });
    $("inp-4cp-rate").addEventListener("change", (e) => { fc.annual_charge_per_mw_year = Number(e.target.value); });
    $("inp-4cp-expected").addEventListener("change", (e) => { fc.expected_intervals_per_year = Number(e.target.value); });
  }

  function bindPolicy(p) {
    $("inp-mip-gap").value = p.mip_rel_gap;
    $("inp-mip-time").value = p.mip_time_limit_secs;
    $("inp-enforce-reserve").checked = !!p.enforce_reserve_capacity;
    $("inp-mip-gap").addEventListener("change", (e) => { p.mip_rel_gap = Number(e.target.value); });
    $("inp-mip-time").addEventListener("change", (e) => { p.mip_time_limit_secs = Number(e.target.value); });
    $("inp-enforce-reserve").addEventListener("change", (e) => { p.enforce_reserve_capacity = e.target.checked; });
  }

  // ------------------------------------------------------------ Solve
  async function runSolve() {
    if (!STATE.scenario) return;
    setStatus("solving…");
    $("btn-solve").disabled = true;
    try {
      const res = await fetch("api/solve", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(STATE.scenario),
      });
      if (!res.ok) {
        const txt = await res.text();
        throw new Error(`solve failed: ${res.status} ${txt}`);
      }
      const result = await res.json();
      if (result.status !== "ok") throw new Error(result.error || "solver returned error");
      STATE.lastResult = result;
      setStatus(`solved · ${(result.elapsed_secs ?? 0).toFixed(2)}s`);
      renderResult(result);
    } catch (err) {
      console.error(err);
      showError(String(err.message || err));
      setStatus("error");
    } finally {
      $("btn-solve").disabled = false;
    }
  }

  function setStatus(msg) {
    $("solve-status").textContent = msg;
  }

  function showError(msg) {
    $("error-banner-text").textContent = msg;
    $("error-banner").classList.add("is-open");
  }

  // ------------------------------------------------------------ Render results
  function renderResult(result) {
    updateNetPnl(result.pl_summary);
    renderDispatchChart(result);
    renderPlChart(result);
    renderSocChart(result);
    renderAsChart(result);
    renderForecastsChart(result);
    renderFourCpChart(result);
    renderCommitmentChart(result);
  }

  function updateNetPnl(s) {
    if (!s) return;
    $("net-pnl").textContent = formatDollars(s.net_pnl_dollars);
    $("net-pnl").classList.toggle("pos", s.net_pnl_dollars >= 0);
    $("net-pnl").classList.toggle("neg", s.net_pnl_dollars < 0);
    $("net-pnl-sub").textContent = `comp ${formatDollarsShort(s.compute_revenue_dollars)} · grid ${formatDollarsShort(s.energy_export_revenue_dollars - s.energy_import_cost_dollars)} · AS ${formatDollarsShort(s.as_revenue_dollars)}`;
  }

  function formatDollars(v) {
    if (v == null) return "—";
    const sign = v < 0 ? "−" : "";
    const abs = Math.abs(v);
    return `${sign}$${abs.toLocaleString(undefined, { maximumFractionDigits: 0 })}`;
  }
  function formatDollarsShort(v) {
    if (v == null) return "—";
    const sign = v < 0 ? "−" : "";
    const abs = Math.abs(v);
    if (abs >= 1e6) return `${sign}$${(abs / 1e6).toFixed(2)}M`;
    if (abs >= 1e3) return `${sign}$${(abs / 1e3).toFixed(1)}k`;
    return `${sign}$${abs.toFixed(0)}`;
  }

  // Asset palette — explicit per-resource colors. Solar is solar-yellow,
  // wind is sky-cyan, the rest follow the Tailwind 400-tier ramp so the
  // stack reads as a coherent gradient from renewable→clean→thermal→grid.
  const COLOR = {
    solar:    "#facc15",
    wind:     "#22d3ee",
    nuclear:  "#60a5fa",
    bess_dis: "#34d399",
    bess_ch:  "#34d399",
    // Fuchsia for fuel cell — purple is reserved for the Amptimal-
    // branded Net POI line below.
    fuel_cell:"#e879f9",
    gas_ct:   "#fb923c",
    diesel:   "#f87171",
    grid_imp: "#cbd5e1",
    grid_exp: "#e2e8f0",
    // Amptimal brand purple — used for the Net POI flow line so the
    // primary grid-utilisation read-out carries the brand colour.
    net_poi:  "#a78bfa",
  };

  function renderDispatchChart(result) {
    const xRaw = result.period_times_iso;
    const x = extendX(xRaw);
    const sched = result.schedule;

    // Per-asset MW per period — extended with a trailing duplicate so
    // the hv-step shape draws the final period's flat top.
    const solar    = extendY(sched.map((p) => p.renewables?.solar_mw || 0));
    const wind     = extendY(sched.map((p) => p.renewables?.wind_mw || 0));
    const nuclear  = extendY(sched.map((p) => p.nuclear_mw || 0));
    const fc       = extendY(sched.map((p) => p.thermals?.fuel_cell?.mw ?? 0));
    const ct       = extendY(sched.map((p) => p.thermals?.gas_ct?.mw ?? 0));
    const diesel   = extendY(sched.map((p) => p.thermals?.diesel?.mw ?? 0));
    const bessDis  = extendY(sched.map((p) => p.bess?.discharge_mw || 0));
    const bessCh   = extendY(sched.map((p) => -(p.bess?.charge_mw || 0))); // negative — below zero
    const gridImp  = extendY(sched.map((p) => p.grid_import_mw || 0));
    const gridExp  = extendY(sched.map((p) => -(p.grid_export_mw || 0))); // negative — below zero

    // Load envelope. `served` = must-serve + tier_served (the bus's
    // primary care, drawn as the bright top line). `potential` adds back
    // the curtailed MW from each tier — anything between potential and
    // served is curtailed compute the LP chose to drop.
    const itServed = extendY(sched.map((p) =>
      (p.must_serve_mw || 0)
      + (p.tiers || []).reduce((acc, t) => acc + (t.served_mw || 0), 0)));
    const itPotential = extendY(sched.map((p) =>
      (p.must_serve_mw || 0)
      + (p.tiers || []).reduce((acc, t) => acc + (t.served_mw || 0) + (t.curtailed_mw || 0), 0)));

    // Only stack assets the operator has enabled; otherwise their
    // (all-zero) traces still land in the legend.
    const site = STATE.scenario?.site || {};
    const traces = [];
    if (site.solar)             traces.push(mkSupplyArea(x, solar,   "Solar",          COLOR.solar));
    if (site.wind)              traces.push(mkSupplyArea(x, wind,    "Wind",           COLOR.wind));
    if (site.nuclear?.enabled)  traces.push(mkSupplyArea(x, nuclear, "Nuclear",        COLOR.nuclear));
    traces.push(                              mkSupplyArea(x, bessDis,"BESS discharge",COLOR.bess_dis));
    if (site.fuel_cell?.enabled) traces.push(mkSupplyArea(x, fc,     "Fuel cell",      COLOR.fuel_cell));
    if (site.gas_ct?.enabled)    traces.push(mkSupplyArea(x, ct,     "Gas CT",         COLOR.gas_ct));
    if (site.diesel?.enabled)    traces.push(mkSupplyArea(x, diesel, "Diesel",         COLOR.diesel));
    traces.push(                              mkSupplyArea(x, gridImp,"Grid import",   COLOR.grid_imp));
    // Consumption (negative y) — BESS charging + grid export.
    traces.push(mkConsumeArea(x, bessCh,  "BESS charge", COLOR.bess_ch));
    traces.push(mkConsumeArea(x, gridExp, "Grid export", COLOR.grid_exp));
    // Reference lines.
    traces.push(
      {
        x, y: itPotential, name: "Max IT load potential",
        type: "scatter", mode: "lines",
        // Distinct teal so the line never reads as a second POI
        // limit when it happens to coincide with +POI (e.g.
        // poi_limit_mw = must_serve + Σ tier capacity).
        line: { color: "#5eead4", width: 1.6, dash: "dashdot", shape: "hv" },
        hovertemplate: "%{x}<br>Max IT load: %{y:.0f} MW<extra></extra>",
      },
      {
        x, y: itServed, name: "IT load served",
        type: "scatter", mode: "lines",
        line: { color: "#ffffff", width: 2.6, shape: "hv" },
        hovertemplate: "%{x}<br>Served: %{y:.0f} MW<extra></extra>",
      },
      // Net flow across the POI per period — grid_import minus
      // grid_export. The single most informative line for
      // "how close are we to the POI cap right now". Always lives
      // between the ±POI dotted limit lines (the LP enforces this);
      // a positive value means net importing, negative means net
      // exporting. Rendered as two stacked traces — a soft halo
      // underneath + a sharp line on top — to lift it visually
      // above the supply stack without thickening the line itself.
      {
        x, y: extendY(sched.map((p) =>
          (p.grid_import_mw || 0) - (p.grid_export_mw || 0))),
        name: "Net POI flow",
        legendgroup: "net_poi", showlegend: false,
        hoverinfo: "skip",
        type: "scatter", mode: "lines",
        line: { color: COLOR.net_poi, width: 8, shape: "hv" },
        opacity: 0.22,
      },
      {
        x, y: extendY(sched.map((p) =>
          (p.grid_import_mw || 0) - (p.grid_export_mw || 0))),
        name: "Net POI flow",
        legendgroup: "net_poi",
        type: "scatter", mode: "lines",
        line: { color: COLOR.net_poi, width: 2.6, shape: "hv" },
        hovertemplate: "%{x}<br>Net POI: %{y:.0f} MW<extra></extra>",
      },
    );
    const poi = Number(STATE.scenario?.site?.poi_limit_mw) || 0;
    if (poi > 0) {
      // POI ±limit lines as real traces (not layout shapes) so they
      // appear in the legend and auto-track the y-axis range. Sharing
      // a legendgroup folds them into a single legend entry that
      // toggles both lines together.
      const xEnds = [x[0], x[x.length - 1]];
      traces.push(
        {
          x: xEnds, y: [poi, poi],
          name: `POI ±${poi.toFixed(0)} MW`,
          legendgroup: "poi",
          type: "scatter", mode: "lines",
          line: { color: "#ef4444", width: 2, dash: "dot" },
          hovertemplate: "POI import limit: %{y:.0f} MW<extra></extra>",
        },
        {
          x: xEnds, y: [-poi, -poi],
          legendgroup: "poi", showlegend: false,
          name: "POI export limit",
          type: "scatter", mode: "lines",
          line: { color: "#ef4444", width: 2, dash: "dot" },
          hovertemplate: "POI export limit: %{y:.0f} MW<extra></extra>",
        },
      );
    }
    // Compute an explicit y-range that always includes ±POI so the
    // import / export reference lines aren't clipped above the supply
    // stack or below the consumption stack. Plotly's `autorange` only
    // considers data traces, not layout shapes.
    const supplyStack = sched.map((_, i) =>
      (sched[i].renewables?.solar_mw || 0)
      + (sched[i].renewables?.wind_mw || 0)
      + (sched[i].nuclear_mw || 0)
      + (sched[i].bess?.discharge_mw || 0)
      + (sched[i].thermals?.fuel_cell?.mw ?? 0)
      + (sched[i].thermals?.gas_ct?.mw ?? 0)
      + (sched[i].thermals?.diesel?.mw ?? 0)
      + (sched[i].grid_import_mw || 0));
    const consumeStack = sched.map((_, i) =>
      -((sched[i].bess?.charge_mw || 0) + (sched[i].grid_export_mw || 0)));
    const dataMax = Math.max(0, ...supplyStack);
    const dataMin = Math.min(0, ...consumeStack);
    const yMax = Math.max(dataMax, poi || 0) * 1.08;
    const yMin = Math.min(dataMin, poi ? -poi : 0) * 1.18; // extra room below for POI export label
    Plotly.newPlot("chart-dispatch", traces,
      dispatchLayout(poi, yMin, yMax),
      { displayModeBar: false, responsive: true });
    renderDispatchSummary(result);
  }

  function mkSupplyArea(x, y, name, color) {
    return {
      x, y, name, type: "scatter", stackgroup: "supply", mode: "lines",
      line: { color, width: 0, shape: "hv" }, fillcolor: color,
    };
  }

  function mkConsumeArea(x, y, name, color) {
    return {
      x, y, name, type: "scatter", stackgroup: "consume", mode: "lines",
      line: { color, width: 0, shape: "hv" }, fillcolor: color, opacity: 0.55,
    };
  }

  // Summary cards rendered directly under the dispatch chart so the
  // operator sees per-asset contribution + load served / curtailed
  // without leaving the dispatch tab.
  function renderDispatchSummary(result) {
    const wrap = $("dispatch-summary");
    if (!wrap) return;
    const sched = result.schedule;
    const dt = sched.length ? sched[0].duration_hours || 1.0 : 1.0;
    const totalH = sched.reduce((acc, p) => acc + (p.duration_hours || 1.0), 0);

    // Per-asset MWh totals + peak MW.
    const sum = (arr) => arr.reduce((a, b) => a + b, 0);
    const peak = (arr) => arr.reduce((a, b) => Math.max(a, b), 0);
    const wMw = (mwArr) => sum(mwArr.map((mw, i) => mw * (sched[i].duration_hours || 1.0)));

    const solarMw = sched.map((p) => p.renewables?.solar_mw || 0);
    const windMw  = sched.map((p) => p.renewables?.wind_mw || 0);
    const nucMw   = sched.map((p) => p.nuclear_mw || 0);
    const fcMw    = sched.map((p) => p.thermals?.fuel_cell?.mw ?? 0);
    const ctMw    = sched.map((p) => p.thermals?.gas_ct?.mw ?? 0);
    const dsMw    = sched.map((p) => p.thermals?.diesel?.mw ?? 0);
    const bessDis = sched.map((p) => p.bess?.discharge_mw || 0);
    const bessCh  = sched.map((p) => p.bess?.charge_mw || 0);
    const gridImp = sched.map((p) => p.grid_import_mw || 0);
    const gridExp = sched.map((p) => p.grid_export_mw || 0);

    // IT load served / curtailed / potential — totals across the horizon.
    const servedMwh = wMw(sched.map((p) =>
      (p.must_serve_mw || 0)
      + (p.tiers || []).reduce((a, t) => a + (t.served_mw || 0), 0)));
    const potentialMwh = wMw(sched.map((p) =>
      (p.must_serve_mw || 0)
      + (p.tiers || []).reduce((a, t) => a + (t.served_mw || 0) + (t.curtailed_mw || 0), 0)));
    const curtailedMwh = potentialMwh - servedMwh;
    const peakItMw = peak(sched.map((p) =>
      (p.must_serve_mw || 0)
      + (p.tiers || []).reduce((a, t) => a + (t.served_mw || 0), 0)));
    const peakPotentialMw = peak(sched.map((p) =>
      (p.must_serve_mw || 0)
      + (p.tiers || []).reduce((a, t) => a + (t.served_mw || 0) + (t.curtailed_mw || 0), 0)));

    // Capacity factor / utilisation per asset (avg dispatch / nameplate).
    const site = STATE.scenario?.site || {};
    const cf = (mwh, nameplate) => (nameplate > 0 && totalH > 0
      ? (mwh / (nameplate * totalH)) * 100.0 : null);

    const cards = [];
    // Load row — primary care, top of the section.
    cards.push(card("IT load served", `${servedMwh.toFixed(0)} MWh`,
      `peak ${peakItMw.toFixed(0)} MW`, "load-served"));
    cards.push(card("IT load curtailed", `${curtailedMwh.toFixed(0)} MWh`,
      potentialMwh > 0 ? `${(100*curtailedMwh/potentialMwh).toFixed(1)}% of potential` : "—",
      curtailedMwh > 0 ? "load-curtailed" : ""));
    cards.push(card("Max potential IT load", `${potentialMwh.toFixed(0)} MWh`,
      `peak ${peakPotentialMw.toFixed(0)} MW`, "load-potential"));

    // Asset row.
    if (sum(solarMw) > 0 || (site.solar?.nameplate_mw || 0) > 0) {
      const mwh = wMw(solarMw);
      const c = cf(mwh, site.solar?.nameplate_mw);
      cards.push(card("Solar", `${mwh.toFixed(0)} MWh`,
        c != null ? `CF ${c.toFixed(1)}% · peak ${peak(solarMw).toFixed(0)} MW` : `peak ${peak(solarMw).toFixed(0)} MW`,
        "asset solar"));
    }
    if (sum(windMw) > 0 || (site.wind?.nameplate_mw || 0) > 0) {
      const mwh = wMw(windMw);
      const c = cf(mwh, site.wind?.nameplate_mw);
      cards.push(card("Wind", `${mwh.toFixed(0)} MWh`,
        c != null ? `CF ${c.toFixed(1)}% · peak ${peak(windMw).toFixed(0)} MW` : `peak ${peak(windMw).toFixed(0)} MW`,
        "asset wind"));
    }
    if (site.nuclear?.enabled) {
      const mwh = wMw(nucMw);
      cards.push(card("Nuclear", `${mwh.toFixed(0)} MWh`, `peak ${peak(nucMw).toFixed(0)} MW`, "asset nuclear"));
    }
    if (site.fuel_cell?.enabled) {
      const mwh = wMw(fcMw);
      const onlineH = fcMw.filter((v, i) => v > 0.01).reduce((a, _, i) => a + (sched[i].duration_hours || 1), 0);
      cards.push(card("Fuel cell", `${mwh.toFixed(0)} MWh`,
        `online ${onlineH.toFixed(1)}h · peak ${peak(fcMw).toFixed(0)} MW`, "asset fuel-cell"));
    }
    if (site.gas_ct?.enabled) {
      const mwh = wMw(ctMw);
      const onlineH = ctMw.filter((v) => v > 0.01).length * dt;
      cards.push(card("Gas CT", `${mwh.toFixed(0)} MWh`,
        `online ${onlineH.toFixed(1)}h · peak ${peak(ctMw).toFixed(0)} MW`, "asset gas-ct"));
    }
    if (site.diesel?.enabled) {
      const mwh = wMw(dsMw);
      const onlineH = dsMw.filter((v) => v > 0.01).length * dt;
      cards.push(card("Diesel", `${mwh.toFixed(0)} MWh`,
        `online ${onlineH.toFixed(1)}h · peak ${peak(dsMw).toFixed(0)} MW`, "asset diesel"));
    }
    // BESS — always shown.
    const bessThru = wMw(bessDis) + wMw(bessCh);
    const cycles = (site.bess?.energy_mwh || 0) > 0
      ? bessThru / (2 * site.bess.energy_mwh) : 0;
    cards.push(card("BESS", `${bessThru.toFixed(0)} MWh thru`,
      `${cycles.toFixed(2)} FEC · ↑${peak(bessDis).toFixed(0)} ↓${peak(bessCh).toFixed(0)} MW`,
      "asset bess"));
    // Grid net.
    cards.push(card("Grid import", `${wMw(gridImp).toFixed(0)} MWh`,
      `peak ${peak(gridImp).toFixed(0)} MW`, "asset grid-imp"));
    cards.push(card("Grid export", `${wMw(gridExp).toFixed(0)} MWh`,
      `peak ${peak(gridExp).toFixed(0)} MW`, "asset grid-exp"));

    wrap.innerHTML = cards.join("");
  }

  function card(label, value, sub, modifier) {
    return `<div class="pl-cell ${modifier || ''}"><div class="pl-cell-label">${label}</div><div class="pl-cell-value">${value}</div>${sub ? `<div class="pl-cell-sub">${sub}</div>` : ''}</div>`;
  }

  function dispatchLayout(poiMw, yMin, yMax) {
    // POI ±limit annotations on the right edge so the line is named
    // even when scrolling/hovering doesn't surface the trace.
    const annotations = [];
    if (poiMw && poiMw > 0) {
      annotations.push(
        {
          xref: "paper", x: 1, yref: "y", y: poiMw,
          xanchor: "right", yanchor: "bottom",
          text: `POI import · ${poiMw.toFixed(0)} MW`,
          showarrow: false,
          font: { color: "#fca5a5", size: 9, family: "JetBrains Mono, monospace" },
        },
        {
          xref: "paper", x: 1, yref: "y", y: -poiMw,
          xanchor: "right", yanchor: "top",
          text: `POI export · ${poiMw.toFixed(0)} MW`,
          showarrow: false,
          font: { color: "#fca5a5", size: 9, family: "JetBrains Mono, monospace" },
        },
      );
    }
    const yaxis = {
      title: { text: "MW", font: { color: "#94a3b8", size: 10 } },
      gridcolor: "#1a1a24", color: "#94a3b8",
    };
    if (Number.isFinite(yMin) && Number.isFinite(yMax) && yMax > yMin) {
      yaxis.range = [yMin, yMax];
      yaxis.autorange = false;
    }
    return {
      ...baseLayout(),
      // Bigger bottom margin so the horizontal legend below the plot
      // stays inside the chart container and doesn't get clipped.
      margin: { l: 60, r: 60, t: 36, b: 110 },
      yaxis,
      title: { text: "Dispatch stack", font: { color: "#e2e8f0", size: 13 } },
      legend: {
        orientation: "h",
        yanchor: "top",
        y: -0.10,
        x: 0.5,
        xanchor: "center",
        font: { color: "#cbd5e1", size: 10 },
        bgcolor: "rgba(0,0,0,0)",
      },
      annotations,
    };
  }

  function baseLayout() {
    return {
      paper_bgcolor: "#0f0f14",
      plot_bgcolor: "#0a0a0f",
      font: { family: "Inter, sans-serif", color: "#e2e8f0", size: 11 },
      margin: { l: 50, r: 50, t: 36, b: 60 },
      xaxis: { gridcolor: "#1a1a24", color: "#94a3b8" },
    };
  }

  function renderPlChart(result) {
    const s = result.pl_summary;
    if (!s) return;

    const assets = computeAssetEconomics(result);

    // Cumulative-value-per-asset line chart at top. Each line shows
    // the running $ contribution that asset has accumulated by the
    // end of period t — gives the operator a "who's earning their
    // keep, when" view that the bar-chart aggregate hides.
    renderPlProgressionChart(result);

    // Sorted descending so the biggest contributors land at the top.
    const sorted = assets.slice().sort((a, b) => b.net - a.net);

    Plotly.newPlot("chart-pl", [{
      type: "bar", orientation: "h",
      x: sorted.map((a) => a.net),
      y: sorted.map((a) => a.label),
      marker: { color: sorted.map((a) => a.color) },
      text: sorted.map((a) => formatDollarsShort(a.net)),
      textposition: "outside",
      textfont: { color: "#e2e8f0", size: 11 },
      hovertemplate: "%{y}: $%{x:,.0f}<extra></extra>",
    }], {
      ...baseLayout(),
      title: { text: "Net contribution per asset", font: { color: "#e2e8f0", size: 13 } },
      xaxis: { ...baseLayout().xaxis, title: { text: "$" } },
      yaxis: { gridcolor: "#1a1a24", color: "#94a3b8", autorange: "reversed" },
      margin: { l: 180, r: 90, t: 40, b: 60 },
    }, { displayModeBar: false, responsive: true });

    renderAssetCards(assets, s);
  }

  // Compute per-asset economics — gross energy revenue (= dispatched
  // MW × LMP at every period), AS revenue from per-resource awards,
  // direct costs (fuel / VOM / degradation / marginal), and the
  // resulting net contribution. Loads earn `compute revenue` (served
  // MW × VOLL) against the energy cost of serving them. Sums across
  // assets equal system net P&L modulo small accounting nuances
  // (e.g. startup costs aren't carried per-period in the result).
  function computeAssetEconomics(result) {
    const sched = result.schedule;
    const dt = (i) => sched[i].duration_hours || 1.0;
    const lmps = sched.map((p) => p.lmp || 0);
    const energyVal = (mwArr) =>
      mwArr.reduce((acc, mw, i) => acc + mw * lmps[i] * dt(i), 0);
    const mwh = (mwArr) => mwArr.reduce((acc, mw, i) => acc + mw * dt(i), 0);

    // AS revenue per resource_id, summed across products + periods.
    const asByResource = new Map();
    sched.forEach((p) => {
      Object.values(p.as_awards || {}).forEach((rows) => {
        (rows || []).forEach((r) => {
          if (!r || r.resource_id == null) return;
          asByResource.set(r.resource_id,
            (asByResource.get(r.resource_id) || 0) + (r.revenue_dollars || 0));
        });
      });
    });

    const site = STATE.scenario?.site || {};
    const out = [];

    if (site.solar) {
      const mw = sched.map((p) => p.renewables?.solar_mw || 0);
      const energyRev = energyVal(mw);
      const totalMwh = mwh(mw);
      const recRev = totalMwh * (site.solar.rec_value_per_mwh || 0);
      const asRev = asByResource.get("site_solar") || 0;
      out.push({
        label: "Solar PV", color: COLOR.solar, tag: "asset solar",
        mwh: totalMwh, energyRev, recRev, asRev, cost: 0,
        net: energyRev + recRev + asRev,
      });
    }
    if (site.wind) {
      const mw = sched.map((p) => p.renewables?.wind_mw || 0);
      const energyRev = energyVal(mw);
      const totalMwh = mwh(mw);
      const recRev = totalMwh * (site.wind.rec_value_per_mwh || 0);
      const asRev = asByResource.get("site_wind") || 0;
      out.push({
        label: "Wind", color: COLOR.wind, tag: "asset wind",
        mwh: totalMwh, energyRev, recRev, asRev, cost: 0,
        net: energyRev + recRev + asRev,
      });
    }
    if (site.nuclear?.enabled) {
      const mw = sched.map((p) => p.nuclear_mw || 0);
      const totalMwh = mwh(mw);
      const energyRev = energyVal(mw);
      const cost = totalMwh * (site.nuclear.marginal_cost_per_mwh || 0);
      out.push({
        label: "Nuclear", color: COLOR.nuclear, tag: "asset nuclear",
        mwh: totalMwh, energyRev, asRev: 0, cost,
        net: energyRev - cost,
      });
    }

    const thermalSlots = [
      ["fuel_cell", "Fuel cell", COLOR.fuel_cell, "asset fuel-cell"],
      ["gas_ct",    "Gas CT",    COLOR.gas_ct,    "asset gas-ct"],
      ["diesel",    "Diesel",    COLOR.diesel,    "asset diesel"],
    ];
    for (const [slot, label, color, tag] of thermalSlots) {
      const t = site[slot];
      if (!t?.enabled) continue;
      const mw = sched.map((p) => p.thermals?.[slot]?.mw ?? 0);
      const totalMwh = mwh(mw);
      const energyRev = energyVal(mw);
      const fuelCost = sched.reduce((a, p) => a + (p.thermals?.[slot]?.fuel_cost_dollars || 0), 0);
      const vomCost  = sched.reduce((a, p) => a + (p.thermals?.[slot]?.vom_dollars || 0), 0);
      const asRev = asByResource.get(t.resource_id) || 0;
      const cost = fuelCost + vomCost;
      out.push({
        label, color, tag,
        mwh: totalMwh, energyRev, asRev, cost,
        fuelCost, vomCost,
        net: energyRev + asRev - cost,
      });
    }

    if (site.bess) {
      const dis = sched.map((p) => p.bess?.discharge_mw || 0);
      const chg = sched.map((p) => p.bess?.charge_mw || 0);
      // Arbitrage value = LMP-weighted net injection. Positive when
      // the LP discharges high and charges low.
      const arbValue = sched.reduce(
        (acc, p, i) => acc + (dis[i] - chg[i]) * (p.lmp || 0) * dt(i), 0);
      const asRev = asByResource.get("site_bess") || 0;
      const throughput = mwh(dis) + mwh(chg);
      const degCost = throughput * (site.bess.degradation_cost_per_mwh || 0);
      out.push({
        label: "BESS", color: COLOR.bess_dis, tag: "asset bess",
        mwh: throughput, energyRev: arbValue, asRev, cost: degCost,
        degCost,
        net: arbValue + asRev - degCost,
      });
    }

    // Grid import — pure cost.
    const gImp = sched.map((p) => p.grid_import_mw || 0);
    out.push({
      label: "Grid import", color: COLOR.grid_imp, tag: "asset grid-imp",
      mwh: mwh(gImp), energyRev: 0, asRev: 0, cost: energyVal(gImp),
      net: -energyVal(gImp),
    });
    // Grid export — pure revenue.
    const gExp = sched.map((p) => p.grid_export_mw || 0);
    out.push({
      label: "Grid export", color: COLOR.grid_exp, tag: "asset grid-exp",
      mwh: mwh(gExp), energyRev: energyVal(gExp), asRev: 0, cost: 0,
      net: energyVal(gExp),
    });

    // IT load — must-serve has no VOLL (treated as critical fixed
    // demand), so its economic line is just the energy cost incurred
    // to serve it (negative). Each curtailable tier earns compute
    // revenue at its VOLL minus the energy cost of serving the
    // delivered MWh, with curtailed MWh shown as opportunity-cost
    // detail (not folded into net).
    const mustServe = sched.map((p) => p.must_serve_mw || 0);
    out.push({
      label: "IT load · must-serve", color: "#ffffff", tag: "load-served",
      mwh: mwh(mustServe), energyRev: 0, asRev: 0,
      cost: energyVal(mustServe),
      net: -energyVal(mustServe),
    });

    const tiers = site.it_load?.tiers || [];
    tiers.forEach((tier, idx) => {
      const served = sched.map((p) => (p.tiers?.[idx]?.served_mw) || 0);
      const curtailed = sched.map((p) => (p.tiers?.[idx]?.curtailed_mw) || 0);
      const servedMwh = mwh(served);
      const curtailedMwh = mwh(curtailed);
      const computeRev = servedMwh * (tier.voll_per_mwh || 0);
      const energyCost = energyVal(served);
      out.push({
        label: `IT load · ${tier.tier_id}`, color: "#e2e8f0",
        tag: "load-tier",
        mwh: servedMwh, computeRev, asRev: 0,
        cost: energyCost,
        curtailedMwh,
        opportunityLoss: curtailedMwh * (tier.voll_per_mwh || 0),
        net: computeRev - energyCost,
      });
    });

    return out;
  }

  function renderAssetCards(assets, plSummary) {
    const wrap = $("pl-breakdown");
    if (!wrap) return;
    const cards = [];

    // System headline row — net P&L plus three category roll-ups so
    // the operator has bottom-line context above the per-asset detail.
    cards.push(systemCard("Net P&L", plSummary.net_pnl_dollars, true));
    cards.push(systemCard("Compute value (served)", plSummary.compute_revenue_dollars, false));
    cards.push(systemCard("Curtailment opportunity", -plSummary.compute_curtailment_loss_dollars, true));
    cards.push(systemCard("4-CP demand charge", -plSummary.tx_demand_charge_dollars, true));

    // Per-asset cards.
    for (const a of assets) {
      const sub = [];
      sub.push(`${a.mwh.toFixed(0)} MWh`);
      if (a.energyRev != null && Math.abs(a.energyRev) >= 0.5) sub.push(`Energy ${formatDollarsShort(a.energyRev)}`);
      if (a.computeRev != null && Math.abs(a.computeRev) >= 0.5) sub.push(`Compute ${formatDollarsShort(a.computeRev)}`);
      if (a.asRev != null && Math.abs(a.asRev) >= 0.5) sub.push(`AS ${formatDollarsShort(a.asRev)}`);
      if (a.fuelCost != null && a.fuelCost >= 0.5) sub.push(`Fuel −${formatDollarsShort(a.fuelCost)}`);
      if (a.vomCost != null && a.vomCost >= 0.5) sub.push(`VOM −${formatDollarsShort(a.vomCost)}`);
      if (a.degCost != null && a.degCost >= 0.5) sub.push(`Deg −${formatDollarsShort(a.degCost)}`);
      // Generic cost line for assets that don't break out fuel/vom/deg.
      if (a.cost != null && a.cost >= 0.5
        && a.fuelCost == null && a.vomCost == null && a.degCost == null) {
        sub.push(`Cost −${formatDollarsShort(a.cost)}`);
      }
      if (a.curtailedMwh != null && a.curtailedMwh >= 0.5) {
        sub.push(`curtailed ${a.curtailedMwh.toFixed(0)} MWh ($${formatDollarsShort(a.opportunityLoss || 0)} opp.)`);
      }
      const valueClass = a.net >= 0 ? "pos" : "neg";
      cards.push(
        `<div class="pl-cell ${a.tag}">`
        + `<div class="pl-cell-label">${a.label}</div>`
        + `<div class="pl-cell-value ${valueClass}">${formatDollars(a.net)}</div>`
        + `<div class="pl-cell-sub">${sub.join(" · ")}</div>`
        + `</div>`,
      );
    }

    wrap.innerHTML = cards.join("");
  }

  function systemCard(label, value, signed) {
    const cls = signed ? (value >= 0 ? "pos" : "neg") : "";
    return `<div class="pl-cell pl-system"><div class="pl-cell-label">${label}</div><div class="pl-cell-value ${cls}">${formatDollars(value)}</div></div>`;
  }

  // Cumulative-value-per-asset line chart for the P&L tab. Each
  // asset's per-period $ contribution is summed period-by-period
  // into a running total so the operator sees the trajectory of
  // value (when does solar pay off; when does the gas CT cross into
  // profit; how steep are the load-tier compute-revenue curves).
  function renderPlProgressionChart(result) {
    const el = $("chart-pl-progression");
    if (!el) return;
    const series = computeAssetValueProgression(result);
    if (!series.length) {
      Plotly.purge(el);
      return;
    }
    const x = extendX(result.period_times_iso);
    const traces = series.map((s) => ({
      x, y: extendY(s.cumulative),
      name: s.label,
      type: "scatter", mode: "lines",
      line: { color: s.color, width: 2, shape: "hv" },
      hovertemplate: "%{x}<br>" + s.label + ": $%{y:,.0f}<extra></extra>",
    }));
    Plotly.newPlot(el, traces, {
      ...baseLayout(),
      title: { text: "Cumulative value per asset", font: { color: "#e2e8f0", size: 13 } },
      yaxis: {
        title: { text: "$ cumulative", font: { color: "#94a3b8", size: 10 } },
        gridcolor: "#1a1a24", color: "#94a3b8",
      },
      legend: {
        orientation: "h", yanchor: "top", y: -0.10, x: 0.5, xanchor: "center",
        font: { color: "#cbd5e1", size: 10 }, bgcolor: "rgba(0,0,0,0)",
      },
      margin: { l: 70, r: 30, t: 36, b: 90 },
    }, { displayModeBar: false, responsive: true });
  }

  // Per-period $ contribution per asset, then a running total. Same
  // economic decomposition as `computeAssetEconomics` (which yields
  // horizon-total nets); this builds the per-period series and a
  // cumulative sum so the line chart reads as "value accrued by
  // the end of period t".
  function computeAssetValueProgression(result) {
    const sched = result.schedule;
    if (!sched.length) return [];
    const dt = (i) => sched[i].duration_hours || 1.0;
    const lmps = sched.map((p) => p.lmp || 0);
    const site = STATE.scenario?.site || {};

    // Per-period AS revenue keyed by resource_id.
    const asByResourcePeriod = new Map();
    sched.forEach((p, t) => {
      Object.values(p.as_awards || {}).forEach((rows) => {
        (rows || []).forEach((r) => {
          if (!r || r.resource_id == null) return;
          const arr = asByResourcePeriod.get(r.resource_id)
            || new Array(sched.length).fill(0);
          arr[t] += r.revenue_dollars || 0;
          asByResourcePeriod.set(r.resource_id, arr);
        });
      });
    });

    const out = [];

    if (site.solar) {
      const rec = site.solar.rec_value_per_mwh || 0;
      const per = sched.map((p, t) => {
        const mw = p.renewables?.solar_mw || 0;
        return mw * lmps[t] * dt(t) + mw * rec * dt(t)
          + (asByResourcePeriod.get("site_solar")?.[t] || 0);
      });
      out.push({ label: "Solar PV", color: COLOR.solar, perPeriod: per });
    }
    if (site.wind) {
      const rec = site.wind.rec_value_per_mwh || 0;
      const per = sched.map((p, t) => {
        const mw = p.renewables?.wind_mw || 0;
        return mw * lmps[t] * dt(t) + mw * rec * dt(t)
          + (asByResourcePeriod.get("site_wind")?.[t] || 0);
      });
      out.push({ label: "Wind", color: COLOR.wind, perPeriod: per });
    }
    if (site.nuclear?.enabled) {
      const marg = site.nuclear.marginal_cost_per_mwh || 0;
      const per = sched.map((p, t) => {
        const mw = p.nuclear_mw || 0;
        return mw * lmps[t] * dt(t) - mw * marg * dt(t);
      });
      out.push({ label: "Nuclear", color: COLOR.nuclear, perPeriod: per });
    }

    const thermalSlots = [
      ["fuel_cell", "Fuel cell", COLOR.fuel_cell],
      ["gas_ct",    "Gas CT",    COLOR.gas_ct],
      ["diesel",    "Diesel",    COLOR.diesel],
    ];
    for (const [slot, label, color] of thermalSlots) {
      if (!site[slot]?.enabled) continue;
      const rid = site[slot].resource_id;
      const per = sched.map((p, t) => {
        const mw = p.thermals?.[slot]?.mw ?? 0;
        const fuelCost = p.thermals?.[slot]?.fuel_cost_dollars || 0;
        const vom = p.thermals?.[slot]?.vom_dollars || 0;
        const asRev = asByResourcePeriod.get(rid)?.[t] || 0;
        return mw * lmps[t] * dt(t) + asRev - fuelCost - vom;
      });
      out.push({ label, color, perPeriod: per });
    }

    if (site.bess) {
      const deg = site.bess.degradation_cost_per_mwh || 0;
      const per = sched.map((p, t) => {
        const dis = p.bess?.discharge_mw || 0;
        const chg = p.bess?.charge_mw || 0;
        const arb = (dis - chg) * lmps[t] * dt(t);
        const degCost = (dis + chg) * deg * dt(t);
        const asRev = asByResourcePeriod.get("site_bess")?.[t] || 0;
        return arb + asRev - degCost;
      });
      out.push({ label: "BESS", color: COLOR.bess_dis, perPeriod: per });
    }

    out.push({
      label: "Grid import", color: COLOR.grid_imp,
      perPeriod: sched.map((p, t) => -(p.grid_import_mw || 0) * lmps[t] * dt(t)),
    });
    out.push({
      label: "Grid export", color: COLOR.grid_exp,
      perPeriod: sched.map((p, t) => (p.grid_export_mw || 0) * lmps[t] * dt(t)),
    });

    out.push({
      label: "IT must-serve", color: "#ffffff",
      perPeriod: sched.map((p, t) => -(p.must_serve_mw || 0) * lmps[t] * dt(t)),
    });

    const tiers = site.it_load?.tiers || [];
    const tierPalette = ["#e2e8f0", "#cbd5e1", "#94a3b8"];
    tiers.forEach((tier, idx) => {
      const voll = tier.voll_per_mwh || 0;
      const per = sched.map((p, t) => {
        const served = p.tiers?.[idx]?.served_mw || 0;
        return served * voll * dt(t) - served * lmps[t] * dt(t);
      });
      out.push({
        label: `IT tier · ${tier.tier_id}`,
        color: tierPalette[idx % tierPalette.length],
        perPeriod: per,
      });
    });

    out.forEach((s) => {
      let cum = 0;
      s.cumulative = s.perPeriod.map((v) => (cum += v));
    });
    return out;
  }

  function renderSocChart(result) {
    // SOC trajectory only — charge/discharge bars now live alongside
    // the AS awards in the combined energy+AS chart below this one,
    // following the battery dashboard's two-chart layout for storage.
    const xRaw = result.period_times_iso;
    const xLine = extendX(xRaw);
    const soc = extendY(result.schedule.map((p) => p.bess.soc_mwh));
    Plotly.newPlot("chart-soc", [
      { x: xLine, y: soc, name: "SOC MWh", type: "scatter", mode: "lines",
        line: { color: "#a78bfa", width: 2, shape: "hv" },
        fill: "tozeroy",
        fillcolor: "rgba(167,139,250,0.10)" },
    ], {
      ...baseLayout(),
      title: { text: "BESS SOC + charge / discharge", font: { color: "#e2e8f0", size: 13 } },
      yaxis: { title: { text: "MWh" }, gridcolor: "#1a1a24", color: "#94a3b8" },
      yaxis2: { title: { text: "MW (signed)" }, overlaying: "y", side: "right", gridcolor: "transparent", color: "#94a3b8" },
      barmode: "relative",
      legend: { orientation: "h", y: -0.18, font: { color: "#94a3b8", size: 10 } },
    }, { displayModeBar: false, responsive: true });
  }

  // Combined energy + AS clearing chart for the storage tab. Mirrors
  // the battery dashboard's DispatchChart layout: per-period stacked
  // bars with discharge + up-direction AS reservations above zero,
  // charge + down-direction AS reservations below zero. AS reservation
  // bars render with a hatched fill so the user can distinguish
  // capacity *held* (AS) from energy actually flowing.
  function renderAsChart(result) {
    const el = $("chart-as");
    if (!el || typeof window.DispatchChart !== "function") return;

    // ERCOT direction map for AS products. Up products stack above
    // zero (alongside discharge); down products stack below.
    const upProducts = ["reg_up", "syn", "ecrs", "nsyn"];
    const downProducts = ["reg_down"];
    const productColor = {
      reg_up:   "#a78bfa",
      reg_down: "#fb923c",
      syn:      "#fbbf24",
      ecrs:     "#22d3ee",
      nsyn:     "#f472b6",
    };
    const productLabel = {
      reg_up:   "Reg Up",
      reg_down: "Reg Down",
      syn:      "RRS",
      ecrs:     "ECRS",
      nsyn:     "Non-Spin",
    };

    const sumAwards = (rows) => (rows || []).reduce((acc, r) => acc + (r.award_mw || 0), 0);

    const data = result.schedule.map((p) => {
      const dis = p.bess?.discharge_mw || 0;
      const chg = p.bess?.charge_mw || 0;
      const up = [];
      const down = [];
      // BESS energy first — solid fill, no hatch.
      if (dis > 1e-3) up.push({ mw: dis, color: COLOR.bess_dis, label: "Discharge" });
      if (chg > 1e-3) down.push({ mw: chg, color: COLOR.bess_dis, label: "Charge" });
      // AS reservations — hatched so they read as "capacity held".
      const awards = p.as_awards || {};
      upProducts.forEach((pid) => {
        const mw = sumAwards(awards[pid]);
        if (mw > 1e-3) up.push({
          mw, color: productColor[pid] || "#a78bfa",
          label: productLabel[pid] || pid, hatch: true,
        });
      });
      downProducts.forEach((pid) => {
        const mw = sumAwards(awards[pid]);
        if (mw > 1e-3) down.push({
          mw, color: productColor[pid] || "#fb923c",
          label: productLabel[pid] || pid, hatch: true,
        });
      });
      return { up, down };
    });

    const upBound = Number(STATE.scenario?.site?.bess?.power_discharge_mw) || 0;
    const downBound = Number(STATE.scenario?.site?.bess?.power_charge_mw) || 0;

    el.innerHTML = "";
    el._dispatchChart = new window.DispatchChart(el, { data, upBound, downBound });
  }

  function renderForecastsChart(result) {
    // Tabbed editors — one for prices (LMP + each AS product), one
    // for IT load segments (must-serve + each curtailable tier),
    // one for renewable capacity factors (solar + wind). All three
    // use the same `.price-tabs` pill strip + a single shared
    // EditableLineChart so the page stays compact regardless of how
    // many AS products / tiers are configured.
    renderTabbedEditor({
      tabsId: "price-tabs", chartId: "chart-prices",
      stateKey: "activePriceTab", getTabs: getPriceTabs,
    });
    renderTabbedEditor({
      tabsId: "load-tabs", chartId: "chart-loads",
      stateKey: "activeLoadTab", getTabs: getLoadTabs,
    });
    renderTabbedEditor({
      tabsId: "renewable-tabs", chartId: "chart-renewables",
      stateKey: "activeRenewableTab", getTabs: getRenewableTabs,
    });

    // 4-CP flag toggle row: clickable Plotly bar chart. Yellow bars
    // mark flagged periods; click a bar to toggle.
    renderFourCpFlagBars(result);

    // Natural-gas price ($/MMBtu) — drives marginal cost of every
    // gas-fed thermal (gas CT + fuel cell). Diesel uses its own
    // scalar price. Materialize a per-period array if missing so the
    // editor can mutate it.
    const periods = STATE.scenario?.time_axis?.periods || 0;
    if (!Array.isArray(STATE.scenario?.natural_gas_price_per_mmbtu)
      || STATE.scenario.natural_gas_price_per_mmbtu.length !== periods) {
      STATE.scenario.natural_gas_price_per_mmbtu = new Array(periods).fill(4.0);
    }
    renderForecastEditor({
      chartId: "chart-gas-price",
      label: "Natural gas price",
      color: "#fb923c",
      enabled: true,
      values: STATE.scenario.natural_gas_price_per_mmbtu,
      inputMin: 0, decimals: 2,
    });

  }

  // 4-CP flag toggle row. Plotly bar chart sized as a thin strip;
  // click a bar to toggle the flag at that period.
  function renderFourCpFlagBars(result) {
    const el = $("chart-4cp-flags");
    if (!el) return;
    const xRaw = result.period_times_iso;
    const flags = STATE.scenario?.four_cp?.period_flags || [];
    const colors = flags.map((f) => (f ? "#fbbf24" : "#1f2937"));
    const yVals = flags.map(() => 1);
    Plotly.newPlot(el, [{
      x: xRaw, y: yVals, type: "bar",
      marker: { color: colors },
      hovertemplate: "%{x}<br>%{customdata}<extra></extra>",
      customdata: flags.map((f) => (f ? "flagged 4-CP day" : "not flagged")),
      name: "4-CP flag",
    }], {
      ...baseLayout(),
      margin: { l: 50, r: 20, t: 6, b: 18 },
      yaxis: { showticklabels: false, fixedrange: true, range: [0, 1] },
      xaxis: { ...baseLayout().xaxis, showticklabels: false, fixedrange: true },
      showlegend: false,
    }, { displayModeBar: false, responsive: true });

    el.removeAllListeners?.("plotly_click");
    el.on("plotly_click", (data) => {
      const point = data.points?.[0];
      if (!point) return;
      const fc = STATE.scenario?.four_cp;
      if (!fc?.period_flags) return;
      const idx = point.pointNumber;
      if (idx == null || idx < 0 || idx >= fc.period_flags.length) return;
      fc.period_flags[idx] = !fc.period_flags[idx];
      renderFourCpFlagBars(result);
    });
  }

  // Generic pill-strip editor — render N pills above one shared
  // EditableLineChart, switch the chart's data on click. Used for
  // LMP+AS prices, IT load segments, and renewable capacity factors.
  function renderTabbedEditor({ tabsId, chartId, stateKey, getTabs }) {
    const tabsEl = $(tabsId);
    if (!tabsEl) return;
    const tabs = getTabs();
    if (!tabs.find((t) => t.key === STATE[stateKey])) {
      STATE[stateKey] = tabs[0]?.key || null;
    }
    tabsEl.innerHTML = tabs.map((tab) => {
      const data = tab.values() || [];
      const mn = data.length ? Math.min(...data) : 0;
      const mx = data.length ? Math.max(...data) : 0;
      const active = tab.key === STATE[stateKey];
      const range = (tab.rangeFormat || defaultRangeFormat)(mn, mx, tab);
      return `<button class="price-tab${active ? " active" : ""}" data-tab="${escapeAttr(tab.key)}">`
        + `<span class="price-tab-swatch" style="background:${tab.color}"></span>`
        + `<span class="price-tab-name">${escapeAttr(tab.title)}</span>`
        + `<span class="price-tab-range">${escapeAttr(range)}</span>`
        + `</button>`;
    }).join("");
    tabsEl.querySelectorAll(".price-tab").forEach((el) => {
      el.addEventListener("click", () => {
        STATE[stateKey] = el.dataset.tab;
        renderTabbedEditor({ tabsId, chartId, stateKey, getTabs });
        renderActiveTabChart({ chartId, stateKey, getTabs });
      });
    });
    renderActiveTabChart({ chartId, stateKey, getTabs });
  }

  function renderActiveTabChart({ chartId, stateKey, getTabs }) {
    const tab = getTabs().find((t) => t.key === STATE[stateKey]);
    if (!tab) return;
    renderForecastEditor({
      chartId,
      label: tab.title,
      color: tab.color,
      enabled: Array.isArray(tab.values()),
      values: tab.values(),
      inputMin: tab.inputMin,
      inputMax: tab.inputMax,
      yRange: tab.yRange,
      decimals: tab.decimals,
    });
  }

  function defaultRangeFormat(mn, mx, tab) {
    const d = tab.decimals == null ? 0 : tab.decimals;
    const prefix = tab.unitPrefix == null ? "$" : tab.unitPrefix;
    return `${prefix}${Number(mn).toFixed(d)}–${prefix}${Number(mx).toFixed(d)}`;
  }

  // Tab descriptors — one per tabbed editor on the forecasts tab.
  function getPriceTabs() {
    const tabs = [];
    tabs.push({
      key: "lmp",
      title: "LMP",
      color: "#c084fc",
      values: () => STATE.scenario?.lmp_forecast_per_mwh,
      inputMin: null, decimals: 0,
      unitPrefix: "$",
    });
    const palette = ["#a78bfa", "#34d399", "#fbbf24", "#22d3ee", "#f472b6", "#60a5fa"];
    (STATE.scenario?.as_products || []).forEach((ap, i) => {
      tabs.push({
        key: `as_${ap.product_id}`,
        title: ap.title || ap.product_id,
        color: palette[i % palette.length],
        values: () => ap.price_forecast_per_mwh,
        inputMin: 0, decimals: 1,
        unitPrefix: "$",
      });
    });
    return tabs;
  }

  function getLoadTabs() {
    const tabs = [];
    const mustServe = STATE.scenario?.site?.it_load?.must_serve_mw;
    if (Array.isArray(mustServe)) {
      tabs.push({
        key: "must_serve",
        title: "Must-serve",
        color: "#ffffff",
        values: () => STATE.scenario.site.it_load.must_serve_mw,
        inputMin: 0, decimals: 0,
        unitPrefix: "",
        rangeFormat: (mn, mx) => `${mn.toFixed(0)}–${mx.toFixed(0)} MW`,
      });
    }
    const tiers = STATE.scenario?.site?.it_load?.tiers || [];
    const tierPalette = ["#e2e8f0", "#cbd5e1", "#94a3b8"];
    const periods = STATE.scenario?.time_axis?.periods || 0;
    tiers.forEach((tier, idx) => {
      // Materialise per-period array if missing (sidebar capacity_mw
      // scalar broadcasts here).
      if (!Array.isArray(tier.capacity_per_period_mw)
        || tier.capacity_per_period_mw.length !== periods) {
        tier.capacity_per_period_mw = new Array(periods).fill(tier.capacity_mw || 0);
      }
      tabs.push({
        key: `tier_${idx}`,
        title: `Tier · ${tier.tier_id}`,
        color: tierPalette[idx % tierPalette.length],
        values: () => tier.capacity_per_period_mw,
        inputMin: 0, decimals: 0,
        rangeFormat: (mn, mx) => `${mn.toFixed(0)}–${mx.toFixed(0)} MW`,
      });
    });
    return tabs;
  }

  function getRenewableTabs() {
    const tabs = [];
    if (STATE.scenario?.site?.solar) {
      tabs.push({
        key: "solar",
        title: "Solar",
        color: COLOR.solar,
        values: () => STATE.scenario.site.solar.capacity_factors,
        inputMin: 0, inputMax: 1, decimals: 2,
        yRange: [0, 1.05],
        rangeFormat: (mn, mx) => `${mn.toFixed(2)}–${mx.toFixed(2)}`,
      });
    }
    if (STATE.scenario?.site?.wind) {
      tabs.push({
        key: "wind",
        title: "Wind",
        color: COLOR.wind,
        values: () => STATE.scenario.site.wind.capacity_factors,
        inputMin: 0, inputMax: 1, decimals: 2,
        yRange: [0, 1.05],
        rangeFormat: (mn, mx) => `${mn.toFixed(2)}–${mx.toFixed(2)}`,
      });
    }
    return tabs;
  }

  // Minimal HTML attribute escaper for product titles spliced into a
  // template literal.
  function escapeAttr(s) {
    return String(s).replace(/[&<>"']/g, (c) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
    }[c]));
  }

  // Build (or rebuild) one editor section per curtailable tier inside
  // Per-period editable forecast chart — drag-to-edit SVG chart from
  // editable-chart.js. Replaces the prior numeric-input-grid editor;
  // dragging a point reshapes the forecast directly.
  function renderForecastEditor(opts) {
    const chartEl = $(opts.chartId);
    if (!chartEl) return;
    if (!opts.enabled || !Array.isArray(opts.values)) {
      chartEl.innerHTML = `<div style="padding:1.2rem; color:var(--text-dim); font-size:0.8rem; text-align:center;">${opts.label} disabled — enable in the sidebar to forecast it.</div>`;
      return;
    }
    if (typeof window.EditableLineChart !== "function") {
      chartEl.innerHTML = `<div style="padding:1.2rem; color:var(--red); font-size:0.8rem;">EditableLineChart failed to load.</div>`;
      return;
    }
    const formatValue = opts.decimals === 0
      ? (v) => Number(v).toFixed(0)
      : (v) => Number(v).toFixed(opts.decimals ?? 2);
    chartEl.innerHTML = "";
    // Construct fresh on each render so a horizon / period change
    // propagates cleanly. The chart's onChange callback syncs the
    // user's drag-edits back into the scenario array on pointer-up.
    const chart = new window.EditableLineChart(chartEl, {
      data: opts.values.slice(),
      // Preserve an explicit `null` floor (LMP can go negative); only
      // fall back to 0 when the caller didn't specify one at all.
      min: opts.inputMin === undefined ? 0 : opts.inputMin,
      max: opts.inputMax === undefined ? null : opts.inputMax,
      fixedAxis: !!opts.yRange,
      color: opts.color,
      formatValue,
      seriesName: opts.label,
      editable: true,
      onChange: (newValues) => {
        for (let i = 0; i < newValues.length && i < opts.values.length; i++) {
          opts.values[i] = newValues[i];
        }
      },
    });
    chartEl._editableChart = chart;
  }

  function periodIsoFromScenario() {
    const ta = STATE.scenario?.time_axis;
    if (!ta) return [];
    const start = new Date(ta.start_iso);
    const dt = (ta.resolution_minutes || 60) * 60_000;
    return Array.from({ length: ta.periods }, (_, t) =>
      new Date(start.getTime() + t * dt).toISOString());
  }

  function formatHour(t) {
    const ta = STATE.scenario?.time_axis;
    if (!ta) return `t${t}`;
    const dt = (ta.resolution_minutes || 60) * 60_000;
    const d = new Date(new Date(ta.start_iso).getTime() + t * dt);
    return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  }

  function renderFourCpChart(result) {
    const x = result.period_times_iso;
    const grid = result.schedule.map((p) => p.grid_import_mw);
    const flags = STATE.scenario?.four_cp?.period_flags || [];
    const flaggedColors = flags.map((f) => (f ? "#fbbf24" : "#475569"));
    Plotly.newPlot("chart-4cp", [
      { x, y: grid, type: "bar", name: "Grid import MW", marker: { color: flaggedColors } },
    ], {
      ...baseLayout(),
      title: { text: "Grid import — flagged periods highlighted", font: { color: "#e2e8f0", size: 13 } },
      yaxis: { title: { text: "MW" }, gridcolor: "#1a1a24", color: "#94a3b8" },
    }, { displayModeBar: false, responsive: true });

    const peak = result.pl_summary?.peak_grid_import_mw ?? 0;
    const charge = result.pl_summary?.tx_demand_charge_dollars ?? 0;
    const wrap = $("fourcp-summary");
    wrap.innerHTML = "";
    [
      ["Peak grid import (flagged)", peak.toFixed(1) + " MW"],
      ["4-CP demand charge", formatDollars(charge)],
      ["Flagged periods", flags.filter(Boolean).length.toString()],
    ].forEach(([label, value]) => {
      const cell = document.createElement("div");
      cell.className = "pl-cell";
      cell.innerHTML = `<div class="pl-cell-label">${label}</div><div class="pl-cell-value">${value}</div>`;
      wrap.appendChild(cell);
    });
  }

  function renderCommitmentChart(result) {
    const x = result.period_times_iso;
    const slots = ["fuel_cell", "gas_ct", "diesel"];
    const traces = slots.map((slot, i) => {
      const y = result.schedule.map((p) => (p.thermals?.[slot]?.mw ?? 0) > 0.01 ? (i + 1) : null);
      const text = result.schedule.map((p) => (p.thermals?.[slot]?.mw ?? 0).toFixed(0) + " MW");
      return {
        x, y, name: slot, type: "scatter", mode: "markers",
        marker: { color: ["#a78bfa", "#fbbf24", "#f87171"][i], size: 18, symbol: "square" },
        text, hoverinfo: "x+text+name",
      };
    });
    Plotly.newPlot("chart-commitment", traces, {
      ...baseLayout(),
      title: { text: "Thermal commitment Gantt", font: { color: "#e2e8f0", size: 13 } },
      yaxis: {
        tickmode: "array",
        tickvals: [1, 2, 3],
        ticktext: ["Fuel cell", "Gas CT", "Diesel"],
        gridcolor: "#1a1a24",
        color: "#94a3b8",
      },
    }, { displayModeBar: false, responsive: true });
  }

  // ------------------------------------------------------------ Boot
  document.addEventListener("DOMContentLoaded", init);
})();
