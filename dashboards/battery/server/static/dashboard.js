// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
// Battery operator dashboard — single-page app. Stateless server; the
// client holds the full scenario in-memory, edits it via form inputs
// and drag-to-edit SVG charts, and POSTs to /api/solve on demand.

'use strict';

// ═══ 1. State ═══════════════════════════════════════════════════════════

const $ = (id) => document.getElementById(id);

const state = {
  scenario: null,
  lastResult: null,
  meta: null,
  priceChart: null,         // unified LMP + AS chart, active series set via activePriceTab
  activePriceTab: 'lmp',    // 'lmp' | 'as_<product_id>'
  dispatchChart: null,
  socChart: null,
  solving: false,
  observers: [],
  // Active PWL segment selector — null = no segment selected (bands
  // shown read-only). Setting this makes the corresponding segment's
  // per-period line draggable on the price chart.
  activePwlSegment: null,   // { direction: 'discharge'|'charge', index: 0..K-1 }
  // Monte Carlo state — when non-null, the Revenue panel and the
  // dispatch / SOC charts display quantile bands across the N runs
  // instead of the last single solve.
  mcRunning: false,
  mcResults: null,          // { netRevenue: {p10,p50,p90,mean}, perPeriod: [...], runs: [...] }
};

const DIST_SHAPES = ['gaussian', 'uniform', 'triangular'];

// Quantile half-width factor per shape, expressed as k such that
// P10 = P50 − k · (spread_fraction · P50). Symmetric around P50;
// prices clamp at 0.
const DIST_QUANTILE_K = {
  // Gaussian: P90 − P50 ≈ 1.282 σ; with σ = spread · P50 → k = 1.282.
  gaussian: 1.282,
  // Uniform(P50−w, P50+w): P90 = P50 + 0.8 w. With w = spread · P50 → k = 0.8.
  uniform: 0.8,
  // Symmetric triangular(mode=P50, half-width w): P90 − P50 = w (1 − √0.2) ≈ 0.553 w.
  triangular: 0.553,
};

function computeDistributionBand(p50, shape, spreadFraction) {
  const k = DIST_QUANTILE_K[shape] || DIST_QUANTILE_K.gaussian;
  const s = Math.max(0, spreadFraction || 0);
  const p10 = p50.map(v => Math.max(0, v - k * s * v));
  const p90 = p50.map(v => v + k * s * v);
  return { p10, p90 };
}

// Fill in any missing distribution metadata on the scenario so the
// UI can rely on a consistent shape. Called after the default
// scenario loads and after time-axis resampling.
function ensureDistributions(scen) {
  scen.distributions = scen.distributions || {};
  const ensure = (key, p50) => {
    const d = scen.distributions[key] || {};
    d.shape = DIST_SHAPES.includes(d.shape) ? d.shape : 'gaussian';
    d.spread_fraction = (typeof d.spread_fraction === 'number' && d.spread_fraction >= 0)
      ? d.spread_fraction : 0.20;
    d.editable = !!d.editable;
    // Compute bands from formula when no overrides exist OR when
    // the arrays don't match the current period count.
    const n = p50.length;
    if (!Array.isArray(d.p10) || d.p10.length !== n ||
        !Array.isArray(d.p90) || d.p90.length !== n) {
      const computed = computeDistributionBand(p50, d.shape, d.spread_fraction);
      d.p10 = computed.p10;
      d.p90 = computed.p90;
    }
    scen.distributions[key] = d;
  };
  ensure('lmp', scen.lmp_forecast_per_mwh);
  (scen.as_products || []).forEach(ap => ensure(`as_${ap.product_id}`, ap.price_forecast_per_mwh));
}

function recomputeBandFromFormula(key, scen) {
  const d = scen.distributions[key];
  if (!d) return;
  const p50 = key === 'lmp'
    ? scen.lmp_forecast_per_mwh
    : (scen.as_products || []).find(ap => `as_${ap.product_id}` === key)?.price_forecast_per_mwh;
  if (!p50) return;
  const b = computeDistributionBand(p50, d.shape, d.spread_fraction);
  d.p10 = b.p10;
  d.p90 = b.p90;
}

const MODE_HINTS = {
  'optimal_foresight,coupled':
    'LP sees the full forecast — theoretical revenue ceiling.',
  'optimal_foresight,sequential':
    'Myopic: each period only sees its own LMP. Typically misses inter-period arbitrage.',
  'pwl_offers,coupled':
    'Day-ahead clearing against your submitted PWL bid curves.',
  'pwl_offers,sequential':
    'Sequential RTM clearing against your bids — bids act as a self-commitment device.',
};

// Up-direction AS must visually separate from the emerald discharge
// bar; down-direction AS from the red charge bar. Warm family for up
// (amber / rose / purple), cool family for down (blue / indigo).
const AS_COLORS = {
  reg_up:    '#fbbf24',   // amber
  syn:       '#22d3ee',   // cyan   — spinning (distinct from LMP purple)
  nsyn:      '#f472b6',   // rose   — non-spin
  ramp_up_on:    '#fb923c',   // orange
  ramp_up_off:   '#f97316',   // deeper orange
  reg_down:  '#60a5fa',   // blue
  ramp_down_on:  '#818cf8',   // indigo
  ramp_down_off: '#6366f1',   // deeper indigo
};

// ═══ 2. EditableLineChart (SVG with draggable points) ═════════════════

class EditableLineChart {
  constructor(container, opts) {
    this.container = container;
    this.data = opts.data || [];
    this.min = opts.min ?? 0;
    this.max = opts.max ?? 100;
    this.color = opts.color || '#a78bfa';
    this.colorDim = opts.colorDim || 'rgba(167,139,250,0.15)';
    this.onChange = opts.onChange || (() => {});
    this.formatValue = opts.formatValue || ((v) => v.toFixed(1));
    this.editable = opts.editable !== false;
    // Optional uncertainty band: two per-period arrays (p10, p90).
    // When present we render a translucent fill between them plus
    // thin dashed outlines. If ``editable`` is set and
    // ``onBandChange`` is provided, per-period P10/P90 points become
    // draggable.
    this.band = opts.band || null;
    // Optional per-period reference lines (e.g. PWL bid prices).
    // Each entry has a per-period value array the same length as
    // ``data`` so it reads on the same x-axis as the P50 line.
    //   { values: number[], color: string, label: string, opacity?: number }
    this.referenceLines = opts.referenceLines || [];
    // Human-readable label for the primary series — used as the row
    // heading in the hover tooltip ("LMP", "Reg Up", etc.).
    this.seriesName = opts.seriesName || 'value';
    this.svg = null;
    this.render();
  }

  setData(data, opts = {}) {
    this.data = data.slice();
    if (opts.min !== undefined) this.min = opts.min;
    if (opts.max !== undefined) this.max = opts.max;
    if (opts.color) this.color = opts.color;
    if (opts.colorDim) this.colorDim = opts.colorDim;
    if (opts.onChange) this.onChange = opts.onChange;
    if ('band' in opts) this.band = opts.band;
    if ('referenceLines' in opts) this.referenceLines = opts.referenceLines || [];
    if (opts.seriesName !== undefined) this.seriesName = opts.seriesName;
    this.render();
  }

  setReferenceLines(lines) {
    this.referenceLines = lines || [];
    this.render();
  }

  setColor(color, colorDim) {
    this.color = color;
    this.colorDim = colorDim || color + '26';
    this.render();
  }

  setBand(band) {
    this.band = band;
    this.render();
  }

  _yRange() {
    if (!this.data.length) return [this.min, this.max];
    // Pull in band bounds + overlay references so the full envelope
    // drives the axis. ``this.min`` is a hard lower clamp; ``this.max``
    // is used as a sanity ceiling only when the effective dmax is zero
    // (e.g. a freshly zeroed AS product) so the chart doesn't collapse.
    const allValues = this.data.slice();
    if (this.band && this.band.p90) allValues.push(...this.band.p90);
    if (this.band && this.band.p10) allValues.push(...this.band.p10);
    if (this.referenceLines && this.referenceLines.length) {
      this.referenceLines.forEach(rl => {
        const nums = (arr) => arr && arr.filter(v => typeof v === 'number' && isFinite(v));
        if (rl.values) allValues.push(...nums(rl.values));
        if (rl.band) {
          allValues.push(...nums(rl.band.lower) || []);
          allValues.push(...nums(rl.band.upper) || []);
        }
        if (rl.segmentValues) {
          rl.segmentValues.forEach(v => allValues.push(...(nums(v) || [])));
        }
      });
    }
    const dmin = Math.min(...allValues);
    const rawMax = Math.max(...allValues);
    const effectiveMax = rawMax > 0 ? rawMax : this.max;
    const span = Math.max(1, effectiveMax - dmin);
    const pad = span * 0.08;
    let yMin;
    if (this.min === null || this.min === undefined) {
      // No floor — always show some negative headroom so the user can
      // drag points below zero on an axis that starts non-negative.
      const headroom = Math.max(20, effectiveMax * 0.25);
      yMin = Math.floor(Math.min(dmin - pad, -headroom));
    } else {
      yMin = Math.max(this.min, Math.floor(dmin - pad));
    }
    const yMax = Math.ceil(effectiveMax + pad);
    return [yMin, yMax];
  }

  render() {
    const W = this.container.clientWidth || 400;
    const H = this.container.clientHeight || 120;
    const PAD_L = 38, PAD_R = 6, PAD_T = 8, PAD_B = 16;
    const innerW = W - PAD_L - PAD_R;
    const innerH = H - PAD_T - PAD_B;
    const n = this.data.length;
    const xStep = n > 1 ? innerW / (n - 1) : 0;
    const [yMin, yMax] = this._yRange();
    const ySpan = yMax - yMin || 1;

    const toX = (i) => PAD_L + i * xStep;
    const toY = (v) => PAD_T + innerH - ((v - yMin) / ySpan) * innerH;
    const fromY = (y) => yMin + ((PAD_T + innerH - y) / innerH) * ySpan;

    this.container.innerHTML = '';
    const svgNS = 'http://www.w3.org/2000/svg';
    const svg = document.createElementNS(svgNS, 'svg');
    svg.setAttribute('class', 'sc-chart');
    svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
    svg.setAttribute('preserveAspectRatio', 'none');
    svg.style.touchAction = 'none';

    // Gridlines + Y labels
    const yTicks = 4;
    for (let i = 0; i <= yTicks; i++) {
      const v = yMin + (ySpan * i) / yTicks;
      const y = toY(v);
      const line = document.createElementNS(svgNS, 'line');
      line.setAttribute('class', 'sc-grid-line');
      line.setAttribute('x1', PAD_L); line.setAttribute('x2', W - PAD_R);
      line.setAttribute('y1', y); line.setAttribute('y2', y);
      svg.appendChild(line);
      const label = document.createElementNS(svgNS, 'text');
      label.setAttribute('class', 'sc-axis-label');
      label.setAttribute('x', PAD_L - 3);
      label.setAttribute('y', y + 3);
      label.setAttribute('text-anchor', 'end');
      label.textContent = Math.round(v);
      svg.appendChild(label);
    }

    // X labels — time-based ticks aligned to start_iso + resolution.
    buildTimeAxisTicks(n, 8).forEach(({ i, label: txt }) => {
      const label = document.createElementNS(svgNS, 'text');
      label.setAttribute('class', 'sc-axis-label');
      label.setAttribute('x', toX(i));
      label.setAttribute('y', PAD_T + innerH + 11);
      label.setAttribute('text-anchor', 'middle');
      label.textContent = txt;
      svg.appendChild(label);
    });

    // Uncertainty band (P10–P90): filled polygon + thin dashed
    // outlines. Rendered before the main line so the line stays on top.
    const bandP10 = this.band && this.band.p10;
    const bandP90 = this.band && this.band.p90;
    const hasBand = bandP10 && bandP90 && bandP10.length === n && bandP90.length === n;
    if (hasBand && n >= 2) {
      const up = bandP90.map((v, i) => `${toX(i)},${toY(v)}`);
      const dn = bandP10.map((v, i) => `${toX(i)},${toY(v)}`).reverse();
      const bandPoly = document.createElementNS(svgNS, 'polygon');
      bandPoly.setAttribute('class', 'sc-dist-band');
      bandPoly.setAttribute('points', [...up, ...dn].join(' '));
      bandPoly.setAttribute('fill', this.color);
      bandPoly.setAttribute('fill-opacity', '0.12');
      svg.appendChild(bandPoly);

      // P10 + P90 thin dashed outlines
      const makeEdge = (values, strokeOpacity) => {
        const p = document.createElementNS(svgNS, 'polyline');
        p.setAttribute('fill', 'none');
        p.setAttribute('stroke', this.color);
        p.setAttribute('stroke-width', 1);
        p.setAttribute('stroke-opacity', strokeOpacity);
        p.setAttribute('stroke-dasharray', '2 3');
        p.setAttribute('points', values.map((v, i) => `${toX(i)},${toY(v)}`).join(' '));
        svg.appendChild(p);
      };
      makeEdge(bandP90, 0.5);
      makeEdge(bandP10, 0.5);
    }

    // Line
    if (n >= 2) {
      const linePts = this.data.map((v, i) => `${toX(i)},${toY(v)}`).join(' ');
      const poly = document.createElementNS(svgNS, 'polyline');
      poly.setAttribute('class', 'sc-line');
      poly.setAttribute('points', linePts);
      poly.setAttribute('stroke', this.color);
      svg.appendChild(poly);
    }

    // Per-period reference overlays — either a stepped dashed polyline
    // (single ``values`` series) or a filled band (``band`` with
    // ``lower`` + ``upper`` arrays, plus optional ``segmentValues`` for
    // dashed step-lines inside the band).
    if (this.referenceLines && this.referenceLines.length && n >= 1) {
      const halfStep = n > 1 ? xStep / 2 : (innerW / 2);
      const stepPath = (vals) => {
        const segs = [];
        for (let i = 0; i < n; i++) {
          const x0 = i === 0 ? PAD_L : toX(i) - halfStep;
          const x1 = i === n - 1 ? W - PAD_R : toX(i) + halfStep;
          const y = toY(vals[i]);
          segs.push(`M ${x0} ${y} L ${x1} ${y}`);
          if (i < n - 1) {
            const yNext = toY(vals[i + 1]);
            segs.push(`M ${x1} ${y} L ${x1} ${yNext}`);
          }
        }
        return segs.join(' ');
      };
      const stepOutline = (vals) => {
        // Returns an array of {x, y} points along the stepped polyline
        // used for building the filled band polygon.
        const pts = [];
        for (let i = 0; i < n; i++) {
          const x0 = i === 0 ? PAD_L : toX(i) - halfStep;
          const x1 = i === n - 1 ? W - PAD_R : toX(i) + halfStep;
          const y = toY(vals[i]);
          pts.push([x0, y]);
          pts.push([x1, y]);
        }
        return pts;
      };
      const makeDashedPath = (vals, color, opacity, dashArr) => {
        const path = document.createElementNS(svgNS, 'path');
        path.setAttribute('d', stepPath(vals));
        path.setAttribute('fill', 'none');
        path.setAttribute('stroke', color);
        path.setAttribute('stroke-width', 1.25);
        path.setAttribute('stroke-dasharray', dashArr);
        path.setAttribute('stroke-opacity', opacity);
        svg.appendChild(path);
      };
      const makeLabel = (val, color, text) => {
        const y = toY(val);
        const txt = document.createElementNS(svgNS, 'text');
        txt.setAttribute('x', W - PAD_R - 4);
        txt.setAttribute('y', y - 4);
        txt.setAttribute('text-anchor', 'end');
        txt.setAttribute('stroke', 'var(--bg-card)');
        txt.setAttribute('stroke-width', '3');
        txt.setAttribute('stroke-linejoin', 'round');
        txt.setAttribute('fill', color);
        txt.setAttribute('font-size', '9.5');
        txt.setAttribute('font-family', 'var(--mono)');
        txt.setAttribute('font-weight', '600');
        txt.setAttribute('paint-order', 'stroke');
        txt.textContent = text;
        svg.appendChild(txt);
      };

      this.referenceLines.forEach(rl => {
        const color = rl.color || 'var(--text-muted)';
        const opacity = rl.opacity ?? 0.85;
        const dashArray = rl.dashArray || '5 4';
        // ── Banded reference (discharge/charge PWL ramp zone) ────
        if (rl.band && rl.band.lower && rl.band.upper
            && rl.band.lower.length === n && rl.band.upper.length === n) {
          const upPts = stepOutline(rl.band.upper);
          const dnPts = stepOutline(rl.band.lower).slice().reverse();
          const poly = document.createElementNS(svgNS, 'polygon');
          poly.setAttribute('points',
            [...upPts, ...dnPts].map(p => `${p[0]},${p[1]}`).join(' '));
          poly.setAttribute('fill', color);
          poly.setAttribute('fill-opacity', rl.fillOpacity ?? 0.12);
          poly.setAttribute('stroke', 'none');
          svg.appendChild(poly);
          if (rl.segmentValues) {
            rl.segmentValues.forEach(vals => {
              if (vals.length === n) makeDashedPath(vals, color, 0.35, '2 3');
            });
          }
          if (rl.label) makeLabel(rl.band.upper[rl.band.upper.length - 1], color, rl.label);
          return;
        }
        // ── Single-value stepped line ─────────────────────────────
        const vals = rl.values;
        if (!vals || vals.length !== n) return;
        if (rl.smooth) {
          // Continuous polyline (no step risers) — used for overlays
          // like the MC sampled-price mean that track the P50 curve.
          const poly = document.createElementNS(svgNS, 'polyline');
          poly.setAttribute(
            'points',
            vals.map((v, i) => `${toX(i)},${toY(v)}`).join(' '),
          );
          poly.setAttribute('fill', 'none');
          poly.setAttribute('stroke', color);
          poly.setAttribute('stroke-width', rl.strokeWidth ?? 1.6);
          poly.setAttribute('stroke-dasharray', dashArray);
          poly.setAttribute('stroke-opacity', opacity);
          svg.appendChild(poly);
        } else {
          makeDashedPath(vals, color, opacity, dashArray);
        }
        if (rl.editable) this._installPwlDragPoints(svg, vals, rl, { n, toX, toY, fromY, PAD_T, innerH, color });
        if (rl.label) makeLabel(vals[vals.length - 1], color, rl.label);
      });
    }

    // Draggable points (stride at high N to keep DOM light)
    const pointStride = n > 160 ? Math.ceil(n / 160) : 1;
    const points = [];
    for (let i = 0; i < n; i += pointStride) {
      const circle = document.createElementNS(svgNS, 'circle');
      circle.setAttribute('class', 'sc-point');
      circle.setAttribute('cx', toX(i));
      circle.setAttribute('cy', toY(this.data[i]));
      circle.setAttribute('r', 3);
      circle.setAttribute('stroke', this.color);
      circle.dataset.idx = String(i);
      if (this.editable) {
        circle.addEventListener('pointerdown', (e) => this._onPointerDown(e, i, toY, fromY));
      } else {
        circle.style.cursor = 'default';
      }
      svg.appendChild(circle);
      const text = document.createElementNS(svgNS, 'text');
      text.setAttribute('class', 'sc-point-value');
      text.setAttribute('x', toX(i));
      text.setAttribute('y', toY(this.data[i]) - 9);
      text.textContent = this.formatValue(this.data[i]);
      svg.appendChild(text);
      points.push({ circle, text, i });
    }

    // Band edit handles — shown only when the user has turned on
    // "Edit bounds" mode on this chart (band.editable === true).
    // Smaller than P50 circles so they don't steal focus.
    const bandPoints = [];
    if (hasBand && this.band.editable && this.editable) {
      for (let i = 0; i < n; i += pointStride) {
        const makeBandDot = (kind, value) => {
          const c = document.createElementNS(svgNS, 'circle');
          c.setAttribute('class', `sc-band-point sc-band-${kind}`);
          c.setAttribute('cx', toX(i));
          c.setAttribute('cy', toY(value));
          c.setAttribute('r', 2.5);
          c.setAttribute('stroke', this.color);
          c.setAttribute('fill', 'var(--bg-card)');
          c.setAttribute('stroke-width', 1);
          c.addEventListener('pointerdown', (e) => this._onBandPointerDown(e, i, kind, toY, fromY));
          svg.appendChild(c);
          return c;
        };
        const p90dot = makeBandDot('p90', this.band.p90[i]);
        const p10dot = makeBandDot('p10', this.band.p10[i]);
        bandPoints.push({ p10dot, p90dot, i });
      }
    }

    this.container.appendChild(svg);
    this.svg = svg;
    this._points = points;
    this._bandPoints = bandPoints;
    this._toX = toX;
    this._toY = toY;
    this._PAD_T = PAD_T;
    this._innerH = innerH;
    this._hasBand = hasBand;

    // Per-period hover tooltip + crosshair. Shows the primary value,
    // P10/P90 (if a band is present), and any reference-line values at
    // the hovered period so the user can read out the full stack.
    const fmt = this.formatValue;
    const seriesName = this.seriesName;
    const color = this.color;
    const band = this.band;
    const refs = this.referenceLines || [];
    installChartHover(this.container, svg,
      { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W },
      (i) => {
        const rows = [];
        rows.push(
          `<div class="sc-tooltip-row"><span><span class="sw" style="background:${color}"></span>${escapeHtml(seriesName)}</span>` +
          `<span class="val">${fmt(this.data[i])}</span></div>`
        );
        if (band && band.p10 && band.p90 && band.p10.length === n && band.p90.length === n) {
          rows.push(
            `<div class="sc-tooltip-row"><span>P10 – P90</span>` +
            `<span class="val">${fmt(band.p10[i])} – ${fmt(band.p90[i])}</span></div>`
          );
        }
        refs.forEach(rl => {
          const rlColor = rl.color || 'var(--text-muted)';
          // Band refs without a name are visual-only context (the
          // envelope fill). Skip them in the tooltip — the individual
          // segment lines carry the per-period info.
          if (rl.band && rl.band.lower && rl.band.upper
              && rl.band.lower.length === n && rl.band.upper.length === n) {
            if (!rl.name) return;
            rows.push(
              `<div class="sc-tooltip-row"><span><span class="sw" style="background:${rlColor}"></span>${escapeHtml(rl.name)}</span>` +
              `<span class="val">${fmt(rl.band.lower[i])} – ${fmt(rl.band.upper[i])}</span></div>`
            );
          } else if (rl.values && rl.values.length === n) {
            const name = rl.name || rl.label;
            if (!name) return;
            rows.push(
              `<div class="sc-tooltip-row"><span><span class="sw" style="background:${rlColor}"></span>${escapeHtml(name)}</span>` +
              `<span class="val">${fmt(rl.values[i])}</span></div>`
            );
          }
        });
        return `<div class="sc-tooltip-title">${escapeHtml(periodTimeLabel(i))}</div>${rows.join('')}`;
      });
  }

