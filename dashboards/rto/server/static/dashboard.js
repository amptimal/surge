// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
// RTO day-ahead dashboard — client-side single-page app.

'use strict';

const $ = (id) => document.getElementById(id);
const escapeHtml = (s) => String(s).replace(/[&<>"']/g, c => (
  { '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;' }[c]
));

const state = {
  meta: null,
  scenario: null,
  lastResult: null,
  activeTab: 'summary',
  solving: false,
  observers: [],
  // Elements we've already attached a ResizeObserver to — prevents the
  // "observe fires on attach → render again → observe again" loop that
  // happens when render helpers unconditionally call observeResize().
  observedEls: (typeof WeakSet !== 'undefined') ? new WeakSet() : null,
};

// Reserve product color palette — echoes the battery dashboard.
const RESERVE_COLORS = {
  reg_up: '#fbbf24',     // amber
  reg_down: '#60a5fa',   // blue
  syn: '#22d3ee',        // cyan
  nsyn: '#f472b6',       // rose
};

// Generator fuel-type → swatch color for stacked charts.
const FUEL_COLORS = {
  solar: '#fbbf24', wind: '#22d3ee', pv: '#fbbf24',
  hydro: '#60a5fa', nuclear: '#a78bfa',
  gas: '#f472b6', naturalgas: '#f472b6', ng: '#f472b6',
  coal: '#64748b', oil: '#9ca3af',
  biomass: '#34d399', geothermal: '#f97316',
};
const GEN_FALLBACK_COLORS = [
  '#a78bfa','#34d399','#fbbf24','#60a5fa','#f472b6','#22d3ee',
  '#f97316','#c084fc','#fb7185','#818cf8','#facc15','#10b981',
];

function colorForGenerator(gen, fallbackIndex) {
  const fuel = String(gen.fuel_type || '').toLowerCase().replace(/\s+/g,'');
  return FUEL_COLORS[fuel] || GEN_FALLBACK_COLORS[fallbackIndex % GEN_FALLBACK_COLORS.length];
}

const fmtMoney = (v, digits) => {
  if (v === null || v === undefined || Number.isNaN(v)) return '—';
  const sign = v < 0 ? '−' : '';
  const abs = Math.abs(v);
  const d = digits ?? (abs >= 10000 ? 0 : abs >= 100 ? 0 : 2);
  return `${sign}$${abs.toLocaleString(undefined, { minimumFractionDigits: d, maximumFractionDigits: d })}`;
};
const fmtPrice = (v) => v === null || v === undefined ? '—' : `$${v.toFixed(2)}`;
const fmtMw = (v) => v === null || v === undefined ? '—' : `${v.toFixed(1)} MW`;

function observeResize(el, cb) {
  if (!el || !window.ResizeObserver) return;
  if (state.observedEls && state.observedEls.has(el)) return;
  // Suppress the spec-mandated initial observation — otherwise attaching
  // triggers an immediate render, which re-enters here, etc.
  let seenInitial = false;
  const ro = new ResizeObserver(() => {
    if (!seenInitial) { seenInitial = true; return; }
    cb();
  });
  ro.observe(el);
  state.observers.push(ro);
  if (state.observedEls) state.observedEls.add(el);
}

// ── Period helpers ────────────────────────────────────────────────

function periodTimeLabel(i) {
  const t = state.scenario?.time_axis;
  if (!t || !t.start_iso) return `P${i}`;
  const start = new Date(t.start_iso);
  if (Number.isNaN(start.getTime())) return `P${i}`;
  const dt = new Date(start.getTime() + i * (t.resolution_minutes || 60) * 60000);
  const pad = (v) => String(v).padStart(2, '0');
  return `P${i} · ${pad(dt.getHours())}:${pad(dt.getMinutes())}`;
}

// ── Chart hover + crosshair (shared by time-series panes) ─────────

function installHover(container, svg, geom, tooltipContent) {
  const { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W } = geom;
  if (!n) return;
  if (getComputedStyle(container).position === 'static') container.style.position = 'relative';
  let tooltip = container.querySelector('.sc-tooltip');
  if (!tooltip) {
    tooltip = document.createElement('div');
    tooltip.className = 'sc-tooltip';
    container.appendChild(tooltip);
  }
  const svgNS = 'http://www.w3.org/2000/svg';
  const crosshair = document.createElementNS(svgNS, 'line');
  crosshair.setAttribute('class', 'sc-crosshair');
  crosshair.style.display = 'none';
  svg.appendChild(crosshair);
  const pxPerPeriod = innerW / n;
  const hide = () => { tooltip.classList.remove('visible'); crosshair.style.display = 'none'; };
  svg.addEventListener('pointermove', (ev) => {
    const rect = svg.getBoundingClientRect();
    const scaleX = svg.viewBox.baseVal.width / rect.width;
    const svgX = (ev.clientX - rect.left) * scaleX;
    if (svgX < PAD_L || svgX > W - PAD_R) { hide(); return; }
    const i = Math.max(0, Math.min(n - 1, Math.floor((svgX - PAD_L) / pxPerPeriod)));
    const html = tooltipContent(i);
    if (!html) { hide(); return; }
    tooltip.innerHTML = html;
    tooltip.classList.add('visible');
    const cr = container.getBoundingClientRect();
    const relX = ev.clientX - cr.left;
    const relY = ev.clientY - cr.top;
    let left = relX + 12;
    let top = relY + 12;
    const ttW = tooltip.offsetWidth, ttH = tooltip.offsetHeight;
    if (left + ttW > cr.width - 4) left = relX - ttW - 12;
    if (top + ttH > cr.height - 4) top = relY - ttH - 12;
    tooltip.style.left = Math.max(4, left) + 'px';
    tooltip.style.top = Math.max(4, top) + 'px';
    const centerX = PAD_L + (i + 0.5) * pxPerPeriod;
    crosshair.setAttribute('x1', centerX);
    crosshair.setAttribute('x2', centerX);
    crosshair.setAttribute('y1', PAD_T);
    crosshair.setAttribute('y2', PAD_T + innerH);
    crosshair.style.display = '';
  });
  svg.addEventListener('pointerleave', hide);
}

// ── Minimal multi-series line chart ───────────────────────────────
// series = [{ name, color, data: [...], unit }]
// opts   = { yMin, yMax, tooltipPrecision }

