// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
/*
 * Editable line chart — ported from dashboards/battery/server/static/dashboard.js.
 * Self-contained SVG-based chart with draggable per-period points,
 * grid + axes, hover tooltip + crosshair, optional uncertainty band,
 * optional reference lines. Exported on window.EditableLineChart so
 * the dashboard JS can construct charts without bundling.
 */
(function () {
  'use strict';

  const MIN_BAND_GAP = 0.5;

  // Pick a "nice" set of axis ticks over [min, max]. Targets ~5
  // intervals using a 1/2/5×10^k step so labels are round numbers
  // (0, 0.25, 0.5, 0.75, 1.0 for a 0–1 range; 0, 200, 400 for a 0–800
  // range; etc.).
  function computeNiceTicks(min, max) {
    if (!isFinite(min) || !isFinite(max) || max <= min) return [min, max];
    const targetCount = 5;
    const rawStep = (max - min) / targetCount;
    const exp = Math.floor(Math.log10(rawStep));
    const base = rawStep / Math.pow(10, exp);
    const niceBase = base < 1.5 ? 1 : base < 3 ? 2 : base < 7 ? 5 : 10;
    const step = niceBase * Math.pow(10, exp);
    const first = Math.ceil(min / step) * step;
    const out = [];
    for (let v = first; v <= max + step * 0.001; v += step) {
      // Trim FP noise that accumulates over the addition loop.
      out.push(Math.abs(v) < step * 1e-9 ? 0 : Number(v.toFixed(12)));
    }
    return out.length ? out : [min, max];
  }

  // Format a tick value with a precision derived from the tick step
  // — short labels for big numbers, decimal places for fractional.
  function formatAxisTick(v, allTicks) {
    if (allTicks.length < 2) return String(v);
    const step = Math.abs(allTicks[1] - allTicks[0]);
    if (step === 0) return String(v);
    if (step >= 100) return v.toFixed(0);
    if (step >= 1) return v.toFixed(0);
    if (step >= 0.1) return v.toFixed(1);
    if (step >= 0.01) return v.toFixed(2);
    return v.toFixed(3);
  }

class EditableLineChart {
  constructor(container, opts) {
    this.container = container;
    this.data = opts.data || [];
    // Use ``!== undefined`` (not ``??``) so an explicit ``null`` — meaning
    // "no floor", e.g. LMPs that can go negative — is preserved. Same
    // pattern as setData() below.
    this.min = opts.min !== undefined ? opts.min : 0;
    this.max = opts.max !== undefined ? opts.max : 100;
    // ``fixedAxis``: pin the y-axis exactly to [min, max] regardless of
    // data, and clamp drag at both ends. Used for BA ACE where the
    // ±100 % envelope is intrinsic and we don't want the axis breathing
    // in and out as P50/band points move around.
    this.fixedAxis = !!opts.fixedAxis;
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
    if (opts.fixedAxis !== undefined) this.fixedAxis = !!opts.fixedAxis;
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
    if (this.fixedAxis) return [this.min, this.max];
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

    // Gridlines + Y labels — pick a "nice" tick step (1·10^k, 2·10^k,
    // 5·10^k) over the data span so labels are readable round numbers
    // (e.g. 0/0.25/0.5/0.75/1.0 for a CF chart instead of 0/0/0/1/1).
    const niceTicks = computeNiceTicks(yMin, yMax);
    niceTicks.forEach((v) => {
      const y = toY(v);
      const line = document.createElementNS(svgNS, 'line');
      line.setAttribute('class', 'sc-grid-line');
      line.setAttribute('x1', PAD_L); line.setAttribute('x2', W - PAD_R);
      line.setAttribute('y1', y); line.setAttribute('y2', y);
      svg.appendChild(line);
      const label = document.createElementNS(svgNS, 'text');
      label.setAttribute('class', 'sc-axis-label');
      label.setAttribute('x', PAD_L - 4);
      label.setAttribute('y', y + 3);
      label.setAttribute('text-anchor', 'end');
      label.textContent = formatAxisTick(v, niceTicks);
      svg.appendChild(label);
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
      if (this.fixedAxis && this.max !== null && this.max !== undefined) {
        newVal = Math.min(this.max, newVal);
      }
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

    const noFloor = this.min === null || this.min === undefined;
    let bandPushed = false;
    const onMove = (ev) => {
      const rect = this.svg.getBoundingClientRect();
      const svgY = (ev.clientY - rect.top) * (this.svg.viewBox.baseVal.height / rect.height);
      // Visual position stays inside the chart viewport so the dot never
      // leaves the plot area. When there's no min floor, the logical
      // value follows the cursor past the bottom edge so the user can
      // drag to arbitrarily negative values — the axis rescales on drop.
      const visualY = Math.max(this._PAD_T, Math.min(this._PAD_T + this._innerH, svgY));
      // For axes that can grow (anything other than ``fixedAxis``), let
      // the *value* follow the cursor freely past the current top or
      // bottom edge — the dot still visually clamps to the plot area,
      // and a single ``render()`` on drop expands the axis to the new
      // bounds. Without this, the cursor pinned at ``PAD_T`` always
      // mapped to the current ``yMax``, which forced the user to
      // release-and-re-grab to grow the axis past one ~8 % padding step.
      const valueY = this.fixedAxis ? visualY : svgY;
      const raw = fromY(valueY);
      let newVal = noFloor ? raw : Math.max(this.min, raw);
      if (this.fixedAxis && this.max !== null && this.max !== undefined) {
        newVal = Math.min(this.max, newVal);
      }
      this.data[idx] = newVal;
      point.circle.setAttribute('cy', visualY);
      point.text.setAttribute('y', visualY - 9);
      point.text.textContent = this.formatValue(newVal);
      // Push P10/P90 band edges only when newVal actually crosses them.
      // One-hop: the band has no further neighbours, so no chain.
      if (this._hasBand && this.band) {
        if (newVal > this.band.p90[idx] - MIN_BAND_GAP) {
          this.band.p90[idx] = newVal + MIN_BAND_GAP;
          bandPushed = true;
        }
        if (newVal < this.band.p10[idx] + MIN_BAND_GAP) {
          this.band.p10[idx] = newVal - MIN_BAND_GAP;
          bandPushed = true;
        }
        if (bandPushed) this._updateBandLive();
      }
      this._updatePathsLive();
    };
    const onUp = () => {
      point.circle.classList.remove('dragging');
      point.circle.releasePointerCapture(e.pointerId);
      point.circle.removeEventListener('pointermove', onMove);
      point.circle.removeEventListener('pointerup', onUp);
      point.circle.removeEventListener('pointercancel', onUp);
      this.onChange(this.data.slice());
      if (bandPushed && this.band && this.band.onBandChange) {
        this.band.onBandChange(this.band.p10.slice(), this.band.p90.slice(), idx, 'p50-push');
      }
      // Any non-pinned axis re-renders on drop so it expands (or
      // contracts) to fit the new data envelope. Skipped on
      // ``fixedAxis`` because the axis is intrinsic there.
      if (!this.fixedAxis) this.render();
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
          // Let the cursor leave the chart viewport so the value can
          // exceed the current yMin/yMax — e.g. pulling the last
          // discharge segment above the axis top, or the last charge
          // segment down to $0 when the axis floor is higher. The
          // economic floor (``Math.max(0, …)``) and the segment-
          // ordering clamp bound the value; the axis rescales on
          // release so the dot ends up at the right on-axis position.
          let newVal = Math.max(0, fromY(svgY));
          if (rl.editable && rl.editable.clamp) {
            newVal = rl.editable.clamp(i, newVal);
          }
          values[i] = newVal;
          const dotY = Math.max(PAD_T, Math.min(PAD_T + innerH, toY(newVal)));
          dot.setAttribute('cy', dotY);
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


function periodTimeLabel(i) {
  const t = STATE.scenario && STATE.scenario.time_axis;
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
  const t = STATE.scenario && STATE.scenario.time_axis;
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

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => ({
    '&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;'
  }[c]));
}

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
      } else if (seg.implied) {
        // AS-implied post-clearing deployment — translucent fill with a
        // dashed outline so it reads as "what we expect to flow on top
        // of what actually cleared".
        rect.setAttribute('fill', seg.color);
        rect.setAttribute('fill-opacity', '0.22');
        rect.setAttribute('stroke', seg.color);
        rect.setAttribute('stroke-width', '1.1');
        rect.setAttribute('stroke-dasharray', '3 2');
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
        // Net counts cleared energy + AS-implied deployment but excludes
        // undeployed AS reservation (the hatched segments) — those are
        // capacity *held*, not power flowing in real time.
        let netUp = 0, netDn = 0;
        (d.up || []).forEach(seg => {
          if (!(seg.mw > 1e-9)) return;
          if (!seg.hatch) netUp += seg.mw;
          rows.push(
            `<div class="sc-tooltip-row"><span><span class="sw" style="background:${seg.color}"></span>${escapeHtml(seg.label)}</span>` +
            `<span class="val">+${seg.mw.toFixed(2)} MW</span></div>`
          );
        });
        (d.down || []).forEach(seg => {
          if (!(seg.mw > 1e-9)) return;
          if (!seg.hatch) netDn += seg.mw;
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

  window.EditableLineChart = EditableLineChart;
  window.DispatchChart = DispatchChart;
  window.editableChartHelpers = {
    periodTimeLabel,
    buildTimeAxisTicks,
    installChartHover,
    escapeHtml,
  };
})();