  _onBandPointerDown(e, idx, kind, toY, fromY) {
    if (!this.band || !this.band.editable) return;
    e.preventDefault();
    e.stopPropagation();
    const bp = this._bandPoints.find(p => p.i === idx);
    if (!bp) return;
    const dot = kind === 'p90' ? bp.p90dot : bp.p10dot;
    dot.setPointerCapture(e.pointerId);
    dot.classList.add('dragging');
    const onMove = (ev) => {
      const rect = this.svg.getBoundingClientRect();
      const svgY = (ev.clientY - rect.top) * (this.svg.viewBox.baseVal.height / rect.height);
      const clampedY = Math.max(this._PAD_T, Math.min(this._PAD_T + this._innerH, svgY));
      const raw = fromY(clampedY);
      let newVal = (this.min === null || this.min === undefined) ? raw : Math.max(this.min, raw);
      // Preserve ordering: P10 ≤ P50 ≤ P90.
      const p50v = this.data[idx];
      if (kind === 'p90') newVal = Math.max(p50v, newVal);
      else newVal = Math.min(p50v, newVal);
      if (kind === 'p90') this.band.p90[idx] = newVal;
      else this.band.p10[idx] = newVal;
      dot.setAttribute('cy', toY(newVal));
      this._updateBandLive();
    };
    const onUp = () => {
      dot.classList.remove('dragging');
      dot.releasePointerCapture(e.pointerId);
      dot.removeEventListener('pointermove', onMove);
      dot.removeEventListener('pointerup', onUp);
      dot.removeEventListener('pointercancel', onUp);
      if (this.band.onBandChange) {
        this.band.onBandChange(this.band.p10.slice(), this.band.p90.slice(), idx, kind);
      }
    };
    dot.addEventListener('pointermove', onMove);
    dot.addEventListener('pointerup', onUp);
    dot.addEventListener('pointercancel', onUp);
  }

  _updateBandLive() {
    if (!this._hasBand) return;
    const n = this.data.length;
    const toX = this._toX;
    const toY = this._toY;
    const PAD_T = this._PAD_T;
    const innerH = this._innerH;
    const bandPoly = this.svg.querySelector('polygon.sc-dist-band');
    if (bandPoly) {
      const up = this.band.p90.map((v, i) => `${toX(i)},${toY(v)}`);
      const dn = this.band.p10.map((v, i) => `${toX(i)},${toY(v)}`).reverse();
      bandPoly.setAttribute('points', [...up, ...dn].join(' '));
    }
    const edges = this.svg.querySelectorAll('polyline');
    // Dashed band edges: the two dashed polylines are indexed after
    // the solid line; find them by stroke-dasharray.
    edges.forEach(edge => {
      if (edge.getAttribute('stroke-dasharray') !== '2 3') return;
      // We don't know which edge (p10 or p90) just by looking, but
      // repainting both is cheap — the two edges are stored in order
      // (p90 then p10). Compare the first y value to decide.
      const firstY = parseFloat(edge.getAttribute('points').split(',')[1]);
      const p90Y = toY(this.band.p90[0]);
      const isP90 = Math.abs(firstY - p90Y) < Math.abs(firstY - toY(this.band.p10[0]));
      const vals = isP90 ? this.band.p90 : this.band.p10;
      edge.setAttribute('points', vals.map((v, i) => `${toX(i)},${toY(v)}`).join(' '));
    });
  }

  _onPointerDown(e, idx, toY, fromY) {
    e.preventDefault();
    const point = this._points.find(p => p.i === idx);
    if (!point) return;
    point.circle.setPointerCapture(e.pointerId);
    point.circle.classList.add('dragging');

    const onMove = (ev) => {
      const rect = this.svg.getBoundingClientRect();
      const svgY = (ev.clientY - rect.top) * (this.svg.viewBox.baseVal.height / rect.height);
      const clampedY = Math.max(this._PAD_T, Math.min(this._PAD_T + this._innerH, svgY));
      const raw = fromY(clampedY);
      const newVal = (this.min === null || this.min === undefined) ? raw : Math.max(this.min, raw);
      this.data[idx] = newVal;
      point.circle.setAttribute('cy', clampedY);
      point.text.setAttribute('y', clampedY - 9);
      point.text.textContent = this.formatValue(newVal);
      this._updatePathsLive();
    };
    const onUp = () => {
      point.circle.classList.remove('dragging');
      point.circle.releasePointerCapture(e.pointerId);
      point.circle.removeEventListener('pointermove', onMove);
      point.circle.removeEventListener('pointerup', onUp);
      point.circle.removeEventListener('pointercancel', onUp);
      this.onChange(this.data.slice());
    };
    point.circle.addEventListener('pointermove', onMove);
    point.circle.addEventListener('pointerup', onUp);
    point.circle.addEventListener('pointercancel', onUp);
  }

  // Install per-period draggable handles on a PWL segment line so the
  // user can customize that segment's bid per period directly from the
  // main price chart.
  _installPwlDragPoints(svg, values, rl, geom) {
    const svgNS = 'http://www.w3.org/2000/svg';
    const { n, toX, toY, fromY, PAD_T, innerH, color } = geom;
    const stride = n > 160 ? Math.ceil(n / 160) : 1;
    for (let i = 0; i < n; i += stride) {
      const dot = document.createElementNS(svgNS, 'circle');
      dot.setAttribute('class', 'sc-pwl-point');
      dot.setAttribute('cx', toX(i));
      dot.setAttribute('cy', toY(values[i]));
      dot.setAttribute('r', 3.2);
      dot.setAttribute('fill', 'var(--bg-card)');
      dot.setAttribute('stroke', color);
      dot.setAttribute('stroke-width', 1.6);
      dot.addEventListener('pointerdown', (e) => {
        e.preventDefault();
        e.stopPropagation();
        dot.setPointerCapture(e.pointerId);
        dot.classList.add('dragging');
        const onMove = (ev) => {
          const rect = this.svg.getBoundingClientRect();
          const svgY = (ev.clientY - rect.top) * (this.svg.viewBox.baseVal.height / rect.height);
          const clampedY = Math.max(PAD_T, Math.min(PAD_T + innerH, svgY));
          const newVal = Math.max(0, fromY(clampedY));
          values[i] = newVal;
          dot.setAttribute('cy', toY(newVal));
          if (rl.editable && rl.editable.onDrag) {
            rl.editable.onDrag(i, newVal);
          }
          this._updatePwlPathLive(dot, values);
        };
        const onUp = () => {
          dot.classList.remove('dragging');
          dot.releasePointerCapture(e.pointerId);
          dot.removeEventListener('pointermove', onMove);
          dot.removeEventListener('pointerup', onUp);
          dot.removeEventListener('pointercancel', onUp);
          if (rl.editable && rl.editable.onDragEnd) rl.editable.onDragEnd();
        };
        dot.addEventListener('pointermove', onMove);
        dot.addEventListener('pointerup', onUp);
        dot.addEventListener('pointercancel', onUp);
      });
      svg.appendChild(dot);
    }
  }

  _updatePwlPathLive(dot, values) {
    // Find the sibling path rendering THIS segment line (the one that
    // was drawn just before the dots) and rebuild its d-attribute from
    // the mutated values. Cheap: re-render the whole chart on drop.
    // Live-update just the nearest dashed path for feedback.
    // (Simpler implementation: noop — the dot moves and values update;
    // full re-render happens on onDragEnd.)
  }

  _updatePathsLive() {
    const poly = this.svg.querySelector('polyline.sc-line');
    const area = this.svg.querySelector('polygon');
    const n = this.data.length;
    if (poly) {
      poly.setAttribute(
        'points',
        this.data.map((v, i) => `${this._toX(i)},${this._toY(v)}`).join(' ')
      );
    }
    if (area) {
      const areaPts = [
        `${this._toX(0)},${this._PAD_T + this._innerH}`,
        ...this.data.map((v, i) => `${this._toX(i)},${this._toY(v)}`),
        `${this._toX(n - 1)},${this._PAD_T + this._innerH}`,
      ].join(' ');
      area.setAttribute('points', areaPts);
    }
  }
}

// ═══ 3. Read-only charts ══════════════════════════════════════════════

// Human label for a period index (e.g. "P12 · 13:00") using the current
// scenario's time axis. Falls back to "Period N" if the axis is missing.
function periodTimeLabel(i) {
  const t = state.scenario && state.scenario.time_axis;
  if (!t || !t.start_iso) return `Period ${i}`;
  const start = new Date(t.start_iso);
  if (isNaN(start.getTime())) return `Period ${i}`;
  const dt = new Date(start.getTime() + i * (t.resolution_minutes || 60) * 60000);
  const pad = (v) => String(v).padStart(2, '0');
  return `P${i} · ${pad(dt.getHours())}:${pad(dt.getMinutes())}`;
}