function renderLineChart(container, series, opts = {}) {
  if (!container || !series.length) return;
  const W = container.clientWidth || 400;
  const H = container.clientHeight || 180;
  const PAD_L = 42, PAD_R = 12, PAD_T = 8, PAD_B = 20;
  const innerW = W - PAD_L - PAD_R;
  const innerH = H - PAD_T - PAD_B;
  const n = series[0].data.length;
  const all = series.flatMap(s => s.data).filter(v => typeof v === 'number' && isFinite(v));
  if (!all.length) { container.innerHTML = '<div class="viol-empty">no data</div>'; return; }
  const rawMax = Math.max(...all), rawMin = Math.min(...all);
  const yMin = opts.yMin !== undefined ? opts.yMin : Math.min(0, Math.floor(rawMin));
  const yMax = opts.yMax !== undefined ? opts.yMax : Math.ceil(rawMax * 1.08 + 1);
  const ySpan = (yMax - yMin) || 1;
  const toX = (i) => PAD_L + (n > 1 ? i / (n - 1) : 0.5) * innerW;
  const toY = (v) => PAD_T + innerH - ((v - yMin) / ySpan) * innerH;

  const svgNS = 'http://www.w3.org/2000/svg';
  container.innerHTML = '';
  const svg = document.createElementNS(svgNS, 'svg');
  svg.setAttribute('class', 'sc-chart');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('preserveAspectRatio', 'none');

  // grid + y labels
  for (let i = 0; i <= 4; i++) {
    const v = yMin + (ySpan * i) / 4;
    const y = toY(v);
    const gl = document.createElementNS(svgNS, 'line');
    gl.setAttribute('class', 'sc-grid-line');
    gl.setAttribute('x1', PAD_L); gl.setAttribute('x2', W - PAD_R);
    gl.setAttribute('y1', y); gl.setAttribute('y2', y);
    svg.appendChild(gl);
    const t = document.createElementNS(svgNS, 'text');
    t.setAttribute('class', 'sc-axis-label');
    t.setAttribute('x', PAD_L - 3); t.setAttribute('y', y + 3); t.setAttribute('text-anchor', 'end');
    t.textContent = v.toFixed(v === Math.floor(v) ? 0 : 1);
    svg.appendChild(t);
  }
  // x labels
  const xTicks = Math.min(n, 10);
  const stride = Math.max(1, Math.floor((n - 1) / (xTicks - 1 || 1)));
  for (let i = 0; i < n; i += stride) {
    const t = document.createElementNS(svgNS, 'text');
    t.setAttribute('class', 'sc-axis-label');
    t.setAttribute('x', toX(i)); t.setAttribute('y', PAD_T + innerH + 13); t.setAttribute('text-anchor', 'middle');
    t.textContent = i;
    svg.appendChild(t);
  }

  series.forEach(s => {
    const poly = document.createElementNS(svgNS, 'polyline');
    poly.setAttribute('fill', 'none');
    poly.setAttribute('stroke', s.color);
    poly.setAttribute('stroke-width', s.strokeWidth ?? 1.8);
    if (s.dashArray) poly.setAttribute('stroke-dasharray', s.dashArray);
    poly.setAttribute('points', s.data.map((v, i) => `${toX(i)},${toY(v)}`).join(' '));
    svg.appendChild(poly);
  });

  // legend
  if (opts.showLegend !== false && series.length > 1) {
    let lx = PAD_L + 4;
    series.forEach(s => {
      const l = document.createElementNS(svgNS, 'line');
      l.setAttribute('x1', lx); l.setAttribute('x2', lx + 12);
      l.setAttribute('y1', PAD_T + 5); l.setAttribute('y2', PAD_T + 5);
      l.setAttribute('stroke', s.color); l.setAttribute('stroke-width', 2);
      svg.appendChild(l);
      const lb = document.createElementNS(svgNS, 'text');
      lb.setAttribute('class', 'sc-axis-label');
      lb.setAttribute('x', lx + 15); lb.setAttribute('y', PAD_T + 8);
      lb.textContent = s.name;
      svg.appendChild(lb);
      lx += 15 + (s.name.length * 5.5) + 14;
    });
  }

  container.appendChild(svg);

  const prec = opts.tooltipPrecision ?? 2;
  installHover(container, svg, { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W }, (i) => {
    const rows = series.map(s => {
      const v = s.data[i];
      const text = (v === null || v === undefined || !isFinite(v)) ? '—' : `${v.toFixed(prec)}${s.unit ? ' ' + s.unit : ''}`;
      return `<div class="sc-tooltip-row"><span><span class="sw" style="background:${s.color}"></span>${escapeHtml(s.name)}</span><span class="val">${text}</span></div>`;
    }).join('');
    return `<div class="sc-tooltip-title">${escapeHtml(periodTimeLabel(i))}</div>${rows}`;
  });
}

// ── Stacked area chart (generators, loads) ────────────────────────
// series = [{ name, color, data: [...] }]  stacked top-down additively.

function renderStackedArea(container, series, opts = {}) {
  if (!container) return;
  if (!series.length) { container.innerHTML = '<div class="viol-empty">no data</div>'; return; }
  const W = container.clientWidth || 400;
  const H = container.clientHeight || 220;
  const PAD_L = 44, PAD_R = 12, PAD_T = 8, PAD_B = 22;
  const innerW = W - PAD_L - PAD_R;
  const innerH = H - PAD_T - PAD_B;
  const n = series[0].data.length;
  // cumulative per period
  const cumLo = new Array(n).fill(0);
  const segments = series.map(s => {
    const lo = cumLo.slice();
    const hi = lo.map((v, i) => v + (s.data[i] || 0));
    for (let i = 0; i < n; i++) cumLo[i] = hi[i];
    return { ...s, lo, hi };
  });
  const topMax = cumLo.reduce((m, v) => v > m ? v : m, 0);
  const yMin = 0;
  const yMax = opts.yMax ?? Math.max(topMax * 1.08, 1);
  const ySpan = yMax - yMin;
  const toX = (i) => PAD_L + (n > 1 ? i / (n - 1) : 0.5) * innerW;
  const toY = (v) => PAD_T + innerH - ((v - yMin) / ySpan) * innerH;

  const svgNS = 'http://www.w3.org/2000/svg';
  container.innerHTML = '';
  const svg = document.createElementNS(svgNS, 'svg');
  svg.setAttribute('class', 'sc-chart');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('preserveAspectRatio', 'none');

  for (let i = 0; i <= 4; i++) {
    const v = yMin + (ySpan * i) / 4;
    const y = toY(v);
    const gl = document.createElementNS(svgNS, 'line');
    gl.setAttribute('class', 'sc-grid-line');
    gl.setAttribute('x1', PAD_L); gl.setAttribute('x2', W - PAD_R);
    gl.setAttribute('y1', y); gl.setAttribute('y2', y);
    svg.appendChild(gl);
    const t = document.createElementNS(svgNS, 'text');
    t.setAttribute('class', 'sc-axis-label');
    t.setAttribute('x', PAD_L - 3); t.setAttribute('y', y + 3); t.setAttribute('text-anchor', 'end');
    t.textContent = v.toFixed(v === Math.floor(v) ? 0 : 1);
    svg.appendChild(t);
  }

  segments.forEach(s => {
    const up = s.hi.map((v, i) => `${toX(i)},${toY(v)}`);
    const down = s.lo.map((v, i) => `${toX(i)},${toY(v)}`).reverse();
    const poly = document.createElementNS(svgNS, 'polygon');
    poly.setAttribute('fill', s.color);
    poly.setAttribute('fill-opacity', '0.75');
    poly.setAttribute('stroke', s.color);
    poly.setAttribute('stroke-width', '0.8');
    poly.setAttribute('points', [...up, ...down].join(' '));
    svg.appendChild(poly);
  });

  // x labels
  const xTicks = Math.min(n, 10);
  const stride = Math.max(1, Math.floor((n - 1) / (xTicks - 1 || 1)));
  for (let i = 0; i < n; i += stride) {
    const t = document.createElementNS(svgNS, 'text');
    t.setAttribute('class', 'sc-axis-label');
    t.setAttribute('x', toX(i)); t.setAttribute('y', PAD_T + innerH + 13); t.setAttribute('text-anchor', 'middle');
    t.textContent = i;
    svg.appendChild(t);
  }

  container.appendChild(svg);

  installHover(container, svg, { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W }, (i) => {
    const rows = segments
      .filter(s => Math.abs(s.data[i] || 0) > 1e-6)
      .map(s => `<div class="sc-tooltip-row"><span><span class="sw" style="background:${s.color}"></span>${escapeHtml(s.name)}</span><span class="val">${(s.data[i] || 0).toFixed(1)} MW</span></div>`)
      .join('');
    const total = segments.reduce((t, s) => t + (s.data[i] || 0), 0);
    return `<div class="sc-tooltip-title">${escapeHtml(periodTimeLabel(i))}</div>${rows}<div class="sc-tooltip-row" style="border-top:1px solid var(--border-sub); padding-top:3px; margin-top:3px"><span>Total</span><span class="val">${total.toFixed(1)} MW</span></div>`;
  });
}

// ── Sparkline (for table rows) ────────────────────────────────────

function renderSparkline(container, values, color) {
  const W = container.clientWidth || 80;
  const H = container.clientHeight || 22;
  const n = values.length;
  if (!n) return;
  const mn = Math.min(0, ...values);
  const mx = Math.max(...values, mn + 1);
  const span = (mx - mn) || 1;
  const svgNS = 'http://www.w3.org/2000/svg';
  container.innerHTML = '';
  const svg = document.createElementNS(svgNS, 'svg');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('preserveAspectRatio', 'none');
  svg.style.width = '100%';
  svg.style.height = '100%';
  const pts = values.map((v, i) => {
    const x = n > 1 ? (i / (n - 1)) * W : W / 2;
    const y = H - 2 - ((v - mn) / span) * (H - 4);
    return `${x},${y}`;
  }).join(' ');
  const poly = document.createElementNS(svgNS, 'polyline');
  poly.setAttribute('fill', 'none');
  poly.setAttribute('stroke', color || 'var(--purple)');
  poly.setAttribute('stroke-width', '1.4');
  poly.setAttribute('points', pts);
  svg.appendChild(poly);
  container.appendChild(svg);
}

// ── Form ↔ scenario binding ───────────────────────────────────────

