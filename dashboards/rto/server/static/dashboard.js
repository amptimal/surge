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

// Cumulative hours from period 0 start. ``bounds[0] = 0``,
// ``bounds[n] = total horizon hours``. Falls back to uniform 1-hour
// periods when the result hasn't shipped per-period durations yet.
function periodBoundsHours(durations, n) {
  const bounds = [0];
  for (let i = 0; i < n; i++) {
    const d = (durations && i < durations.length) ? Number(durations[i]) || 1.0 : 1.0;
    bounds.push(bounds[i] + d);
  }
  return bounds;
}

// Resolve the time axis from the active scenario / result. Variable-
// interval cases (goc3_73 ships 18 × 0.25 h then steps to longer
// blocks) honor their actual ``period_durations_hours``; the
// uniform fallback handles fresh scaffolds before a solve completes.
function resolveTimeAxis(r) {
  const periods = r?.periods ?? state.scenario?.time_axis?.periods ?? 1;
  const durations =
    r?.period_durations_hours ??
    (state.scenario?.time_axis?.resolution_minutes
      ? Array(periods).fill(state.scenario.time_axis.resolution_minutes / 60)
      : null);
  const bounds = periodBoundsHours(durations, periods);
  const startIso = state.scenario?.time_axis?.start_iso;
  const start = startIso ? new Date(startIso) : null;
  const startMs = (start && !Number.isNaN(start.getTime())) ? start.getTime() : null;
  return { periods, durations, bounds, startMs };
}

function _formatHHMM(ms) {
  const dt = new Date(ms);
  const pad = (v) => String(v).padStart(2, '0');
  return `${pad(dt.getHours())}:${pad(dt.getMinutes())}`;
}

// Wall-clock time at the *start* of period i (block left edge).
function periodTimeLabel(i, axis) {
  axis = axis || resolveTimeAxis(state.lastResult);
  const offsetHours = axis.bounds[Math.max(0, Math.min(axis.periods, i))] || 0;
  if (axis.startMs == null) {
    // No wall-clock anchor — show offset hours since start.
    const h = Math.floor(offsetHours);
    const m = Math.round((offsetHours - h) * 60);
    return `P${i} · +${h}h${m ? ':' + String(m).padStart(2, '0') : ''}`;
  }
  return `P${i} · ${_formatHHMM(axis.startMs + offsetHours * 3600000)}`;
}

// Pick ~``maxTicks`` axis labels evenly spaced through the horizon
// at canonical wall-clock instants (00:00, 06:00, 12:00, 18:00 …).
// Returns an array of ``{ offsetHours, label }`` for the chart's
// x-axis layer to render at the matching x-coordinate.
function timeAxisTicks(axis, maxTicks = 8) {
  const total = axis.bounds[axis.periods] || 1;
  if (axis.startMs == null) {
    // Fallback: show period indices at evenly-spaced sample points.
    const step = Math.max(1, Math.ceil(axis.periods / maxTicks));
    const ticks = [];
    for (let i = 0; i < axis.periods; i += step) {
      ticks.push({ offsetHours: axis.bounds[i] || 0, label: `P${i}` });
    }
    return ticks;
  }
  // Canonical wall-clock spacings in hours; pick the largest that
  // still gives at least 4 ticks across the horizon.
  const candidates = [1, 2, 3, 4, 6, 8, 12, 24];
  let stepH = candidates.find(h => total / h <= maxTicks && total / h >= 3) || 6;
  if (total / stepH < 2) stepH = Math.max(1, total / maxTicks);
  // Snap to canonical times: round the start offset up to the next
  // multiple of stepH wall-clock hour.
  const startMs = axis.startMs;
  const startHourOfDay = new Date(startMs).getHours() + new Date(startMs).getMinutes() / 60;
  const firstTickOffset = (Math.ceil(startHourOfDay / stepH) * stepH) - startHourOfDay;
  const ticks = [];
  for (let oh = firstTickOffset; oh <= total + 1e-6; oh += stepH) {
    if (oh < -1e-6) continue;
    ticks.push({ offsetHours: oh, label: _formatHHMM(startMs + oh * 3600000) });
  }
  // Always include the final boundary so the user sees the horizon end.
  if (!ticks.length || Math.abs(ticks[ticks.length - 1].offsetHours - total) > 1e-6) {
    ticks.push({ offsetHours: total, label: _formatHHMM(startMs + total * 3600000) });
  }
  return ticks;
}

// ── Chart hover + crosshair (shared by time-series panes) ─────────