// Build x-axis tick positions + labels for a chart spanning ``n``
// periods. Picks a "nice" interval (15m / 30m / 1h / 2h / 3h / 6h /
// 12h / 1d / 2d / 7d) so that no more than ``maxTicks`` labels fit,
// snaps to the chart's resolution, and formats labels by horizon:
//   * sub-day:  "06:00"
//   * sub-week + sub-day interval:  "04/18 06:00"
//   * day-or-coarser interval:      "04/18"
// Falls back to period indices when the time axis is missing/invalid.
function buildTimeAxisTicks(n, maxTicks = 8) {
  const t = state.scenario && state.scenario.time_axis;
  const start = t && t.start_iso ? new Date(t.start_iso) : null;
  const resMin = t && t.resolution_minutes;
  const valid = start && !isNaN(start.getTime()) && resMin > 0;
  if (!valid) {
    const stride = Math.max(1, Math.floor((n - 1) / Math.max(1, maxTicks - 1)));
    const out = [];
    for (let i = 0; i < n; i += stride) out.push({ i, label: String(i) });
    return out;
  }
  const candidates = [15, 30, 60, 120, 180, 360, 720, 1440, 2880, 10080];
  const target = (n * resMin) / Math.max(1, maxTicks);
  let intervalMin = candidates[candidates.length - 1];
  for (const c of candidates) { if (c >= target) { intervalMin = c; break; } }
  const stride = Math.max(1, Math.round(intervalMin / resMin));
  const totalH = (n * resMin) / 60;
  const pad = (v) => String(v).padStart(2, '0');
  const fmt = totalH <= 24
    ? (d) => `${pad(d.getHours())}:${pad(d.getMinutes())}`
    : intervalMin < 1440
    ? (d) => `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
    : (d) => `${pad(d.getMonth() + 1)}/${pad(d.getDate())}`;
  const out = [];
  for (let i = 0; i < n; i += stride) {
    const d = new Date(start.getTime() + i * resMin * 60000);
    out.push({ i, label: fmt(d) });
  }
  return out;
}

// Install a hover tooltip + vertical crosshair on a rendered chart SVG.
// ``geom`` carries the axis geometry; ``tooltipContent(i)`` returns
// HTML for the period-index i or null to hide.
function installChartHover(container, svg, geom, tooltipContent) {
  const { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W } = geom;
  if (!n) return;
  if (getComputedStyle(container).position === 'static') {
    container.style.position = 'relative';
  }
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
  const hide = () => {
    tooltip.classList.remove('visible');
    crosshair.style.display = 'none';
  };
  svg.addEventListener('pointermove', (ev) => {
    const rect = svg.getBoundingClientRect();
    const vbW = svg.viewBox.baseVal.width;
    const scaleX = vbW / rect.width;
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
    const ttW = tooltip.offsetWidth;
    const ttH = tooltip.offsetHeight;
    let left = relX + 12;
    let top = relY + 12;
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

// Stacked dispatch chart: per period, up-products stack above zero,
// down-products below. Energy (discharge / charge) + AS awards share
// the column; dashed lines mark pmax / pmin bounds.
class DispatchChart {
  constructor(container, opts) {
    this.container = container;
    this.data = opts.data || [];     // [{up: [{mw, color, label}], down: [...]}]
    this.upBound = opts.upBound ?? 0;     // +discharge_max_mw
    this.downBound = opts.downBound ?? 0; // |charge_max_mw|
    // Optional Monte Carlo fan band — per-period quantiles of NET MW
    // (discharge − charge). Arrays of length n; when set, a shaded band
    // + median line overlay the stacked bars.
    this.fanBand = opts.fanBand || null;
    this.render();
  }
  setData(data, opts = {}) {
    this.data = data;
    if (opts.upBound !== undefined) this.upBound = opts.upBound;
    if (opts.downBound !== undefined) this.downBound = opts.downBound;
    if ('fanBand' in opts) this.fanBand = opts.fanBand || null;
    this.render();
  }
  render() {
    const W = this.container.clientWidth || 400;
    const H = this.container.clientHeight || 140;
    const PAD_L = 36, PAD_R = 8, PAD_T = 6, PAD_B = 15;
    const innerW = W - PAD_L - PAD_R;
    const innerH = H - PAD_T - PAD_B;
    const n = this.data.length;
    if (n === 0) { this.container.innerHTML = '<div class="empty">—</div>'; return; }

    // Span = max(bounds, observed sum) so bounds are always visible
    // and over-utilization (shouldn't happen but safety) also shows.
    const maxUpObserved = Math.max(
      0,
      ...this.data.map(d => (d.up || []).reduce((s, seg) => s + (seg.mw || 0), 0)),
    );
    const maxDownObserved = Math.max(
      0,
      ...this.data.map(d => (d.down || []).reduce((s, seg) => s + (seg.mw || 0), 0)),
    );
    // If a MC fan band is present, expand the span to include its envelope
    // so the overlay always fits on screen.
    let fanMaxAbs = 0;
    if (this.fanBand && this.fanBand.upper && this.fanBand.lower
        && this.fanBand.upper.length === n && this.fanBand.lower.length === n) {
      for (let i = 0; i < n; i++) {
        fanMaxAbs = Math.max(fanMaxAbs, Math.abs(this.fanBand.upper[i]),
                             Math.abs(this.fanBand.lower[i]));
      }
    }
    const span = Math.max(
      1e-6,
      this.upBound, this.downBound,
      maxUpObserved, maxDownObserved, fanMaxAbs,
    ) * 1.1;

    const zeroY = PAD_T + innerH / 2;
    const scaleY = (innerH / 2) / span;
    const barW = Math.max(1, innerW / n - 0.5);

    const svgNS = 'http://www.w3.org/2000/svg';
    this.container.innerHTML = '';
    const svg = document.createElementNS(svgNS, 'svg');
    svg.setAttribute('class', 'sc-chart');
    svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
    svg.setAttribute('preserveAspectRatio', 'none');

    // Defs: one diagonal-stripe pattern per AS segment color. Ensures
    // reservation bars read as "capacity held" vs. solid energy bars
    // that read as "power actually flowing".
    const defs = document.createElementNS(svgNS, 'defs');
    const patternIds = new Map();
    const registerHatch = (color) => {
      if (patternIds.has(color)) return patternIds.get(color);
      const id = `sc-hatch-${patternIds.size}`;
      const pat = document.createElementNS(svgNS, 'pattern');
      pat.setAttribute('id', id);
      pat.setAttribute('patternUnits', 'userSpaceOnUse');
      pat.setAttribute('width', '5');
      pat.setAttribute('height', '5');
      pat.setAttribute('patternTransform', 'rotate(45)');
      const bg = document.createElementNS(svgNS, 'rect');
      bg.setAttribute('width', '5');
      bg.setAttribute('height', '5');
      bg.setAttribute('fill', color);
      bg.setAttribute('fill-opacity', '0.18');
      pat.appendChild(bg);
      const stripe = document.createElementNS(svgNS, 'line');
      stripe.setAttribute('x1', '0');
      stripe.setAttribute('y1', '0');
      stripe.setAttribute('x2', '0');
      stripe.setAttribute('y2', '5');
      stripe.setAttribute('stroke', color);
      stripe.setAttribute('stroke-width', '2.2');
      stripe.setAttribute('stroke-opacity', '0.9');
      pat.appendChild(stripe);
      defs.appendChild(pat);
      patternIds.set(color, id);
      return id;
    };
    svg.appendChild(defs);

    // Y gridlines
    [-span, -span/2, span/2, span].forEach(v => {
      const y = zeroY - v * scaleY;
      const gl = document.createElementNS(svgNS, 'line');
      gl.setAttribute('class', 'sc-grid-line');
      gl.setAttribute('x1', PAD_L); gl.setAttribute('x2', W - PAD_R);
      gl.setAttribute('y1', y); gl.setAttribute('y2', y);
      svg.appendChild(gl);
    });
    // Y labels
    [-span, -span/2, 0, span/2, span].forEach(v => {
      const y = zeroY - v * scaleY;
      const label = document.createElementNS(svgNS, 'text');
      label.setAttribute('class', 'sc-axis-label');
      label.setAttribute('x', PAD_L - 3);
      label.setAttribute('y', y + 3);
      label.setAttribute('text-anchor', 'end');
      label.textContent = v === 0 ? '0' : v.toFixed(Math.abs(v) < 10 ? 1 : 0);
      svg.appendChild(label);
    });

    // Zero line
    const zeroLine = document.createElementNS(svgNS, 'line');
    zeroLine.setAttribute('class', 'sc-axis-line');
    zeroLine.setAttribute('x1', PAD_L); zeroLine.setAttribute('x2', W - PAD_R);
    zeroLine.setAttribute('y1', zeroY); zeroLine.setAttribute('y2', zeroY);
    svg.appendChild(zeroLine);

    // X labels — time-based ticks aligned to start_iso + resolution.
    buildTimeAxisTicks(n, 8).forEach(({ i, label: txt }) => {
      const x = PAD_L + i * (innerW / n) + barW / 2;
      const label = document.createElementNS(svgNS, 'text');
      label.setAttribute('class', 'sc-axis-label');
      label.setAttribute('x', x);
      label.setAttribute('y', H - 3);
      label.setAttribute('text-anchor', 'middle');
      label.textContent = txt;
      svg.appendChild(label);
    });

    // Stacked bars per period. ``seg.hatch === true`` swaps the flat
    // fill for a diagonal-stripe pattern keyed on the segment's color
    // so reservations (AS awards) visually separate from energy flow.
    const styleSeg = (rect, seg) => {
      if (seg.hatch) {
        const id = registerHatch(seg.color);
        rect.setAttribute('fill', `url(#${id})`);
        rect.setAttribute('stroke', seg.color);
        rect.setAttribute('stroke-width', '0.8');
        rect.setAttribute('stroke-opacity', '0.85');
      } else {
        rect.setAttribute('fill', seg.color);
        if (seg.opacity !== undefined) rect.setAttribute('fill-opacity', seg.opacity);
      }
    };
    this.data.forEach((d, i) => {
      const x = PAD_L + i * (innerW / n) + 0.5;
      // Up segments stack upward from zero
      let cursor = zeroY;
      (d.up || []).forEach(seg => {
        if (!seg.mw || seg.mw <= 1e-9) return;
        const h = seg.mw * scaleY;
        const rect = document.createElementNS(svgNS, 'rect');
        rect.setAttribute('class', 'sc-bar');
        rect.setAttribute('x', x); rect.setAttribute('width', barW);
        rect.setAttribute('y', cursor - h); rect.setAttribute('height', h);
        styleSeg(rect, seg);
        svg.appendChild(rect);
        cursor -= h;
      });
      // Down segments stack downward from zero
      cursor = zeroY;
      (d.down || []).forEach(seg => {
        if (!seg.mw || seg.mw <= 1e-9) return;
        const h = seg.mw * scaleY;
        const rect = document.createElementNS(svgNS, 'rect');
        rect.setAttribute('class', 'sc-bar');
        rect.setAttribute('x', x); rect.setAttribute('width', barW);
        rect.setAttribute('y', cursor); rect.setAttribute('height', h);
        styleSeg(rect, seg);
        svg.appendChild(rect);
        cursor += h;
      });
    });

    // Dashed bound lines at +pmax and -pmin
    const drawBound = (y, label) => {
      const line = document.createElementNS(svgNS, 'line');
      line.setAttribute('x1', PAD_L); line.setAttribute('x2', W - PAD_R);
      line.setAttribute('y1', y); line.setAttribute('y2', y);
      line.setAttribute('stroke', 'rgba(248,113,113,0.5)');
      line.setAttribute('stroke-width', 1);
      line.setAttribute('stroke-dasharray', '3 3');
      svg.appendChild(line);
      if (label) {
        const text = document.createElementNS(svgNS, 'text');
        text.setAttribute('class', 'sc-axis-label');
        text.setAttribute('fill', 'rgba(248,113,113,0.7)');
        text.setAttribute('x', W - PAD_R - 2);
        text.setAttribute('y', y - 2);
        text.setAttribute('text-anchor', 'end');
        text.textContent = label;
        svg.appendChild(text);
      }
    };
    if (this.upBound > 1e-9) drawBound(zeroY - this.upBound * scaleY, `pmax ${this.upBound.toFixed(0)}`);
    if (this.downBound > 1e-9) drawBound(zeroY + this.downBound * scaleY, `−pmax ${this.downBound.toFixed(0)}`);

    // Monte Carlo fan band overlay: a shaded polygon spanning the
    // per-period net-MW P10–P90 range, with a median line on top.
    if (this.fanBand && this.fanBand.upper && this.fanBand.lower && n >= 2) {
      const xCenter = (i) => PAD_L + i * (innerW / n) + barW / 2;
      const netY = (v) => zeroY - v * scaleY;
      const upPts = this.fanBand.upper.map((v, i) => `${xCenter(i)},${netY(v)}`);
      const dnPts = this.fanBand.lower.map((v, i) => `${xCenter(i)},${netY(v)}`).reverse();
      const poly = document.createElementNS(svgNS, 'polygon');
      poly.setAttribute('class', 'sc-fan-band');
      poly.setAttribute('points', [...upPts, ...dnPts].join(' '));
      svg.appendChild(poly);
      // Dashed edges on the P10 and P90 lines for clarity.
      const edge = (vals) => {
        const p = document.createElementNS(svgNS, 'polyline');
        p.setAttribute('class', 'sc-fan-edge');
        p.setAttribute('points', vals.map((v, i) => `${xCenter(i)},${netY(v)}`).join(' '));
        svg.appendChild(p);
      };
      edge(this.fanBand.upper);
      edge(this.fanBand.lower);
      if (this.fanBand.median && this.fanBand.median.length === n) {
        const med = document.createElementNS(svgNS, 'polyline');
        med.setAttribute('class', 'sc-fan-median');
        med.setAttribute('points',
          this.fanBand.median.map((v, i) => `${xCenter(i)},${netY(v)}`).join(' '));
        svg.appendChild(med);
      }
    }

    this.container.appendChild(svg);

    installChartHover(this.container, svg,
      { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W },
      (i) => {
        const d = this.data[i] || { up: [], down: [] };
        const rows = [];
        let netUp = 0, netDn = 0;
        (d.up || []).forEach(seg => {
          if (!(seg.mw > 1e-9)) return;
          netUp += seg.mw;
          rows.push(
            `<div class="sc-tooltip-row"><span><span class="sw" style="background:${seg.color}"></span>${escapeHtml(seg.label)}</span>` +
            `<span class="val">+${seg.mw.toFixed(2)} MW</span></div>`
          );
        });
        (d.down || []).forEach(seg => {
          if (!(seg.mw > 1e-9)) return;
          netDn += seg.mw;
          rows.push(
            `<div class="sc-tooltip-row"><span><span class="sw" style="background:${seg.color}"></span>${escapeHtml(seg.label)}</span>` +
            `<span class="val">−${seg.mw.toFixed(2)} MW</span></div>`
          );
        });
        if (!rows.length) {
          rows.push('<div class="sc-tooltip-row"><span>Idle</span><span class="val">0 MW</span></div>');
        }
        const net = netUp - netDn;
        const sign = net >= 0 ? '+' : '−';
        rows.push(
          `<div class="sc-tooltip-row net"><span>Net</span>` +
          `<span class="val">${sign}${Math.abs(net).toFixed(2)} MW</span></div>`
        );
        return `<div class="sc-tooltip-title">${escapeHtml(periodTimeLabel(i))}</div>${rows.join('')}`;
      });
  }
}

class LineReadOnlyChart {
  constructor(container, opts) {
    this.container = container;
    this.series = opts.series || [];
    this.yMin = opts.yMin ?? 0;
    this.yMax = opts.yMax ?? null;
    this.showLegend = opts.showLegend !== false;
    this.guideLines = opts.guideLines || [];
    this.unit = opts.unit || '';
    this.valuePrecision = opts.valuePrecision ?? 2;
    // Optional Monte Carlo fan band: {lower, median, upper} arrays of
    // length n. Rendered as a shaded polygon with dashed edges and a
    // median polyline, ignoring ``series`` when set.
    this.fanBand = opts.fanBand || null;
    this.render();
  }
  setSeries(series, opts = {}) {
    this.series = series;
    if (opts.yMin !== undefined) this.yMin = opts.yMin;
    if (opts.yMax !== undefined) this.yMax = opts.yMax;
    if (opts.guideLines !== undefined) this.guideLines = opts.guideLines;
    if (opts.unit !== undefined) this.unit = opts.unit;
    if (opts.valuePrecision !== undefined) this.valuePrecision = opts.valuePrecision;
    if ('fanBand' in opts) this.fanBand = opts.fanBand || null;
    this.render();
  }
  render() {
    const W = this.container.clientWidth || 400;
    const H = this.container.clientHeight || 120;
    const PAD_L = 36, PAD_R = 6, PAD_T = 6, PAD_B = 15;
    const innerW = W - PAD_L - PAD_R;
    const innerH = H - PAD_T - PAD_B;
    if (!this.series.length || !this.series[0].data.length) {
      this.container.innerHTML = '<div class="empty">—</div>';
      return;
    }
    const n = this.series[0].data.length;
    const allValues = this.series.flatMap(s => s.data);
    if (this.fanBand) {
      if (this.fanBand.upper) allValues.push(...this.fanBand.upper);
      if (this.fanBand.lower) allValues.push(...this.fanBand.lower);
    }
    const yMax = this.yMax ?? Math.max(1, ...allValues) * 1.08;
    const yMin = this.yMin ?? Math.min(0, ...allValues);
    const ySpan = yMax - yMin || 1;
    const toX = (i) => PAD_L + (i / Math.max(1, n - 1)) * innerW;
    const toY = (v) => PAD_T + innerH - ((v - yMin) / ySpan) * innerH;

    const svgNS = 'http://www.w3.org/2000/svg';
    this.container.innerHTML = '';
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
      const label = document.createElementNS(svgNS, 'text');
      label.setAttribute('class', 'sc-axis-label');
      label.setAttribute('x', PAD_L - 3);
      label.setAttribute('y', y + 3);
      label.setAttribute('text-anchor', 'end');
      label.textContent = v.toFixed(v === Math.floor(v) ? 0 : 1);
      svg.appendChild(label);
    }

    // Guide lines (e.g. SOC min/max)
    this.guideLines.forEach(g => {
      if (g.y < yMin || g.y > yMax) return;
      const y = toY(g.y);
      const line = document.createElementNS(svgNS, 'line');
      line.setAttribute('x1', PAD_L); line.setAttribute('x2', W - PAD_R);
      line.setAttribute('y1', y); line.setAttribute('y2', y);
      line.setAttribute('stroke', g.color);
      line.setAttribute('stroke-width', 1);
      line.setAttribute('stroke-dasharray', '3 3');
      svg.appendChild(line);
    });