function writeForm() {
  const scen = state.scenario;
  if (!scen) return;
  // Case
  const sel = $('sel-case');
  if (sel && scen.source) {
    // Matches by builtin id; custom uploads stay as "uploaded".
    sel.value = scen.source.case_id || sel.value;
  }
  // Network summary
  const sm = scen.network_summary || {};
  $('network-summary').textContent = `${sm.buses ?? 0} buses · ${sm.generators ?? 0} gens · ${sm.loads ?? 0} loads · ${Math.round(sm.total_load_mw ?? 0)} MW load · ${Math.round(sm.total_capacity_mw ?? 0)} MW cap`;
  $('case-note').textContent = scen.source?.title || '—';
  // Time
  const t = scen.time_axis || {};
  if (t.start_iso) {
    const d = new Date(t.start_iso);
    if (!Number.isNaN(d.getTime())) {
      const pad = (n) => String(n).padStart(2, '0');
      $('inp-start').value = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
    }
  }
  $('inp-periods').value = t.periods ?? 24;
  $('sel-resolution').value = String(t.resolution_minutes ?? 60);
  // Load
  const lc = scen.load_config || {};
  $('sel-load-handling').value = lc.handling || 'fixed';
  $('sel-load-profile').value = lc.profile_shape || 'duck';
  $('inp-voll').value = lc.default_voll_per_mwh ?? 9000;
  toggleVollRow();
  // Offers
  const oc = scen.offers_config || {};
  $('sel-offers-synthesis').value = oc.synthesis || 'from_cost_coeffs';
  renderOffersHint();
  // Renewables — hide section if no renewable gens in the case.
  const hasRen = (scen.generators || []).some(g => g.is_renewable);
  const renSect = $('renewables-section');
  renSect.hidden = !hasRen;
  if (hasRen) {
    $('sel-renewables-profile').value = (scen.renewables_config || {}).profile_shape || 'solar';
    const count = (scen.generators || []).filter(g => g.is_renewable).length;
    const cap = (scen.generators || []).filter(g => g.is_renewable).reduce((s, g) => s + (g.pmax_mw || 0), 0);
    $('renewables-hint').textContent = `${count} renewable gens · ${cap.toFixed(1)} MW nameplate`;
  }
  // Reserves — rebuild rows
  renderReserveRows();
  // Policy
  const p = scen.policy || {};
  $('sel-commitment').value = p.commitment_mode || 'optimize';
  $('sel-lp-solver').value = p.lp_solver || 'highs';
  $('inp-mip-gap').value = p.mip_gap ?? 0.001;
  $('sel-run-pricing').value = (p.run_pricing === false) ? 'false' : 'true';
  $('inp-voll-penalty').value = p.voll_per_mwh ?? 9000;
  $('inp-thermal').value = p.thermal_overload_per_mwh ?? 5000;
  $('inp-reserve-short').value = p.reserve_shortfall_per_mwh ?? 1000;
  $('inp-time-limit').value = p.time_limit_secs ?? '';
}

function readForm() {
  const scen = state.scenario;
  if (!scen) return;
  const t = scen.time_axis || (scen.time_axis = {});
  const startStr = $('inp-start').value;
  if (startStr) t.start_iso = new Date(startStr).toISOString().slice(0, 19);
  t.periods = Math.max(1, parseInt($('inp-periods').value, 10) || 24);
  t.resolution_minutes = parseInt($('sel-resolution').value, 10) || 60;
  t.horizon_minutes = t.periods * t.resolution_minutes;

  const lc = scen.load_config || (scen.load_config = {});
  lc.handling = $('sel-load-handling').value;
  lc.profile_shape = $('sel-load-profile').value;
  lc.default_voll_per_mwh = parseFloat($('inp-voll').value) || 9000;

  const oc = scen.offers_config || (scen.offers_config = {});
  oc.synthesis = $('sel-offers-synthesis').value;

  const rc = scen.renewables_config || (scen.renewables_config = {});
  if (!$('renewables-section').hidden) rc.profile_shape = $('sel-renewables-profile').value;

  // Reserves
  const res = scen.reserves_config || (scen.reserves_config = { zone_id: 1, products: {} });
  document.querySelectorAll('.reserves-row').forEach(row => {
    const pid = row.dataset.product;
    const pct = parseFloat(row.querySelector('.res-pct').value);
    const abs = row.querySelector('.res-abs').value.trim();
    res.products[pid] = {
      percent_of_peak: Number.isFinite(pct) ? pct : 0,
      absolute_mw: abs === '' ? null : parseFloat(abs),
    };
  });

  const p = scen.policy || (scen.policy = {});
  p.commitment_mode = $('sel-commitment').value;
  p.lp_solver = $('sel-lp-solver').value;
  p.mip_gap = parseFloat($('inp-mip-gap').value) || 0.001;
  p.run_pricing = $('sel-run-pricing').value === 'true';
  p.voll_per_mwh = parseFloat($('inp-voll-penalty').value) || 9000;
  p.thermal_overload_per_mwh = parseFloat($('inp-thermal').value) || 5000;
  p.reserve_shortfall_per_mwh = parseFloat($('inp-reserve-short').value) || 1000;
  const tl = $('inp-time-limit').value.trim();
  p.time_limit_secs = tl === '' ? null : parseFloat(tl);
}

function toggleVollRow() {
  $('voll-row').classList.toggle('visible', $('sel-load-handling').value === 'dispatchable');
}

function renderReserveRows() {
  const stack = $('reserves-stack');
  stack.innerHTML = '';
  const prods = (state.scenario.reserves_config || {}).products || {};
  const order = ['reg_up', 'reg_down', 'syn', 'nsyn'];
  const labels = { reg_up: 'Reg up', reg_down: 'Reg down', syn: 'Spinning', nsyn: 'Non-spin' };
  order.forEach(pid => {
    const spec = prods[pid] || { percent_of_peak: 0, absolute_mw: null };
    const row = document.createElement('div');
    row.className = `reserves-row ${pid}`;
    row.dataset.product = pid;
    row.innerHTML = `
      <span class="reserve-dot"></span>
      <span>${escapeHtml(labels[pid] || pid)}</span>
      <input type="number" class="res-pct" step="0.5" value="${spec.percent_of_peak ?? 0}" title="Percent of peak load">
      <input type="number" class="res-abs" step="1" value="${spec.absolute_mw ?? ''}" placeholder="MW" title="Absolute MW override">
    `;
    stack.appendChild(row);
  });
  stack.querySelectorAll('input').forEach(el => el.addEventListener('change', readForm));
}

function renderOffersHint() {
  const gens = state.scenario?.generators || [];
  const withCost = gens.filter(g => g.has_cost).length;
  const synth = $('sel-offers-synthesis').value;
  const suffix = (state.scenario?.offers_config?.per_gen && Object.keys(state.scenario.offers_config.per_gen).length)
    ? ` · ${Object.keys(state.scenario.offers_config.per_gen).length} override(s)`
    : '';
  $('offers-hint').textContent = synth === 'from_cost_coeffs'
    ? `${withCost} / ${gens.length} gens have cost coeffs; remainder falls back to flat 3-tier${suffix}.`
    : `Flat tiers ($20 / $40 / $80) at 33% / 67% / 100% of Pmax${suffix}.`;
}

// ── Case loading ──────────────────────────────────────────────────

async function loadCases() {
  const res = await fetch('api/meta');
  state.meta = await res.json();
  const sel = $('sel-case');
  sel.innerHTML = state.meta.cases.map(c =>
    `<option value="${escapeHtml(c.id)}">${escapeHtml(c.title)} — ${escapeHtml(c.family)}/${escapeHtml(c.size)}</option>`
  ).join('') + '<option value="__upload__">Upload network file…</option>';
}

async function loadScaffold(caseId) {
  $('solve-status').textContent = 'loading case…';
  $('solve-status').className = 'solve-status busy';
  try {
    const res = await fetch(`api/cases/${encodeURIComponent(caseId)}/scaffold`);
    if (!res.ok) throw new Error((await res.json()).detail || res.statusText);
    const scen = await res.json();
    state.scenario = scen;
    state.lastResult = null;
    writeForm();
    clearResultsUI();
    $('solve-status').textContent = 'ready';
    $('solve-status').className = 'solve-status';
  } catch (err) {
    showError(err.message);
    $('solve-status').textContent = 'error';
    $('solve-status').className = 'solve-status err';
  }
}

// ── Solve orchestration ───────────────────────────────────────────