function installHover(container, svg, geom, tooltipContent) {
  const { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W, axis } = geom;
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
  // Variable-interval support: map svgX → period via cumulative bounds.
  // ``axis.bounds[i]`` is the cumulative hour offset of period i's left
  // edge; period i spans pixels [boundsToX(i), boundsToX(i+1)].
  const totalH = axis ? (axis.bounds[axis.periods] || n) : n;
  const bounds = axis
    ? axis.bounds.map(h => PAD_L + (h / totalH) * innerW)
    : Array.from({ length: n + 1 }, (_, i) => PAD_L + (i / n) * innerW);
  const periodAt = (svgX) => {
    // Linear scan is fine for typical horizons (<= 48 periods).
    for (let i = 0; i < n; i++) {
      if (svgX < bounds[i + 1]) return i;
    }
    return n - 1;
  };
  const hide = () => { tooltip.classList.remove('visible'); crosshair.style.display = 'none'; };
  svg.addEventListener('pointermove', (ev) => {
    const rect = svg.getBoundingClientRect();
    const scaleX = svg.viewBox.baseVal.width / rect.width;
    const svgX = (ev.clientX - rect.left) * scaleX;
    if (svgX < PAD_L || svgX > W - PAD_R) { hide(); return; }
    const i = Math.max(0, Math.min(n - 1, periodAt(svgX)));
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
    const centerX = (bounds[i] + bounds[i + 1]) / 2;
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
  // Fit y-range to the actual data with 8% headroom — don't force yMin ≤ 0
  // (loads / LMPs / etc. that never reach 0 should fill the chart, not
  // collapse to the top half).
  const range = rawMax - rawMin;
  const pad = range > 0 ? range * 0.08 : Math.max(1, Math.abs(rawMax) * 0.08);
  const yMin = opts.yMin !== undefined ? opts.yMin : (rawMin - pad);
  const yMax = opts.yMax !== undefined ? opts.yMax : (rawMax + pad);
  const ySpan = (yMax - yMin) || 1;
  // Period blocks: period i spans [xL[i], xR[i]] proportional to its
  // duration in hours, so variable-interval horizons (goc3_73 ships
  // 18 × 0.25 h, goc3_2000 mixes 0.25 h / 1 h / 4 h) lay out with the
  // right widths. Time axis comes from state.lastResult /
  // state.scenario when available.
  const axis = opts.timeAxis || resolveTimeAxis(state.lastResult);
  const totalH = axis.bounds[axis.periods] || n;
  const hourToX = (h) => PAD_L + (h / totalH) * innerW;
  const xL = (i) => hourToX(axis.bounds[i] || 0);
  const xR = (i) => hourToX(axis.bounds[i + 1] || (axis.bounds[i] || 0) + 1);
  const toX = (i) => (xL(i) + xR(i)) / 2; // block center
  const toY = (v) => PAD_T + innerH - ((v - yMin) / ySpan) * innerH;

  const svgNS = 'http://www.w3.org/2000/svg';
  container.innerHTML = '';
  const svg = document.createElementNS(svgNS, 'svg');
  svg.setAttribute('class', 'sc-chart');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('preserveAspectRatio', 'none');

  // grid + y labels — pick decimals from the data range so sub-
  // dollar series (Q-LMPs in $/MVAr-h, often 0.01–2.0) don't all
  // round to "0.0".
  const yLabelDecimals = (() => {
    if (ySpan >= 50) return 0;
    if (ySpan >= 5) return 1;
    if (ySpan >= 0.5) return 2;
    if (ySpan >= 0.05) return 3;
    return 4;
  })();
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
    t.textContent = v.toFixed(yLabelDecimals);
    svg.appendChild(t);
  }
  // x labels — wall-clock times at canonical 1/2/3/4/6/8/12/24-hour
  // spacings (or P-indices when no start_iso is set).
  const xtTicks = timeAxisTicks(axis, 8);
  for (const tick of xtTicks) {
    const t = document.createElementNS(svgNS, 'text');
    t.setAttribute('class', 'sc-axis-label');
    t.setAttribute('x', hourToX(tick.offsetHours));
    t.setAttribute('y', PAD_T + innerH + 13);
    t.setAttribute('text-anchor', 'middle');
    t.textContent = tick.label;
    svg.appendChild(t);
  }

  series.forEach(s => {
    // Stepped (rectangular block) path: hold flat across each period
    // [xL[i], xR[i]] at toY(V[i]), step vertically at the boundary.
    const points = [];
    for (let i = 0; i < n; i++) {
      const v = s.data[i];
      if (v === null || v === undefined || !isFinite(v)) continue;
      points.push(`${xL(i)},${toY(v)}`);
      points.push(`${xR(i)},${toY(v)}`);
    }
    const poly = document.createElementNS(svgNS, 'polyline');
    poly.setAttribute('fill', 'none');
    poly.setAttribute('stroke', s.color);
    poly.setAttribute('stroke-width', s.strokeWidth ?? 1.8);
    poly.setAttribute('stroke-linejoin', 'miter');
    if (s.dashArray) poly.setAttribute('stroke-dasharray', s.dashArray);
    poly.setAttribute('points', points.join(' '));
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
  installHover(container, svg, { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W, axis }, (i) => {
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
  // Fit y-range to the actual rendered shape. For active dispatch
  // (all-positive series) the cumulative top monotonically rises so
  // the final segment's hi is the bound — but reactive dispatch
  // mixes positive and negative contributions, and the cumulative
  // can swing above / below the final-period total inside the
  // horizon. Walk every segment's lo + hi to capture the true
  // peaks and dips.
  let topMax = -Infinity;
  let topMin = Infinity;
  for (const seg of segments) {
    for (let i = 0; i < n; i++) {
      const lo = seg.lo[i] || 0;
      const hi = seg.hi[i] || 0;
      if (lo > topMax) topMax = lo;
      if (hi > topMax) topMax = hi;
      if (lo < topMin) topMin = lo;
      if (hi < topMin) topMin = hi;
    }
  }
  if (!isFinite(topMax)) topMax = 0;
  if (!isFinite(topMin)) topMin = 0;
  const range = topMax - topMin;
  const pad = range > 0 ? range * 0.08 : Math.max(1, Math.abs(topMax) * 0.08);
  const yMin = opts.yMin ?? (topMin - pad);
  const yMax = opts.yMax ?? (topMax + pad);
  const ySpan = (yMax - yMin) || 1;
  // Period blocks proportional to actual duration so variable-
  // interval horizons (goc3_73 = 18 × 0.25 h, goc3_2000 mixes
  // 0.25 / 1 / 4 h) lay out with the right widths.
  const axis = opts.timeAxis || resolveTimeAxis(state.lastResult);
  const totalH = axis.bounds[axis.periods] || n;
  const hourToX = (h) => PAD_L + (h / totalH) * innerW;
  const xL = (i) => hourToX(axis.bounds[i] || 0);
  const xR = (i) => hourToX(axis.bounds[i + 1] || (axis.bounds[i] || 0) + 1);
  const toX = (i) => (xL(i) + xR(i)) / 2;
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
    // Stepped (rectangular block) polygon: each period holds flat
    // across [xL[i], xR[i]] at hi[i] / lo[i]. Top edge runs L→R as
    // a step function, bottom edge runs R→L the same way, and the
    // polygon closes with a vertical jump at each end.
    const up = [];
    for (let i = 0; i < n; i++) {
      up.push(`${xL(i)},${toY(s.hi[i])}`);
      up.push(`${xR(i)},${toY(s.hi[i])}`);
    }
    const down = [];
    for (let i = n - 1; i >= 0; i--) {
      down.push(`${xR(i)},${toY(s.lo[i])}`);
      down.push(`${xL(i)},${toY(s.lo[i])}`);
    }
    const poly = document.createElementNS(svgNS, 'polygon');
    poly.setAttribute('fill', s.color);
    poly.setAttribute('fill-opacity', '0.75');
    poly.setAttribute('stroke', s.color);
    poly.setAttribute('stroke-width', '0.8');
    poly.setAttribute('stroke-linejoin', 'miter');
    poly.setAttribute('points', [...up, ...down].join(' '));
    svg.appendChild(poly);
  });

  // x labels — wall-clock ticks honoring variable-interval durations.
  const xtTicks = timeAxisTicks(axis, 8);
  for (const tick of xtTicks) {
    const t = document.createElementNS(svgNS, 'text');
    t.setAttribute('class', 'sc-axis-label');
    t.setAttribute('x', hourToX(tick.offsetHours));
    t.setAttribute('y', PAD_T + innerH + 13);
    t.setAttribute('text-anchor', 'middle');
    t.textContent = tick.label;
    svg.appendChild(t);
  }

  container.appendChild(svg);

  const unitLabel = opts.unit ?? 'MW';
  installHover(container, svg, { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W, axis }, (i) => {
    const rows = segments
      .filter(s => Math.abs(s.data[i] || 0) > 1e-6)
      .map(s => `<div class="sc-tooltip-row"><span><span class="sw" style="background:${s.color}"></span>${escapeHtml(s.name)}</span><span class="val">${(s.data[i] || 0).toFixed(1)} ${unitLabel}</span></div>`)
      .join('');
    const total = segments.reduce((t, s) => t + (s.data[i] || 0), 0);
    return `<div class="sc-tooltip-title">${escapeHtml(periodTimeLabel(i))}</div>${rows}<div class="sc-tooltip-row" style="border-top:1px solid var(--border-sub); padding-top:3px; margin-top:3px"><span>Total</span><span class="val">${total.toFixed(1)} ${unitLabel}</span></div>`;
  });
}

// ── Sparkline (for table rows) ────────────────────────────────────

function renderSparkline(container, values, color, opts = {}) {
  const W = container.clientWidth || 80;
  const H = container.clientHeight || 22;
  const n = values.length;
  if (!n) return;
  // ``opts.overlay`` lets a second series share the y-range with the
  // primary series — used on the Branches tab to plot SCUC vs AC SCED
  // shadow prices on the same axis. Both series feed the min/max
  // calculation so neither gets clipped.
  const overlay = opts.overlay;
  const overlayValues = (overlay && Array.isArray(overlay.values)) ? overlay.values : null;
  const finite = values.filter(v => typeof v === 'number' && isFinite(v));
  const overlayFinite = overlayValues
    ? overlayValues.filter(v => typeof v === 'number' && isFinite(v))
    : [];
  if (!finite.length && !overlayFinite.length) return;
  const all = finite.concat(overlayFinite);
  let mn = Math.min(...all);
  let mx = Math.max(...all);
  // ``opts.refLine`` draws a dashed horizontal reference line at the
  // given value (e.g. 1.0 for the rated 100% utilization on the
  // Branches tab). Expand the y-range so the line is always visible
  // even when the data doesn't reach it (a branch hovering at 92 %
  // util in every period should still show where 100 % sits).
  const refLine = opts.refLine;
  if (typeof refLine === 'number' && isFinite(refLine)) {
    if (refLine < mn) mn = refLine;
    if (refLine > mx) mx = refLine;
  }
  const span = (mx - mn) || 1;
  const svgNS = 'http://www.w3.org/2000/svg';
  container.innerHTML = '';
  const svg = document.createElementNS(svgNS, 'svg');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('preserveAspectRatio', 'none');
  svg.style.width = '100%';
  svg.style.height = '100%';
  // Stepped (block) rendering: each period i spans [i/n, (i+1)/n] of the
  // canvas, held flat at its value. Y range fits the actual data.
  const xL = (i) => (i / n) * W;
  const xR = (i) => ((i + 1) / n) * W;
  const yFor = (v) => H - 2 - ((v - mn) / span) * (H - 4);
  if (typeof refLine === 'number' && isFinite(refLine)) {
    const refY = yFor(refLine);
    const ref = document.createElementNS(svgNS, 'line');
    ref.setAttribute('x1', 0);
    ref.setAttribute('x2', W);
    ref.setAttribute('y1', refY);
    ref.setAttribute('y2', refY);
    ref.setAttribute('stroke', opts.refColor || 'rgba(248, 113, 113, 0.5)');
    ref.setAttribute('stroke-width', '0.8');
    ref.setAttribute('stroke-dasharray', '3 2');
    svg.appendChild(ref);
  }
  // Optional overlay series rendered behind the primary stroke so the
  // primary line stays the dominant signal.
  if (overlayValues) {
    const oPts = [];
    for (let i = 0; i < overlayValues.length; i++) {
      const v = overlayValues[i];
      if (v === null || v === undefined || !isFinite(v)) continue;
      oPts.push(`${xL(i)},${yFor(v)}`);
      oPts.push(`${xR(i)},${yFor(v)}`);
    }
    if (oPts.length) {
      const oPoly = document.createElementNS(svgNS, 'polyline');
      oPoly.setAttribute('fill', 'none');
      oPoly.setAttribute('stroke', overlay.color || 'rgba(96, 165, 250, 0.6)');
      oPoly.setAttribute('stroke-width', '1.0');
      oPoly.setAttribute('stroke-linejoin', 'miter');
      if (overlay.dashed !== false) {
        oPoly.setAttribute('stroke-dasharray', '2 2');
      }
      oPoly.setAttribute('points', oPts.join(' '));
      svg.appendChild(oPoly);
    }
  }
  const pts = [];
  for (let i = 0; i < n; i++) {
    const v = values[i];
    if (v === null || v === undefined || !isFinite(v)) continue;
    pts.push(`${xL(i)},${yFor(v)}`);
    pts.push(`${xR(i)},${yFor(v)}`);
  }
  const poly = document.createElementNS(svgNS, 'polyline');
  poly.setAttribute('fill', 'none');
  poly.setAttribute('stroke', color || 'var(--purple)');
  poly.setAttribute('stroke-width', '1.4');
  poly.setAttribute('stroke-linejoin', 'miter');
  poly.setAttribute('points', pts.join(' '));
  svg.appendChild(poly);
  container.appendChild(svg);

  // Hover tooltip: per-period readout of the metric. Mini-sparks
  // are too small for axis labels, so the tooltip is the only way
  // for a user to know what number they're looking at.
  // Opt-in via ``opts.tooltip`` (a ``{ label, unit, fmt? }`` object
  // or a plain ``label`` string). Falls back to a generic readout
  // when omitted so even uncalibrated callers get something.
  const tip = opts.tooltip;
  if (tip !== false) {
    attachSparklineTooltip(container, svg, {
      values,
      overlay: overlayValues ? { values: overlayValues, label: overlay.label } : null,
      n,
      W,
      label: (typeof tip === 'string') ? tip : (tip && tip.label) || '',
      unit: (tip && tip.unit) || '',
      fmt: (tip && tip.fmt) || null,
      color,
      overlayColor: overlay && overlay.color,
    });
  }
}

// Shared sparkline tooltip — attaches a single absolute-positioned
// tooltip element to the container and updates it on pointermove.
// One tooltip element is reused across hovers; it lives inside the
// sparkline cell so it doesn't bleed into neighbouring rows.
function attachSparklineTooltip(container, svg, info) {
  if (getComputedStyle(container).position === 'static') {
    container.style.position = 'relative';
  }
  let tip = container.querySelector('.spark-tooltip');
  if (!tip) {
    tip = document.createElement('div');
    tip.className = 'spark-tooltip';
    container.appendChild(tip);
  }
  const fmtNum = info.fmt || ((v) => {
    if (v === null || v === undefined || !isFinite(v)) return '—';
    const a = Math.abs(v);
    if (a >= 1000) return v.toFixed(0);
    if (a >= 10) return v.toFixed(1);
    if (a >= 1) return v.toFixed(2);
    if (a >= 0.01) return v.toFixed(3);
    return v.toFixed(4);
  });
  const renderRow = (label, color, val) => {
    const swatch = color ? `<span class="sw" style="background:${color}"></span>` : '';
    const u = info.unit ? ` ${escapeHtml(info.unit)}` : '';
    return `<div class="spark-tooltip-row"><span>${swatch}${escapeHtml(label || '')}</span><span class="val">${fmtNum(val)}${u}</span></div>`;
  };
  const periodAt = (clientX) => {
    const rect = svg.getBoundingClientRect();
    const x = clientX - rect.left;
    const i = Math.floor((x / Math.max(1, rect.width)) * info.n);
    return Math.max(0, Math.min(info.n - 1, i));
  };
  const hide = () => tip.classList.remove('visible');
  svg.addEventListener('pointermove', (ev) => {
    const i = periodAt(ev.clientX);
    const v = info.values[i];
    const haveOverlay = info.overlay && Array.isArray(info.overlay.values);
    const ov = haveOverlay ? info.overlay.values[i] : null;
    if ((v === null || v === undefined || !isFinite(v)) &&
        (!haveOverlay || ov === null || ov === undefined || !isFinite(ov))) {
      hide();
      return;
    }
    const periodLabel = periodTimeLabel(i);
    const rows = [];
    rows.push(renderRow(info.label || 'value', info.color, v));
    if (haveOverlay) {
      rows.push(renderRow(info.overlay.label || 'overlay', info.overlayColor, ov));
    }
    tip.innerHTML = `<div class="spark-tooltip-title">${escapeHtml(periodLabel)}</div>${rows.join('')}`;
    tip.classList.add('visible');
    const cr = container.getBoundingClientRect();
    const relX = ev.clientX - cr.left;
    const relY = ev.clientY - cr.top;
    let left = relX + 10;
    let top = relY - tip.offsetHeight - 8;
    if (top < 0) top = relY + 14;
    // Mini-sparks are narrow — let the tooltip overflow the cell to the
    // right by absolute-positioning it without clipping at cr.width.
    tip.style.left = Math.max(0, left) + 'px';
    tip.style.top = top + 'px';
  });
  svg.addEventListener('pointerleave', hide);
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
  $('sel-solve-mode').value = p.solve_mode || 'scuc';
  $('sel-commitment').value = p.commitment_mode || 'optimize';
  $('sel-lp-solver').value = p.lp_solver || 'highs';
  if ($('sel-nlp-solver')) $('sel-nlp-solver').value = p.nlp_solver || 'ipopt';
  $('inp-mip-gap').value = p.mip_gap ?? 0.001;
  $('sel-run-pricing').value = (p.run_pricing === false) ? 'false' : 'true';
  $('inp-voll-penalty').value = p.voll_per_mwh ?? 9000;
  $('inp-thermal').value = p.thermal_overload_per_mwh ?? 5000;
  $('inp-reserve-short').value = p.reserve_shortfall_per_mwh ?? 1000;
  $('inp-time-limit').value = p.time_limit_secs ?? '';
  // Losses
  if ($('sel-loss-mode')) {
    $('sel-loss-mode').value = p.loss_mode || 'disabled';
    $('inp-loss-rate').value = p.loss_rate ?? 0.02;
    $('inp-loss-iters').value = p.loss_max_iterations ?? 0;
    updateLossVisibility();
  }
  // Security
  if ($('sel-security-enabled')) {
    $('sel-security-enabled').value = p.security_enabled ? 'true' : 'false';
    $('inp-security-max-iter').value = p.security_max_iterations ?? 10;
    $('inp-security-max-cuts').value = p.security_max_cuts_per_iteration ?? 2500;
    $('inp-security-preseed').value = p.security_preseed_count_per_period ?? 250;
    updateSecurityVisibility();
  }
  // AC SCED tuning (goc3 native pipeline)
  if ($('inp-reactive-pin')) {
    $('inp-reactive-pin').value = p.reactive_support_pin_factor ?? 0.0;
    $('inp-ac-opf-tol').value = (p.sced_ac_opf_tolerance != null ? p.sced_ac_opf_tolerance : '');
    $('inp-ac-opf-max-iter').value = (p.sced_ac_opf_max_iterations != null ? p.sced_ac_opf_max_iterations : '');
    $('sel-disable-sced-thermal').value = p.disable_sced_thermal_limits ? 'true' : 'false';
    $('sel-relax-committed-pmin').value = p.ac_relax_committed_pmin_to_zero ? 'true' : 'false';
  }
}

function updateLossVisibility() {
  const mode = $('sel-loss-mode').value;
  const showRate = (mode === 'uniform' || mode === 'load_pattern');
  // ``disabled`` and ``dc_pf`` ignore the rate; ``disabled`` also
  // ignores refinement iterations.
  $('loss-rate-row').style.display = showRate ? '' : 'none';
  $('loss-iter-row').style.display = (mode === 'disabled') ? 'none' : '';
}

function updateSecurityVisibility() {
  const on = $('sel-security-enabled').value === 'true';
  $('security-fields').hidden = !on;
}

function readForm() {
  const scen = state.scenario;
  if (!scen) return;
  const t = scen.time_axis || (scen.time_axis = {});
  const startStr = $('inp-start').value;
  // Keep ``start_iso`` in local-time space (no Z, no offset) so that
  // ``new Date(start_iso)`` parses back to the same wall-clock time
  // the user entered — passing through ``toISOString()`` would
  // shift by the local UTC offset and the chart axis would no
  // longer line up with what the form shows.
  if (startStr) t.start_iso = `${startStr}:00`;
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
  p.solve_mode = $('sel-solve-mode').value;
  p.commitment_mode = $('sel-commitment').value;
  p.lp_solver = $('sel-lp-solver').value;
  if ($('sel-nlp-solver')) p.nlp_solver = $('sel-nlp-solver').value;
  p.mip_gap = parseFloat($('inp-mip-gap').value) || 0.001;
  p.run_pricing = $('sel-run-pricing').value === 'true';
  p.voll_per_mwh = parseFloat($('inp-voll-penalty').value) || 9000;
  p.thermal_overload_per_mwh = parseFloat($('inp-thermal').value) || 5000;
  p.reserve_shortfall_per_mwh = parseFloat($('inp-reserve-short').value) || 1000;
  const tl = $('inp-time-limit').value.trim();
  p.time_limit_secs = tl === '' ? null : parseFloat(tl);
  if ($('sel-loss-mode')) {
    p.loss_mode = $('sel-loss-mode').value;
    p.loss_rate = parseFloat($('inp-loss-rate').value);
    if (!Number.isFinite(p.loss_rate)) p.loss_rate = 0.02;
    p.loss_max_iterations = parseInt($('inp-loss-iters').value, 10) || 0;
  }
  if ($('sel-security-enabled')) {
    p.security_enabled = $('sel-security-enabled').value === 'true';
    p.security_max_iterations = parseInt($('inp-security-max-iter').value, 10) || 10;
    p.security_max_cuts_per_iteration = parseInt($('inp-security-max-cuts').value, 10) || 2500;
    p.security_preseed_count_per_period = parseInt($('inp-security-preseed').value, 10) || 250;
  }
  if ($('inp-reactive-pin')) {
    const pin = parseFloat($('inp-reactive-pin').value);
    p.reactive_support_pin_factor = Number.isFinite(pin) ? pin : 0.0;
    const tol = $('inp-ac-opf-tol').value.trim();
    p.sced_ac_opf_tolerance = tol === '' ? null : parseFloat(tol);
    const maxIter = $('inp-ac-opf-max-iter').value.trim();
    p.sced_ac_opf_max_iterations = maxIter === '' ? null : parseInt(maxIter, 10);
    p.disable_sced_thermal_limits = $('sel-disable-sced-thermal').value === 'true';
    p.ac_relax_committed_pmin_to_zero = $('sel-relax-committed-pmin').value === 'true';
  }
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
    `<option value="${escapeHtml(c.id)}">${escapeHtml(c.title)}</option>`
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
  // Block the whole UI with a modal overlay so the user can't edit
  // inputs or fire another solve while this one is running.
  showSolveOverlay(state.scenario?.source?.case_id);
  const t0 = performance.now();
  try {
    const body = await streamSolve(state.scenario);
    if (body.status !== 'ok') throw new Error(body.error || `solver: ${body.status}`);
    state.lastResult = body;
    const elapsed = ((performance.now() - t0) / 1000).toFixed(2);
    $('solve-status').textContent = `solved · ${elapsed}s`;
    $('solve-status').className = 'solve-status ok';
    renderAllPanes();
    hideSolveOverlay();
  } catch (err) {
    $('solve-status').textContent = 'error';
    $('solve-status').className = 'solve-status err';
    showError(err.message);
    showSolveOverlayError(err.message);
  } finally {
    state.solving = false;
    $('btn-solve').disabled = false;
  }
}

// Stream the solve via SSE (text/event-stream). Each ``event: log``
// chunk gets appended to the modal's log box; the final
// ``event: result`` chunk carries the JSON the rest of the dashboard
// reads. ``event: error`` raises a JS error that the caller surfaces
// as a dismissable overlay.
async function streamSolve(scenario) {
  const res = await fetch('api/solve/stream', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'Accept': 'text/event-stream' },
    body: JSON.stringify(scenario),
  });
  if (!res.ok) {
    let detail = res.statusText;
    try { detail = (await res.json()).detail || detail; } catch (_) {}
    throw new Error(detail);
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder('utf-8');
  let buf = '';
  let result = null;
  let errMsg = null;

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    // Split on the SSE event delimiter (blank line).
    let idx;
    while ((idx = buf.indexOf('\n\n')) >= 0) {
      const chunk = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      if (chunk.startsWith(': ')) continue; // heartbeat / comment
      const lines = chunk.split('\n');
      let event = 'message';
      const dataLines = [];
      for (const ln of lines) {
        if (ln.startsWith('event: ')) event = ln.slice(7).trim();
        else if (ln.startsWith('data: ')) dataLines.push(ln.slice(6));
      }
      const data = dataLines.join('\n');
      if (event === 'log') {
        appendSolveLog(data);
      } else if (event === 'result') {
        try { result = JSON.parse(data); }
        catch (e) { errMsg = `invalid result JSON: ${e.message}`; }
      } else if (event === 'error') {
        errMsg = data;
      }
    }
  }
  if (errMsg) throw new Error(errMsg);
  if (!result) throw new Error('solve stream ended without a result');
  return result;
}

function appendSolveLog(line) {
  const el = $('solve-overlay-log');
  if (!el) return;
  // Cap at 500 lines to keep the modal responsive on long runs.
  const MAX_LINES = 500;
  el.textContent += (el.textContent ? '\n' : '') + line;
  const all = el.textContent.split('\n');
  if (all.length > MAX_LINES) {
    el.textContent = all.slice(all.length - MAX_LINES).join('\n');
  }
  // Auto-scroll to keep the newest line visible.
  el.scrollTop = el.scrollHeight;
}

function showSolveOverlayError(msg) {
  const overlay = $('solve-overlay');
  if (!overlay) return;
  // Stop the spinner / timer; leave the modal up with the error
  // message so the user can read the log + click Dismiss.
  if (state._solveTickHandle) {
    clearInterval(state._solveTickHandle);
    state._solveTickHandle = null;
  }
  const title = $('solve-overlay-title');
  if (title) {
    title.textContent = 'Solve failed';
    title.style.color = '#f87171';
  }
  const sub = $('solve-overlay-sub');
  if (sub) sub.textContent = msg;
  const dismiss = $('solve-overlay-dismiss');
  if (dismiss) dismiss.hidden = false;
}

function showSolveOverlay(caseId) {
  const overlay = $('solve-overlay');
  if (!overlay) return;
  overlay.hidden = false;
  const title = $('solve-overlay-title');
  if (title) {
    title.textContent = 'Solving…';
    title.style.color = '';
  }
  const sub = $('solve-overlay-sub');
  if (sub) sub.textContent = caseId ? `case ${caseId}` : 'building dispatch request';
  const log = $('solve-overlay-log');
  if (log) log.textContent = '';
  const dismiss = $('solve-overlay-dismiss');
  if (dismiss) dismiss.hidden = true;
  const elapsed = $('solve-overlay-elapsed');
  const start = performance.now();
  if (state._solveTickHandle) clearInterval(state._solveTickHandle);
  if (elapsed) elapsed.textContent = '0.0 s';
  state._solveTickHandle = setInterval(() => {
    if (elapsed) elapsed.textContent = `${((performance.now() - start) / 1000).toFixed(1)} s`;
  }, 100);
}

function hideSolveOverlay() {
  const overlay = $('solve-overlay');
  if (overlay) overlay.hidden = true;
  if (state._solveTickHandle) {
    clearInterval(state._solveTickHandle);
    state._solveTickHandle = null;
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
  // Update the LMP heatmap header note to show which solve pass produced
  // the LMPs we're rendering (DC SCUC / DC SCED / AC SCED).
  const lmpSourceEl = $('lmp-heatmap-source');
  if (lmpSourceEl) {
    const src = r.lmp_source || 'DC SCUC';
    lmpSourceEl.textContent = `buses × periods · $/MWh · ${src}`;
  }
  // Reveal the per-tab reactive toggles only when the AC SCED stage
  // actually produced Q-LMP / MVAr data; SCUC-only runs hide them.
  const showReactive = !!r.has_reactive;
  ['grid-power-toggle-wrap', 'gen-power-toggle-wrap', 'load-power-toggle-wrap'].forEach(id => {
    const el = $(id);
    if (el) el.hidden = !showReactive;
  });
  const qSection = $('qlmp-section');
  if (qSection) qSection.hidden = !showReactive;
  // Side-by-side P / Q layout on the LMPs tab — toggling .has-q on
  // the grid container reveals the second column. CSS handles the
  // narrow-viewport stacking fallback at <=1240 px.
  const lmpGrid = $('lmp-grid');
  if (lmpGrid) lmpGrid.classList.toggle('has-q', showReactive);
  renderSummary(r);
  // Render all panes so tab switches are instant.
  renderGrid(r);
  renderLmps(r);
  renderGenerators(r);
  renderLoads(r);
  renderHvdc(r);
  renderReserves(r);
  renderAsPricing(r);
  renderObjective(r);
  renderBranches(r);
  renderContingencies(r);
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

  // Collect LMPs for the selected period — P-LMP by default, Q-LMP
  // when the user flips the Mode toggle (only available on AC SCED
  // runs that produced reactive duals).
  const gridMode = ($('sel-grid-power-mode')?.value) || 'active';
  const useQ = gridMode === 'reactive' && r.has_reactive;
  const lmpsByBus = useQ ? (r.q_lmps_by_bus || {}) : (r.lmps_by_bus || {});
  const lmps = topo.buses.map(b => {
    const arr = lmpsByBus[String(b.number)];
    return arr ? arr[period] : null;
  });
  const valid = lmps.filter(v => v !== null && isFinite(v));
  let mn = valid.length ? Math.min(...valid) : 0;
  let mx = valid.length ? Math.max(...valid) : 1;
  // Color scale spans the actual per-period distribution — pinning the
  // lower bound at $0 collapses the visible spread into the upper
  // portion of the gradient whenever prices are all positive (the
  // common case) and obscures node-to-node congestion.
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
  // Color scale spans the actual ``[minV, maxV]`` window the caller
  // passed — typically the per-period distribution across nodes — so
  // the gradient resolves node-to-node spread regardless of where the
  // window sits on the absolute price axis.
  const span = Math.max(1e-6, maxV - minV);
  const t = Math.max(0, Math.min(1, (v - minV) / span));
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

  // Reactive companion heatmap + line chart — visible when the AC
  // SCED stage produced Q-balance duals.
  if (r.has_reactive && Object.keys(r.q_lmps_by_bus || {}).length) {
    const qByBus = r.q_lmps_by_bus;
    let qmn = Infinity, qmx = -Infinity;
    visible.forEach(b => (qByBus[b] || []).forEach(v => {
      if (v < qmn) qmn = v;
      if (v > qmx) qmx = v;
    }));
    if (!isFinite(qmn)) { qmn = 0; qmx = 1; }
    const qRows = [`<div class="lmp-heatmap-bus"></div>`];
    for (let t = 0; t < n; t++) qRows.push(`<div class="lmp-heatmap-head">${t}</div>`);
    visible.forEach(b => {
      qRows.push(`<div class="lmp-heatmap-bus">bus ${b}</div>`);
      const arr = qByBus[b] || [];
      for (let t = 0; t < n; t++) {
        const v = arr[t] ?? 0;
        const color = lmpColor(v, qmn, qmx);
        qRows.push(`<div class="lmp-heatmap-cell" style="background:${color}" title="bus ${b} · ${periodTimeLabel(t)}: $${v.toFixed(3)}/MVAr-h">${v.toFixed(2)}</div>`);
      }
    });
    $('qlmp-heatmap').innerHTML = `<div class="lmp-heatmap-grid" style="grid-template-columns: 60px repeat(${n}, ${W_per_cell}px)">${qRows.join('')}</div>`;
    // Pick the top-5 Q-LMP buses by ITS OWN variance rather than
    // reusing the P-LMP variance ranking — the buses where price
    // moves on the active side are not necessarily the same ones
    // where reactive price moves, and reusing the P ranking left
    // the Q chart showing five flat traces.
    const qVariance = (arr) => {
      const m = arr.reduce((a, b) => a + b, 0) / arr.length;
      return arr.reduce((a, b) => a + (b - m) ** 2, 0) / arr.length;
    };
    const sortedQBuses = [...visible]
      .filter(b => (qByBus[b] || []).length)
      .sort((a, b) => qVariance(qByBus[b]) - qVariance(qByBus[a]))
      .slice(0, 5);
    const qSeries = sortedQBuses.map((b, i) => ({
      name: `bus ${b}`,
      color: palette[i % palette.length],
      data: qByBus[b] || [],
      unit: '$/MVAr-h',
    }));
    renderLineChart($('qlmp-lines'), qSeries, { tooltipPrecision: 3 });
  }
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

  // P/Q toggle drives which trace (power_mw vs q_mvar) feeds the
  // stacked area. Update the panel-head subtitle to match the unit.
  const genMode = ($('sel-gen-power-mode')?.value) || 'active';
  const isQ = genMode === 'reactive' && r.has_reactive;
  const traceField = isQ ? 'q_mvar' : 'power_mw';
  const unitLabel = isQ ? 'MVAr' : 'MW';
  const stackSub = $('gen-stack-sub');
  if (stackSub) stackSub.textContent = `stacked ${unitLabel} · fuel mix by period`;

  // Stacked area of dispatch (active or reactive), grouped by fuel
  // when > ~15 gens else per-gen.
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
      const arr = g[traceField] || [];
      arr.forEach((v, i) => agg.data[i] += v || 0);
    });
    series = [...byFuel.values()];
  } else {
    series = gens.map((g, i) => ({
      name: g.resource_id,
      color: colorForGenerator(g, i),
      data: g[traceField] || [],
    }));
  }
  const stack = $('gen-stack');
  renderStackedArea(stack, series, { unit: unitLabel });
  // Re-derive from state.lastResult on resize so a case switch
  // (e.g. d1_303 18 periods → d3_315 42 periods) doesn't redraw
  // with the previous run's series via a stale closure.
  observeResize(stack, () => {
    if (state.lastResult) renderGenerators(state.lastResult);
  });

  // Table with per-gen sparkline. Revenue is split into Energy / AS
  // columns so the merchant view shows where each $ came from. The
  // sparkline gets a thin colored band above it on periods where the
  // unit was carrying any AS award (color from the AS swatch palette).
  // Dispatch / Σ-energy columns retitle with the active P/Q unit.
  // For reactive mode the integral is MVAr·h; we still show the
  // sparkline per period so the time-series picture is intact.
  const dispatchHeader = `Dispatch ${unitLabel}`;
  const energyHeader = isQ ? 'Σ MVAr·h' : 'Σ MWh';
  const parts = [`
    <div class="gen-row head">
      <span>Resource</span><span>Bus</span><span>Pmax</span><span>${dispatchHeader}</span><span>${energyHeader}</span><span>Σ Cost</span><span>Σ Energy</span><span>Σ AS</span><span>Σ Net</span>
    </div>
  `];
  sorted.forEach((g, i) => {
    const trace = g[traceField] || [];
    const totalEnergy = sum(trace);
    const totalCost = sum(g.energy_cost_dollars);
    const totalEnergyRev = sum(g.revenue_dollars);
    const totalAsRev = g.as_revenue_dollars || 0;
    const totalRev = totalEnergyRev + totalAsRev;
    const totalNet = totalRev - totalCost;
    const color = colorForGenerator(g, i);
    const netClass = totalNet > 0.01 ? 'net pos' : totalNet < -0.01 ? 'net neg' : 'net zero';
    parts.push(`
      <div class="gen-row">
        <span class="gen-id"><span class="dot" style="background:${color}"></span>${escapeHtml(g.resource_id)}</span>
        <span>${g.bus}</span>
        <span class="mw">${(g.pmax_mw || 0).toFixed(1)}</span>
        <span class="mini-spark" data-gen="${escapeHtml(g.resource_id)}"></span>
        <span class="mw">${totalEnergy.toFixed(1)}</span>
        <span class="cost">${fmtMoney(totalCost)}</span>
        <span class="revenue">${fmtMoney(totalEnergyRev)}</span>
        <span class="revenue ${totalAsRev > 0.01 ? 'as-pos' : 'as-zero'}">${fmtMoney(totalAsRev)}</span>
        <span class="${netClass}">${fmtMoney(totalNet)}</span>
      </div>
    `);
  });
  const body = $('gen-table');
  body.innerHTML = parts.join('');
  body.querySelectorAll('.mini-spark').forEach(el => {
    const g = sorted.find(x => x.resource_id === el.dataset.gen);
    const c = colorForGenerator(g, sorted.indexOf(g));
    // Track the active P/Q toggle for the per-gen sparkline too —
    // otherwise switching the panel to "Reactive" changes the
    // aggregate stack but every row still draws its MW trace.
    const trace = g[traceField] || [];
    renderSparkline(el, trace, c, {
      tooltip: { label: g.resource_id, unit: unitLabel },
    });
    // AS-carrying band: a thin colored row above the sparkline marking
    // each period the unit cleared any AS award. Color picks from the
    // first product the unit cleared in that period (most common is
    // a single product per gen). Empty mask = no overlay.
    const mask = g.as_carrying_mask || [];
    const products = g.reserve_awards_by_product || {};
    if (mask.some(Boolean)) {
      const span = document.createElement('span');
      span.className = 'spark-as-band';
      const n = mask.length;
      const segs = mask.map((on, i) => {
        if (!on) return `<span class="spark-as-cell" style="opacity:0"></span>`;
        // Pick the first product with an award at this period.
        let color = '#94a3b8';
        for (const [pid, rec] of Object.entries(products)) {
          if ((rec.award_mw || [])[i] > 1e-9) {
            color = RESERVE_COLORS[pid] || color;
            break;
          }
        }
        return `<span class="spark-as-cell" style="background:${color}"></span>`;
      }).join('');
      span.innerHTML = segs;
      el.insertBefore(span, el.firstChild);
    }
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
  // System-load chart: per-period Σ served across all buses. P/Q
  // toggle picks which trace (served_mw or served_mvar) feeds the
  // line; per-bus detail stays in the table below regardless.
  const loadMode = ($('sel-load-power-mode')?.value) || 'active';
  const isLoadQ = loadMode === 'reactive' && r.has_reactive;
  const traceField = isLoadQ ? 'served_mvar' : 'served_mw';
  const unitLabel = isLoadQ ? 'MVAr' : 'MW';
  const stackSub = $('load-stack-sub');
  if (stackSub) stackSub.textContent = `bus × period ${unitLabel}`;

  const periods = (loads[0][traceField] || []).length;
  const systemLoad = new Array(periods).fill(0);
  for (const l of loads) {
    const arr = l[traceField] || [];
    for (let t = 0; t < periods; t++) systemLoad[t] += arr[t] || 0;
  }
  const el = $('load-chart');
  const systemSeries = [{
    name: `System load (${unitLabel})`,
    color: isLoadQ ? '#a78bfa' : '#60a5fa',
    data: systemLoad,
    unit: unitLabel,
  }];
  renderLineChart(el, systemSeries);
  // Same stale-closure caution as gen-stack — re-render from
  // ``state.lastResult`` on resize.
  observeResize(el, () => {
    if (state.lastResult) renderLoads(state.lastResult);
  });

  const totalsHeader = isLoadQ ? 'Σ MVAr·h' : 'Σ MWh';
  const parts = [`
    <div class="load-row head">
      <span>Bus</span><span>Handling</span><span>${totalsHeader}</span><span>Σ Shed</span><span>Σ Cost</span><span>Profile</span>
    </div>
  `];
  const sum = (arr) => arr.reduce((a, b) => a + b, 0);
  // Per-load cost: served_mw[t] × LMP[bus][t] summed across periods.
  // Falls back to 0 when the bus has no LMP series (shouldn't happen
  // post-solve but defensive against edge cases). Cost stays $/MWh
  // even on the reactive view — load is paid for active energy.
  const lmpsByBus = r.lmps_by_bus || {};
  const costFor = (l) => {
    const lmps = lmpsByBus[String(l.bus)] || [];
    let acc = 0;
    for (let t = 0; t < l.served_mw.length; t++) {
      acc += (l.served_mw[t] || 0) * (lmps[t] || 0);
    }
    return acc;
  };
  let totalLoadCost = 0;
  let totalLoadEnergy = 0;
  let totalLoadShed = 0;
  loads.forEach((l, i) => {
    const trace = l[traceField] || [];
    const tot = sum(trace);
    const shed = sum(l.shed_mw);
    const cost = costFor(l);
    totalLoadCost += cost;
    totalLoadEnergy += tot;
    totalLoadShed += shed;
    parts.push(`
      <div class="load-row">
        <span>${l.bus}</span>
        <span>${escapeHtml(l.handling)}</span>
        <span>${tot.toFixed(1)}</span>
        <span>${shed.toFixed(1)}</span>
        <span class="cost">${fmtMoney(cost)}</span>
        <span class="mini-spark" data-load="${l.bus}"></span>
      </div>
    `);
  });
  // System totals footer — load-side energy cost across the horizon.
  parts.push(`
    <div class="load-row total">
      <span>Σ all</span>
      <span></span>
      <span>${totalLoadEnergy.toFixed(1)}</span>
      <span>${totalLoadShed.toFixed(1)}</span>
      <span class="cost">${fmtMoney(totalLoadCost)}</span>
      <span></span>
    </div>
  `);
  const tb = $('load-table');
  tb.innerHTML = parts.join('');
  tb.querySelectorAll('.mini-spark').forEach(el => {
    const l = loads.find(x => String(x.bus) === el.dataset.load);
    // Per-bus sparkline tracks the active P/Q toggle, same as the
    // system-load chart above.
    const trace = l[traceField] || [];
    renderSparkline(el, trace, palette[loads.indexOf(l) % palette.length], {
      tooltip: { label: `Bus ${l.bus}`, unit: unitLabel },
    });
  });
}

// ── HVDC ─────────────────────────────────────────────────────────

function renderHvdc(r) {
  const tabBtn = document.querySelector('.results-tab[data-hvdc-tab]');
  const links = r.hvdc_links || [];
  // Hide the tab button entirely on cases without HVDC links so the
  // strip stays clean. The tab content stays in the DOM but is
  // unreachable when the button is hidden.
  if (tabBtn) tabBtn.hidden = !links.length;
  const chart = $('hvdc-chart');
  const tbl = $('hvdc-table');
  if (!chart || !tbl) return;
  if (!links.length) {
    chart.innerHTML = '';
    tbl.innerHTML = '';
    return;
  }
  // Aggregate chart: per-link from-end MW as a stacked area so the
  // total HVDC inter-area transfer is visible at a glance plus each
  // link's contribution.
  const palette = ['#a78bfa', '#60a5fa', '#34d399', '#fbbf24', '#f87171', '#fb923c'];
  const series = links.map((link, i) => ({
    name: link.name || link.link_id,
    color: palette[i % palette.length],
    data: link.power_mw || [],
  }));
  renderStackedArea(chart, series, { unit: 'MW' });
  observeResize(chart, () => {
    if (state.lastResult) renderHvdc(state.lastResult);
  });

  const sum = (arr) => arr.reduce((a, b) => a + b, 0);
  const meanAbs = (arr) => arr.length ? arr.reduce((a, b) => a + Math.abs(b), 0) / arr.length : 0;
  const parts = [`
    <div class="hvdc-row head">
      <span>Link</span>
      <span>From → To</span>
      <span>Σ MWh (from)</span>
      <span>Σ Delivered MWh</span>
      <span>P trace (MW)</span>
      <span>Q from (MVAr)</span>
      <span>Q to (MVAr)</span>
    </div>
  `];
  links.forEach((link, i) => {
    const c = palette[i % palette.length];
    const fromBus = link.from_bus ?? '?';
    const toBus = link.to_bus ?? '?';
    parts.push(`
      <div class="hvdc-row">
        <span><span class="dot" style="background:${c}"></span>${escapeHtml(link.name || link.link_id)}</span>
        <span>${fromBus} → ${toBus}</span>
        <span class="mw">${sum(link.power_mw || []).toFixed(1)}</span>
        <span class="mw">${sum(link.delivered_mw || []).toFixed(1)}</span>
        <span class="mini-spark" data-link="${escapeHtml(link.link_id)}" data-trace="p"></span>
        <span class="mini-spark" data-link="${escapeHtml(link.link_id)}" data-trace="qfr"></span>
        <span class="mini-spark" data-link="${escapeHtml(link.link_id)}" data-trace="qto"></span>
      </div>
    `);
  });
  tbl.innerHTML = parts.join('');
  const linkById = Object.fromEntries(links.map(l => [l.link_id, l]));
  tbl.querySelectorAll('.mini-spark').forEach(el => {
    const link = linkById[el.dataset.link];
    if (!link) return;
    const idx = links.indexOf(link);
    const c = palette[idx % palette.length];
    if (el.dataset.trace === 'p') {
      renderSparkline(el, link.power_mw || [], c, {
        tooltip: { label: `${link.name} P (from)`, unit: 'MW' },
      });
    } else if (el.dataset.trace === 'qfr') {
      renderSparkline(el, link.q_from_mvar || [], '#a78bfa', {
        tooltip: { label: `${link.name} Q (from)`, unit: 'MVAr' },
      });
    } else {
      renderSparkline(el, link.q_to_mvar || [], '#fbbf24', {
        tooltip: { label: `${link.name} Q (to)`, unit: 'MVAr' },
      });
    }
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
    // Reserves tab focuses on quantities; clearing price lives on
    // the AS Pricing tab so we don't duplicate it here.
    const seriesSpec = [
      { name: 'Required', color: 'rgba(148,163,184,0.7)', data: a.requirement_mw, unit: 'MW', dashArray: '3 2' },
      { name: 'Provided', color, data: a.provided_mw, unit: 'MW', strokeWidth: 2.4 },
    ];
    renderLineChart(el, seriesSpec, { tooltipPrecision: 2 });
    observeResize(el, () => {
      if (state.lastResult) renderReserves(state.lastResult);
    });
  });
}

// ── AS Pricing ─────────────────────────────────────────────────────
// One line chart per (zone, product) showing the clearing price across
// periods. Provided MW renders as a secondary line so the user can see
// when scarcity drove the price (provided ≪ requirement → shortfall).

function renderAsPricing(r) {
  const grid = $('as-pricing-grid');
  if (!grid) return;
  const awards = r.reserve_awards || [];
  if (!awards.length) {
    grid.innerHTML = '<div class="viol-empty">no AS products cleared</div>';
    $('as-pricing-note').textContent = '';
    return;
  }
  const sum = (arr) => arr.reduce((a, b) => a + b, 0);
  const labels = { reg_up: 'Regulation up', reg_down: 'Regulation down', syn: 'Spinning', nsyn: 'Non-spin' };
  const totalPayment = awards.reduce((s, a) => s + sum(a.payment_dollars || []), 0);
  $('as-pricing-note').textContent = `Σ payment ${fmtMoney(totalPayment)} · ${awards.length} (zone × product) markets`;
  grid.innerHTML = awards.map(a => {
    const color = RESERVE_COLORS[a.product_id] || 'var(--purple)';
    const meanPrice = sum(a.clearing_price || []) / Math.max(1, (a.clearing_price || []).length);
    const peakPrice = Math.max(0, ...(a.clearing_price || [0]));
    const shortfallPeriods = (a.shortfall_mw || []).filter(v => v > 1e-6).length;
    return `
      <div class="as-pricing-card">
        <div class="as-pricing-head">
          <div class="as-pricing-title">
            <span class="dot" style="background:${color}"></span>
            ${escapeHtml(labels[a.product_id] || a.product_id)}
            <span class="panel-sub">zone ${a.zone_id}</span>
          </div>
          <div class="as-pricing-stats">
            <span title="Mean clearing price">μ ${fmtPrice(meanPrice)}</span>
            <span title="Peak clearing price">peak ${fmtPrice(peakPrice)}</span>
            <span title="Periods with positive shortfall MW">short ${shortfallPeriods}p</span>
          </div>
        </div>
        <div class="as-pricing-chart" data-pid="${escapeHtml(a.product_id)}"></div>
      </div>
    `;
  }).join('');
  grid.querySelectorAll('.as-pricing-chart').forEach(el => {
    const a = awards.find(x => x.product_id === el.dataset.pid);
    if (!a) return;
    const color = RESERVE_COLORS[a.product_id] || 'var(--purple)';
    // AS Pricing tab is price-only — provided MW lives on the
    // Reserves tab where it belongs.
    const series = [
      { name: 'Clearing $/MWh', color, data: a.clearing_price || [], unit: '$/MWh', strokeWidth: 2.4 },
    ];
    renderLineChart(el, series, { tooltipPrecision: 2 });
    observeResize(el, () => {
      if (state.lastResult) renderAsPricing(state.lastResult);
    });
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

// ── Objective breakdown ──────────────────────────────────────────

const OBJECTIVE_BUCKET_COLORS = {
  // Keys must match the display labels emitted by
  // ``_objective_breakdown`` in ``dashboards/rto/api.py``. Each
  // commitment-decided + redispatch + penalty bucket gets its own
  // hue so the stacked-area chart segments stay distinguishable.
  'Generator energy': '#60a5fa',
  'Load energy (DR)': '#3b82f6',
  'HVDC energy': '#0ea5e9',
  'No-load': '#a78bfa',
  'Startup': '#fbbf24',
  'Shutdown': '#fb923c',
  'AS clearing': '#34d399',
  'AS shortfall': '#10b981',
  'Thermal penalty': '#f87171',
  'P-balance penalty': '#ef4444',
  'Q-balance penalty': '#dc2626',
  'Voltage penalty': '#e11d48',
  'Angle penalty': '#be185d',
  'Ramp penalty': '#94a3b8',
  'Flowgate penalty': '#9333ea',
  'Interface penalty': '#7c3aed',
  'Headroom penalty': '#84cc16',
  'Footroom penalty': '#65a30d',
  'Energy window penalty': '#facc15',
  'CO₂': '#475569',
  Other: '#64748b',
};

function objectiveBucketColor(bucket) {
  return OBJECTIVE_BUCKET_COLORS[bucket] || '#9ca3af';
}

function renderObjective(r) {
  const ob = r.objective_breakdown || {};
  const totals = ob.total_by_bucket || {};
  const perPeriod = ob.per_period_by_bucket || {};
  const grand = ob.grand_total_dollars || 0;
  const buckets = (ob.buckets || []).slice().sort(
    (a, b) => Math.abs(totals[b] || 0) - Math.abs(totals[a] || 0)
  );

  // Top cards: total per bucket + share of grand total.
  const cards = $('objective-cards');
  if (!buckets.length) {
    cards.innerHTML = '<div class="viol-empty">no objective_terms reported</div>';
    $('objective-stack').innerHTML = '';
    return;
  }
  const fmtPct = (x) => `${(100 * x).toFixed(1)}%`;
  cards.innerHTML = [
    `<div class="obj-card grand">
       <span class="obj-card-label">Total</span>
       <span class="obj-card-value">${fmtMoney(grand)}</span>
     </div>`,
    ...buckets.map(b => {
      const v = totals[b] || 0;
      const share = grand !== 0 ? v / grand : 0;
      const color = objectiveBucketColor(b);
      return `
        <div class="obj-card" style="border-left-color:${color}">
          <span class="obj-card-label">${escapeHtml(b)}</span>
          <span class="obj-card-value">${fmtMoney(v)}</span>
          <span class="obj-card-share">${fmtPct(share)}</span>
        </div>
      `;
    }),
  ].join('');

  // Stacked bars per period — buckets with negative dollars (penalties
  // can be negative when the validator-aligned penalty net to a credit)
  // get a separate inline note. Use a simple line chart with bucket as
  // series for the per-period view.
  const series = buckets.map(b => ({
    name: b,
    color: objectiveBucketColor(b),
    data: (perPeriod[b] || []).map(v => v || 0),
  }));
  const stack = $('objective-stack');
  renderStackedArea(stack, series, { unit: '$' });
  observeResize(stack, () => {
    if (state.lastResult) renderObjective(state.lastResult);
  });
}

// ── Branches ─────────────────────────────────────────────────────

function renderBranches(r) {
  const body = $('branches-body');
  const allRows = r.branches_summary || [];
  const thrInput = $('inp-branches-threshold');
  const threshold = parseFloat(thrInput?.value || '') || (r.ui?.branches_threshold ?? 0.9);
  const rows = allRows.filter(row => (row.worst_utilization ?? 0) >= threshold);
  if (!rows.length) {
    body.innerHTML = `<div class="viol-empty">no branch reached ${(threshold*100).toFixed(0)}% utilisation in any period</div>`;
    return;
  }
  const periodCount = rows[0].flow_mw.length;
  const anyHasScuc = rows.some(r => Array.isArray(r.shadow_price_scuc));
  const anyHasSced = rows.some(r => Array.isArray(r.shadow_price_sced));
  const dualStages = anyHasScuc && anyHasSced;
  const shadowHeader = dualStages
    ? `Shadow $/MWh<br><span class="legend-key"><span class="dot sced"></span>SCED<span class="dot scuc"></span>SCUC</span>`
    : 'Shadow $/MWh';
  const head = `
    <div class="br-row head">
      <span>Branch</span>
      <span>Rating MVA</span>
      <span>Worst util</span>
      <span>Flow trace (MW)</span>
      <span>Utilisation</span>
      <span>${shadowHeader}</span>
    </div>
  `;
  const cells = rows.map(row => {
    const worst = row.worst_utilization || 0;
    const cls = row.is_breached ? 'breach' : (worst >= 1.0 ? 'binding' : 'near');
    return `
      <div class="br-row ${cls}">
        <span>${row.from_bus} → ${row.to_bus}</span>
        <span>${row.rating_mva.toFixed(1)}</span>
        <span class="worst">${(worst*100).toFixed(1)}%</span>
        <span class="mini-spark" data-flow='${JSON.stringify(row.flow_mw)}' data-rating="${row.rating_mva}"></span>
        <span class="mini-spark" data-util='${JSON.stringify(row.utilization)}'></span>
        <span class="mini-spark"
              data-shadow='${JSON.stringify(row.shadow_price)}'
              data-shadow-scuc='${JSON.stringify(row.shadow_price_scuc || null)}'
              data-shadow-sced='${JSON.stringify(row.shadow_price_sced || null)}'></span>
      </div>
    `;
  });
  body.innerHTML = head + cells.join('');
  body.querySelectorAll('.mini-spark[data-flow]').forEach(el => {
    const arr = JSON.parse(el.dataset.flow).map(x => Math.abs(x || 0));
    // Flow trace plots magnitude (since flow can be either direction);
    // overlay the per-row MVA rating as a dashed reference line so
    // the viewer sees how close each period's flow runs to the limit.
    const rating = parseFloat(el.dataset.rating);
    const opts = { tooltip: { label: '|Flow|', unit: 'MW' } };
    if (isFinite(rating) && rating > 0) opts.refLine = rating;
    renderSparkline(el, arr, '#60a5fa', opts);
  });
  body.querySelectorAll('.mini-spark[data-util]').forEach(el => {
    const arr = JSON.parse(el.dataset.util).map(x => x || 0);
    // 100 % rating drawn as a dashed reference line so the user can
    // see how close the branch sits to binding regardless of where
    // its peaks land. The y-range expands to include the line.
    renderSparkline(el, arr, '#fbbf24', {
      refLine: 1.0,
      tooltip: { label: 'Utilisation', unit: '×', fmt: (v) => (v * 100).toFixed(1) + '%' },
    });
  });
  body.querySelectorAll('.mini-spark[data-shadow]').forEach(el => {
    const sced = JSON.parse(el.dataset.shadowSced || 'null');
    const scuc = JSON.parse(el.dataset.shadowScuc || 'null');
    const fallback = JSON.parse(el.dataset.shadow).map(x => x || 0);
    // When both stages produced shadow prices, plot SCED solid + SCUC
    // dashed overlay. When only one is present, plot it solid.
    const tipOpts = { label: 'Shadow', unit: '$/MWh' };
    if (Array.isArray(sced) && Array.isArray(scuc)) {
      renderSparkline(
        el,
        sced.map(x => x ?? 0),
        '#f87171',
        {
          overlay: {
            values: scuc.map(x => x ?? 0),
            color: 'rgba(96, 165, 250, 0.85)',
            dashed: true,
            label: 'SCUC',
          },
          tooltip: { label: 'SCED', unit: '$/MWh' },
        },
      );
    } else if (Array.isArray(sced)) {
      renderSparkline(el, sced.map(x => x ?? 0), '#f87171', { tooltip: { label: 'SCED', unit: '$/MWh' } });
    } else if (Array.isArray(scuc)) {
      renderSparkline(el, scuc.map(x => x ?? 0), '#60a5fa', { tooltip: { label: 'SCUC', unit: '$/MWh' } });
    } else {
      renderSparkline(el, fallback, '#f87171', { tooltip: tipOpts });
    }
  });
}

// ── Contingencies ────────────────────────────────────────────────

function renderContingencies(r) {
  const body = $('contingencies-body');
  const allRows = r.contingencies_summary || [];
  const thrInput = $('inp-branches-threshold');
  // Default to 0.95 — the surge-dispatch near-binding screen emits
  // at this threshold, so anything below would hide every entry.
  // The user's threshold input still takes precedence when set.
  const threshold = parseFloat(thrInput?.value || '') || (r.ui?.branches_threshold ?? 0.95);
  const rows = allRows.filter(row => (row.worst_utilization ?? 0) >= threshold);
  if (!rows.length) {
    body.innerHTML = `<div class="viol-empty">no N-1 contingency cut reached ${(threshold*100).toFixed(0)}% post-contingency utilisation</div>`;
    return;
  }
  const anyHasScuc = rows.some(rr => Array.isArray(rr.shadow_price_scuc));
  const anyHasSced = rows.some(rr => Array.isArray(rr.shadow_price_sced));
  const dualStages = anyHasScuc && anyHasSced;
  const shadowHeader = dualStages
    ? `Shadow $/MWh<br><span class="legend-key"><span class="dot sced"></span>SCED<span class="dot scuc"></span>SCUC</span>`
    : 'Shadow $/MWh';
  const head = `
    <div class="ctg-row head">
      <span>Outage</span>
      <span>Monitored</span>
      <span>Worst util</span>
      <span>Post-contingency flow (MW)</span>
      <span>Utilisation</span>
      <span>${shadowHeader}</span>
    </div>
  `;
  const cells = rows.map(row => {
    const worst = row.worst_utilization || 0;
    const cls = row.is_breached ? 'breach' : (worst >= 1.0 - 1e-6 ? 'binding' : 'near');
    const rating = (typeof row.rating_mva === 'number' && row.rating_mva > 0)
      ? row.rating_mva : '';
    return `
      <div class="ctg-row ${cls}">
        <span>${escapeHtml(row.outage_branch)}</span>
        <span>${escapeHtml(row.monitored_branch)}</span>
        <span class="worst">${(worst*100).toFixed(1)}%</span>
        <span class="mini-spark" data-flow='${JSON.stringify(row.flow_mw.map(x => x ?? 0))}' data-rating="${rating}"></span>
        <span class="mini-spark" data-util='${JSON.stringify(row.utilization.map(x => x ?? 0))}'></span>
        <span class="mini-spark"
              data-shadow='${JSON.stringify(row.shadow_price.map(x => x ?? 0))}'
              data-shadow-scuc='${JSON.stringify(row.shadow_price_scuc || null)}'
              data-shadow-sced='${JSON.stringify(row.shadow_price_sced || null)}'></span>
      </div>
    `;
  });
  body.innerHTML = head + cells.join('');
  body.querySelectorAll('.mini-spark[data-flow]').forEach(el => {
    const arr = JSON.parse(el.dataset.flow).map(x => Math.abs(x));
    const rating = parseFloat(el.dataset.rating);
    const opts = { tooltip: { label: 'Post-ctg |flow|', unit: 'MW' } };
    if (isFinite(rating) && rating > 0) opts.refLine = rating;
    renderSparkline(el, arr, '#60a5fa', opts);
  });
  body.querySelectorAll('.mini-spark[data-util]').forEach(el => {
    // 100 % rating reference line so post-contingency util is read
    // against the emergency limit.
    renderSparkline(el, JSON.parse(el.dataset.util), '#fbbf24', {
      refLine: 1.0,
      tooltip: { label: 'Post-ctg util', unit: '×', fmt: (v) => (v * 100).toFixed(1) + '%' },
    });
  });
  body.querySelectorAll('.mini-spark[data-shadow]').forEach(el => {
    const sced = JSON.parse(el.dataset.shadowSced || 'null');
    const scuc = JSON.parse(el.dataset.shadowScuc || 'null');
    const fallback = JSON.parse(el.dataset.shadow).map(x => x || 0);
    if (Array.isArray(sced) && Array.isArray(scuc)) {
      renderSparkline(
        el,
        sced.map(x => x ?? 0),
        '#f87171',
        {
          overlay: {
            values: scuc.map(x => x ?? 0),
            color: 'rgba(96, 165, 250, 0.85)',
            dashed: true,
            label: 'SCUC',
          },
          tooltip: { label: 'SCED', unit: '$/MWh' },
        },
      );
    } else if (Array.isArray(sced)) {
      renderSparkline(el, sced.map(x => x ?? 0), '#f87171', { tooltip: { label: 'SCED', unit: '$/MWh' } });
    } else if (Array.isArray(scuc)) {
      renderSparkline(el, scuc.map(x => x ?? 0), '#60a5fa', { tooltip: { label: 'SCUC', unit: '$/MWh' } });
    } else {
      renderSparkline(el, fallback, '#f87171', { tooltip: { label: 'Shadow', unit: '$/MWh' } });
    }
  });
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
    else if (tab === 'hvdc') renderHvdc(r);
    else if (tab === 'reserves') renderReserves(r);
    else if (tab === 'objective') renderObjective(r);
    else if (tab === 'branches') renderBranches(r);
    else if (tab === 'contingencies') renderContingencies(r);
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
  const dismiss = $('solve-overlay-dismiss');
  if (dismiss) dismiss.addEventListener('click', hideSolveOverlay);

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
    'sel-solve-mode', 'sel-commitment', 'sel-lp-solver', 'sel-nlp-solver',
    'inp-mip-gap', 'sel-run-pricing',
    'inp-voll-penalty', 'inp-thermal', 'inp-reserve-short', 'inp-time-limit',
    'sel-loss-mode', 'inp-loss-rate', 'inp-loss-iters',
    'sel-security-enabled', 'inp-security-max-iter',
    'inp-security-max-cuts', 'inp-security-preseed',
    'inp-reactive-pin', 'inp-ac-opf-tol', 'inp-ac-opf-max-iter',
    'sel-disable-sced-thermal', 'sel-relax-committed-pmin',
  ]);
  $('sel-load-handling').addEventListener('change', toggleVollRow);
  $('sel-offers-synthesis').addEventListener('change', renderOffersHint);
  if ($('sel-loss-mode')) $('sel-loss-mode').addEventListener('change', updateLossVisibility);
  // Threshold knob on Branches / Contingencies tabs — re-renders both
  // panes from the existing payload, no re-solve needed.
  const thrInput = $('inp-branches-threshold');
  if (thrInput) {
    thrInput.addEventListener('input', () => {
      if (state.lastResult) {
        renderBranches(state.lastResult);
        renderContingencies(state.lastResult);
      }
    });
  }
  if ($('sel-security-enabled')) {
    $('sel-security-enabled').addEventListener('change', updateSecurityVisibility);
  }

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
  // P/Q toggles re-render the affected pane in place — no re-solve.
  if ($('sel-grid-power-mode')) {
    $('sel-grid-power-mode').addEventListener('change', () => {
      if (state.lastResult) renderGrid(state.lastResult);
    });
  }
  if ($('sel-gen-power-mode')) {
    $('sel-gen-power-mode').addEventListener('change', () => {
      if (state.lastResult) renderGenerators(state.lastResult);
    });
  }
  if ($('sel-load-power-mode')) {
    $('sel-load-power-mode').addEventListener('change', () => {
      if (state.lastResult) renderLoads(state.lastResult);
    });
  }

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