    // X labels — time-based ticks aligned to start_iso + resolution.
    buildTimeAxisTicks(n, 8).forEach(({ i, label: txt }) => {
      const label = document.createElementNS(svgNS, 'text');
      label.setAttribute('class', 'sc-axis-label');
      label.setAttribute('x', toX(i));
      label.setAttribute('y', PAD_T + innerH + 11);
      label.setAttribute('text-anchor', 'middle');
      label.textContent = txt;
      svg.appendChild(label);
    });

    // Monte Carlo fan band. Rendered underneath the series line so the
    // primary values stay legible on top of the shaded envelope.
    if (this.fanBand && this.fanBand.upper && this.fanBand.lower
        && this.fanBand.upper.length === n && this.fanBand.lower.length === n && n >= 2) {
      const upPts = this.fanBand.upper.map((v, i) => `${toX(i)},${toY(v)}`);
      const dnPts = this.fanBand.lower.map((v, i) => `${toX(i)},${toY(v)}`).reverse();
      const poly = document.createElementNS(svgNS, 'polygon');
      poly.setAttribute('class', 'sc-fan-band');
      poly.setAttribute('points', [...upPts, ...dnPts].join(' '));
      svg.appendChild(poly);
      const edge = (vals) => {
        const p = document.createElementNS(svgNS, 'polyline');
        p.setAttribute('class', 'sc-fan-edge');
        p.setAttribute('points', vals.map((v, i) => `${toX(i)},${toY(v)}`).join(' '));
        svg.appendChild(p);
      };
      edge(this.fanBand.upper);
      edge(this.fanBand.lower);
      if (this.fanBand.median && this.fanBand.median.length === n) {
        const med = document.createElementNS(svgNS, 'polyline');
        med.setAttribute('class', 'sc-fan-median');
        med.setAttribute('points',
          this.fanBand.median.map((v, i) => `${toX(i)},${toY(v)}`).join(' '));
        svg.appendChild(med);
      }
    }

    this.series.forEach(s => {
      const poly = document.createElementNS(svgNS, 'polyline');
      poly.setAttribute('fill', 'none');
      poly.setAttribute('stroke', s.color);
      poly.setAttribute('stroke-width', 2);
      poly.setAttribute('points', s.data.map((v, i) => `${toX(i)},${toY(v)}`).join(' '));
      svg.appendChild(poly);
    });

    if (this.showLegend && this.series.length > 1) {
      let legX = PAD_L + 4;
      this.series.forEach(s => {
        if (!s.name) return;
        const line = document.createElementNS(svgNS, 'line');
        line.setAttribute('x1', legX); line.setAttribute('x2', legX + 12);
        line.setAttribute('y1', PAD_T + 5); line.setAttribute('y2', PAD_T + 5);
        line.setAttribute('stroke', s.color); line.setAttribute('stroke-width', 2);
        svg.appendChild(line);
        const label = document.createElementNS(svgNS, 'text');
        label.setAttribute('class', 'sc-axis-label');
        label.setAttribute('x', legX + 15);
        label.setAttribute('y', PAD_T + 8);
        label.textContent = s.name;
        svg.appendChild(label);
        legX += 15 + (s.name.length * 5.5) + 12;
      });
    }

    this.container.appendChild(svg);

    const unit = this.unit;
    const prec = this.valuePrecision;
    installChartHover(this.container, svg,
      { n, PAD_L, PAD_R, PAD_T, innerW, innerH, W },
      (i) => {
        const rows = this.series.map(s => {
          const v = s.data[i];
          if (v === undefined || v === null) return '';
          const name = s.name || 'value';
          return (
            `<div class="sc-tooltip-row"><span><span class="sw" style="background:${s.color}"></span>${escapeHtml(name)}</span>` +
            `<span class="val">${Number(v).toFixed(prec)}${unit ? ' ' + unit : ''}</span></div>`
          );
        }).filter(Boolean);
        if (!rows.length) return null;
        return `<div class="sc-tooltip-title">${escapeHtml(periodTimeLabel(i))}</div>${rows.join('')}`;
      });
  }
}

// ═══ 4. Form ↔ state binding ══════════════════════════════════════════

function readAsset() {
  const s = state.scenario.site;
  s.bess_power_charge_mw = parseFloat($('inp-charge-mw').value) || 0;
  s.bess_power_discharge_mw = parseFloat($('inp-discharge-mw').value) || 0;
  s.bess_energy_mwh = parseFloat($('inp-energy').value) || 0;
  s.bess_charge_efficiency = parseFloat($('inp-eff-charge').value) || 0;
  s.bess_discharge_efficiency = parseFloat($('inp-eff-discharge').value) || 0;
  s.bess_soc_min_fraction = parseFloat($('inp-soc-min').value) || 0;
  s.bess_soc_max_fraction = parseFloat($('inp-soc-max').value) || 1;
  // Foldback fields accept blank ("off") as well as a fraction.
  const foldDis = $('inp-foldback-dis').value.trim();
  const foldCh = $('inp-foldback-ch').value.trim();
  s.bess_discharge_foldback_fraction = foldDis === '' ? null : parseFloat(foldDis);
  s.bess_charge_foldback_fraction = foldCh === '' ? null : parseFloat(foldCh);
  s.bess_initial_soc_mwh = parseFloat($('inp-soc-init').value) || 0;
  s.bess_degradation_cost_per_mwh = parseFloat($('inp-deg').value) || 0;
  // POI is no longer configured from the UI — the server derives a
  // non-binding default from the BESS MW limits. Same for the legacy
  // single-field round-trip, which the server will split sqrt-wise if it
  // slips back in from an older saved scenario.
  delete s.poi_limit_mw;
  delete s.bess_round_trip_efficiency;
}
function writeAsset() {
  const s = state.scenario.site;
  $('inp-charge-mw').value = s.bess_power_charge_mw;
  $('inp-discharge-mw').value = s.bess_power_discharge_mw;
  $('inp-energy').value = s.bess_energy_mwh;
  // Back-compat: if an older scenario carries only the single round-trip
  // number, split it sqrt-wise into the two UI fields.
  if (s.bess_charge_efficiency === undefined || s.bess_discharge_efficiency === undefined) {
    if (s.bess_round_trip_efficiency !== undefined) {
      const leg = Math.sqrt(Math.max(0, s.bess_round_trip_efficiency));
      s.bess_charge_efficiency = leg;
      s.bess_discharge_efficiency = leg;
    } else {
      s.bess_charge_efficiency = 0.90;
      s.bess_discharge_efficiency = 0.98;
    }
  }
  $('inp-eff-charge').value = s.bess_charge_efficiency;
  $('inp-eff-discharge').value = s.bess_discharge_efficiency;
  $('inp-soc-min').value = s.bess_soc_min_fraction;
  $('inp-soc-max').value = s.bess_soc_max_fraction;
  const foldDis = s.bess_discharge_foldback_fraction;
  const foldCh = s.bess_charge_foldback_fraction;
  $('inp-foldback-dis').value = (foldDis === null || foldDis === undefined) ? '' : foldDis;
  $('inp-foldback-ch').value = (foldCh === null || foldCh === undefined) ? '' : foldCh;
  $('inp-soc-init').value = s.bess_initial_soc_mwh;
  $('inp-deg').value = s.bess_degradation_cost_per_mwh;
}