async function solve() {
  if (state.solving || !state.scenario) return;
  readForm();
  state.solving = true;
  $('btn-solve').disabled = true;
  $('solve-status').textContent = 'solving…';
  $('solve-status').className = 'solve-status busy';
  clearError();
  const t0 = performance.now();
  try {
    const res = await fetch('api/solve', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(state.scenario),
    });
    const body = await res.json().catch(() => ({ detail: 'invalid JSON' }));
    if (!res.ok) throw new Error(body.detail || res.statusText);
    if (body.status !== 'ok') throw new Error(body.error || `solver: ${body.status}`);
    state.lastResult = body;
    const elapsed = ((performance.now() - t0) / 1000).toFixed(2);
    $('solve-status').textContent = `solved · ${elapsed}s`;
    $('solve-status').className = 'solve-status ok';
    renderAllPanes();
  } catch (err) {
    $('solve-status').textContent = 'error';
    $('solve-status').className = 'solve-status err';
    showError(err.message);
  } finally {
    state.solving = false;
    $('btn-solve').disabled = false;
  }
}

function showError(msg) {
  const b = $('error-banner');
  $('error-banner-text').textContent = msg;
  b.classList.add('visible');
}
function clearError() { $('error-banner').classList.remove('visible'); }

// ── Results rendering ─────────────────────────────────────────────

function clearResultsUI() {
  $('prod-cost').textContent = '—';
  $('prod-cost-sub').textContent = '';
  ['metric-production-cost','metric-energy-payment','metric-load-payment','metric-congestion',
   'metric-as-payment','metric-shortfall','metric-mean-lmp','metric-peak-lmp'].forEach(id => {
    const el = $(id); if (el) el.textContent = '—';
  });
  ['summary-lmp-chart','lmp-heatmap','lmp-lines','gen-stack','gen-table','load-chart','load-table',
   'reserves-grid','violations-body','grid-canvas','grid-legend'].forEach(id => {
    const el = $(id); if (el) el.innerHTML = '';
  });
  const runLog = $('run-log');
  if (runLog) runLog.textContent = '—';
}

function renderAllPanes() {
  const r = state.lastResult;
  if (!r) return;
  renderSummary(r);
  // Render all panes so tab switches are instant.
  renderGrid(r);
  renderLmps(r);
  renderGenerators(r);
  renderLoads(r);
  renderReserves(r);
  renderViolations(r);
  renderRunLog(r);
}

function renderSummary(r) {
  const s = r.summary || {};
  const money = (v) => fmtMoney(v);
  $('metric-production-cost').textContent = money(s.production_cost_dollars);
  $('metric-energy-payment').textContent = money(s.energy_payment_dollars);
  $('metric-load-payment').textContent = money(s.load_payment_dollars);
  const cong = $('metric-congestion');
  cong.textContent = money(s.congestion_rent_dollars);
  cong.classList.toggle('pos', (s.congestion_rent_dollars || 0) > 1);
  cong.classList.toggle('dim', Math.abs(s.congestion_rent_dollars || 0) < 1);
  $('metric-as-payment').textContent = money(s.as_payment_dollars);
  const short = $('metric-shortfall');
  short.textContent = money(-Math.abs(s.shortfall_penalty_dollars || 0));
  short.classList.toggle('neg', (s.shortfall_penalty_dollars || 0) > 1);
  short.classList.toggle('dim', (s.shortfall_penalty_dollars || 0) <= 1);
  $('metric-mean-lmp').textContent = fmtPrice(s.mean_system_lmp);
  $('metric-peak-lmp').textContent = fmtPrice(s.peak_system_lmp);

  $('prod-cost').textContent = money(s.production_cost_dollars);
  $('prod-cost-sub').textContent = `${r.periods}p · LMP $${(s.mean_system_lmp || 0).toFixed(1)} avg`;

  // System-LMP aggregate lines (min / mean / peak per period)
  const agg = r.lmp_aggregates || {};
  renderLineChart($('summary-lmp-chart'), [
    { name: 'min', color: '#64748b', data: agg.per_period_min || [], unit: '$/MWh' },
    { name: 'mean', color: 'var(--purple)', data: agg.per_period_mean || [], unit: '$/MWh', strokeWidth: 2.4 },
    { name: 'peak', color: 'var(--amber)', data: agg.per_period_peak || [], unit: '$/MWh' },
  ], { tooltipPrecision: 2 });
}

// ── Grid topology ─────────────────────────────────────────────────