function readTimeAxis() {
  const t = state.scenario.time_axis;
  const startStr = $('inp-start').value;
  if (startStr) t.start_iso = new Date(startStr).toISOString().slice(0, 19);
  const horizonH = parseInt($('inp-horizon').value, 10) || 24;
  const resMin = parseInt($('sel-resolution').value, 10) || 60;
  t.horizon_minutes = horizonH * 60;
  t.resolution_minutes = resMin;
  t.periods = Math.floor(t.horizon_minutes / t.resolution_minutes);
}
function writeTimeAxis() {
  const t = state.scenario.time_axis;
  const d = new Date(t.start_iso);
  if (!isNaN(d.getTime())) {
    const pad = (n) => String(n).padStart(2, '0');
    $('inp-start').value =
      `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }
  $('inp-horizon').value = Math.floor(t.horizon_minutes / 60);
  $('sel-resolution').value = String(t.resolution_minutes);
  renderPeriodsDisplay();
}
function renderPeriodsDisplay() {
  const t = state.scenario.time_axis;
  $('periods-display').textContent = `${t.periods} × ${t.resolution_minutes}min`;
}

function readPolicy() {
  state.scenario.policy.dispatch_mode = $('sel-dispatch-mode').value;
  state.scenario.policy.period_coupling = $('sel-period-coupling').value;
}
function writePolicy() {
  $('sel-dispatch-mode').value = state.scenario.policy.dispatch_mode || 'optimal_foresight';
  $('sel-period-coupling').value = state.scenario.policy.period_coupling || 'coupled';
  updateModeHint();
  const pwlVisible = state.scenario.policy.dispatch_mode === 'pwl_offers';
  $('pwl-section').style.display = pwlVisible ? '' : 'none';
}
function updateModeHint() {
  const key = `${state.scenario.policy.dispatch_mode},${state.scenario.policy.period_coupling}`;
  $('mode-hint').textContent = MODE_HINTS[key] || '';
}

// Normalize the PWL strategy to the multi-segment schema.  Keeps the
// rest of the UI decoupled from the legacy scalar format that older
// scenarios might still carry, and ensures per-period override matrices
// exist so edits can write into them directly.
function ensurePwlSegments(s) {
  if (!s) s = {};
  const dischargeMax = state.scenario?.site?.bess_power_discharge_mw || 25;
  const chargeMax = state.scenario?.site?.bess_power_charge_mw || 25;
  if (!Array.isArray(s.discharge_offer_segments) || s.discharge_offer_segments.length === 0) {
    if (typeof s.discharge_price === 'number' && typeof s.discharge_capacity_mw === 'number') {
      s.discharge_offer_segments = [[s.discharge_capacity_mw, s.discharge_price]];
    } else {
      s.discharge_offer_segments = [
        [dischargeMax * 0.25, 40],
        [dischargeMax * 0.5, 55],
        [dischargeMax * 0.75, 75],
        [dischargeMax, 95],
      ];
    }
  }
  if (!Array.isArray(s.charge_bid_segments) || s.charge_bid_segments.length === 0) {
    if (typeof s.charge_price === 'number' && typeof s.charge_capacity_mw === 'number') {
      s.charge_bid_segments = [[s.charge_capacity_mw, s.charge_price]];
    } else {
      s.charge_bid_segments = [
        [chargeMax * 0.25, 35],
        [chargeMax * 0.5, 25],
        [chargeMax * 0.75, 15],
        [chargeMax, 5],
      ];
    }
  }
  // Clean up the legacy scalar fields so they don't ship in the solve payload.
  delete s.discharge_capacity_mw;
  delete s.discharge_price;
  delete s.charge_capacity_mw;
  delete s.charge_price;
  s.as_offer_prices_per_mwh = s.as_offer_prices_per_mwh || {};
  // Resize per-period override matrices to match {periods × segments}.
  const n = (state.scenario?.lmp_forecast_per_mwh || []).length;
  s.discharge_offer_price_per_period = resizePerPeriodMatrix(
    s.discharge_offer_price_per_period, n, s.discharge_offer_segments.length,
  );
  s.charge_bid_price_per_period = resizePerPeriodMatrix(
    s.charge_bid_price_per_period, n, s.charge_bid_segments.length,
  );
  return s;
}

// Resize ``mat`` to ``[periods × segments]``, defaulting any missing
// cells to ``null`` (= use scalar baseline). Returns ``null`` when the
// matrix contains no overrides so the solve payload stays sparse.
function resizePerPeriodMatrix(mat, periods, segments) {
  if (periods === 0) return null;
  const out = Array.from({ length: periods }, (_, p) => {
    const prev = (mat && mat[p]) || null;
    return Array.from({ length: segments }, (_, s) => {
      if (!prev) return null;
      const v = prev[s];
      return (v === null || v === undefined || !isFinite(v)) ? null : v;
    });
  });
  // Drop the whole matrix when nothing is actually overridden.
  const anyCustom = out.some(row => row.some(v => v !== null));
  return anyCustom ? out : null;
}

// Read the effective per-period price array for a single segment —
// returns a length-``periods`` array where each entry is the override
// (if any) or the scalar baseline price.
function effectiveSegmentPrices(directionKey, segmentIdx) {
  const s = state.scenario.pwl_strategy || {};
  const segments = directionKey === 'discharge'
    ? (s.discharge_offer_segments || [])
    : (s.charge_bid_segments || []);
  const matrix = directionKey === 'discharge'
    ? s.discharge_offer_price_per_period
    : s.charge_bid_price_per_period;
  const n = (state.scenario.lmp_forecast_per_mwh || []).length;
  const baseline = segments[segmentIdx]?.[1] ?? 0;
  const out = new Array(n);
  for (let i = 0; i < n; i++) {
    const v = matrix && matrix[i] && matrix[i][segmentIdx];
    out[i] = (v === null || v === undefined || !isFinite(v)) ? baseline : v;
  }
  return out;
}

// Mutate the per-period override matrix: called on drag.
function setSegmentPriceOverride(directionKey, segmentIdx, periodIdx, price) {
  const s = state.scenario.pwl_strategy;
  const key = directionKey === 'discharge'
    ? 'discharge_offer_price_per_period' : 'charge_bid_price_per_period';
  const segments = directionKey === 'discharge'
    ? s.discharge_offer_segments : s.charge_bid_segments;
  const n = (state.scenario.lmp_forecast_per_mwh || []).length;
  if (!s[key]) {
    s[key] = Array.from({ length: n }, () =>
      Array.from({ length: segments.length }, () => null));
  }
  s[key][periodIdx][segmentIdx] = price;
}

// Clear all per-period overrides for one direction, or for both if
// ``direction`` is not given.
function clearPwlOverrides(direction) {
  const s = state.scenario.pwl_strategy;
  if (!direction || direction === 'discharge') s.discharge_offer_price_per_period = null;
  if (!direction || direction === 'charge')    s.charge_bid_price_per_period = null;
}

// True when there is at least one per-period override set for the
// given direction + segment.
function segmentHasOverrides(directionKey, segmentIdx) {
  const s = state.scenario.pwl_strategy || {};
  const matrix = directionKey === 'discharge'
    ? s.discharge_offer_price_per_period
    : s.charge_bid_price_per_period;
  if (!matrix) return false;
  return matrix.some(row => row && row[segmentIdx] !== null && row[segmentIdx] !== undefined);
}

function readPwl() {
  const s = ensurePwlSegments(state.scenario.pwl_strategy);
  s.discharge_offer_segments = readSegmentRows('pwl-discharge-segments');
  s.charge_bid_segments = readSegmentRows('pwl-charge-segments');
  (state.scenario.as_products || []).forEach(ap => {
    const input = document.getElementById(`inp-pwl-as-${ap.product_id}`);
    if (input) s.as_offer_prices_per_mwh[ap.product_id] = parseFloat(input.value) || 0;
  });
  state.scenario.pwl_strategy = s;
}

function readSegmentRows(containerId) {
  const container = $(containerId);
  if (!container) return [];
  const rows = container.querySelectorAll('.pwl-seg-row');
  const out = [];
  rows.forEach(row => {
    const mw = parseFloat(row.querySelector('.seg-mw')?.value) || 0;
    const price = parseFloat(row.querySelector('.seg-price')?.value) || 0;
    if (mw > 1e-9) out.push([mw, price]);
  });
  out.sort((a, b) => a[0] - b[0]);
  return out;
}

function writePwl() {
  const s = ensurePwlSegments(state.scenario.pwl_strategy);
  renderSegmentRows('pwl-discharge-segments', s.discharge_offer_segments);
  renderSegmentRows('pwl-charge-segments', s.charge_bid_segments);
  const grid = $('pwl-as-grid');
  grid.innerHTML = '';
  (state.scenario.as_products || []).forEach(ap => {
    const price = (s.as_offer_prices_per_mwh || {})[ap.product_id] ?? 0;
    const label = document.createElement('label');
    label.className = 'field-row';
    label.innerHTML = `
      <span class="field-label">${escapeHtml(shortProductName(ap.title))} <em>$/MWh</em></span>
      <input type="number" id="inp-pwl-as-${escapeHtml(ap.product_id)}" value="${price}" min="0" step="0.5">
    `;
    grid.appendChild(label);
  });
  state.scenario.pwl_strategy = s;
  attachPwlInputListeners();
  renderPwlEditorBar();
}

function renderSegmentRows(containerId, segments) {
  const container = $(containerId);
  if (!container) return;
  const direction = containerId.includes('discharge') ? 'discharge' : 'charge';
  container.innerHTML = '';
  const canRemove = segments.length > 1;
  segments.forEach(([mw, price], idx) => {
    const row = document.createElement('div');
    row.className = 'pwl-seg-row';
    row.innerHTML = `
      <input type="number" class="seg-mw" min="0" step="0.5" value="${mw}" data-idx="${idx}">
      <input type="number" class="seg-price" step="0.5" value="${price}" data-idx="${idx}">
      <button type="button" class="pwl-seg-remove"
              data-direction="${direction}" data-idx="${idx}"
              ${canRemove ? '' : 'disabled'}
              title="${canRemove ? 'Remove this band' : 'At least one band is required'}">−</button>
    `;
    container.appendChild(row);
  });
}

// Append a new PWL segment to the given direction. Heuristic: extrapolate
// from the current last segment — bump MW a notch (capped at the site
// power limit) and shift price by a direction-appropriate amount.
function addPwlSegment(direction) {
  const s = state.scenario.pwl_strategy;
  const segsKey = direction === 'discharge'
    ? 'discharge_offer_segments' : 'charge_bid_segments';
  const matKey = direction === 'discharge'
    ? 'discharge_offer_price_per_period' : 'charge_bid_price_per_period';
  const segs = s[segsKey];
  const siteMax = direction === 'discharge'
    ? (state.scenario.site?.bess_power_discharge_mw || 25)
    : (state.scenario.site?.bess_power_charge_mw || 25);
  const lastMw = segs.length ? segs[segs.length - 1][0] : 0;
  const lastPrice = segs.length ? segs[segs.length - 1][1] : 50;
  const gap = Math.max(1, (siteMax - lastMw) / 2);
  const newMw = Math.max(lastMw + 1, Math.min(siteMax, lastMw + gap));
  const priceStep = direction === 'discharge' ? 15 : -10;
  const newPrice = Math.max(0, lastPrice + priceStep);
  segs.push([newMw, newPrice]);
  // Extend each override row by one (null = use baseline for new segment).
  if (s[matKey]) {
    s[matKey] = s[matKey].map(row => row ? [...row, null] : row);
  }
}

function removePwlSegment(direction, idx) {
  const s = state.scenario.pwl_strategy;
  const segsKey = direction === 'discharge'
    ? 'discharge_offer_segments' : 'charge_bid_segments';
  const matKey = direction === 'discharge'
    ? 'discharge_offer_price_per_period' : 'charge_bid_price_per_period';
  const segs = s[segsKey];
  if (segs.length <= 1) return;
  segs.splice(idx, 1);
  if (s[matKey]) {
    s[matKey] = s[matKey].map(row =>
      row ? row.slice(0, idx).concat(row.slice(idx + 1)) : row);
    // Drop the whole matrix if nothing meaningful is left.
    if (!s[matKey].some(r => r && r.some(v => v !== null && v !== undefined))) {
      s[matKey] = null;
    }
  }
  // Keep ``state.activePwlSegment`` consistent with the trimmed list.
  const a = state.activePwlSegment;
  if (a && a.direction === direction) {
    if (a.index === idx) state.activePwlSegment = null;
    else if (a.index > idx) state.activePwlSegment = { ...a, index: a.index - 1 };
  }
}

function onPwlAdd(direction) {
  addPwlSegment(direction);
  state.scenario.pwl_strategy = ensurePwlSegments(state.scenario.pwl_strategy);
  writePwl();
  renderPriceChart();
}

function onPwlRemove(direction, idx) {
  removePwlSegment(direction, idx);
  state.scenario.pwl_strategy = ensurePwlSegments(state.scenario.pwl_strategy);
  writePwl();
  renderPriceChart();
}

// Any edit in the PWL sidebar section feeds the price-chart overlay live.
function attachPwlInputListeners() {
  const scope = $('pwl-section');
  if (!scope) return;
  scope.querySelectorAll('input').forEach(el => {
    if (el.dataset.pwlBound) return;
    el.dataset.pwlBound = '1';
    el.addEventListener('input', onPwlInputChange);
  });
  // Add-band buttons are static in the HTML — bind once.
  scope.querySelectorAll('.pwl-seg-add').forEach(btn => {
    if (btn.dataset.pwlBound) return;
    btn.dataset.pwlBound = '1';
    btn.addEventListener('click', () => onPwlAdd(btn.dataset.direction));
  });
  // Remove buttons live inside the re-rendered segment rows, so rebind
  // each time writePwl runs.
  scope.querySelectorAll('.pwl-seg-remove').forEach(btn => {
    btn.addEventListener('click', () => {
      if (btn.disabled) return;
      const dir = btn.dataset.direction;
      const idx = parseInt(btn.dataset.idx, 10);
      onPwlRemove(dir, idx);
    });
  });
}

function onPwlInputChange() {
  readPwl();
  if (state.scenario?.policy?.dispatch_mode === 'pwl_offers') {
    renderPriceChart();
    renderPwlEditorBar();
  }
}

// ─── Per-period PWL editor bar (above the price chart) ────────────────
// Visible only when dispatch_mode === 'pwl_offers' AND the LMP tab is
// active. Lets the user pick which segment line to edit on the chart.

function renderPwlEditorBar() {
  const bar = $('pwl-editor-bar');
  const pills = $('pwl-pill-group');
  const reset = $('pwl-editor-reset');
  const note = $('pwl-editor-note');
  if (!bar) return;
  const pwlMode = state.scenario?.policy?.dispatch_mode === 'pwl_offers';
  const onLmpTab = state.activePriceTab === 'lmp';
  if (!pwlMode || !onLmpTab) {
    bar.style.display = 'none';
    return;
  }
  bar.style.display = '';
  const s = state.scenario.pwl_strategy;
  const disc = s.discharge_offer_segments || [];
  const chrg = s.charge_bid_segments || [];
  const active = state.activePwlSegment;

  const pillHtml = (direction, segs, shortLabel) => segs.map((seg, i) => {
    const isActive = active && active.direction === direction && active.index === i;
    const hasOv = segmentHasOverrides(direction, i);
    const mw = Number(seg[0]).toFixed(seg[0] < 10 ? 1 : 0);
    return `
      <button class="pwl-pill ${direction}${isActive ? ' active' : ''}${hasOv ? ' has-overrides' : ''}"
              type="button"
              data-direction="${direction}" data-index="${i}"
              title="${escapeHtml(shortLabel)} segment ${i + 1}: up to ${mw} MW">
        <span class="pwl-pill-dot"></span>
        ${shortLabel}${i + 1}
      </button>`;
  }).join('');

  pills.innerHTML =
    pillHtml('discharge', disc, 'D') +
    '<span class="pwl-pill-group-sep"></span>' +
    pillHtml('charge', chrg, 'C');

  pills.querySelectorAll('.pwl-pill').forEach(btn => {
    btn.addEventListener('click', () => {
      const dir = btn.dataset.direction;
      const idx = parseInt(btn.dataset.index, 10);
      const same = active && active.direction === dir && active.index === idx;
      state.activePwlSegment = same ? null : { direction: dir, index: idx };
      renderPwlEditorBar();
      renderPriceChart();
    });
  });

  const anyOverrides = segmentHasOverrides('discharge', 0)
    || segmentHasOverrides('discharge', 1)
    || segmentHasOverrides('discharge', 2)
    || segmentHasOverrides('discharge', 3)
    || segmentHasOverrides('charge', 0)
    || segmentHasOverrides('charge', 1)
    || segmentHasOverrides('charge', 2)
    || segmentHasOverrides('charge', 3);
  reset.disabled = !anyOverrides;

  const coupling = state.scenario?.policy?.period_coupling;
  if (coupling === 'coupled' && anyOverrides) {
    note.textContent = 'per-period applies in sequential coupling — currently coupled';
  } else if (active) {
    const seg = (active.direction === 'discharge'
      ? s.discharge_offer_segments : s.charge_bid_segments)[active.index];
    const mw = Number(seg[0]).toFixed(seg[0] < 10 ? 1 : 0);
    note.textContent = `drag points on chart → override ${active.direction} ≤${mw} MW`;
  } else {
    note.textContent = 'select a segment to edit';
  }
}

function resetAllPwlOverrides() {
  clearPwlOverrides();
  state.scenario.pwl_strategy = ensurePwlSegments(state.scenario.pwl_strategy);
  renderPwlEditorBar();
  renderPriceChart();
}

// ═══ 5. LMP + AS chart rendering ══════════════════════════════════════

// ═══ Unified price chart (LMP + AS as tabs) ═══════════════════════════

// Each tab descriptor: { key, title, color, getData, setData, min, max,
//                        formatValue, distKey, pillLabel }
function getPriceTabs() {
  const tabs = [
    {
      key: 'lmp',
      title: 'LMP',
      color: '#a78bfa',
      getData: () => state.scenario.lmp_forecast_per_mwh,
      setData: (d) => { state.scenario.lmp_forecast_per_mwh = d; },
      // ``min: null`` signals "no floor" — LMPs can go negative during
      // over-generation events, so the chart must allow drag below 0
      // and render some negative headroom on the axis.
      min: null,
      max: 100,
      formatValue: (v) => '$' + v.toFixed(0),
      distKey: 'lmp',
    },
  ];
  (state.scenario.as_products || []).forEach(ap => {
    const color = AS_COLORS[ap.product_id] || '#a78bfa';
    tabs.push({
      key: `as_${ap.product_id}`,
      title: shortProductName(ap.title),
      color,
      getData: () => ap.price_forecast_per_mwh,
      setData: (d) => { ap.price_forecast_per_mwh = d; },
      // AS prices are non-negative by construction (shortage costs).
      min: 0,
      max: 30,
      formatValue: (v) => '$' + v.toFixed(1),
      distKey: `as_${ap.product_id}`,
    });
  });
  return tabs;
}

function activePriceTab() {
  const tabs = getPriceTabs();
  return tabs.find(t => t.key === state.activePriceTab) || tabs[0];
}

function renderPriceChart() {
  const container = $('chart-price');
  const tab = activePriceTab();
  if (!tab) return;
  const band = buildBandForKey(tab.distKey);
  const colorDim = tab.color + '1f';
  const referenceLines = buildPwlReferenceLines(tab);
  // Monte Carlo mean-of-samples overlay for the active tab. Drawn as a
  // smooth dashed line in the MC purple so it reads as "what did the
  // solver actually see" against the P50 baseline.
  const mc = state.mcResults;
  if (mc && mc.pricesMean) {
    let meanArr = null;
    if (tab.key === 'lmp') meanArr = mc.pricesMean.lmp;
    else if (tab.key.startsWith('as_')) meanArr = mc.pricesMean.as[tab.key.slice(3)];
    const data = tab.getData() || [];
    if (meanArr && meanArr.length === data.length && data.length > 0) {
      referenceLines.unshift({
        values: meanArr.slice(),
        color: 'var(--purple-bright)',
        opacity: 0.9,
        dashArray: '6 3',
        smooth: true,
        strokeWidth: 1.7,
        name: `MC mean · ${mc.successful} runs`,
        label: `mean · ${mc.successful} runs`,
      });
    }
  }
  const onChange = (d) => {
    tab.setData(d);
    const dist = state.scenario.distributions && state.scenario.distributions[tab.distKey];
    if (dist && !dist.dirty) {
      recomputeBandFromFormula(tab.distKey, state.scenario);
      state.priceChart.setBand(buildBandForKey(tab.distKey));
    }
    refreshActiveTabRange();
  };
  if (state.priceChart) {
    state.priceChart.setData(tab.getData(), {
      min: tab.min, max: tab.max,
      color: tab.color, colorDim,
      onChange, band, referenceLines,
      seriesName: tab.title,
    });
  } else {
    state.priceChart = new EditableLineChart(container, {
      data: tab.getData(),
      min: tab.min, max: tab.max,
      color: tab.color, colorDim,
      formatValue: tab.formatValue,
      onChange,
      band,
      referenceLines,
      seriesName: tab.title,
    });
    observeResize(container, () => state.priceChart.render());
  }
  // Chart.formatValue needs to update when the active tab changes because
  // it's set on instance creation. Patch directly.
  state.priceChart.formatValue = tab.formatValue;
}

// Build the PWL bid reference lines for the active price tab. In
// pwl_offers mode, the LMP tab shows two filled bands — the discharge
// offer curve (start-of-ramp to full-output) and the charge bid curve —
// each with a stepped edge per segment and a per-period price line
// (which becomes draggable for the currently-active segment). AS tabs
// show the single-scalar bid per product.
function buildPwlReferenceLines(tab) {
  if (state.scenario?.policy?.dispatch_mode !== 'pwl_offers') return [];
  const pwl = state.scenario.pwl_strategy || {};
  const n = (state.scenario.lmp_forecast_per_mwh || []).length;
  if (!n) return [];
  const broadcast = (val) => new Array(n).fill(Number(val) || 0);
  const active = state.activePwlSegment;
  const refs = [];
  if (tab.key === 'lmp') {
    const disc = pwl.discharge_offer_segments || [];
    const chg = pwl.charge_bid_segments || [];
    const segmentsBlock = (direction, segs, color) => {
      if (!segs.length) return;
      // Per-period effective price for each segment (respects overrides).
      const perSegValues = segs.map((_seg, i) => effectiveSegmentPrices(direction, i));
      // Band envelope = min / max across segments for each period.
      const lower = new Array(n).fill(Infinity);
      const upper = new Array(n).fill(-Infinity);
      perSegValues.forEach(arr => {
        for (let p = 0; p < n; p++) {
          if (arr[p] < lower[p]) lower[p] = arr[p];
          if (arr[p] > upper[p]) upper[p] = arr[p];
        }
      });
      const isActiveDir = active && active.direction === direction;
      // The band polygon is visual context only — individual segments
      // carry the meaningful tooltip rows, so leave ``name`` unset and
      // the tooltip will skip this entry.
      refs.push({
        band: { lower, upper },
        color,
        fillOpacity: 0.11,
        opacity: isActiveDir ? 0.35 : 0.6,
      });
      // Draw every segment's per-period line. Dim non-active segments
      // when one is selected so the focus segment stands out. Discharge
      // segments are emitted in reverse (D4 first → D1 last) so the
      // tooltip lists them top-down, matching the visual stack near
      // pmax; charge keeps its natural C1→C4 ordering which already
      // reads top-down (highest willingness-to-pay first).
      const order = direction === 'discharge'
        ? [...perSegValues.keys()].reverse()
        : [...perSegValues.keys()];
      order.forEach(i => {
        const values = perSegValues[i];
        const isActiveSeg = active && active.direction === direction && active.index === i;
        const editable = isActiveSeg ? {
          direction, index: i,
          onDrag: (periodIdx, newPrice) => {
            setSegmentPriceOverride(direction, i, periodIdx, newPrice);
          },
          onDragEnd: () => {
            renderPwlEditorBar();
            renderPriceChart();
          },
        } : null;
        const letter = direction === 'discharge' ? 'D' : 'C';
        refs.push({
          values,
          color,
          opacity: !active ? 0.55 : (isActiveSeg ? 0.95 : 0.18),
          dashArray: isActiveSeg ? '4 3' : '2 4',
          editable,
          label: isActiveSeg ? `${letter}${i + 1} custom` : null,
          name: `${letter}${i + 1} bid`,
        });
      });
    };
    segmentsBlock('discharge', disc, 'var(--emerald)');
    segmentsBlock('charge', chg, 'var(--red)');
    return refs;
  }
  if (tab.key.startsWith('as_')) {
    const productId = tab.key.slice(3);
    const bid = (pwl.as_offer_prices_per_mwh || {})[productId];
    if (bid === undefined) return [];
    return [{
      values: broadcast(bid),
      color: tab.color,
      label: `Bid $${Number(bid).toFixed(1)}`,
      opacity: 0.9,
      name: 'PWL bid',
    }];
  }
  return [];
}

function renderPriceTabs() {
  const container = $('price-tabs');
  const tabs = getPriceTabs();
  if (!tabs.find(t => t.key === state.activePriceTab)) {
    state.activePriceTab = tabs[0]?.key || 'lmp';
  }
  container.innerHTML = tabs.map(tab => {
    const data = tab.getData() || [];
    const mn = data.length ? Math.min(...data) : 0;
    const mx = data.length ? Math.max(...data) : 0;
    const active = tab.key === state.activePriceTab;
    return `
      <button class="price-tab${active ? ' active' : ''}" data-tab="${escapeHtml(tab.key)}">
        <span class="price-tab-swatch" style="background:${tab.color}"></span>
        <span class="price-tab-name">${escapeHtml(tab.title)}</span>
        <span class="price-tab-range">$${mn.toFixed(0)}–${mx.toFixed(0)}</span>
      </button>
    `;
  }).join('');
  container.querySelectorAll('.price-tab').forEach(el => {
    el.addEventListener('click', () => {
      state.activePriceTab = el.dataset.tab;
      renderPriceTabs();
      renderPriceChart();
      renderPriceDistControls();
      updatePresetsVisibility();
      renderPwlEditorBar();
    });
  });
}

function refreshActiveTabRange() {
  const tab = activePriceTab();
  if (!tab) return;
  const data = tab.getData() || [];
  const mn = data.length ? Math.min(...data) : 0;
  const mx = data.length ? Math.max(...data) : 0;
  const activeEl = $('price-tabs').querySelector('.price-tab.active .price-tab-range');
  if (activeEl) activeEl.textContent = `$${mn.toFixed(0)}–${mx.toFixed(0)}`;
}

function renderPriceDistControls() {
  const container = $('price-dist-controls');
  const tab = activePriceTab();
  if (!container || !tab) return;
  renderDistControls(container, tab.distKey, () => state.priceChart);
}

function updatePresetsVisibility() {
  const presets = $('price-presets');
  const asActions = $('as-actions');
  const isLmp = state.activePriceTab === 'lmp';
  if (presets) presets.classList.toggle('hidden', !isLmp);
  if (asActions) asActions.classList.toggle('hidden', isLmp);
}

function zeroActiveAsProduct() {
  if (!state.activePriceTab || !state.activePriceTab.startsWith('as_')) return;
  const productId = state.activePriceTab.slice(3);
  const ap = (state.scenario.as_products || []).find(p => p.product_id === productId);
  if (!ap) return;
  const n = ap.price_forecast_per_mwh.length;
  ap.price_forecast_per_mwh = new Array(n).fill(0);
  // Reset the distribution band to formula-driven (p50 = 0 collapses).
  const d = state.scenario.distributions && state.scenario.distributions[`as_${productId}`];
  if (d) {
    d.dirty = false;
    recomputeBandFromFormula(`as_${productId}`, state.scenario);
  }
  renderPriceTabs();
  renderPriceChart();
  renderPriceDistControls();
}

function buildBandForKey(key) {
  const d = state.scenario.distributions && state.scenario.distributions[key];
  if (!d || !d.spread_fraction) return null;
  return {
    p10: d.p10,
    p90: d.p90,
    editable: !!d.editable,
    onBandChange: (p10, p90, periodIdx, kind) => {
      d.p10 = p10;
      d.p90 = p90;
      d.dirty = true;
    },
  };
}

// ═══ Distribution controls UI ══════════════════════════════════════

function renderDistControls(containerEl, key, chartRef) {
  const d = state.scenario.distributions[key];
  if (!d) return;
  containerEl.innerHTML = `
    <span class="dist-label">Dist</span>
    <select class="dist-select">
      ${DIST_SHAPES.map(s => `<option value="${s}"${d.shape === s ? ' selected' : ''}>${shapeLabel(s)}</option>`).join('')}
    </select>
    <span class="dist-label" style="padding-left:4px">±</span>
    <input type="number" class="dist-spread" value="${Math.round(d.spread_fraction * 100)}" min="0" max="100" step="5">
    <span class="dist-spread-suffix">%</span>
    <button class="dist-edit-toggle${d.editable ? ' on' : ''}" type="button" title="Edit P10/P90 band bounds">Edit</button>
    <button class="dist-reset${d.dirty ? ' visible' : ''}" type="button" title="Reset custom overrides">reset</button>
  `;
  const selectEl = containerEl.querySelector('.dist-select');
  const spreadEl = containerEl.querySelector('.dist-spread');
  const toggleEl = containerEl.querySelector('.dist-edit-toggle');
  const resetEl = containerEl.querySelector('.dist-reset');

  selectEl.addEventListener('change', () => {
    d.shape = selectEl.value;
    d.dirty = false;
    recomputeBandFromFormula(key, state.scenario);
    resetEl.classList.remove('visible');
    chartRef().setBand(buildBandForKey(key));
  });
  spreadEl.addEventListener('input', () => {
    d.spread_fraction = Math.max(0, Math.min(100, parseFloat(spreadEl.value) || 0)) / 100;
    d.dirty = false;
    recomputeBandFromFormula(key, state.scenario);
    resetEl.classList.remove('visible');
    chartRef().setBand(buildBandForKey(key));
  });
  toggleEl.addEventListener('click', () => {
    d.editable = !d.editable;
    toggleEl.classList.toggle('on', d.editable);
    chartRef().setBand(buildBandForKey(key));
  });
  resetEl.addEventListener('click', () => {
    d.dirty = false;
    recomputeBandFromFormula(key, state.scenario);
    resetEl.classList.remove('visible');
    chartRef().setBand(buildBandForKey(key));
  });

  // Poll for dirty state periodically so the reset button lights up
  // when the user drags a band edge. Cheaper than threading a callback
  // through the chart class.
  const observer = setInterval(() => {
    if (!document.body.contains(containerEl)) { clearInterval(observer); return; }
    resetEl.classList.toggle('visible', !!d.dirty);
  }, 500);
}

function shapeLabel(s) {
  return { gaussian: 'Gaussian', uniform: 'Uniform', triangular: 'Triangular' }[s] || s;
}


// ═══ 6. Results ═══════════════════════════════════════════════════════

function renderResults(result) {
  // Prefer the MC aggregate when present so the single-solve snapshot
  // from the last run doesn't clobber the probabilistic display.
  const mc = state.mcResults;
  if (result) state.lastResult = result;
  const sum = (mc
    ? mcRevenueSummary(mc)
    : ((result || state.lastResult || {}).revenue_summary || {}));
  const net = sum.net_revenue_dollars;
  const netEl = $('net-rev');
  netEl.textContent = net !== undefined ? fmtMoney(net) : '—';
  netEl.classList.toggle('zero', net === undefined || Math.abs(net || 0) < 0.01);
  netEl.classList.toggle('pos', net !== undefined && net > 0.01);
  netEl.classList.toggle('neg', net !== undefined && net < -0.01);

  // Solve bar's net-revenue widget: badge + sub-label in MC mode.
  const wrap = document.querySelector('.net-rev-wrap');
  if (wrap) wrap.classList.toggle('mc', !!mc);
  const sub = $('net-rev-sub');
  const label = $('net-rev-label');
  if (mc && sub && label) {
    label.textContent = `Net revenue · P50 · ${mc.successful} runs`;
    sub.textContent = `P10 ${fmtMoney(mc.netRevenue.p10)} · P90 ${fmtMoney(mc.netRevenue.p90)}`;
  } else if (sub && label) {
    label.textContent = 'Net revenue';
    sub.textContent = '';
  }

  $('rev-energy').textContent = fmtMoney(sum.energy_revenue_dollars);
  $('rev-as').textContent = fmtMoney(sum.as_revenue_dollars);
  $('rev-deg').textContent = fmtMoney(-Math.abs(sum.degradation_cost_dollars || 0));
  $('rev-net').textContent = fmtMoney(sum.net_revenue_dollars);
  const chargeMwh = sum.total_charge_mwh ?? 0;
  const dischargeMwh = sum.total_discharge_mwh ?? 0;
  $('rev-charge-mwh').textContent = chargeMwh.toFixed(1) + ' MWh';
  $('rev-discharge-mwh').textContent = dischargeMwh.toFixed(1) + ' MWh';
  $('rev-cycles').textContent = (sum.full_equivalent_cycles || 0).toFixed(2);

  // Render the small note in the Revenue panel header.
  if (mc) {
    $('results-note').textContent = `MC · ${mc.successful}/${mc.requested} runs`;
  } else {
    const r = result || state.lastResult;
    if (r) {
      $('results-note').textContent = r.status === 'ok'
        ? `${r.periods} periods · ${(r.elapsed_secs || 0).toFixed(2)}s`
        : `${r.status}${r.error ? ': ' + r.error : ''}`;
    }
  }

  // ── Dispatch chart: stacked energy + AS awards, with pmax/pmin bounds.
  // Direction comes from the AS product definition (Up / Down). Unknown
  // products default to up-direction. In MC mode we pick the run whose
  // net revenue sits closest to the median as the "representative"
  // schedule shown underneath the fan band — the bars convey the
  // stacking shape (energy + AS) while the fan conveys uncertainty.
  let scheduleSource;
  if (mc && mc.runs.length > 0) {
    const sorted = mc.runs
      .map((r, i) => ({ i, net: r.netRevenue }))
      .sort((a, b) => a.net - b.net);
    const medianRun = mc.runs[sorted[Math.floor(sorted.length / 2)].i];
    scheduleSource = {
      schedule: medianRun.schedule,
      as_breakdown: medianRun.asBreakdown,
    };
  } else {
    scheduleSource = result || state.lastResult || { schedule: [], as_breakdown: [] };
  }
  const schedule = scheduleSource.schedule || [];
  const asDirections = {};
  (state.scenario.as_products || []).forEach(ap => {
    asDirections[ap.product_id] = ap.direction || 'Up';
  });
  const awardsByPeriod = new Map();
  (scheduleSource.as_breakdown || []).forEach(p => {
    const awards = {};
    (p.awards || []).forEach(a => { awards[a.product_id] = a.award_mw; });
    awardsByPeriod.set(p.period, awards);
  });
  // Stable stacking order so colors don't shuffle period-to-period.
  const stackOrder = (state.scenario.as_products || [])
    .map(ap => ap.product_id)
    .filter(pid => (scheduleSource.as_breakdown || []).some(p =>
      (p.awards || []).some(a => a.product_id === pid && Math.abs(a.award_mw) > 1e-9)
    ));

  const DISPATCH_COLOR = 'var(--emerald)';
  const CHARGE_COLOR = 'var(--red)';
  const EMERALD = getComputedStyle(document.body).getPropertyValue('--emerald').trim() || '#34d399';
  const RED = getComputedStyle(document.body).getPropertyValue('--red').trim() || '#f87171';
  const activeSeries = [];  // for legend rendering
  const pushActive = (label, color, hatch = false) => {
    if (!activeSeries.some(s => s.label === label)) activeSeries.push({ label, color, hatch });
  };
  let anyDischarge = false, anyCharge = false;

  const dispatchData = schedule.map((row) => {
    const disch = row.discharge_mw || 0;
    const chg = row.charge_mw || 0;
    if (disch > 1e-9) anyDischarge = true;
    if (chg > 1e-9) anyCharge = true;

    const up = [];
    if (disch > 1e-9) up.push({ mw: disch, color: EMERALD, label: 'Discharge' });
    const down = [];
    if (chg > 1e-9) down.push({ mw: chg, color: RED, label: 'Charge' });

    const awards = awardsByPeriod.get(row.period) || {};
    stackOrder.forEach(pid => {
      const mw = awards[pid] || 0;
      if (mw <= 1e-9) return;
      const color = AS_COLORS[pid] || '#a78bfa';
      const dir = asDirections[pid] || 'Up';
      // AS segments rendered with a diagonal-stripe pattern to read as
      // "reserved capacity" rather than the solid fill used for energy
      // flow. Reservations and flow can then stack on the same column
      // without the eye collapsing them into one block.
      const seg = { mw, color, label: pid, hatch: true };
      if (dir === 'Down') down.push(seg);
      else up.push(seg);
      pushActive(shortProductName(pid), color, true);
    });
    return { up, down };
  });

  if (anyDischarge) pushActive('Discharge', EMERALD);
  if (anyCharge) pushActive('Charge', RED);

  const dispatchEl = $('chart-dispatch');
  const site = state.scenario.site || {};
  const upBound = site.bess_power_discharge_mw || 0;
  const downBound = site.bess_power_charge_mw || 0;
  // In MC mode, attach a net-MW fan band built from the per-period
  // quantiles. In deterministic mode, strip any previous band.
  const dispatchFan = mc
    ? {
        lower: mc.perPeriod.map(p => p.netMw.p10),
        upper: mc.perPeriod.map(p => p.netMw.p90),
        median: mc.perPeriod.map(p => p.netMw.p50),
      }
    : null;
  if (state.dispatchChart) {
    state.dispatchChart.setData(dispatchData, { upBound, downBound, fanBand: dispatchFan });
  } else {
    state.dispatchChart = new DispatchChart(dispatchEl, {
      data: dispatchData, upBound, downBound, fanBand: dispatchFan,
    });
    observeResize(dispatchEl, () => state.dispatchChart.render());
  }
  renderDispatchLegend(activeSeries, upBound, downBound);

  // SOC: in deterministic mode plot the run's SoC; in MC mode plot an
  // empty primary series so the fan band carries the display.
  const socData = mc ? [] : schedule.map(r => r.soc_mwh || 0);
  const socSeries = [{
    name: 'SOC',
    color: '#a78bfa',
    data: socData,
  }];
  const socFan = mc ? {
    lower: mc.perPeriod.map(p => p.soc.p10),
    upper: mc.perPeriod.map(p => p.soc.p90),
    median: mc.perPeriod.map(p => p.soc.p50),
  } : null;
  // Fallback data for the primary series when MC is active so the chart
  // still knows the period count — use the median SOC line.
  if (mc) socSeries[0].data = socFan.median.slice();
  const socEl = $('chart-soc');
  const socMin = (site.bess_soc_min_fraction || 0) * (site.bess_energy_mwh || 1);
  const socMax = (site.bess_soc_max_fraction || 1) * (site.bess_energy_mwh || 1);
  const socOpts = {
    series: socSeries,
    yMin: 0,
    yMax: site.bess_energy_mwh || undefined,
    showLegend: false,
    unit: 'MWh',
    valuePrecision: 2,
    guideLines: [
      { y: socMin, color: 'rgba(248,113,113,0.35)', label: 'min' },
      { y: socMax, color: 'rgba(248,113,113,0.35)', label: 'max' },
    ],
    fanBand: socFan,
  };
  if (state.socChart) state.socChart.setSeries(socSeries, socOpts);
  else {
    state.socChart = new LineReadOnlyChart(socEl, socOpts);
    observeResize(socEl, () => state.socChart.render());
  }
}

function renderDispatchLegend(items, upBound, downBound) {
  const el = $('dispatch-legend');
  if (!el) return;
  const parts = items.map(s => {
    // Hatched swatches — same diagonal stripe pattern the chart uses —
    // make AS reservations read as "held capacity" in the legend too.
    const swatchStyle = s.hatch
      ? `background:
          repeating-linear-gradient(45deg,
            ${s.color} 0 1.5px,
            ${s.color}2e 1.5px 4px);`
      : `background:${s.color}`;
    return `
      <span class="chart-legend-item">
        <span class="chart-legend-swatch${s.hatch ? ' hatch' : ''}" style="${swatchStyle}"></span>
        <span>${escapeHtml(s.label)}</span>
      </span>
    `;
  });
  if (upBound > 0 || downBound > 0) {
    parts.push(`<span class="chart-legend-bound">±pmax ${Math.max(upBound, downBound).toFixed(0)}</span>`);
  }
  el.innerHTML = parts.join('');
}

function fmtMoney(v) {
  if (v === undefined || v === null || Number.isNaN(v)) return '—';
  const sign = v < 0 ? '−' : '';
  const abs = Math.abs(v);
  const digits = abs >= 1000 ? 0 : abs >= 10 ? 1 : 2;
  return `${sign}$${abs.toLocaleString(undefined, { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}
function shortProductName(name) {
  return name
    .replace('Regulation ', 'Reg ')
    .replace('Synchronized Reserve', 'Spin')
    .replace('Non-Synchronized Reserve', 'Non-Spin')
    .replace('Ramping Reserve ', 'Ramp ')
    .replace(' (Online)', '↻')
    .replace(' (Offline)', '↺');
}
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => ({
    '&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;'
  }[c]));
}

// ═══ 7. ResizeObserver — re-render charts on container resize ═════════

function observeResize(el, cb) {
  if (!('ResizeObserver' in window)) return;
  let last = { w: 0, h: 0 };
  let rafId = null;
  const ro = new ResizeObserver((entries) => {
    for (const e of entries) {
      const cr = e.contentRect;
      if (Math.abs(cr.width - last.w) < 1 && Math.abs(cr.height - last.h) < 1) continue;
      last = { w: cr.width, h: cr.height };
      if (rafId) cancelAnimationFrame(rafId);
      rafId = requestAnimationFrame(() => { try { cb(); } catch (_) {} });
    }
  });
  ro.observe(el);
  state.observers.push(ro);
}

// ═══ 8. Solve + time-axis resampling + presets ═══════════════════════

async function solve() {
  if (state.solving || state.mcRunning) return;
  // A fresh deterministic solve invalidates any cached MC aggregation;
  // re-render the price chart so the MC mean overlay is removed.
  const hadMc = !!state.mcResults;
  clearMcResults();
  if (hadMc) renderPriceChart();

  // Run cheap validation before even hitting the backend — catches the
  // most common mistakes (SOC init out of the [min, max] envelope,
  // negative capacities, etc.) with a precise message tied to the
  // exact field that's wrong. Solve is blocked if any hard error
  // shows up here.
  const preflight = validateScenarioLocal();
  if (preflight.length > 0) {
    showSolveError(preflight.map(v => v.message).join(' · '));
    return;
  }

  state.solving = true;
  $('btn-solve').disabled = true;
  $('solve-status').textContent = 'solving…';
  $('solve-status').className = 'solve-status busy';
  clearSolveError();

  readAsset();
  readTimeAxis();
  readPolicy();
  readPwl();
  // Normalize the per-period override matrix dimensions to the current
  // horizon before serializing the scenario.
  state.scenario.pwl_strategy = ensurePwlSegments(state.scenario.pwl_strategy);

  const t0 = performance.now();
  try {
    const res = await fetch('api/solve', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(state.scenario),
    });
    const body = await res.json().catch(() => ({ detail: 'invalid JSON' }));
    if (!res.ok) throw new Error(body.detail || res.statusText);
    // The backend can return 200 with status='error' when the solver
    // itself (not the HTTP layer) rejects the scenario. Catch that.
    if (body.status !== 'ok') {
      throw new Error(body.error || `solver returned status=${body.status}`);
    }
    const elapsed = ((performance.now() - t0) / 1000).toFixed(2);
    $('solve-status').textContent = `solved · ${elapsed}s`;
    $('solve-status').className = 'solve-status ok';
    renderResults(body);
  } catch (err) {
    // Keep the last good result visible — blanking everything on a
    // bad edit is hostile. Just surface the error prominently.
    $('solve-status').textContent = 'error';
    $('solve-status').className = 'solve-status err';
    showSolveError(err.message);
  } finally {
    state.solving = false;
    $('btn-solve').disabled = false;
  }
}

// ═══ Monte Carlo over price distributions ════════════════════════════

// Draw a single sample from a price distribution centered on ``p50``
// with the 90th quantile at ``p90``. ``shape`` picks the distribution
// family; ``minValue`` caps the sample at a floor (null = no floor).
function drawPriceSample(shape, p50, p90, minValue) {
  const spread = Math.max(0, p90 - p50);
  let v = p50;
  if (spread > 0) {
    if (shape === 'uniform') {
      // Symmetric uniform band: P90 − P50 = 0.4·(b−a), so half-range = 1.25·spread.
      const half = spread / 0.8;
      v = p50 + (Math.random() * 2 - 1) * half;
    } else if (shape === 'triangular') {
      // Symmetric triangular with mode = P50, width w such that
      // P90 − P50 = 0.553·w, so w = spread / 0.553.
      const w = spread / 0.553;
      const u = Math.random();
      v = u < 0.5
        ? p50 - w + Math.sqrt(u * 2) * w
        : p50 + w - Math.sqrt((1 - u) * 2) * w;
    } else {
      // Gaussian (default). σ = spread / 1.282.
      const sigma = spread / 1.282;
      const u1 = Math.random() || 1e-9;
      const u2 = Math.random();
      const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
      v = p50 + z * sigma;
    }
  }
  return (minValue === null || minValue === undefined) ? v : Math.max(minValue, v);
}

// Build a scenario for one MC draw: deep-copy the current scenario and
// replace LMP + each AS forecast with a sampled path drawn from that
// series' own distribution spec.
function buildSampledScenario() {
  const base = state.scenario;
  const dist = base.distributions || {};
  // JSON round-trip is a cheap deep clone for our scenario shape.
  const scen = JSON.parse(JSON.stringify(base));

  const n = (base.lmp_forecast_per_mwh || []).length;
  if (n === 0) return scen;

  const drawSeries = (p50Arr, distSpec, minValue) => {
    if (!distSpec || !distSpec.p90 || distSpec.p90.length !== p50Arr.length
        || !(distSpec.spread_fraction > 0)) {
      return p50Arr.slice();
    }
    return p50Arr.map((p50, i) => drawPriceSample(
      distSpec.shape || 'gaussian', p50, distSpec.p90[i], minValue
    ));
  };

  // LMP has no lower floor (negative prices happen during overgen).
  scen.lmp_forecast_per_mwh = drawSeries(
    base.lmp_forecast_per_mwh, dist.lmp, null
  );
  // AS prices are non-negative by construction.
  (scen.as_products || []).forEach(ap => {
    const apDist = dist['as_' + ap.product_id];
    ap.price_forecast_per_mwh = drawSeries(
      ap.price_forecast_per_mwh, apDist, 0
    );
  });
  return scen;
}

// Run N single-period solves against independently sampled price
// paths. Sequential to keep the backend simple; we ran the math and
// at N ≤ 100 with ~100 ms per solve this stays snappy.
async function runMonteCarlo() {
  if (state.mcRunning || state.solving) return;
  const preflight = validateScenarioLocal();
  if (preflight.length > 0) {
    showSolveError(preflight.map(v => v.message).join(' · '));
    return;
  }
  const nInput = parseInt($('inp-mc-n').value, 10);
  const N = Math.max(2, Math.min(500, isFinite(nInput) ? nInput : 10));

  state.mcRunning = true;
  state.mcResults = null;
  document.body.classList.add('mc-running');
  clearSolveError();
  $('btn-mc').disabled = true;
  $('btn-solve').disabled = true;

  // Snapshot scenario form state so samples use the committed values.
  readAsset();
  readTimeAxis();
  readPolicy();
  readPwl();
  state.scenario.pwl_strategy = ensurePwlSegments(state.scenario.pwl_strategy);

  const t0 = performance.now();
  const successes = [];
  let failures = 0;
  for (let i = 0; i < N; i++) {
    $('solve-status').textContent = `mc · ${i + 1}/${N}`;
    $('solve-status').className = 'solve-status busy';
    try {
      const scen = buildSampledScenario();
      // Snapshot the sampled price paths before posting so the UI can
      // show "what prices did the solver see" as an MC mean overlay.
      const sampledLmp = (scen.lmp_forecast_per_mwh || []).slice();
      const sampledAs = {};
      (scen.as_products || []).forEach(ap => {
        sampledAs[ap.product_id] = (ap.price_forecast_per_mwh || []).slice();
      });
      const res = await fetch('api/solve', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(scen),
      });
      const body = await res.json().catch(() => ({ status: 'error' }));
      if (res.ok && body.status === 'ok') {
        successes.push({
          netRevenue: body.revenue_summary?.net_revenue_dollars ?? 0,
          energyRev: body.revenue_summary?.energy_revenue_dollars ?? 0,
          asRev: body.revenue_summary?.as_revenue_dollars ?? 0,
          degCost: body.revenue_summary?.degradation_cost_dollars ?? 0,
          chargeMwh: body.revenue_summary?.total_charge_mwh ?? 0,
          dischargeMwh: body.revenue_summary?.total_discharge_mwh ?? 0,
          schedule: body.schedule || [],
          asBreakdown: body.as_breakdown || [],
          sampledLmp,
          sampledAs,
        });
      } else {
        failures++;
      }
    } catch (err) {
      failures++;
    }
  }
  const elapsed = ((performance.now() - t0) / 1000).toFixed(1);

  if (successes.length === 0) {
    $('solve-status').textContent = 'mc failed';
    $('solve-status').className = 'solve-status err';
    showSolveError(`All ${N} Monte Carlo runs failed.`);
  } else {
    state.mcResults = aggregateMcRuns(successes, N);
    const label = failures > 0
      ? `mc · ${successes.length}/${N} ok · ${elapsed}s`
      : `mc · ${N} runs · ${elapsed}s`;
    $('solve-status').textContent = label;
    $('solve-status').className = 'solve-status ok';
    renderResults(null);
    // Refresh the price chart so the sampled-mean overlay appears for
    // whichever tab is active.
    renderPriceChart();
  }

  state.mcRunning = false;
  document.body.classList.remove('mc-running');
  $('btn-mc').disabled = false;
  $('btn-solve').disabled = false;
}