function renderGrid(r) {
  const canvas = $('grid-canvas');
  const topo = state.scenario?.topology;
  if (!topo || !topo.buses?.length) {
    canvas.innerHTML = '<div class="viol-empty">no topology</div>';
    return;
  }
  const slider = $('grid-period');
  const n = r.periods;
  slider.max = Math.max(0, n - 1);
  let period = Math.min(parseInt(slider.value, 10) || 0, n - 1);
  if (period < 0) period = 0;
  slider.value = period;
  $('grid-period-label').textContent = periodTimeLabel(period);

  // Collect LMPs for the selected period.
  const lmpsByBus = r.lmps_by_bus || {};
  const lmps = topo.buses.map(b => {
    const arr = lmpsByBus[String(b.number)];
    return arr ? arr[period] : null;
  });
  const valid = lmps.filter(v => v !== null && isFinite(v));
  let mn = valid.length ? Math.min(...valid) : 0;
  let mx = valid.length ? Math.max(...valid) : 1;
  // Symmetric around 0 when LMPs go negative — highlights congestion.
  if (mn > 0) mn = 0;
  if (mx - mn < 1e-6) mx = mn + 1;

  const showBranches = $('grid-show-branches').checked;
  const showGens = $('grid-show-gens').checked;

  // Build a bus→generator map for the gen overlay.
  const gensByBus = new Map();
  (state.scenario.generators || []).forEach(g => {
    const arr = gensByBus.get(g.bus) || [];
    arr.push(g);
    gensByBus.set(g.bus, arr);
  });

  const svgNS = 'http://www.w3.org/2000/svg';
  const W = canvas.clientWidth || 900;
  const H = canvas.clientHeight || 460;
  const pad = 26;
  const x = (u) => pad + u * (W - 2 * pad);
  const y = (v) => pad + (1 - v) * (H - 2 * pad);

  canvas.innerHTML = '';
  const svg = document.createElementNS(svgNS, 'svg');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('preserveAspectRatio', 'none');
  canvas.appendChild(svg);

  const posByBus = new Map(topo.buses.map(b => [b.number, { x: x(b.x), y: y(b.y) }]));

  // Edges — drawn first so bus dots sit on top.
  // Color code:
  //   · gray    — < 75% utilization (or no rating, light flow)
  //   · amber   — 75–95%   near-binding
  //   · orange  — 95–100%  binding
  //   · red     — > 100%   breached / overloaded
  // When a case ships no ratings (most IEEE cases), fall back to a
  // normalized-by-peak-flow grayscale so the heaviest lines still read.
  const flowsThisPeriod = (r.branch_flows_by_period || [])[period] || [];
  const flowByKey = new Map();
  flowsThisPeriod.forEach(f => {
    flowByKey.set(`${f.from}→${f.to}`, f);
    flowByKey.set(`${f.to}→${f.from}`, f); // topology edges may be reversed
  });
  const anyRated = flowsThisPeriod.some(f => f.utilization !== null && f.utilization !== undefined);
  const peakFlowAbs = flowsThisPeriod.reduce(
    (m, f) => Math.max(m, Math.abs(f.flow_mw || 0)),
    1e-6,
  );
  const edgeStyle = (util, absFlow) => {
    if (util !== null && util !== undefined) {
      if (util > 1.0)   return { stroke: '#ef4444', width: 2.6, opacity: 0.95 }; // breached
      if (util > 0.95)  return { stroke: '#f97316', width: 2.2, opacity: 0.9 };  // binding
      if (util > 0.75)  return { stroke: '#fbbf24', width: 1.6, opacity: 0.85 }; // near-binding
      return { stroke: 'rgba(148,163,184,0.32)', width: 0.9, opacity: 0.55 };
    }
    // Fallback: normalized-by-peak grayscale when no ratings provided.
    const f = Math.min(1, absFlow / peakFlowAbs);
    const alpha = 0.18 + 0.55 * f;
    return { stroke: `rgba(148,163,184,${alpha.toFixed(3)})`, width: 0.7 + f * 0.8, opacity: 1 };
  };

  if (showBranches) {
    topo.branches.forEach(br => {
      const p1 = posByBus.get(br.from);
      const p2 = posByBus.get(br.to);
      if (!p1 || !p2) return;
      const flow = flowByKey.get(`${br.from}→${br.to}`);
      const util = flow ? flow.utilization : null;
      const absFlow = flow ? Math.abs(flow.flow_mw || 0) : 0;
      const st = edgeStyle(util, absFlow);
      const line = document.createElementNS(svgNS, 'line');
      line.setAttribute('class', 'grid-edge');
      line.setAttribute('x1', p1.x); line.setAttribute('y1', p1.y);
      line.setAttribute('x2', p2.x); line.setAttribute('y2', p2.y);
      line.setAttribute('stroke', st.stroke);
      line.setAttribute('stroke-width', st.width);
      line.setAttribute('stroke-opacity', st.opacity);
      // Hover — flow MW, rating, utilization.
      if (flow) {
        line.addEventListener('pointerenter', (ev) => {
          const utilTxt = (util === null || util === undefined)
            ? '—'
            : `${(util * 100).toFixed(0)}%`;
          const rateTxt = (flow.rating_mva && flow.rating_mva > 0)
            ? `${flow.rating_mva.toFixed(0)} MVA`
            : 'no rating';
          const cls = (util !== null && util !== undefined)
            ? (util > 1.0 ? 'breached'
              : util > 0.95 ? 'binding'
              : util > 0.75 ? 'near'
              : 'ok')
            : '—';
          showGridTooltip(ev, `${br.from} → ${br.to}`, [
            `<div class="t-row"><span>Flow</span><span class="val">${flow.flow_mw.toFixed(1)} MW</span></div>`,
            `<div class="t-row"><span>Rating</span><span class="val">${rateTxt}</span></div>`,
            `<div class="t-row"><span>Utilization</span><span class="val">${utilTxt}</span></div>`,
            `<div class="t-row"><span>Status</span><span class="val">${cls}</span></div>`,
          ].join(''));
        });
        line.addEventListener('pointerleave', hideGridTooltip);
      }
      svg.appendChild(line);
    });
  }

  // Bus dots — radius scaled with total bus count so small cases read
  // well and large cases don't explode visually.
  const baseR = Math.max(3.5, Math.min(9, 220 / Math.sqrt(topo.buses.length + 1)));
  topo.buses.forEach((b, i) => {
    const p = posByBus.get(b.number);
    if (!p) return;
    const lmp = lmps[i];
    const color = lmp === null ? '#3f3f46' : lmpColor(lmp, mn, mx);
    const circle = document.createElementNS(svgNS, 'circle');
    circle.setAttribute('class', 'grid-bus');
    circle.setAttribute('cx', p.x);
    circle.setAttribute('cy', p.y);
    circle.setAttribute('r', baseR);
    circle.setAttribute('fill', color);
    circle.dataset.bus = b.number;
    // Inline hover — light and custom to this tab.
    circle.addEventListener('pointerenter', (ev) => {
      const g = gensByBus.get(b.number) || [];
      const rows = [
        `<div class="t-row"><span>Bus</span><span class="val">${b.number}</span></div>`,
        `<div class="t-row"><span>LMP</span><span class="val">${lmp === null ? '—' : `$${lmp.toFixed(2)}/MWh`}</span></div>`,
        g.length ? `<div class="t-row"><span>Gens</span><span class="val">${g.length} · ${g.reduce((s,x)=>s+(x.pmax_mw||0),0).toFixed(0)} MW</span></div>` : '',
      ].join('');
      showGridTooltip(ev, `${periodTimeLabel(period)}`, rows);
    });
    circle.addEventListener('pointerleave', hideGridTooltip);
    svg.appendChild(circle);
    // Small bus-number label when the network is small enough to read them.
    if (topo.buses.length <= 60) {
      const tx = document.createElementNS(svgNS, 'text');
      tx.setAttribute('class', 'grid-bus-label');
      tx.setAttribute('x', p.x);
      tx.setAttribute('y', p.y - baseR - 4);
      tx.textContent = b.number;
      svg.appendChild(tx);
    }
  });

  // Generator pins — tiny white dots inset at each bus that has a gen.
  if (showGens) {
    gensByBus.forEach((gens, busNum) => {
      const p = posByBus.get(busNum);
      if (!p) return;
      const dot = document.createElementNS(svgNS, 'circle');
      dot.setAttribute('class', 'grid-gen-dot');
      dot.setAttribute('cx', p.x + baseR + 1);
      dot.setAttribute('cy', p.y - baseR - 1);
      dot.setAttribute('r', 2);
      svg.appendChild(dot);
    });
  }

  // Legend — min/max LMP swatch + branch utilization legend.
  const periodStats = flowsThisPeriod.filter(f =>
    f.utilization !== null && f.utilization !== undefined);
  const nBreach = periodStats.filter(f => f.utilization > 1.0).length;
  const nBinding = periodStats.filter(f => f.utilization > 0.95 && f.utilization <= 1.0).length;
  const nNear = periodStats.filter(f => f.utilization > 0.75 && f.utilization <= 0.95).length;
  const branchLegend = anyRated
    ? `<span style="margin-left:10px">·</span>
       <span style="color:#fbbf24">▬ 75-95%${nNear ? ` (${nNear})` : ''}</span>
       <span style="color:#f97316">▬ 95-100%${nBinding ? ` (${nBinding})` : ''}</span>
       <span style="color:#ef4444">▬ &gt;100%${nBreach ? ` (${nBreach})` : ''}</span>`
    : `<span style="margin-left:10px; color: var(--text-dim)">no branch ratings — flow magnitude shown in grayscale</span>`;
  $('grid-legend').innerHTML = `
    <span>$${mn.toFixed(0)}</span>
    <div class="grid-legend-bar"></div>
    <span>$${mx.toFixed(0)}</span>
    <span style="margin-left:10px; color: var(--text-dim)">·  LMP at bus</span>
    ${branchLegend}
    ${topo.note ? `<span style="margin-left:auto; color: var(--text-dim)">${escapeHtml(topo.note)}</span>` : ''}
  `;
}

function showGridTooltip(ev, title, rowsHtml) {
  let el = document.querySelector('.grid-grid-tooltip');
  if (!el) {
    el = document.createElement('div');
    el.className = 'grid-grid-tooltip';
    document.body.appendChild(el);
  }
  el.innerHTML = `<div class="t-title">${escapeHtml(title)}</div>${rowsHtml}`;
  el.classList.add('visible');
  el.style.left = `${ev.clientX + 12}px`;
  el.style.top = `${ev.clientY + 12}px`;
}
function hideGridTooltip() {
  const el = document.querySelector('.grid-grid-tooltip');
  if (el) el.classList.remove('visible');
}

// ── LMPs ─────────────────────────────────────────────────────────

function lmpColor(v, minV, maxV) {
  if (v === null || v === undefined || !isFinite(v)) return '#1a1a24';
  // Diverging: negative = deep blue → green near 0 → amber → red high.
  const span = Math.max(1, maxV - Math.min(0, minV));
  const t = Math.max(0, Math.min(1, (v - Math.min(0, minV)) / span));
  // Cheap 3-stop gradient: blue (0) → emerald (0.5) → amber (0.8) → red (1)
  const stops = [
    [0.00, [0x60, 0xa5, 0xfa]],
    [0.33, [0x34, 0xd3, 0x99]],
    [0.66, [0xfb, 0xbf, 0x24]],
    [1.00, [0xef, 0x44, 0x44]],
  ];
  let c0 = stops[0], c1 = stops[stops.length - 1];
  for (let i = 0; i < stops.length - 1; i++) {
    if (t >= stops[i][0] && t <= stops[i + 1][0]) { c0 = stops[i]; c1 = stops[i + 1]; break; }
  }
  const f = (t - c0[0]) / Math.max(1e-9, c1[0] - c0[0]);
  const mix = c0[1].map((v0, i) => Math.round(v0 + (c1[1][i] - v0) * f));
  return `rgb(${mix[0]},${mix[1]},${mix[2]})`;
}

function renderLmps(r) {
  const heatmap = $('lmp-heatmap');
  const n = r.periods;
  const byBus = r.lmps_by_bus || {};
  const buses = Object.keys(byBus).map(k => parseInt(k, 10)).sort((a, b) => a - b);
  if (!buses.length) { heatmap.innerHTML = '<div class="viol-empty">no LMPs</div>'; return; }

  // Filter
  const raw = $('inp-lmp-filter').value.trim();
  let visible = buses;
  if (raw) {
    const want = new Set(raw.split(/[,\s]+/).map(s => parseInt(s, 10)).filter(n => !Number.isNaN(n)));
    visible = buses.filter(b => want.has(b));
    if (!visible.length) visible = buses;
  }

  // Build color scale bounds
  let mn = Infinity, mx = -Infinity;
  visible.forEach(b => byBus[b].forEach(v => {
    if (v < mn) mn = v; if (v > mx) mx = v;
  }));
  if (!isFinite(mn)) { mn = 0; mx = 1; }

  // Heatmap grid: row per bus, col per period. Header row with period indices.
  const W_per_cell = 26;
  const rows = [];
  // Header row
  rows.push(`<div class="lmp-heatmap-bus"></div>`);
  for (let t = 0; t < n; t++) rows.push(`<div class="lmp-heatmap-head">${t}</div>`);
  visible.forEach(b => {
    rows.push(`<div class="lmp-heatmap-bus">bus ${b}</div>`);
    const arr = byBus[b];
    for (let t = 0; t < n; t++) {
      const v = arr[t];
      const color = lmpColor(v, mn, mx);
      rows.push(`<div class="lmp-heatmap-cell" style="background:${color}" title="bus ${b} · ${periodTimeLabel(t)}: $${v.toFixed(2)}/MWh">${v.toFixed(0)}</div>`);
    }
  });
  heatmap.innerHTML = `<div class="lmp-heatmap-grid" style="grid-template-columns: 60px repeat(${n}, ${W_per_cell}px)">${rows.join('')}</div>`;

  // Companion line chart: top-5 buses by variance for context.
  const variance = (arr) => {
    const m = arr.reduce((a, b) => a + b, 0) / arr.length;
    return arr.reduce((a, b) => a + (b - m) ** 2, 0) / arr.length;
  };
  const sortedBuses = [...visible].sort((a, b) => variance(byBus[b]) - variance(byBus[a])).slice(0, 5);
  const palette = ['var(--purple)', 'var(--emerald)', 'var(--amber)', 'var(--blue)', 'var(--rose)'];
  const series = sortedBuses.map((b, i) => ({
    name: `bus ${b}`,
    color: palette[i % palette.length],
    data: byBus[b] || [],
    unit: '$/MWh',
  }));
  renderLineChart($('lmp-lines'), series, { tooltipPrecision: 2 });
}

// ── Generators ───────────────────────────────────────────────────

function renderGenerators(r) {
  const gens = r.generators || [];
  if (!gens.length) {
    $('gen-stack').innerHTML = '<div class="viol-empty">no gens</div>';
    $('gen-table').innerHTML = '';
    return;
  }
  // Sort according to user pref.
  const sortKey = $('sel-gen-sort')?.value || 'revenue';
  const sum = (arr) => arr.reduce((a, b) => a + b, 0);
  const sorted = gens.slice().sort((a, b) => {
    if (sortKey === 'bus') return a.bus - b.bus;
    if (sortKey === 'id') return a.resource_id.localeCompare(b.resource_id);
    if (sortKey === 'mw') return sum(b.power_mw) - sum(a.power_mw);
    return sum(b.revenue_dollars) - sum(a.revenue_dollars);
  });

  // Stacked area of MW dispatched, grouped by fuel when > ~15 gens else per-gen.
  let series;
  if (gens.length > 18) {
    const byFuel = new Map();
    gens.forEach((g, i) => {
      const key = (g.fuel_type || 'unknown').toLowerCase();
      if (!byFuel.has(key)) byFuel.set(key, {
        name: g.fuel_type || 'unknown',
        color: colorForGenerator(g, byFuel.size),
        data: new Array(r.periods).fill(0),
      });
      const agg = byFuel.get(key);
      g.power_mw.forEach((v, i) => agg.data[i] += v || 0);
    });
    series = [...byFuel.values()];
  } else {
    series = gens.map((g, i) => ({
      name: g.resource_id,
      color: colorForGenerator(g, i),
      data: g.power_mw,
    }));
  }
  const stack = $('gen-stack');
  renderStackedArea(stack, series);
  observeResize(stack, () => renderStackedArea(stack, series));

  // Table with per-gen sparkline.
  const parts = [`
    <div class="gen-row head">
      <span>Resource</span><span>Bus</span><span>Pmax</span><span>Dispatch MW</span><span>Σ MWh</span><span>Σ Cost</span><span>Σ Revenue</span>
    </div>
  `];
  sorted.forEach((g, i) => {
    const totalMwh = sum(g.power_mw);
    const totalCost = sum(g.energy_cost_dollars);
    const totalRev = sum(g.revenue_dollars);
    const color = colorForGenerator(g, i);
    parts.push(`
      <div class="gen-row">
        <span class="gen-id"><span class="dot" style="background:${color}"></span>${escapeHtml(g.resource_id)}</span>
        <span>${g.bus}</span>
        <span class="mw">${(g.pmax_mw || 0).toFixed(1)}</span>
        <span class="mini-spark" data-gen="${escapeHtml(g.resource_id)}"></span>
        <span class="mw">${totalMwh.toFixed(1)}</span>
        <span class="cost">${fmtMoney(totalCost)}</span>
        <span class="revenue">${fmtMoney(totalRev)}</span>
      </div>
    `);
  });
  const body = $('gen-table');
  body.innerHTML = parts.join('');
  body.querySelectorAll('.mini-spark').forEach(el => {
    const g = sorted.find(x => x.resource_id === el.dataset.gen);
    const c = colorForGenerator(g, sorted.indexOf(g));
    renderSparkline(el, g.power_mw, c);
  });
}

// ── Loads ────────────────────────────────────────────────────────

function renderLoads(r) {
  const loads = r.loads || [];
  if (!loads.length) {
    $('load-chart').innerHTML = '<div class="viol-empty">no loads</div>';
    $('load-table').innerHTML = '';
    return;
  }
  const palette = GEN_FALLBACK_COLORS;
  const series = loads.map((l, i) => ({
    name: `bus ${l.bus}`,
    color: palette[i % palette.length],
    data: l.served_mw,
  }));
  const el = $('load-chart');
  renderStackedArea(el, series);
  observeResize(el, () => renderStackedArea(el, series));

  const parts = [`
    <div class="load-row head">
      <span>Bus</span><span>Handling</span><span>Σ MWh</span><span>Σ Shed</span><span>Profile</span>
    </div>
  `];
  const sum = (arr) => arr.reduce((a, b) => a + b, 0);
  loads.forEach((l, i) => {
    const tot = sum(l.served_mw);
    const shed = sum(l.shed_mw);
    parts.push(`
      <div class="load-row">
        <span>${l.bus}</span>
        <span>${escapeHtml(l.handling)}</span>
        <span>${tot.toFixed(1)}</span>
        <span>${shed.toFixed(1)}</span>
        <span class="mini-spark" data-load="${l.bus}"></span>
      </div>
    `);
  });
  const tb = $('load-table');
  tb.innerHTML = parts.join('');
  tb.querySelectorAll('.mini-spark').forEach(el => {
    const l = loads.find(x => String(x.bus) === el.dataset.load);
    renderSparkline(el, l.served_mw, palette[loads.indexOf(l) % palette.length]);
  });
}

// ── Reserves ─────────────────────────────────────────────────────