function clearMcResults() {
  if (!state.mcResults) return;
  state.mcResults = null;
}

// Shape the MC aggregate into a ``revenue_summary``-compatible dict so
// the existing Revenue panel renderer can consume it. The single values
// we emit here are the P50 across runs; the solve-bar header carries
// P10/P90 separately.
function mcRevenueSummary(mc) {
  const energyCap = state.scenario?.site?.bess_energy_mwh || 0;
  const throughput = mc.chargeMwh.p50 + mc.dischargeMwh.p50;
  return {
    net_revenue_dollars: mc.netRevenue.p50,
    energy_revenue_dollars: mc.energyRev.p50,
    as_revenue_dollars: mc.asRev.p50,
    degradation_cost_dollars: mc.degCost.p50,
    total_charge_mwh: mc.chargeMwh.p50,
    total_discharge_mwh: mc.dischargeMwh.p50,
    full_equivalent_cycles: energyCap > 0 ? throughput / (2 * energyCap) : 0,
  };
}

// Aggregate N successful solves into per-metric quantiles.
function aggregateMcRuns(runs, nRequested) {
  if (!runs.length) return null;
  const n = runs[0].schedule.length;
  const sort = (arr) => arr.slice().sort((a, b) => a - b);
  const quantile = (sorted, q) => {
    if (!sorted.length) return 0;
    const idx = (sorted.length - 1) * q;
    const lo = Math.floor(idx), hi = Math.ceil(idx);
    if (lo === hi) return sorted[lo];
    return sorted[lo] + (idx - lo) * (sorted[hi] - sorted[lo]);
  };
  const mean = (arr) => arr.reduce((s, v) => s + v, 0) / arr.length;
  const summarize = (vals) => {
    const s = sort(vals);
    return { p10: quantile(s, 0.1), p50: quantile(s, 0.5), p90: quantile(s, 0.9), mean: mean(vals) };
  };

  // Per-period quantiles: net MW (discharge − charge), charge MW,
  // discharge MW, SOC. Also roll up per-AS-product award.
  const perPeriod = Array.from({ length: n }, (_, i) => {
    const disV = runs.map(r => (r.schedule[i]?.discharge_mw || 0));
    const chV = runs.map(r => (r.schedule[i]?.charge_mw || 0));
    const netV = runs.map(r => (r.schedule[i]?.discharge_mw || 0) - (r.schedule[i]?.charge_mw || 0));
    const socV = runs.map(r => (r.schedule[i]?.soc_mwh || 0));
    return {
      netMw: summarize(netV),
      dischargeMw: summarize(disV),
      chargeMw: summarize(chV),
      soc: summarize(socV),
    };
  });

  // Per-period mean of the sampled price paths — one line per tab on
  // the price chart. Catches the realised expectation of each series
  // given the current distribution spec.
  const meanPerPeriod = (selector) => {
    if (!runs.length || !selector(runs[0])) return null;
    const len = selector(runs[0]).length;
    const out = new Array(len).fill(0);
    for (const r of runs) {
      const arr = selector(r) || [];
      for (let i = 0; i < len; i++) out[i] += (arr[i] || 0) / runs.length;
    }
    return out;
  };
  const pricesMean = {
    lmp: meanPerPeriod(r => r.sampledLmp),
    as: {},
  };
  const asIds = Object.keys(runs[0].sampledAs || {});
  asIds.forEach(pid => {
    pricesMean.as[pid] = meanPerPeriod(r => r.sampledAs[pid]);
  });

  return {
    runs,
    requested: nRequested,
    successful: runs.length,
    netRevenue: summarize(runs.map(r => r.netRevenue)),
    energyRev: summarize(runs.map(r => r.energyRev)),
    asRev: summarize(runs.map(r => r.asRev)),
    degCost: summarize(runs.map(r => r.degCost)),
    chargeMwh: summarize(runs.map(r => r.chargeMwh)),
    dischargeMwh: summarize(runs.map(r => r.dischargeMwh)),
    perPeriod,
    pricesMean,
  };
}