function renderReserves(r) {
  const grid = $('reserves-grid');
  const awards = r.reserve_awards || [];
  if (!awards.length) {
    grid.innerHTML = '<div class="viol-empty">no reserve products cleared</div>';
    return;
  }
  const sum = (arr) => arr.reduce((a, b) => a + b, 0);
  const labels = { reg_up: 'Regulation up', reg_down: 'Regulation down', syn: 'Spinning', nsyn: 'Non-spin' };
  grid.innerHTML = awards.map(a => {
    const color = RESERVE_COLORS[a.product_id] || 'var(--purple)';
    return `
      <div class="reserve-card">
        <div class="reserve-card-head">
          <div class="reserve-card-title">
            <span class="dot" style="background:${color}"></span>
            ${escapeHtml(labels[a.product_id] || a.product_id)}
          </div>
          <span class="panel-sub">zone ${a.zone_id}</span>
        </div>
        <div class="reserve-card-stats">
          <div><div class="stat-label">Σ req MW</div><div class="stat-value">${sum(a.requirement_mw).toFixed(1)}</div></div>
          <div><div class="stat-label">Σ provided</div><div class="stat-value">${sum(a.provided_mw).toFixed(1)}</div></div>
          <div><div class="stat-label">Σ shortfall</div><div class="stat-value">${sum(a.shortfall_mw).toFixed(1)}</div></div>
          <div><div class="stat-label">Mean price</div><div class="stat-value">${fmtPrice(a.clearing_price.reduce((s,v)=>s+v,0) / Math.max(1,a.clearing_price.length))}</div></div>
          <div><div class="stat-label">Σ payment</div><div class="stat-value">${fmtMoney(sum(a.payment_dollars))}</div></div>
          <div><div class="stat-label">Periods</div><div class="stat-value">${a.requirement_mw.length}</div></div>
        </div>
        <div class="reserve-card-chart" data-pid="${escapeHtml(a.product_id)}"></div>
      </div>
    `;
  }).join('');
  grid.querySelectorAll('.reserve-card-chart').forEach(el => {
    const a = awards.find(x => x.product_id === el.dataset.pid);
    const color = RESERVE_COLORS[a.product_id] || 'var(--purple)';
    const seriesSpec = [
      { name: 'Required', color: 'rgba(148,163,184,0.7)', data: a.requirement_mw, unit: 'MW', dashArray: '3 2' },
      { name: 'Provided', color, data: a.provided_mw, unit: 'MW', strokeWidth: 2.4 },
      { name: 'Clearing price', color: 'var(--emerald)', data: a.clearing_price, unit: '$/MWh', dashArray: '5 3' },
    ];
    renderLineChart(el, seriesSpec, { tooltipPrecision: 2 });
    observeResize(el, () => renderLineChart(el, seriesSpec, { tooltipPrecision: 2 }));
  });
}

// ── Run log ──────────────────────────────────────────────────────

function renderRunLog(r) {
  const el = $('run-log');
  if (!el) return;
  const text = (r && r.solve_log) || '';
  if (!text.trim()) {
    el.textContent = '— no log output captured —';
    return;
  }
  // Light-touch colorization by level token.
  const lines = text.split('\n').map(line => {
    const safe = escapeHtml(line);
    if (/\bERROR\b/.test(line)) return `<span class="log-error">${safe}</span>`;
    if (/\bWARN(ING)?\b/.test(line)) return `<span class="log-warn">${safe}</span>`;
    if (/\bINFO\b/.test(line)) return `<span class="log-info">${safe}</span>`;
    if (/\bDEBUG\b/.test(line)) return `<span class="log-debug">${safe}</span>`;
    return safe;
  });
  el.innerHTML = lines.join('\n');
  el.scrollTop = el.scrollHeight;
}

// ── Violations ───────────────────────────────────────────────────

function renderViolations(r) {
  const body = $('violations-body');
  const viols = r.violations || [];
  if (!viols.length) {
    body.innerHTML = '<div class="viol-empty">✓ No thermal overloads, load shed, or over-generation.</div>';
    return;
  }
  const parts = [`
    <div class="viol-row head">
      <span>Period</span><span>Kind</span><span>Element</span><span>MW</span>
    </div>
  `];
  viols.forEach(v => {
    parts.push(`
      <div class="viol-row">
        <span>${v.period}</span>
        <span class="kind-${escapeHtml(v.kind)}">${escapeHtml(v.kind)}</span>
        <span>${escapeHtml(v.element)}</span>
        <span class="severity">${v.severity_mw.toFixed(2)}</span>
      </div>
    `);
  });
  body.innerHTML = parts.join('');
}

// ── CSV uploads ──────────────────────────────────────────────────

function parseCsvText(text) {
  const lines = text.replace(/\r\n/g, '\n').split('\n').filter(l => l.trim().length);
  return lines.map(line => {
    // Simple split; full RFC-4180 parser not required for this MVP.
    return line.split(',').map(c => c.trim().replace(/^"|"$/g, ''));
  });
}

function setCsvStatus(id, cls, msg) {
  const el = $(id);
  if (!el) return;
  el.className = 'csv-status' + (cls ? ' ' + cls : '');
  el.textContent = msg || '';
}

async function handleLoadCsv(file) {
  if (!file) return;
  try {
    const text = await file.text();
    const rows = parseCsvText(text);
    if (rows.length < 2) throw new Error('need a header row plus data');
    const header = rows[0].map(h => h.toLowerCase());
    const lc = state.scenario.load_config;
    // Two shapes: wide (bus, v0, v1, …) or long (bus, period, value_mw).
    if (header.includes('period') && header.includes('value_mw')) {
      const busCol = header.indexOf('bus_number') >= 0 ? header.indexOf('bus_number') : header.indexOf('bus');
      const perCol = header.indexOf('period');
      const valCol = header.indexOf('value_mw');
      const byBus = {};
      rows.slice(1).forEach(r => {
        const bus = parseInt(r[busCol], 10);
        const t = parseInt(r[perCol], 10);
        const v = parseFloat(r[valCol]);
        if (!Number.isNaN(bus) && !Number.isNaN(t) && !Number.isNaN(v)) {
          (byBus[bus] = byBus[bus] || []);
          byBus[bus][t] = v;
        }
      });
      // Normalise — promote to per-bus custom profile multipliers.
      const nomByBus = Object.fromEntries((state.scenario.loads || []).map(l => [l.bus, l.nominal_mw]));
      const perBus = lc.per_bus || (lc.per_bus = {});
      let npBuses = 0;
      Object.entries(byBus).forEach(([b, arr]) => {
        const nom = nomByBus[b] || 1;
        perBus[String(b)] = perBus[String(b)] || {};
        perBus[String(b)].profile_shape = 'custom';
        perBus[String(b)].custom_profile = arr.map(v => (v || 0) / (nom || 1));
        npBuses++;
      });
      setCsvStatus('load-csv-status', 'ok', `✓ ${file.name}: ${npBuses} bus profile(s)`);
    } else {
      // Wide: first column "bus", rest are per-period MW.
      const busCol = header.indexOf('bus') >= 0 ? header.indexOf('bus') : 0;
      const nomByBus = Object.fromEntries((state.scenario.loads || []).map(l => [l.bus, l.nominal_mw]));
      const perBus = lc.per_bus || (lc.per_bus = {});
      let np = 0;
      rows.slice(1).forEach(r => {
        const bus = parseInt(r[busCol], 10);
        if (Number.isNaN(bus)) return;
        const vals = r.filter((_, i) => i !== busCol).map(v => parseFloat(v)).filter(v => !Number.isNaN(v));
        if (!vals.length) return;
        const nom = nomByBus[bus] || 1;
        perBus[String(bus)] = perBus[String(bus)] || {};
        perBus[String(bus)].profile_shape = 'custom';
        perBus[String(bus)].custom_profile = vals.map(v => v / (nom || 1));
        np++;
      });
      setCsvStatus('load-csv-status', 'ok', `✓ ${file.name}: ${np} bus profile(s)`);
    }
    $('btn-load-csv-clear').hidden = false;
  } catch (err) {
    setCsvStatus('load-csv-status', 'err', '✗ ' + err.message);
  }
}

function clearLoadCsv() {
  const lc = state.scenario.load_config;
  if (lc?.per_bus) {
    Object.keys(lc.per_bus).forEach(b => {
      if (lc.per_bus[b].profile_shape === 'custom') delete lc.per_bus[b];
    });
  }
  setCsvStatus('load-csv-status', '', 'cleared');
  $('btn-load-csv-clear').hidden = true;
  $('inp-load-csv').value = '';
}

async function handleOffersCsv(file) {
  if (!file) return;
  try {
    const text = await file.text();
    const rows = parseCsvText(text);
    if (rows.length < 2) throw new Error('need a header row plus data');
    const header = rows[0].map(h => h.toLowerCase());
    const rid = header.indexOf('resource_id');
    if (rid < 0) throw new Error('missing resource_id column');
    // Remaining columns are mw_1,price_1,mw_2,price_2,…
    const per = state.scenario.offers_config.per_gen || (state.scenario.offers_config.per_gen = {});
    let n = 0;
    rows.slice(1).forEach(r => {
      const id = r[rid];
      if (!id) return;
      const pairs = [];
      for (let c = 0; c < r.length; c += 2) {
        if (c === rid) continue;
        const mw = parseFloat(r[c]);
        const price = parseFloat(r[c + 1]);
        if (!Number.isNaN(mw) && !Number.isNaN(price)) pairs.push([mw, price]);
      }
      if (pairs.length) { per[id] = pairs; n++; }
    });
    setCsvStatus('offers-csv-status', 'ok', `✓ ${file.name}: ${n} gen curve(s)`);
    $('btn-offers-csv-clear').hidden = false;
    renderOffersHint();
  } catch (err) {
    setCsvStatus('offers-csv-status', 'err', '✗ ' + err.message);
  }
}

function clearOffersCsv() {
  state.scenario.offers_config.per_gen = {};
  setCsvStatus('offers-csv-status', '', 'cleared');
  $('btn-offers-csv-clear').hidden = true;
  renderOffersHint();
}

// ── Scenario save/load ────────────────────────────────────────────

function saveScenarioJson() {
  readForm();
  const copy = JSON.parse(JSON.stringify(state.scenario));
  // Topology is deterministic per network — recomputed on load from the
  // case id, so no need to bake the positions + edges into the file.
  delete copy.topology;
  const caseId = copy.source?.case_id || 'scenario';
  const blob = new Blob([JSON.stringify(copy, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `rto-${caseId}-${Date.now()}.json`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

async function loadScenarioFile(file) {
  if (!file) return;
  try {
    const text = await file.text();
    const scen = JSON.parse(text);
    if (!scen.source || !scen.time_axis || !scen.load_config) {
      throw new Error('file does not look like an RTO scenario');
    }
    const caseId = scen.source.case_id;
    // Back-fill the structural parts (topology, network_summary,
    // generators, loads) by re-scaffolding off the referenced case —
    // saved JSON intentionally omits them since they're deterministic
    // per network.
    if (caseId && state.meta?.cases?.some(c => c.id === caseId)) {
      $('sel-case').value = caseId;
      try {
        const res = await fetch(`api/cases/${encodeURIComponent(caseId)}/scaffold`);
        if (res.ok) {
          const fresh = await res.json();
          if (!scen.topology) scen.topology = fresh.topology;
          if (!scen.network_summary) scen.network_summary = fresh.network_summary;
          if (!scen.generators || !scen.generators.length) scen.generators = fresh.generators;
          if (!scen.loads || !scen.loads.length) scen.loads = fresh.loads;
        }
      } catch (_) { /* continue with what we have */ }
    }
    state.scenario = scen;
    writeForm();
    clearResultsUI();
    $('solve-status').textContent = 'scenario loaded';
    $('solve-status').className = 'solve-status';
  } catch (err) {
    showError('load failed: ' + err.message);
  }
}

// ── Network upload ────────────────────────────────────────────────

async function uploadNetwork(file) {
  if (!file) return;
  try {
    const text = await file.text();
    const body = { title: file.name, payload: text };
    const res = await fetch('api/upload-network', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error((await res.json()).detail || res.statusText);
    const scen = await res.json();
    state.scenario = scen;
    writeForm();
    clearResultsUI();
    $('solve-status').textContent = 'uploaded';
    $('solve-status').className = 'solve-status';
  } catch (err) {
    showError('upload failed: ' + err.message);
  }
}

// ── Tabs ──────────────────────────────────────────────────────────

function activateTab(tab) {
  state.activeTab = tab;
  document.querySelectorAll('.results-tab').forEach(el =>
    el.classList.toggle('active', el.dataset.tab === tab));
  document.querySelectorAll('.results-pane').forEach(el =>
    el.hidden = el.dataset.pane !== tab);
  if (state.lastResult) {
    // Re-render the active pane on switch so ResizeObserver picks up
    // the pane's now-visible dimensions.
    const r = state.lastResult;
    if (tab === 'summary') renderSummary(r);
    else if (tab === 'grid') renderGrid(r);
    else if (tab === 'lmps') renderLmps(r);
    else if (tab === 'generators') renderGenerators(r);
    else if (tab === 'loads') renderLoads(r);
    else if (tab === 'reserves') renderReserves(r);
    else if (tab === 'violations') renderViolations(r);
    else if (tab === 'log') renderRunLog(r);
  }
}

// ── Init ──────────────────────────────────────────────────────────

async function init() {
  try {
    await loadCases();
  } catch (err) {
    document.body.innerHTML = `<div style="padding:2rem;color:#f87171">Failed to load dashboard: ${err.message}</div>`;
    return;
  }
  // Start on the first available case.
  const firstCase = state.meta.cases[0]?.id;
  if (firstCase) await loadScaffold(firstCase);

  // Sidebar
  const toggle = $('sidebar-toggle');
  const shell = document.querySelector('.shell');
  toggle.addEventListener('click', () => shell.classList.toggle('sidebar-collapsed'));

  // Case selector
  $('sel-case').addEventListener('change', async (e) => {
    const v = e.target.value;
    if (v === '__upload__') {
      const tmp = document.createElement('input');
      tmp.type = 'file';
      tmp.accept = '.json,.zst,.m';
      tmp.addEventListener('change', (ev) => uploadNetwork(ev.target.files[0]));
      tmp.click();
      // Reset dropdown to current value.
      e.target.value = state.scenario?.source?.case_id || state.meta.cases[0]?.id || '';
      return;
    }
    await loadScaffold(v);
  });

  // Solve
  $('btn-solve').addEventListener('click', solve);

  // Error banner
  $('error-banner-close').addEventListener('click', clearError);

  // Live form changes → readForm
  const bindChange = (ids) => ids.forEach(id => {
    const el = $(id);
    if (el) el.addEventListener('change', readForm);
  });
  bindChange([
    'inp-start', 'inp-periods', 'sel-resolution',
    'sel-load-handling', 'sel-load-profile', 'inp-voll',
    'sel-offers-synthesis', 'sel-renewables-profile',
    'sel-commitment', 'sel-lp-solver', 'inp-mip-gap', 'sel-run-pricing',
    'inp-voll-penalty', 'inp-thermal', 'inp-reserve-short', 'inp-time-limit',
  ]);
  $('sel-load-handling').addEventListener('change', toggleVollRow);
  $('sel-offers-synthesis').addEventListener('change', renderOffersHint);

  // Tabs
  document.querySelectorAll('.results-tab').forEach(el => {
    el.addEventListener('click', () => activateTab(el.dataset.tab));
  });

  // Copy run log to clipboard
  $('btn-log-copy').addEventListener('click', async () => {
    const text = (state.lastResult && state.lastResult.solve_log) || '';
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      const btn = $('btn-log-copy');
      const prev = btn.textContent;
      btn.textContent = 'copied';
      setTimeout(() => { btn.textContent = prev; }, 1200);
    } catch (_) { /* ignore */ }
  });

  // Grid period slider + toggles
  const rerenderGrid = () => {
    if (state.lastResult && state.activeTab === 'grid') renderGrid(state.lastResult);
  };
  $('grid-period').addEventListener('input', rerenderGrid);
  $('grid-show-branches').addEventListener('change', rerenderGrid);
  $('grid-show-gens').addEventListener('change', rerenderGrid);

  // Heatmap filter live
  $('inp-lmp-filter').addEventListener('input', () => {
    if (state.activeTab === 'lmps' && state.lastResult) renderLmps(state.lastResult);
  });
  $('sel-gen-sort').addEventListener('change', () => {
    if (state.lastResult) renderGenerators(state.lastResult);
  });

  // CSV uploads
  $('btn-load-csv').addEventListener('click', () => $('inp-load-csv').click());
  $('inp-load-csv').addEventListener('change', (e) => {
    const f = e.target.files?.[0];
    if (f) handleLoadCsv(f);
    e.target.value = '';
  });
  $('btn-load-csv-clear').addEventListener('click', clearLoadCsv);
  $('btn-offers-csv').addEventListener('click', () => $('inp-offers-csv').click());
  $('inp-offers-csv').addEventListener('change', (e) => {
    const f = e.target.files?.[0];
    if (f) handleOffersCsv(f);
    e.target.value = '';
  });
  $('btn-offers-csv-clear').addEventListener('click', clearOffersCsv);

  // Scenario save / load
  $('btn-save-scenario').addEventListener('click', saveScenarioJson);
  $('btn-load-scenario').addEventListener('click', () => $('inp-load-scenario').click());
  $('inp-load-scenario').addEventListener('change', (e) => {
    const f = e.target.files?.[0];
    if (f) loadScenarioFile(f);
    e.target.value = '';
  });

  // First solve when the dashboard comes up — gives the user something to look at.
  solve();
}

init();