// ═══ Local (pre-flight) validation ═══════════════════════════════════

function validateScenarioLocal() {
  const errors = [];
  // Flush form into state without triggering side effects.
  readAsset();
  readTimeAxis();
  readPolicy();
  readPwl();

  const s = state.scenario.site;
  const socMinMwh = s.bess_soc_min_fraction * s.bess_energy_mwh;
  const socMaxMwh = s.bess_soc_max_fraction * s.bess_energy_mwh;

  // SOC init must sit inside [SOC min, SOC max].
  const socInputs = [$('inp-soc-init'), $('inp-soc-min'), $('inp-soc-max')];
  socInputs.forEach(el => el.classList.remove('invalid'));
  if (s.bess_initial_soc_mwh < socMinMwh - 1e-9) {
    $('inp-soc-init').classList.add('invalid');
    errors.push({
      field: 'bess_initial_soc_mwh',
      message: `SOC init (${s.bess_initial_soc_mwh.toFixed(1)} MWh) is below SOC min (${socMinMwh.toFixed(1)} MWh). Raise SOC init or lower SOC min fraction.`,
    });
  }
  if (s.bess_initial_soc_mwh > socMaxMwh + 1e-9) {
    $('inp-soc-init').classList.add('invalid');
    errors.push({
      field: 'bess_initial_soc_mwh',
      message: `SOC init (${s.bess_initial_soc_mwh.toFixed(1)} MWh) exceeds SOC max (${socMaxMwh.toFixed(1)} MWh).`,
    });
  }
  if (s.bess_soc_min_fraction > s.bess_soc_max_fraction + 1e-9) {
    $('inp-soc-min').classList.add('invalid');
    $('inp-soc-max').classList.add('invalid');
    errors.push({
      field: 'soc-range',
      message: `SOC min fraction (${s.bess_soc_min_fraction}) exceeds SOC max (${s.bess_soc_max_fraction}).`,
    });
  }

  // Efficiencies each in (0, 1].
  const checkEff = (val, inputId, label) => {
    const el = $(inputId);
    if (!(val > 0 && val <= 1)) {
      if (el) el.classList.add('invalid');
      errors.push({
        field: inputId,
        message: `${label} must be in (0, 1]; got ${val}.`,
      });
    } else if (el) {
      el.classList.remove('invalid');
    }
  };
  checkEff(s.bess_charge_efficiency, 'inp-eff-charge', 'Charge efficiency');
  checkEff(s.bess_discharge_efficiency, 'inp-eff-discharge', 'Discharge efficiency');

  // Foldback thresholds (optional). Discharge foldback must sit in
  // (soc_min, soc_max]; charge foldback must sit in [soc_min, soc_max).
  const fbDis = s.bess_discharge_foldback_fraction;
  const fbCh = s.bess_charge_foldback_fraction;
  const fbDisEl = $('inp-foldback-dis');
  const fbChEl = $('inp-foldback-ch');
  if (fbDis !== null && fbDis !== undefined) {
    if (!(fbDis > s.bess_soc_min_fraction && fbDis <= s.bess_soc_max_fraction)) {
      if (fbDisEl) fbDisEl.classList.add('invalid');
      errors.push({
        field: 'inp-foldback-dis',
        message: `Discharge foldback SOC (${fbDis}) must be in (soc_min, soc_max].`,
      });
    } else if (fbDisEl) {
      fbDisEl.classList.remove('invalid');
    }
  } else if (fbDisEl) {
    fbDisEl.classList.remove('invalid');
  }
  if (fbCh !== null && fbCh !== undefined) {
    if (!(fbCh >= s.bess_soc_min_fraction && fbCh < s.bess_soc_max_fraction)) {
      if (fbChEl) fbChEl.classList.add('invalid');
      errors.push({
        field: 'inp-foldback-ch',
        message: `Charge foldback SOC (${fbCh}) must be in [soc_min, soc_max).`,
      });
    } else if (fbChEl) {
      fbChEl.classList.remove('invalid');
    }
  } else if (fbChEl) {
    fbChEl.classList.remove('invalid');
  }

  // Energy > 0, MW bounds >= 0.
  if (s.bess_energy_mwh <= 0) {
    $('inp-energy').classList.add('invalid');
    errors.push({ field: 'energy', message: `BESS energy must be > 0 MWh.` });
  } else {
    $('inp-energy').classList.remove('invalid');
  }

  return errors;
}

function showSolveError(message) {
  const banner = $('error-banner');
  const text = $('error-banner-text');
  if (!banner || !text) return;
  text.textContent = message;
  banner.classList.add('visible');
}
function clearSolveError() {
  const banner = $('error-banner');
  if (banner) banner.classList.remove('visible');
}

// ═══ Price CSV upload ════════════════════════════════════════════════

// Strip surrounding whitespace + quotes on a single CSV cell. Handles
// the three things spreadsheets commonly emit: plain values, quoted
// values, and stray BOM characters at the front of the first field.
function cleanCsvCell(s) {
  let v = (s || '').trim();
  if (v.charCodeAt(0) === 0xFEFF) v = v.slice(1);
  if (v.length >= 2 && v[0] === '"' && v[v.length - 1] === '"') {
    v = v.slice(1, -1).replace(/""/g, '"');
  }
  return v;
}

// Lightweight CSV parser — handles quoted fields with embedded commas.
// Enough for hand-edited price tables; not a full RFC 4180 impl.
function parseCsvText(text) {
  const rows = [];
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  for (const raw of lines) {
    if (!raw.length) continue;
    const cells = [];
    let cur = '';
    let inQuote = false;
    for (let i = 0; i < raw.length; i++) {
      const ch = raw[i];
      if (inQuote) {
        if (ch === '"' && raw[i + 1] === '"') { cur += '"'; i++; }
        else if (ch === '"') { inQuote = false; }
        else { cur += ch; }
      } else if (ch === '"') {
        inQuote = true;
      } else if (ch === ',') {
        cells.push(cur); cur = '';
      } else {
        cur += ch;
      }
    }
    cells.push(cur);
    if (cells.some(c => c.trim().length > 0)) {
      rows.push(cells.map(cleanCsvCell));
    }
  }
  return rows;
}

// Parse a price CSV into { series: { lmp: [...], as: {pid: [...]} }, warnings }.
// Header row is required; recognizes ``lmp`` (or ``LMP``) and any column
// whose name matches an AS product_id on the current scenario.
function parsePriceCsv(text) {
  const rows = parseCsvText(text);
  if (rows.length < 2) {
    throw new Error('CSV must have a header row and at least one data row.');
  }
  const header = rows[0].map(h => h.trim().toLowerCase());
  const asProductIds = (state.scenario.as_products || []).map(ap => ap.product_id.toLowerCase());

  const lmpCol = header.findIndex(h => h === 'lmp' || h === 'lmp_per_mwh' || h === 'price');
  const asColByPid = {};
  const unknownCols = [];
  header.forEach((h, i) => {
    if (i === lmpCol || !h) return;
    // Skip index/time columns silently.
    if (['period', 'hour', 'time', 'timestamp', 'datetime', 'date'].includes(h)) return;
    if (asProductIds.includes(h)) asColByPid[h] = i;
    else unknownCols.push(header[i]);
  });
  if (lmpCol < 0 && Object.keys(asColByPid).length === 0) {
    throw new Error('CSV must have at least an ``lmp`` column or an AS product column.');
  }

  const data = rows.slice(1);
  const toFloat = (cell) => {
    if (!cell) return NaN;
    const v = parseFloat(cell.replace(/[$,]/g, ''));
    return isFinite(v) ? v : NaN;
  };
  const series = { lmp: null, as: {} };
  if (lmpCol >= 0) {
    series.lmp = data.map((r) => toFloat(r[lmpCol]));
  }
  Object.entries(asColByPid).forEach(([pid, col]) => {
    series.as[pid] = data.map((r) => toFloat(r[col]));
  });

  // Validate numeric coverage; report rows that failed to parse.
  const anyBad = (arr) => arr && arr.some(v => !isFinite(v));
  if (anyBad(series.lmp)) {
    throw new Error('LMP column contains non-numeric values.');
  }
  for (const pid of Object.keys(series.as)) {
    if (anyBad(series.as[pid])) {
      throw new Error(`Column "${pid}" contains non-numeric values.`);
    }
  }
  return {
    series,
    nRows: data.length,
    matchedProducts: Object.keys(series.as),
    warnings: unknownCols.length
      ? [`Ignored unknown columns: ${unknownCols.join(', ')}`]
      : [],
  };
}

// Apply a parsed CSV to the current scenario. Resizes the horizon to
// match the CSV's row count (keeps resolution + start time), resets
// distribution bands to formula-driven so P10/P90 follow the new P50,
// and re-renders the UI.
function applyParsedCsv(parsed) {
  const { series, nRows } = parsed;
  // Resize horizon to the CSV's row count. We keep the current
  // resolution and start time so only the LENGTH changes.
  const t = state.scenario.time_axis;
  t.periods = nRows;
  t.horizon_minutes = nRows * (t.resolution_minutes || 60);
  writeTimeAxis();

  // LMP: use CSV if present, otherwise resample existing to new length.
  if (series.lmp) {
    state.scenario.lmp_forecast_per_mwh = series.lmp.slice();
  } else {
    state.scenario.lmp_forecast_per_mwh = resample(
      state.scenario.lmp_forecast_per_mwh, nRows,
    );
  }

  // AS: overwrite any product we saw a column for; resample the others.
  (state.scenario.as_products || []).forEach(ap => {
    const csvPrices = series.as[ap.product_id.toLowerCase()];
    if (csvPrices) {
      ap.price_forecast_per_mwh = csvPrices.slice();
    } else {
      ap.price_forecast_per_mwh = resample(ap.price_forecast_per_mwh, nRows);
    }
  });

  // Distribution bands are computed off P50 — reset the dirty flag so
  // the formula re-computes matching the fresh P50 curves.
  const dists = state.scenario.distributions || {};
  Object.keys(dists).forEach(key => {
    const d = dists[key];
    if (!d) return;
    d.dirty = false;
  });
  resamplePwlOverrides(nRows);
  state.scenario.pwl_strategy = ensurePwlSegments(state.scenario.pwl_strategy);
  ensureDistributions(state.scenario);
  renderPeriodsDisplay();
  renderPriceTabs();
  renderPriceChart();
  renderPriceDistControls();
  renderPwlEditorBar();
}

function setCsvStatus(cls, text) {
  const el = $('csv-status');
  if (!el) return;
  el.textContent = text || '';
  el.className = 'csv-status' + (cls ? ' ' + cls : '');
}

async function handleCsvFile(file) {
  if (!file) return;
  try {
    const text = await file.text();
    const parsed = parsePriceCsv(text);
    applyParsedCsv(parsed);
    state.csvLoaded = true;
    $('btn-csv-clear').hidden = false;
    const bits = [`${parsed.nRows} periods`];
    if (parsed.series.lmp) bits.push('lmp');
    if (parsed.matchedProducts.length) bits.push(parsed.matchedProducts.join(', '));
    const cls = parsed.warnings.length ? 'warn' : 'ok';
    const msg = `✓ ${file.name}: ${bits.join(' · ')}`
      + (parsed.warnings.length ? ` (${parsed.warnings.join(' ')})` : '');
    setCsvStatus(cls, msg);
  } catch (err) {
    setCsvStatus('err', '✗ ' + err.message);
  }
}

async function resetCsvOverrides() {
  // Refetch the server's default scenario and replay its LMP + AS
  // prices onto the current scenario. Preserves site / policy / PWL
  // edits; only the price series go back to defaults.
  try {
    const scen = await fetch('api/default-scenario').then(r => r.json());
    const srcN = scen.lmp_forecast_per_mwh.length;
    const t = state.scenario.time_axis;
    t.periods = srcN;
    t.horizon_minutes = srcN * (t.resolution_minutes || 60);
    writeTimeAxis();
    state.scenario.lmp_forecast_per_mwh = scen.lmp_forecast_per_mwh.slice();
    const defAs = new Map((scen.as_products || []).map(a => [a.product_id, a.price_forecast_per_mwh]));
    (state.scenario.as_products || []).forEach(ap => {
      const src = defAs.get(ap.product_id);
      if (src) ap.price_forecast_per_mwh = src.slice();
      else ap.price_forecast_per_mwh = resample(ap.price_forecast_per_mwh, srcN);
    });
    const dists = state.scenario.distributions || {};
    Object.keys(dists).forEach(key => { dists[key].dirty = false; });
    resamplePwlOverrides(srcN);
    state.scenario.pwl_strategy = ensurePwlSegments(state.scenario.pwl_strategy);
    ensureDistributions(state.scenario);
    renderPeriodsDisplay();
    renderPriceTabs();
    renderPriceChart();
    renderPriceDistControls();
    renderPwlEditorBar();
    state.csvLoaded = false;
    $('btn-csv-clear').hidden = true;
    $('inp-csv').value = '';
    setCsvStatus('', 'restored to defaults');
  } catch (err) {
    setCsvStatus('err', '✗ reset failed: ' + err.message);
  }
}

function applyTimeAxis() {
  const oldRes = state.scenario.time_axis.resolution_minutes;
  readTimeAxis();
  const newN = state.scenario.time_axis.periods;
  const newRes = state.scenario.time_axis.resolution_minutes;
  // Resolution unchanged → treat the existing series as a *daily*
  // shape and tile/truncate it across the new horizon (extend = repeat
  // the duck curve, shrink = chop down). Resolution changed → keep
  // the existing shape and just resample to the new period count.
  const transform = (oldRes === newRes)
    ? (src) => tileDaily(src, newN, newRes)
    : (src) => resample(src, newN);
  state.scenario.lmp_forecast_per_mwh = transform(state.scenario.lmp_forecast_per_mwh);
  (state.scenario.as_products || []).forEach(ap => {
    ap.price_forecast_per_mwh = transform(ap.price_forecast_per_mwh);
  });
  // Drop any cached distribution bands so ensureDistributions reseeds
  // them from the new P50 values (the tiled / resampled prices).
  Object.values(state.scenario.distributions || {}).forEach(d => {
    d.p10 = null;
    d.p90 = null;
  });
  ensureDistributions(state.scenario);
  // Per-period PWL override matrices need to follow the horizon size
  // too — resample each segment's override array, then let
  // ensurePwlSegments drop empty rows.
  resamplePwlOverrides(newN);
  state.scenario.pwl_strategy = ensurePwlSegments(state.scenario.pwl_strategy);
  renderPeriodsDisplay();
  renderPriceTabs();
  renderPriceChart();
  renderPriceDistControls();
  renderPwlEditorBar();
}

function resamplePwlOverrides(newN) {
  const s = state.scenario.pwl_strategy;
  if (!s) return;
  const resampleMat = (mat) => {
    if (!Array.isArray(mat) || mat.length === 0) return mat;
    const segs = (mat[0] || []).length;
    // For each segment column independently: resample the column with a
    // nearest-neighbor fall-through. null is treated as "no override"
    // and preserved as null.
    const cols = Array.from({ length: segs }, (_, s) =>
      mat.map(row => (row && row[s] !== null && row[s] !== undefined) ? row[s] : null)
    ).map(col => resampleNullable(col, newN));
    return Array.from({ length: newN }, (_, p) =>
      cols.map(col => col[p]));
  };
  s.discharge_offer_price_per_period = resampleMat(s.discharge_offer_price_per_period);
  s.charge_bid_price_per_period = resampleMat(s.charge_bid_price_per_period);
}

function resampleNullable(arr, newN) {
  if (!Array.isArray(arr) || arr.length === 0) return new Array(newN).fill(null);
  if (arr.length === newN) return arr.slice();
  const out = new Array(newN);
  const ratio = arr.length / newN;
  for (let i = 0; i < newN; i++) {
    out[i] = arr[Math.min(arr.length - 1, Math.floor(i * ratio))] ?? null;
  }
  return out;
}

function resample(src, newN) {
  if (!src || src.length === 0) return new Array(newN).fill(0);
  if (src.length === newN) return src.slice();
  const out = new Array(newN);
  for (let i = 0; i < newN; i++) {
    const srcIdx = (i * (src.length - 1)) / Math.max(1, newN - 1);
    const lo = Math.floor(srcIdx);
    const hi = Math.min(src.length - 1, lo + 1);
    const frac = srcIdx - lo;
    out[i] = src[lo] * (1 - frac) + src[hi] * frac;
  }
  return out;
}

// Treat ``src`` as a daily price shape sampled at ``resMin`` minutes,
// then tile (or truncate) to fill ``newN`` periods. Used when the user
// changes the horizon: extending past 24h repeats the duck curve;
// shrinking below 24h takes the head of the day.
function tileDaily(src, newN, resMin) {
  if (!src || src.length === 0) return new Array(newN).fill(0);
  const periodsPerDay = Math.max(1, Math.round((24 * 60) / (resMin || 60)));
  // Use the first day of the existing series as the daily template;
  // if it's shorter than a day, tile whatever's there.
  const tmpl = src.slice(0, Math.min(src.length, periodsPerDay));
  return Array.from({ length: newN }, (_, i) => tmpl[i % tmpl.length]);
}

function applyLmpPreset(name) {
  const t = state.scenario.time_axis;
  const n = t.periods;
  // Build hour-of-day per period from the current resolution and start
  // time, then mod 24 — so presets tile across multi-day horizons and
  // truncate cleanly for sub-day horizons (no stretch-fitting one day
  // across the entire horizon).
  const hoursPerPeriod = (t.resolution_minutes || 60) / 60;
  const startDate = new Date(t.start_iso);
  const startHour = isNaN(startDate.getTime())
    ? 0
    : startDate.getHours() + startDate.getMinutes() / 60;
  const hourOfDay = (i) => ((startHour + i * hoursPerPeriod) % 24 + 24) % 24;
  let shape;
  if (name === 'flat') {
    shape = new Array(n).fill(40.0);
  } else if (name === 'peak') {
    shape = Array.from({ length: n }, (_, i) => {
      const h = hourOfDay(i);
      return 20 + 80 * Math.exp(-Math.pow(h - 18, 2) / 4);
    });
  } else {
    shape = Array.from({ length: n }, (_, i) => {
      const h = hourOfDay(i);
      const overnight = 32.0;
      const solar = -20.0 * Math.exp(-Math.pow(h - 12, 2) / 8);
      const morning = 15.0 * Math.exp(-Math.pow(h - 7.5, 2) / 3);
      const evening = 60.0 * Math.exp(-Math.pow(h - 18.5, 2) / 3.5);
      return Math.max(5, overnight + solar + morning + evening);
    });
  }
  state.scenario.lmp_forecast_per_mwh = shape.map(v => Math.round(v * 10) / 10);
  // Reset the LMP distribution to formula-driven values for the new P50.
  const d = state.scenario.distributions && state.scenario.distributions.lmp;
  if (d) {
    d.dirty = false;
    recomputeBandFromFormula('lmp', state.scenario);
  }
  // Switch to LMP tab so the user sees the preset they just applied.
  state.activePriceTab = 'lmp';
  renderPriceTabs();
  renderPriceChart();
  renderPriceDistControls();
  updatePresetsVisibility();
}

// ═══ 9. Bootstrap ═════════════════════════════════════════════════════

async function init() {
  try {
    const [meta, scen] = await Promise.all([
      fetch('api/meta').then(r => r.json()),
      fetch('api/default-scenario').then(r => r.json()),
    ]);
    state.meta = meta;
    state.scenario = scen;
    ensureDistributions(state.scenario);
  } catch (err) {
    document.body.innerHTML = `<div style="padding:2rem;color:#f87171">Failed to load scenario: ${err.message}</div>`;
    return;
  }

  writeAsset();
  writeTimeAxis();
  writePolicy();
  writePwl();
  renderPriceTabs();
  renderPriceChart();
  renderPriceDistControls();
  updatePresetsVisibility();
  renderPwlEditorBar();

  $('btn-solve').addEventListener('click', solve);
  $('btn-mc').addEventListener('click', runMonteCarlo);
  // Price CSV upload — delegate click on the visible button to the
  // hidden <input type=file>, so the styling matches the rest of the
  // sidebar instead of the browser's native file input chrome.
  $('btn-csv-upload').addEventListener('click', () => $('inp-csv').click());
  $('inp-csv').addEventListener('change', (e) => {
    const f = e.target.files && e.target.files[0];
    if (f) handleCsvFile(f);
    e.target.value = '';  // allow re-selecting the same file
  });
  $('btn-csv-clear').addEventListener('click', resetCsvOverrides);
  $('sel-dispatch-mode').addEventListener('change', () => {
    readPolicy(); writePolicy(); renderPriceChart(); renderPwlEditorBar();
  });
  $('sel-period-coupling').addEventListener('change', () => {
    readPolicy(); writePolicy(); renderPwlEditorBar();
  });
  const resetBtn = $('pwl-editor-reset');
  if (resetBtn) resetBtn.addEventListener('click', resetAllPwlOverrides);

  // Live time-axis updates — any change immediately resamples and
  // re-renders, no Apply button needed.
  ['inp-start', 'inp-horizon', 'sel-resolution'].forEach(id => {
    const el = $(id);
    if (el) el.addEventListener('change', applyTimeAxis);
  });

  document.querySelectorAll('button[data-preset]').forEach(btn => {
    btn.addEventListener('click', () => applyLmpPreset(btn.dataset.preset));
  });
  const zeroBtn = $('btn-as-zero');
  if (zeroBtn) zeroBtn.addEventListener('click', zeroActiveAsProduct);

  // Sidebar collapse/expand.
  const toggle = $('sidebar-toggle');
  const shell = document.querySelector('.shell');
  if (toggle && shell) {
    toggle.addEventListener('click', () => {
      shell.classList.toggle('sidebar-collapsed');
      // Let the layout settle, then re-render charts so SVG sizes match.
      setTimeout(() => {
        if (state.priceChart) state.priceChart.render();
        if (state.dispatchChart) state.dispatchChart.render();
        if (state.socChart) state.socChart.render();
      }, 280);
    });
  }
  // Dismiss the error banner.
  const closeBtn = $('error-banner-close');
  if (closeBtn) closeBtn.addEventListener('click', clearSolveError);
  // Revalidate on edit so the red border clears as soon as the user fixes the value.
  ['inp-soc-init', 'inp-soc-min', 'inp-soc-max', 'inp-energy', 'inp-eff-charge', 'inp-eff-discharge', 'inp-foldback-dis', 'inp-foldback-ch'].forEach(id => {
    const el = $(id);
    if (el) el.addEventListener('input', () => {
      // Cheap: just re-run validation so red borders track in real time.
      validateScenarioLocal();
    });
  });

  solve();
}

init();
