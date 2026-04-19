// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
// Dashboard client. Extracted from markets/go_c3/dashboard.py's inline script
// and re-wired to hit /api/* endpoints. Rendering logic is unchanged — only
// bootstrap + fetch paths changed.

let CASE_INDEX = {};
let caseKeys = [];
const caseCache = {};
let currentCase = null;
let currentCaseKey = null;
let currentPeriod = 0;
let sortState = {};

function $(id) { return document.getElementById(id); }
function fmt(v, d=2) { return v == null ? '-' : Number(v).toFixed(d); }
function fmtK(v) { return v == null ? '-' : '$' + Number(v).toLocaleString(undefined, {maximumFractionDigits:0}); }

// Extract bus-count from a dataset key like "event4_73" or "event4_8316_d3".
// Returns null when the pattern doesn't match. Trailing `_d{N}` is treated as
// archive-split metadata — the 8316-bus event ships across three tarballs but
// it's still one grid, so all three datasets collapse into the same group.
function _busCount(datasetKey) {
  const m = /^event\d+_(\d+)/.exec(datasetKey);
  return m ? parseInt(m[1]) : null;
}

function buildSidebar() {
  const cl = $('case-list');
  cl.innerHTML = '';
  // Three-level nest: bus count → division → switching mode → scenarios.
  // Group by bus count (not dataset key) so archive splits like
  // event4_8316 / event4_8316_d2 / event4_8316_d3 collapse into one entry.
  const groups = {};  // {buses: {division: {sw: [key, ...]}}}
  caseKeys.forEach(k => {
    const parts = k.split('/');
    const buses = _busCount(parts[0]);
    const div = parts[1];
    const sw = (parts[2] || 'sw0').toUpperCase();
    if (buses == null) return;
    groups[buses] = groups[buses] || {};
    groups[buses][div] = groups[buses][div] || {};
    groups[buses][div][sw] = groups[buses][div][sw] || [];
    groups[buses][div][sw].push(k);
  });

  const busOrder = Object.keys(groups).map(Number).sort((a, b) => a - b);

  busOrder.forEach(buses => {
    const dsHdr = document.createElement('div');
    dsHdr.className = 'group-hdr collapsed';
    dsHdr.innerHTML = `<span class="arrow">&#9660;</span> ${buses} buses`;
    const dsBody = document.createElement('div');
    dsBody.className = 'group-body hidden';
    dsHdr.onclick = () => { dsHdr.classList.toggle('collapsed'); dsBody.classList.toggle('hidden'); };
    cl.appendChild(dsHdr);
    cl.appendChild(dsBody);

    Object.keys(groups[buses]).sort().forEach(div => {
      const divHdr = document.createElement('div');
      divHdr.className = 'div-hdr collapsed';
      divHdr.innerHTML = `<span class="arrow">&#9660;</span> ${div}`;
      const divBody = document.createElement('div');
      divBody.className = 'div-body hidden';
      divHdr.onclick = (e) => { e.stopPropagation(); divHdr.classList.toggle('collapsed'); divBody.classList.toggle('hidden'); };
      dsBody.appendChild(divHdr);
      dsBody.appendChild(divBody);

      Object.keys(groups[buses][div]).sort().forEach(sw => {
        const swHdr = document.createElement('div');
        swHdr.className = 'sw-hdr collapsed';
        swHdr.innerHTML = `<span class="arrow">&#9660;</span> ${sw}`;
        const swBody = document.createElement('div');
        swBody.className = 'sw-body hidden';
        swHdr.onclick = (e) => { e.stopPropagation(); swHdr.classList.toggle('collapsed'); swBody.classList.toggle('hidden'); };
        divBody.appendChild(swHdr);
        divBody.appendChild(swBody);

        groups[buses][div][sw]
          .slice()
          .sort((a, b) => (parseInt(a.split('/')[3]) || 0) - (parseInt(b.split('/')[3]) || 0))
          .forEach(k => {
            const parts = k.split('/');
            const solved = CASE_INDEX[k]?.solved;
            const d = document.createElement('div');
            d.className = 'case-item' + (solved ? '' : ' unsolved');
            const scenarioLabel = 'Scenario ' + (parts[3] || parts[2]);
            d.innerHTML = `<div class="label">${scenarioLabel}</div>`;
            d.onclick = () => selectCase(k);
            d.dataset.key = k;
            swBody.appendChild(d);
          });
      });
    });
  });
}

// Tabs
function wireTabs() {
  $('tab-bar').onclick = e => {
    const tab = e.target.closest('.tab');
    if (!tab) return;
    document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'));
    document.querySelectorAll('.panel').forEach(p => p.classList.remove('active'));
    tab.classList.add('active');
    $('panel-' + tab.dataset.tab).classList.add('active');
  };
}

// Period controls
function syncPeriod(val) {
  currentPeriod = parseInt(val) || 0;
  $('period-num').value = currentPeriod;
  $('period-slider').value = currentPeriod;
  if (currentCase) { renderDevices(); renderBuses(); renderBranches(); renderReserves(); renderReserveSystem(); updateGridPeriod(); }
}

function wirePeriod() {
  $('period-num').oninput = e => syncPeriod(e.target.value);
  $('period-slider').oninput = e => syncPeriod(e.target.value);
}

function selectCase(key) {
  currentCaseKey = key;
  document.querySelectorAll('.case-item').forEach(d => d.classList.toggle('active', d.dataset.key === key));
  $('case-title').textContent = key;

  if (caseCache[key]) {
    _activateCase(key, caseCache[key]);
    return;
  }

  $('no-case').style.display = 'none';
  $('case-view').style.display = '';
  $('score-content').innerHTML = '<div style="padding:32px;color:var(--text-dim)">Loading…</div>';

  fetch('api/cases/' + encodeURI(key), { cache: 'no-store' })
    .then(r => { if (!r.ok) throw new Error(r.status); return r.json(); })
    .then(data => {
      caseCache[key] = data;
      if (currentCaseKey === key) _activateCase(key, data);
    })
    .catch(err => {
      if (currentCaseKey === key) {
        $('score-content').innerHTML = `<div class="banner rose">Failed to load: ${err.message}</div>`;
      }
    });
}

function _activateCase(key, data) {
  currentCase = data;
  currentPeriod = 0;
  $('no-case').style.display = 'none';
  $('case-view').style.display = '';
  $('case-title').textContent = key;

  // Headline objective number
  const oz = data.our_z || {};
  const obj = oz.z;
  const objEl = $('case-objective');
  if (obj != null) {
    objEl.classList.remove('empty');
    objEl.textContent = Number(obj).toLocaleString(undefined, {maximumFractionDigits: 0});
  } else {
    objEl.classList.add('empty');
    objEl.textContent = '—';
  }

  const np = currentCase.periods.count;
  $('period-slider').max = np - 1; $('period-slider').value = 0;
  $('period-num').max = np - 1; $('period-num').value = 0;
  renderSummary();
  renderScores();
  renderObjective();
  renderDevices();
  renderBuses();
  renderBranches();
  renderReserves();
  renderReserveSystem();
  renderContingencies();
  renderViolations();
  renderLog();
  renderGrid();
}

function renderSummary() {
  const c = currentCase;
  const ci = c.case_info || {};
  const dc = c.dc_summary || {};
  const ac = c.ac_summary || {};
  const pol = c.policy || {};
  const rr = c.run_report || {};
  const vs = c.violation_summary || {};

  function infoRow(lbl, val, cls) { return `<div class="row"><span class="lbl">${lbl}</span><span class="val ${cls||''}">${val}</span></div>`; }

  const escErr = (s) => String(s).replace(/[&<>]/g, m => ({'&':'&amp;','<':'&lt;','>':'&gt;'}[m]));
  const unsolvedBanner = c.solved === false
    ? (c.solve_error
        ? `<div class="banner rose">Solve failed — showing winner/leaderboard reference only<br><span style="font-family:monospace;font-size:11px;opacity:0.85">${escErr(c.solve_error)}</span></div>`
        : '<div class="banner rose">Not solved — showing winner/leaderboard reference only</div>')
    : '';

  $('panel-case-info').innerHTML = unsolvedBanner + '<h3>Case</h3>' +
    infoRow('Network', ci.network_model || '-') +
    infoRow('Scenario', ci.scenario_id ?? '-') +
    infoRow('Periods', ci.n_periods ?? '-') +
    infoRow('Base MVA', ci.base_mva ?? '-') +
    infoRow('Buses', ci.n_buses ?? '-') +
    infoRow('AC Lines', ci.n_ac_lines ?? '-') +
    infoRow('Transformers', ci.n_transformers ?? '-') +
    infoRow('DC Lines', ci.n_dc_lines ?? '-') +
    infoRow('Producers', ci.n_producers ?? '-') +
    infoRow('Consumers', ci.n_consumers ?? '-') +
    infoRow('Formulation', pol.formulation || '-') +
    infoRow('LP Solver', pol.lp_solver || '-') +
    infoRow('Total Solve', fmt(rr.solve_seconds, 1) + 's');

  const dcv = c.dc_violations || {};
  const dcvk = dcv.by_kind || {};
  const dcStatus = dc.total_cost != null ? 'Solved' : 'N/A';
  let dcHtml = '<h3>DC SCUC</h3>' +
    infoRow('Status', dcStatus, dc.total_cost != null ? 'ok' : '') +
    infoRow('Solve Time', dc.solve_time_secs != null ? fmt(dc.solve_time_secs, 1) + 's' : '-') +
    infoRow('Commitment', pol.commitment_mode || '-') +
    (pol.commitment_solution_path ? infoRow('Commit Source', 'fixed (winner)', 'warn') :
     pol.commitment_seed_mode && pol.commitment_seed_mode !== 'none' ? infoRow('Commit Seed', pol.commitment_seed_mode, 'warn') : '') +
    infoRow('Total Cost', fmtK(dc.total_cost)) +
    infoRow('Energy Cost', fmtK(dc.energy_cost)) +
    infoRow('No-Load Cost', fmtK(dc.no_load_cost)) +
    infoRow('Startup Cost', fmtK(dc.startup_cost)) +
    infoRow('Reserve Cost', fmtK(dc.reserve_cost));
  if (dcv.total_violations) {
    dcHtml += '<div style="border-top:1px solid var(--border-sub);margin-top:6px;padding-top:6px">';
    dcHtml += infoRow('Violations', dcv.total_violations, 'warn');
    dcHtml += infoRow('Total Penalty', fmtK(dcv.total_penalty), dcv.total_penalty > 0 ? 'penalty' : 'ok');
    dcHtml += infoRow('Curtailment', fmt(dcv.curtailment_mw, 2) + ' MW', dcv.curtailment_mw > 0 ? 'penalty' : '');
    dcHtml += infoRow('Excess', fmt(dcv.excess_mw, 2) + ' MW', dcv.excess_mw > 0 ? 'penalty' : '');
    for (const [kind, kv] of Object.entries(dcvk)) {
      dcHtml += infoRow(kind, `${kv.count} (${fmt(kv.total_slack_mw,1)} MW, ${fmtK(kv.total_penalty)})`, kv.total_penalty > 0 ? 'warn' : '');
    }
    dcHtml += '</div>';
  }
  $('panel-dc-stats').innerHTML = dcHtml;

  const acMode = ac.mode || pol.ac_reconcile_mode || 'none';
  const acSolver = ac.nlp_solver || pol.nlp_solver || pol.ac_nlp_solver || '-';
  const acStatus = ac.solve_time_secs != null ? 'Solved' : 'N/A';
  const thermCls = (ac.thermal_slack_count || 0) > 0 ? 'warn' : 'ok';
  const busPCls = (vs.bus_p_balance?.total_penalty_cost || 0) > 0 ? 'penalty' : 'ok';
  $('panel-ac-stats').innerHTML = '<h3>AC SCED</h3>' +
    infoRow('Status', acStatus, ac.solve_time_secs != null ? 'ok' : '') +
    infoRow('Mode', acMode) +
    infoRow('NLP Solver', acSolver) +
    infoRow('Solve Time', ac.solve_time_secs != null ? fmt(ac.solve_time_secs, 2) + 's' : '-') +
    infoRow('Total Cost', fmtK(ac.total_cost)) +
    infoRow('Energy Cost', fmtK(ac.energy_cost)) +
    infoRow('Thermal Slacks', (ac.thermal_slack_count ?? '-'), thermCls) +
    infoRow('Commit Refine Iters', ac.commitment_refinement_iterations ?? '-') +
    infoRow('Bus P Penalty', fmtK(vs.bus_p_balance?.total_penalty_cost), busPCls) +
    infoRow('Bus Q Penalty', fmtK(vs.bus_q_balance?.total_penalty_cost)) +
    infoRow('Thermal Penalty', fmtK(vs.branch_thermal?.total_penalty_cost)) +
    infoRow('Total Viol Penalty', fmtK(vs.total_penalty_cost), (vs.total_penalty_cost || 0) > 0 ? 'penalty' : 'ok');
}

const Z_ROWS = [
  ['obj', 'Objective (z)'],
  ['z_base', 'z_base (surplus)'],
  ['z_cost', 'z_cost (energy+commit)'],
  ['z_penalty', 'z_penalty (violations)'],
  ['z_value', 'z_value (load served)'],
  ['z_k_worst_case', 'z_k (worst contingency)'],
  ['z_k_average_case', 'z_k (avg contingency)'],
  ['_sep', ''],
  ['sum_sd_t_z_on', 'No-load cost slack'],
  ['sum_sd_t_z_su', 'Startup cost slack'],
  ['sum_pr_t_z_p', 'Producer P slack'],
  ['sum_cs_t_z_p', 'Consumer P slack'],
  ['sum_bus_t_z_p', 'Bus P balance penalty'],
  ['sum_bus_t_z_q', 'Bus Q balance penalty'],
  ['sum_acl_t_z_s', 'AC line thermal slack'],
  ['sum_xfr_t_z_s', 'Transformer thermal slack'],
  ['sum_prz_t_z_rgu', 'Reserve up shortfall'],
  ['sum_prz_t_z_rgd', 'Reserve down shortfall'],
  ['sum_prz_t_z_scr', 'Spinning reserve shortfall'],
  ['sum_prz_t_z_nsc', 'Non-spin reserve shortfall'],
  ['sum_prz_t_z_rru', 'Ramp up reserve shortfall'],
  ['sum_prz_t_z_rrd', 'Ramp down reserve shortfall'],
  ['sum_sd_t_z_rgd', 'Device reserve down slack'],
  ['sum_sd_t_z_rgu', 'Device reserve up slack'],
  ['_sep2', ''],
  ['feas', 'Feasible'],
  ['phys_feas', 'Physically Feasible'],
];

function renderScores() {
  const c = currentCase;
  const oz = c.our_z || {};
  const wz = c.winner_z || {};
  const lb = c.leaderboard || [];
  const winTeam = lb.length > 0 ? `#${lb[0].rank||1} ${lb[0].team}` : 'Winner';
  const ourObj = oz.z;
  const ourTime = c.run_report?.solve_seconds;

  let lbRows = lb.map(e => ({ team: e.team, obj: e.objective, time: e.runtime_seconds, rank: e.rank, sw: e.switching_mode || '', isSurge: false }));
  if (ourObj != null) {
    lbRows.sort((a, b) => (b.obj || 0) - (a.obj || 0));
    let inserted = false;
    const merged = [];
    for (const e of lbRows) {
      if (!inserted && e.obj != null && ourObj > e.obj) {
        merged.push({ team: 'Surge', obj: ourObj, time: ourTime, rank: null, isSurge: true });
        inserted = true;
      }
      merged.push(e);
    }
    if (!inserted) merged.push({ team: 'Surge', obj: ourObj, time: ourTime, rank: null, isSurge: true });
    lbRows = merged;
  }

  let html = '';
  if (c.leaderboard_source_sw) {
    html += `<div class="banner amber">No ${currentCaseKey?.split('/')[2]?.toUpperCase()||'SW0'} competition data — showing ${c.leaderboard_source_sw} results</div>`;
  }
  html += '<h3>Leaderboard</h3>';
  html += '<table><thead><tr><th style="text-align:left">Team</th><th>Mode</th><th>Objective (z)</th><th>Runtime</th></tr></thead><tbody>';
  lbRows.forEach(e => {
    const trCls = e.isSurge ? ' class="surge-row"' : '';
    html += `<tr${trCls}><td style="text-align:left">${e.isSurge ? '&#9654; ' : ''}${e.team}</td><td>${e.isSurge ? 'SW0' : (e.sw||'-')}</td><td>${e.obj != null ? Number(e.obj).toLocaleString(undefined,{maximumFractionDigits:0}) : '-'}</td><td>${fmt(e.time,1)}s</td></tr>`;
  });
  html += '</tbody></table>';

  html += '<h3>Z-Score Breakdown</h3>';
  html += `<table><thead><tr><th style="text-align:left">Component</th><th>Surge</th><th>${winTeam}</th><th>Delta</th></tr></thead><tbody>`;
  for (const [key, label] of Z_ROWS) {
    if (key.startsWith('_sep')) {
      html += '<tr><td colspan="4" style="border-bottom:1px solid var(--border);padding:2px"></td></tr>';
      continue;
    }
    const ov = oz[key];
    const wv = wz[key];
    const isBool = key === 'feas' || key === 'phys_feas';
    let oStr, wStr, dStr;
    if (isBool) {
      oStr = ov != null ? (ov ? '<span class="ok">YES</span>' : '<span class="penalty">NO</span>') : '-';
      wStr = wv != null ? (wv ? '<span class="ok">YES</span>' : '<span class="penalty">NO</span>') : '-';
      dStr = '';
    } else {
      oStr = ov != null ? Number(ov).toLocaleString(undefined, {maximumFractionDigits:2}) : '-';
      wStr = wv != null ? Number(wv).toLocaleString(undefined, {maximumFractionDigits:2}) : '-';
      if (ov != null && wv != null) {
        const d = ov - wv;
        const cls = Math.abs(d) > 0.01 ? (d > 0 ? 'penalty' : 'ok') : '';
        dStr = `<span class="${cls}">${d >= 0 ? '+' : ''}${Number(d).toLocaleString(undefined, {maximumFractionDigits:2})}</span>`;
      } else { dStr = '-'; }
    }
    html += `<tr><td style="text-align:left">${label}</td><td>${oStr}</td><td>${wStr}</td><td>${dStr}</td></tr>`;
  }
  html += '</tbody></table>';

  $('score-content').innerHTML = html;
}

// ─── Objective tab ──────────────────────────────────────────────────────
const OBJ_BUCKETS = [
  { key: 'energy',   label: 'Energy',   note: 'DC is negative = net surplus · AC is producer cost only' },
  { key: 'no_load',  label: 'No-load',  note: 'commitment-driven' },
  { key: 'startup',  label: 'Startup',  note: '' },
  { key: 'shutdown', label: 'Shutdown', note: '' },
  { key: 'reserve',  label: 'Reserve',  note: '' },
  { key: 'tracking', label: 'Tracking', note: 'AC regularization pulling producers toward DC schedule — kept in total' },
  { key: 'adder',    label: 'Adder',    note: '' },
  { key: 'other',    label: 'Other',    note: '' },
];

const OBJ_PENALTY_ORDER = [
  'thermal', 'reserve_shortfall', 'power_balance_p', 'power_balance_q',
  'flowgate', 'ramp', 'voltage', 'angle', 'headroom_footroom', 'energy_window',
];

const OBJ_PENALTY_LABELS = {
  thermal: 'Thermal overload',
  reserve_shortfall: 'Reserve shortfall',
  power_balance_p: 'Power balance P',
  power_balance_q: 'Power balance Q',
  flowgate: 'Flowgate',
  ramp: 'Ramp',
  voltage: 'Voltage',
  angle: 'Angle',
  headroom_footroom: 'Headroom / footroom',
  energy_window: 'Energy window',
};

function fmtDollar(v, withSign=false) {
  if (v == null || !isFinite(v)) return '—';
  const n = Math.abs(v) < 0.005 ? 0 : v;
  const sign = (withSign && n > 0) ? '+' : '';
  return sign + '$' + Number(n).toLocaleString(undefined, { maximumFractionDigits: 0 });
}

function fmtQty(v, unit) {
  if (v == null || !isFinite(v)) return '—';
  return Number(v).toLocaleString(undefined, { maximumFractionDigits: 2 }) + (unit ? ' ' + unit : '');
}

function penaltyLookup(list) {
  const out = {};
  (list || []).forEach(p => { out[p.key] = p; });
  return out;
}

function renderObjective() {
  const obj = currentCase.objective || { dc: null, ac: null };
  const dc = obj.dc;
  const ac = obj.ac;
  const dcSum = dc?.summary || {};
  const acSum = ac?.summary || {};
  const rr = currentCase.run_report || {};
  const dcMeta = currentCase.dc_summary || {};
  const acMeta = currentCase.ac_summary || {};
  const pol = currentCase.policy || {};

  // ─── Section 1 · headline cards ────────────────────────────────────────
  function headlineCard(title, totalCost, noteText, metaBits) {
    const valStr = totalCost != null
      ? (totalCost < 0 ? '-$' : '$') + Math.abs(totalCost).toLocaleString(undefined, { maximumFractionDigits: 0 })
      : null;
    const metaHtml = metaBits.map(m => `<div><span class="lbl">${m.lbl}</span>${m.val}</div>`).join('');
    return `
      <div class="obj-headline">
        <div class="hdr">${title}</div>
        <div class="value ${valStr ? '' : 'empty'}">${valStr ?? '—'}</div>
        <div class="note">${noteText}</div>
        <div class="meta">${metaHtml}</div>
      </div>`;
  }

  const dcCard = dc
    ? headlineCard(
        'DC SCUC · Objective',
        dcSum.total_cost,
        'Social surplus maximization (negative = net surplus) · MIP · unit commitment · DC flow',
        [
          { lbl: 'solver', val: pol.lp_solver || '—' },
          { lbl: 'time', val: (dcMeta.solve_time_secs != null ? fmt(dcMeta.solve_time_secs, 1) + 's' : '—') },
        ])
    : '<div class="obj-headline"><div class="hdr">DC SCUC</div><div class="value empty">—</div><div class="note">No DC dispatch result on disk.</div></div>';

  const acCard = ac
    ? headlineCard(
        'AC SCED · Objective',
        acSum.total_cost,
        'Producer cost minimization with commitment fixed · NLP · AC flow · includes DC-tracking regularization',
        [
          { lbl: 'solver', val: pol.nlp_solver || pol.ac_nlp_solver || '—' },
          { lbl: 'time', val: (acMeta.solve_time_secs != null ? fmt(acMeta.solve_time_secs, 2) + 's' : '—') },
        ])
    : '<div class="obj-headline"><div class="hdr">AC SCED</div><div class="value empty">—</div><div class="note">No AC dispatch result on disk.</div></div>';

  let html = `<div class="obj-headline-row">${dcCard}${acCard}</div>`;

  // ─── Section 2 · aggregate components table ────────────────────────────
  html += '<div class="obj-section-title">Objective Components · DC vs AC</div>';
  html += '<table class="obj-table"><thead><tr>' +
    '<th style="text-align:left">Component</th><th>DC ($)</th><th>AC ($)</th><th>Δ (AC − DC)</th><th style="text-align:left">Note</th>' +
    '</tr></thead><tbody>';
  for (const row of OBJ_BUCKETS) {
    const d = dcSum[row.key];
    const a = acSum[row.key];
    const delta = (d != null && a != null) ? (a - d) : null;
    const deltaCls = delta != null && Math.abs(delta) >= 1 ? (delta > 0 ? 'penalty' : 'ok') : '';
    html += `<tr><td class="bucket-label">${row.label}</td>` +
      `<td>${fmtDollar(d)}</td><td>${fmtDollar(a)}</td>` +
      `<td class="${deltaCls}">${fmtDollar(delta, true)}</td>` +
      `<td style="text-align:left;color:var(--text-muted);font-family:var(--font);font-size:10.5px">${row.note}</td></tr>`;
  }
  // Penalty row — expandable.
  const dcPenTotal = dcSum.penalty;
  const acPenTotal = acSum.penalty;
  const penDelta = (dcPenTotal != null && acPenTotal != null) ? (acPenTotal - dcPenTotal) : null;
  const penDeltaCls = penDelta != null && Math.abs(penDelta) >= 0.5 ? (penDelta > 0 ? 'penalty' : 'ok') : '';
  html += `<tr id="pen-row"><td class="bucket-label"><span class="expand-toggle" id="pen-toggle">▸</span>Penalty</td>` +
    `<td>${fmtDollar(dcPenTotal)}</td><td>${fmtDollar(acPenTotal)}</td>` +
    `<td class="${penDeltaCls}">${fmtDollar(penDelta, true)}</td>` +
    `<td style="text-align:left;color:var(--text-muted);font-family:var(--font);font-size:10.5px">click to expand breakdown</td></tr>`;
  const dcPenLookup = penaltyLookup(dc?.penalty_summary);
  const acPenLookup = penaltyLookup(ac?.penalty_summary);
  for (const key of OBJ_PENALTY_ORDER) {
    const dEntry = dcPenLookup[key] || {};
    const aEntry = acPenLookup[key] || {};
    const dc_ = dEntry.cost;
    const ac_ = aEntry.cost;
    const delta = (dc_ != null && ac_ != null) ? (ac_ - dc_) : null;
    const deltaCls = delta != null && Math.abs(delta) >= 0.5 ? (delta > 0 ? 'penalty' : 'ok') : '';
    const qtyDisplay = (aEntry.quantity ?? dEntry.quantity) != null
      ? ' · ' + fmtQty(Math.max(dEntry.quantity || 0, aEntry.quantity || 0), dEntry.quantity_unit || aEntry.quantity_unit)
      : '';
    html += `<tr class="penalty-sub hidden"><td class="bucket-label">${OBJ_PENALTY_LABELS[key]}</td>` +
      `<td>${fmtDollar(dc_)}</td><td>${fmtDollar(ac_)}</td>` +
      `<td class="${deltaCls}">${fmtDollar(delta, true)}</td>` +
      `<td style="text-align:left;color:var(--text-dim);font-family:var(--mono);font-size:10px">${qtyDisplay.replace(/^ · /, '')}</td></tr>`;
  }
  // Total row
  html += `<tr class="total-row"><td class="bucket-label">Total (solver objective)</td>` +
    `<td>${fmtDollar(dcSum.total_cost)}</td><td>${fmtDollar(acSum.total_cost)}</td>` +
    `<td>${fmtDollar((dcSum.total_cost != null && acSum.total_cost != null) ? (acSum.total_cost - dcSum.total_cost) : null, true)}</td>` +
    `<td style="text-align:left;color:var(--purple);font-family:var(--font);font-size:10.5px">raw value minimized by each solver</td></tr>`;
  html += '</tbody></table>';

  // ─── Section 3 · per-period table (transposed: periods as columns) ─────
  html += '<div class="obj-section-title">Per-Period Dispatch Cost · DC vs AC</div>';
  const dcRows = dc?.per_period || [];
  const acRows = ac?.per_period || [];
  const np = Math.max(dcRows.length, acRows.length);

  // Row categories — each expands to three sub-rows (DC, AC, Δ).
  const ROW_CATEGORIES = [
    { key: 'energy',   label: 'Energy' },
    { key: 'no_load',  label: 'No-Load' },
    { key: 'startup',  label: 'Startup' },
    { key: 'reserve',  label: 'Reserve' },
    { key: 'tracking', label: 'Tracking' },
    { key: 'penalty',  label: 'Penalty' },
    { key: 'total',    label: 'Total', emphasized: true },
  ];

  // Build header: category label column + period columns + TOTAL column.
  const periodHeaders = [];
  for (let i = 0; i < np; i++) periodHeaders.push(`P${i}`);
  periodHeaders.push('Σ');
  html += '<div class="scroll-table"><table class="obj-table obj-periods"><thead><tr>' +
    '<th style="text-align:left">Component</th>' +
    '<th style="text-align:left;width:34px">Stage</th>' +
    periodHeaders.map(h => `<th>${h}</th>`).join('') +
    '</tr></thead><tbody>';

  function cellD(v, cls='') {
    return `<td class="${cls}">${fmtDollar(v)}</td>`;
  }

  function getPeriodValue(rows, i, key) {
    if (key === 'total') {
      return rows[i]?.total ?? null;
    }
    return rows[i]?.[key] ?? null;
  }
  function getSummaryValue(summary, key) {
    if (key === 'total') return summary?.total_cost ?? null;
    return summary?.[key] ?? null;
  }

  for (const cat of ROW_CATEGORIES) {
    const emphasize = cat.emphasized ? 'obj-cat-total' : '';
    // DC row
    let dcRow = `<tr class="obj-cat-row obj-cat-row-first ${emphasize}"><td class="bucket-label" rowspan="3">${cat.label}</td><td class="stage-label stage-dc">DC</td>`;
    for (let i = 0; i < np; i++) {
      const v = getPeriodValue(dcRows, i, cat.key);
      const cls = (cat.key === 'penalty' && v != null && v > 0.5) ? 'penalty' : '';
      dcRow += cellD(v, cls);
    }
    dcRow += cellD(getSummaryValue(dcSum, cat.key), 'stage-total-cell');
    dcRow += '</tr>';
    // AC row
    let acRow = `<tr class="obj-cat-row ${emphasize}"><td class="stage-label stage-ac">AC</td>`;
    for (let i = 0; i < np; i++) {
      const v = getPeriodValue(acRows, i, cat.key);
      const cls = (cat.key === 'penalty' && v != null && v > 0.5) ? 'penalty' : '';
      acRow += cellD(v, cls);
    }
    acRow += cellD(getSummaryValue(acSum, cat.key), 'stage-total-cell');
    acRow += '</tr>';
    // Δ row
    let dRow = `<tr class="obj-cat-row obj-cat-row-last ${emphasize}"><td class="stage-label stage-delta">Δ</td>`;
    for (let i = 0; i < np; i++) {
      const dv = getPeriodValue(dcRows, i, cat.key);
      const av = getPeriodValue(acRows, i, cat.key);
      const delta = (dv != null && av != null) ? (av - dv) : null;
      const cls = delta != null && Math.abs(delta) >= 1 ? (delta > 0 ? 'penalty' : 'ok') : '';
      dRow += cellD(delta, cls);
    }
    const dcS_ = getSummaryValue(dcSum, cat.key);
    const acS_ = getSummaryValue(acSum, cat.key);
    const gDelta = (dcS_ != null && acS_ != null) ? (acS_ - dcS_) : null;
    const gDeltaCls = gDelta != null && Math.abs(gDelta) >= 1 ? (gDelta > 0 ? 'penalty' : 'ok') : '';
    dRow += cellD(gDelta, `stage-total-cell ${gDeltaCls}`);
    dRow += '</tr>';
    html += dcRow + acRow + dRow;
  }
  html += '</tbody></table></div>';

  $('objective-content').innerHTML = html;

  // Wire the penalty toggle now that the DOM is in place.
  const toggle = $('pen-toggle');
  if (toggle) {
    toggle.parentElement.parentElement.style.cursor = 'pointer';
    toggle.parentElement.parentElement.onclick = () => {
      const subs = document.querySelectorAll('#panel-objective .penalty-sub');
      const expanded = toggle.textContent === '▾';
      toggle.textContent = expanded ? '▸' : '▾';
      subs.forEach(el => el.classList.toggle('hidden', expanded));
    };
  }
}

function renderRows(rows) {
  return rows.map(r => {
    const ccls = r._ccls || [];
    const cells = r._cells.map((c, i) => ccls[i] ? `<td class="${ccls[i]}">${c}</td>` : `<td>${c}</td>`);
    const uid = r._uid || '';
    const uidAttr = uid ? ` data-uid="${uid}" style="cursor:pointer"` : '';
    return `<tr class="${r._cls||''}"${uidAttr}>${cells.join('')}</tr>`;
  }).join('');
}

function makeTable(tableId, headers, rows, defaultSortCol, defaultAsc) {
  const thead = $(tableId).querySelector('thead');
  const tbody = $(tableId).querySelector('tbody');
  if (defaultSortCol != null) {
    const asc = defaultAsc ?? true;
    rows.sort((a, b) => {
      let va = a._vals[defaultSortCol], vb = b._vals[defaultSortCol];
      if (typeof va === 'string') return asc ? va.localeCompare(vb) : vb.localeCompare(va);
      return asc ? va - vb : vb - va;
    });
  }
  thead.innerHTML = '<tr>' + headers.map((h,i) => `<th data-col="${i}">${h}</th>`).join('') + '</tr>';
  tbody.innerHTML = renderRows(rows);
  thead.onclick = e => {
    const th = e.target.closest('th');
    if (!th) return;
    const col = parseInt(th.dataset.col);
    const key = tableId + '_' + col;
    sortState[key] = !(sortState[key] || false);
    const asc = sortState[key];
    rows.sort((a, b) => {
      let va = a._vals[col], vb = b._vals[col];
      if (typeof va === 'string') return asc ? va.localeCompare(vb) : vb.localeCompare(va);
      return asc ? va - vb : vb - va;
    });
    tbody.innerHTML = renderRows(rows);
  };
}

function renderDevices() {
  const t = currentPeriod;
  const devs = currentCase.periods.devices;
  const gens = devs.filter(d => d.type === 'producer');
  const loads = devs.filter(d => d.type === 'consumer');

  const genHdr = ['UID', 'Bus', 'Pmin', 'Pmax', 'On(AC)', 'On(Win)', 'DC P', 'AC P', 'Win P', 'dP', 'AC Q', 'Win Q', 'DC LMP', 'AC LMP', 'MC($/MWh)'];
  const loadHdr = ['UID', 'Bus', 'Kind', 'Pmin', 'Pmax', 'On(AC)', 'On(Win)', 'DC P', 'AC P', 'Win P', 'dP', 'AC Q', 'Win Q', 'DC LMP', 'AC LMP', 'MC($/MWh)'];
  function devFields(d) {
    const pmin = (d.p_lb||[])[t] || 0;
    const pmax = (d.p_ub||[])[t] || 0;
    const dc_p = (d.dc_p||[])[t] || 0;
    const ac_p = (d.ac_p||[])[t] || 0;
    const win_p = (d.winner_p||[])[t];
    const ac_q = (d.ac_q||[])[t] || 0;
    const win_q = (d.winner_q||[])[t];
    const ac_on = (d.ac_on||[])[t] ?? '-';
    const win_on = (d.winner_on||[])[t];
    const dp = win_p != null ? ac_p - win_p : null;
    const pDev = dp != null && Math.abs(dp) > 1;
    const qDev = (win_q != null && Math.abs(ac_q - win_q) > 1);
    const dc_lmp = (d.dc_lmp||[])[t] || 0;
    const ac_lmp = (d.ac_lmp||[])[t] || 0;
    const mc = (d.mc||[])[t] || 0;
    return { pmin, pmax, dc_p, ac_p, win_p, ac_q, win_q, ac_on, win_on, dp, pDev, qDev, dc_lmp, ac_lmp, mc };
  }
  function genRow(d) {
    const f = devFields(d);
    return {
      _uid: d.uid,
      _cells: [d.uid, d.bus, fmt(f.pmin,1), fmt(f.pmax,1), f.ac_on, f.win_on ?? '-', fmt(f.dc_p,1), fmt(f.ac_p,1), f.win_p != null ? fmt(f.win_p,1) : '-', f.dp != null ? fmt(f.dp,1) : '-', fmt(f.ac_q,1), f.win_q != null ? fmt(f.win_q,1) : '-', fmt(f.dc_lmp,2), fmt(f.ac_lmp,2), fmt(f.mc,2)],
      _vals: [d.uid, d.bus, f.pmin, f.pmax, f.ac_on, f.win_on ?? 0, f.dc_p, Math.abs(f.ac_p), f.win_p ?? 0, f.dp ?? 0, f.ac_q, f.win_q ?? 0, f.dc_lmp, f.ac_lmp, f.mc],
      _ccls: ['','','','','','','', f.pDev?'dev':'', f.pDev?'dev':'', f.pDev?'dev':'', f.qDev?'dev':'', f.qDev?'dev':'', '','',''],
      _cls: '',
    };
  }
  function loadRow(d) {
    const f = devFields(d);
    const kind = d.dispatchable ? 'disp' : 'fixed';
    return {
      _uid: d.uid,
      _cells: [d.uid, d.bus, kind, fmt(f.pmin,1), fmt(f.pmax,1), f.ac_on, f.win_on ?? '-', fmt(f.dc_p,1), fmt(f.ac_p,1), f.win_p != null ? fmt(f.win_p,1) : '-', f.dp != null ? fmt(f.dp,1) : '-', fmt(f.ac_q,1), f.win_q != null ? fmt(f.win_q,1) : '-', fmt(f.dc_lmp,2), fmt(f.ac_lmp,2), fmt(f.mc,2)],
      _vals: [d.uid, d.bus, kind, f.pmin, f.pmax, f.ac_on, f.win_on ?? 0, f.dc_p, Math.abs(f.ac_p), f.win_p ?? 0, f.dp ?? 0, f.ac_q, f.win_q ?? 0, f.dc_lmp, f.ac_lmp, f.mc],
      _ccls: ['','','','','','','','', f.pDev?'dev':'', f.pDev?'dev':'', f.pDev?'dev':'', f.qDev?'dev':'', f.qDev?'dev':'', '','',''],
      _cls: '',
    };
  }
  makeTable('gen-table', genHdr, gens.map(genRow), 7, false);
  makeTable('load-table', loadHdr, loads.map(loadRow), 8, false);
  for (const tid of ['gen-table', 'load-table']) {
    $(tid).querySelector('tbody').onclick = e => {
      const tr = e.target.closest('tr');
      if (tr && tr.dataset.uid) showOfferCurve(tr.dataset.uid);
    };
  }

  const hvdc = currentCase.periods.hvdc;
  const hhdr = ['UID', 'DC P(MW)', 'AC P(MW)', 'Win P(MW)', 'AC Q_fr', 'AC Q_to', 'Win Q_fr', 'Win Q_to'];
  const hrows = hvdc.map(h => {
    const dc_p = (h.dc_p||[])[t] || 0;
    const ac_p = (h.ac_p||[])[t] || 0;
    const win_p = (h.winner_p||[])[t];
    return {
      _cells: [h.uid, fmt(dc_p,1), fmt(ac_p,1), win_p != null ? fmt(win_p,1) : '-', fmt((h.ac_q_fr||[])[t]||0,1), fmt((h.ac_q_to||[])[t]||0,1), (h.winner_q_fr||[])[t] != null ? fmt((h.winner_q_fr||[])[t],1) : '-', (h.winner_q_to||[])[t] != null ? fmt((h.winner_q_to||[])[t],1) : '-'],
      _vals: [h.uid, dc_p, ac_p, win_p ?? 0, (h.ac_q_fr||[])[t]||0, (h.ac_q_to||[])[t]||0, (h.winner_q_fr||[])[t]??0, (h.winner_q_to||[])[t]??0],
      _cls: '',
    };
  });
  makeTable('hvdc-table', hhdr, hrows);
}

function renderBuses() {
  const t = currentPeriod;
  const buses = currentCase.periods.buses || [];
  const hdr = ['UID', 'AC Vm', 'Win Vm', 'AC Va', 'Win Va', 'AC P_inj', 'Win P_inj', 'dP_inj', 'DC P_inj', 'AC Q_inj', 'Win Q_inj', 'DC LMP', 'AC LMP'];
  const rows = buses.map(b => {
    const ac_vm = (b.ac_vm||[])[t];
    const win_vm = (b.winner_vm||[])[t];
    const ac_va = (b.ac_va||[])[t];
    const win_va = (b.winner_va||[])[t];
    const ac_pi = (b.ac_p_inj||[])[t] || 0;
    const dc_pi = (b.dc_p_inj||[])[t] || 0;
    const ac_qi = (b.ac_q_inj||[])[t] || 0;
    const dc_lmp = (b.dc_lmp||[])[t] || 0;
    const ac_lmp = (b.ac_lmp||[])[t] || 0;
    const win_pi = (b.winner_p_inj||[])[t];
    const win_qi = (b.winner_q_inj||[])[t];
    const dp = win_pi != null ? ac_pi - win_pi : null;
    const vmDev = (ac_vm != null && win_vm != null && Math.abs(ac_vm - win_vm) > 0.005);
    const vaDev = (ac_va != null && win_va != null && Math.abs(ac_va - win_va) > 0.01);
    const pDev = dp != null && Math.abs(dp) > 1;
    return {
      _cells: [b.uid, fmt(ac_vm,4), win_vm != null ? fmt(win_vm,4) : '-', fmt(ac_va,5), win_va != null ? fmt(win_va,5) : '-', fmt(ac_pi,1), win_pi != null ? fmt(win_pi,1) : '-', dp != null ? fmt(dp,1) : '-', fmt(dc_pi,1), fmt(ac_qi,1), win_qi != null ? fmt(win_qi,1) : '-', fmt(dc_lmp,2), fmt(ac_lmp,2)],
      _vals: [b.uid, ac_vm??0, win_vm??0, ac_va??0, win_va??0, ac_pi, win_pi??0, dp??0, dc_pi, ac_qi, win_qi??0, dc_lmp, ac_lmp],
      _ccls: ['', vmDev?'dev':'', vmDev?'dev':'', vaDev?'dev':'', vaDev?'dev':'', pDev?'dev':'', pDev?'dev':'', pDev?'dev':'', '','','', '',''],
      _cls: '',
    };
  });
  makeTable('bus-table', hdr, rows);
}

function renderBranches() {
  const t = currentPeriod;
  const br = currentCase.periods.branches;
  const hdr = ['UID', 'Type', 'From', 'To', 'DC Flow', 'AC Flow', 'Win Flow', 'Limit', 'AC Overload', 'DC Slack', 'DC Penalty'];
  const rows = br.map(b => {
    const dcflow = (b.dc_flow_mva||[])[t] || 0;
    const flow = (b.flow_mva||[])[t] || 0;
    const wflow = (b.winner_flow_mva||[])[t];
    const limit = b.limit_mva || 9999;
    const over = Math.max(0, flow - limit);
    const dcslack = (b.dc_slack_mw||[])[t] || 0;
    const dcpen = (b.dc_penalty||[])[t] || 0;
    return {
      _cells: [b.uid, b.type, b.fr_bus, b.to_bus, fmt(dcflow,1), fmt(flow,1), wflow != null ? fmt(wflow,1) : '-', fmt(limit,1), over > 0.1 ? `<span class="penalty">${fmt(over,1)}</span>` : '-', dcslack > 0.01 ? `<span class="warn">${fmt(dcslack,1)}</span>` : '-', dcpen > 0.001 ? `<span class="warn">${fmtK(dcpen)}</span>` : '-'],
      _vals: [b.uid, b.type, b.fr_bus, b.to_bus, dcflow, flow, wflow ?? 0, limit, over, dcslack, dcpen],
      _cls: over > 0.1 ? 'overload' : '',
    };
  });
  makeTable('branch-table', hdr, rows);
}

const RES_PRODUCTS = ['reg_up','reg_down','syn','nsyn','ramp_up_on','ramp_up_off','ramp_down_on','ramp_down_off'];
const RES_SHORT = ['RegUp','RegDn','Syn','NSyn','RmpUpOn','RmpUpOff','RmpDnOn','RmpDnOff'];

function renderReserves() {
  const t = currentPeriod;
  const devs = currentCase.periods.devices;
  const hdr = ['UID', 'Bus', 'Type', ...RES_SHORT];
  const rows = [];
  for (const d of devs) {
    const res = d.reserves || {};
    const vals = RES_PRODUCTS.map(p => (res[p]||[])[t] || 0);
    if (vals.some(v => Math.abs(v) > 0.001)) {
      rows.push({
        _cells: [d.uid, d.bus, d.type, ...vals.map(v => fmt(v,1))],
        _vals: [d.uid, d.bus, d.type, ...vals],
        _cls: '',
      });
    }
  }
  if (rows.length === 0) {
    rows.push({ _cells: ['No reserve awards this period', '', '', ...RES_PRODUCTS.map(() => '')], _vals: ['','','', ...RES_PRODUCTS.map(() => 0)], _cls: '' });
  }
  makeTable('reserves-table', hdr, rows);
}

function renderReserveSystem() {
  const t = currentPeriod;
  const sysRes = (currentCase.reserve_system || [])[t] || [];
  const hdr = ['Product', 'Zone', 'Requirement (MW)', 'Provided (MW)', 'Shortfall (MW)', 'Clearing Price'];
  const rows = sysRes.map(r => {
    return {
      _cells: [r.product_id, r.zone_id, fmt(r.requirement_mw,1), fmt(r.provided_mw,1), r.shortfall_mw > 0.01 ? `<span class="penalty">${fmt(r.shortfall_mw,1)}</span>` : fmt(r.shortfall_mw,1), fmt(r.clearing_price,2)],
      _vals: [r.product_id, r.zone_id, r.requirement_mw, r.provided_mw, r.shortfall_mw, r.clearing_price],
      _cls: r.shortfall_mw > 0.01 ? 'overload' : '',
    };
  });
  if (rows.length === 0) {
    rows.push({ _cells: ['No reserve data', '', '', '', '', ''], _vals: ['',0,0,0,0,0], _cls: '' });
  }
  makeTable('reserve-system-table', hdr, rows);
}

function renderContingencies() {
  const ctgs = currentCase.contingencies || [];
  const hdr = ['UID', 'Outaged Components', 'Component Count'];
  const rows = ctgs.map(c => {
    const comps = (c.components || []).join(', ');
    return {
      _cells: [c.uid, comps, c.components.length],
      _vals: [c.uid, comps, c.components.length],
      _cls: '',
    };
  });
  if (rows.length === 0) {
    rows.push({ _cells: ['No contingencies defined', '', ''], _vals: ['', '', 0], _cls: '' });
  }
  makeTable('ctg-table', hdr, rows);
}

function renderViolations() {
  const vp = currentCase.violation_periods || [];
  const hdr = ['Period', 'Bus P (MW)', 'Bus P $', 'Bus Q (Mvar)', 'Bus Q $', 'Thermal (MVA)', 'Thermal $', 'Reserve $', 'Total $'];
  const rows = vp.map(p => {
    const bp = p.bus_p_balance || {};
    const bq = p.bus_q_balance || {};
    const bt = p.branch_thermal || {};
    const rp = p.reserve || {};
    const total = (bp.penalty_cost||0) + (bq.penalty_cost||0) + (bt.penalty_cost||0) + (rp.total_penalty||0);
    return {
      _cells: [p.period_index, fmt(bp.total_abs_mismatch_mw,2), fmtK(bp.penalty_cost), fmt(bq.total_abs_mismatch_mvar,2), fmtK(bq.penalty_cost), fmt(bt.total_overload_mva,2), fmtK(bt.penalty_cost), fmtK(rp.total_penalty), fmtK(total)],
      _vals: [p.period_index, bp.total_abs_mismatch_mw||0, bp.penalty_cost||0, bq.total_abs_mismatch_mvar||0, bq.penalty_cost||0, bt.total_overload_mva||0, bt.penalty_cost||0, rp.total_penalty||0, total],
      _cls: '',
    };
  });
  const vs = currentCase.violation_summary || {};
  const rs = vs.reserve || {};
  rows.push({
    _cells: ['<b>TOTAL</b>', fmt(vs.bus_p_balance?.total_mismatch_mw,2), fmtK(vs.bus_p_balance?.total_penalty_cost), fmt(vs.bus_q_balance?.total_mismatch_mvar,2), fmtK(vs.bus_q_balance?.total_penalty_cost), fmt(vs.branch_thermal?.total_overload_mva,2), fmtK(vs.branch_thermal?.total_penalty_cost), fmtK(rs.total_penalty), fmtK(vs.total_penalty_cost)],
    _vals: [9999, vs.bus_p_balance?.total_mismatch_mw||0, vs.bus_p_balance?.total_penalty_cost||0, vs.bus_q_balance?.total_mismatch_mvar||0, vs.bus_q_balance?.total_penalty_cost||0, vs.branch_thermal?.total_overload_mva||0, vs.branch_thermal?.total_penalty_cost||0, rs.total_penalty||0, vs.total_penalty_cost||0],
    _cls: 'surge-row',
  });
  makeTable('viol-table', hdr, rows);
}

function renderLog() {
  $('log-content').textContent = currentCase.solve_log || 'Log not found';
}

function closeModal() { $('modal-overlay').classList.remove('show'); }

function showOfferCurve(uid) {
  const t = currentPeriod;
  const devs = currentCase.periods.devices;
  const d = devs.find(x => x.uid === uid);
  if (!d) return;
  const blocks = (d.cost_blocks || [])[t] || [];
  const baseMva = currentCase.case_info?.base_mva || 100;
  const ac_p = Math.abs((d.ac_p||[])[t] || 0);
  const dc_p = Math.abs((d.dc_p||[])[t] || 0);
  const pmin = (d.p_lb||[])[t] ?? 0;
  const pmax = (d.p_ub||[])[t] ?? 0;
  const isLoad = d.type === 'consumer';
  const isFixedLoad = isLoad && (d.dispatchable === false || pmin === pmax);
  const ac_on = (d.ac_on||[])[t];

  const curveLabel = isLoad ? 'Demand Bid' : 'Supply Offer';
  const titleKind = isFixedLoad ? 'Fixed Load' : curveLabel;

  const hdrLine = `<div style="margin-bottom:8px;color:var(--text-muted);font-size:11px">${d.type} at ${d.bus} · Period ${t} · [${fmt(pmin,1)}, ${fmt(pmax,1)}] MW · ${ac_on == null ? '—' : (ac_on ? 'on' : 'off')}</div>`;

  let html = hdrLine;

  if (isFixedLoad) {
    html += `<div class="banner amber">Fixed load — no bid curve (P is pinned, not a market decision).</div>`;
  } else if (blocks.length === 0) {
    html += `<div class="banner amber">No cost-block data available for this device at this period.</div>`;
  } else {
    html += '<table><thead><tr><th style="text-align:left">Block</th><th>Price ($/MWh)</th><th>Qty (MW)</th><th>Cumul (MW)</th><th>Curve</th></tr></thead><tbody>';
    let cumul = 0;
    const totalCap = blocks.reduce((s, b) => s + (b[1]||0) * baseMva, 0) || pmax || 1;
    for (let i = 0; i < blocks.length; i++) {
      // GO C3 stores cost blocks as [price_$/pu, qty_pu]. Convert both sides
      // to per-MWh / per-MW for display.
      const raw_price_per_pu = blocks[i][0] || 0;
      const price_per_mwh = baseMva > 0 ? raw_price_per_pu / baseMva : raw_price_per_pu;
      const qty_mw = (blocks[i][1] || 0) * baseMva;
      cumul += qty_mw;
      const pct = (cumul / totalCap * 100).toFixed(0);
      html += `<tr><td style="text-align:left">${i}</td><td>${Number(price_per_mwh).toLocaleString(undefined,{maximumFractionDigits:2})}</td><td>${fmt(qty_mw,1)}</td><td>${fmt(cumul,1)}</td>`;
      html += `<td><div class="bar-wrap"><div class="bar" style="width:${pct}%"></div></div></td></tr>`;
    }
    html += '</tbody></table>';
  }

  html += `<div style="margin-top:10px;border-top:1px solid var(--border-sub);padding-top:8px;font-size:12px">`;
  html += `<b>DC:</b> ${fmt(dc_p,1)} MW &nbsp; <b>AC:</b> ${fmt(ac_p,1)} MW`;
  const winP = (d.winner_p||[])[t];
  if (winP != null) html += ` &nbsp; <b>Winner:</b> ${fmt(Math.abs(winP),1)} MW`;
  html += `</div>`;

  $('modal-title').textContent = `${uid} — ${titleKind} (Period ${t})`;
  $('modal-body').innerHTML = html;
  $('modal-overlay').classList.add('show');
}

// ─── Grid topology view ──────────────────────────────────────────────────
// State scoped to the currently rendered case. Reset in renderGrid().
let _gridState = {
  mode: localStorage.getItem('grid.mode') || 'voltage',
  showLabels: true,
  showAssets: true,
  // layout ↔ screen transform
  zoom: 1.0, panX: 0, panY: 0,
  // cached pixel coords for the current viewport
  busPx: {},
  svgW: 0, svgH: 0,
};
const GRID_MARGIN = 20;
const BUS_RADIUS = 7;
const ASSET_ORBIT = 13;
const ASSET_SIZE = 4;

function renderGrid() {
  const svg = $('grid-svg');
  if (!svg) return;
  while (svg.firstChild) svg.removeChild(svg.firstChild);

  const layout = currentCase?.grid_layout || {};
  if (Object.keys(layout).length === 0) {
    svg.innerHTML = `<text x="50%" y="50%" fill="var(--text-dim)" font-family="var(--font)" font-size="13" text-anchor="middle" dominant-baseline="central">Grid layout not available for this case.</text>`;
    return;
  }

  _gridState.svgW = svg.clientWidth || 1200;
  _gridState.svgH = svg.clientHeight || 720;

  // Root group that pans / zooms.
  const root = _svg('g', { id: 'grid-root' });
  svg.appendChild(root);

  // Layers (ordered back → front): branches, bus rings, bus circles, asset glyphs, labels.
  const gBranch = _svg('g', { 'class': 'layer-branches' });
  const gRing   = _svg('g', { 'class': 'layer-rings' });
  const gBus    = _svg('g', { 'class': 'layer-buses' });
  const gAsset  = _svg('g', { 'class': 'layer-assets' });
  const gLabel  = _svg('g', { 'class': 'layer-labels' });
  root.append(gBranch, gRing, gBus, gAsset, gLabel);

  // ─── coords ─────────────────────────────────────────────────────────
  const W = _gridState.svgW, H = _gridState.svgH;
  const innerW = W - 2 * GRID_MARGIN;
  const innerH = H - 2 * GRID_MARGIN;
  const px = {};
  for (const [uid, [x, y]] of Object.entries(layout)) {
    px[uid] = [GRID_MARGIN + x * innerW, GRID_MARGIN + y * innerH];
  }
  _gridState.busPx = px;

  // ─── branches ───────────────────────────────────────────────────────
  const branches = currentCase?.periods?.branches || [];
  for (const br of branches) {
    const p0 = px[br.fr_bus], p1 = px[br.to_bus];
    if (!p0 || !p1) continue;
    const kindCls = br.type === 'xfmr' || br.type === 'transformer' ? 'xfmr'
                  : br.type === 'hvdc' || br.type === 'dc_line' ? 'hvdc' : 'ac';
    // Visible branch line.
    const line = _svg('line', {
      'class': `branch ${kindCls}`,
      x1: p0[0], y1: p0[1], x2: p1[0], y2: p1[1],
      'data-uid': br.uid, 'stroke-width': 1.1,
    });
    // Invisible wide hit target stacked underneath for easier clicking.
    const hit = _svg('line', {
      'class': 'branch-hit',
      x1: p0[0], y1: p0[1], x2: p1[0], y2: p1[1],
      'data-uid': br.uid, 'data-kind': kindCls,
    });
    hit.addEventListener('click', () => _gridOpenBranchModal(br.uid));
    hit.addEventListener('mouseenter', (e) => _gridHoverBranch(e, br.uid));
    hit.addEventListener('mouseleave', () => _gridTooltip(null));
    gBranch.appendChild(hit);
    gBranch.appendChild(line);
  }

  // ─── buses ──────────────────────────────────────────────────────────
  const busMeta = (currentCase?.periods?.buses || []).reduce((m, b) => { m[b.uid] = b; return m; }, {});
  for (const [uid, [x, y]] of Object.entries(px)) {
    const g = _svg('g', { 'class': 'bus-group', transform: `translate(${x},${y})`, 'data-uid': uid });
    const circle = _svg('circle', { 'class': 'bus-circle', r: BUS_RADIUS });
    g.appendChild(circle);
    const ringBreach = _svg('circle', { 'class': 'bus-ring-breach', r: BUS_RADIUS + 3, style: 'display:none' });
    const ringNear = _svg('circle', { 'class': 'bus-ring-near', r: BUS_RADIUS + 3, style: 'display:none' });
    g.append(ringBreach, ringNear);
    const lbl = _svg('text', { 'class': 'bus-label', y: BUS_RADIUS + 10 });
    lbl.textContent = uid.replace(/^bus_/, '');
    gLabel.appendChild(_svg('text', {
      'class': 'bus-label', x, y: y + BUS_RADIUS + 10, 'data-uid': uid,
    }, uid.replace(/^bus_/, '')));
    gBus.appendChild(g);
    g.addEventListener('mouseenter', e => _gridHover(e, uid));
    g.addEventListener('mouseleave', () => _gridTooltip(null));
    g.addEventListener('click', () => _gridOpenBusModal(uid));
  }

  // ─── asset glyphs ───────────────────────────────────────────────────
  _gridDrawAssets(gAsset);

  // Initial per-period styling pass.
  updateGridPeriod();

  // Hook up controls + interactions (idempotent: replace the onclick each render).
  _gridWireControls();
  _gridWirePanZoom(svg);
  _gridRefreshLayers();
}

function _svg(tag, attrs = {}, text) {
  const el = document.createElementNS('http://www.w3.org/2000/svg', tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  if (text != null) el.textContent = text;
  return el;
}

// Walk grid_assets, drop small glyphs around each bus based on what's attached.
function _gridDrawAssets(gAsset) {
  const px = _gridState.busPx;
  const assets = currentCase?.grid_assets || {};
  for (const [uid, groups] of Object.entries(assets)) {
    const p = px[uid];
    if (!p) continue;
    const glyphs = [];
    (groups.producers || []).forEach(id => glyphs.push({ kind: 'producer', id }));
    (groups.consumers || []).forEach(id => glyphs.push({ kind: 'consumer', id }));
    (groups.shunts    || []).forEach(id => glyphs.push({ kind: 'shunt',    id }));
    // Skip transformer/hvdc ends — shown as distinctive branches instead of satellites
    if (glyphs.length === 0) continue;
    // Radial layout around the bus. Start at top, go clockwise.
    const n = glyphs.length;
    for (let i = 0; i < n; i++) {
      const angle = -Math.PI / 2 + (2 * Math.PI * i) / Math.max(n, 3);
      const gx = p[0] + ASSET_ORBIT * Math.cos(angle);
      const gy = p[1] + ASSET_ORBIT * Math.sin(angle);
      const g = glyphs[i];
      const node = _gridGlyphFor(g.kind, gx, gy);
      if (!node) continue;
      node.setAttribute('data-uid', g.id);
      node.setAttribute('data-bus', uid);
      node.setAttribute('data-kind', g.kind);
      // Invisible hit disc for easier clicking on the ~5px glyph.
      const hit = _svg('circle', {
        'class': 'asset-hit', cx: gx, cy: gy, r: ASSET_SIZE + 3,
        'data-uid': g.id, 'data-kind': g.kind,
      });
      hit.addEventListener('click', () => _gridOpenAssetModal(g.kind, g.id));
      hit.addEventListener('mouseenter', (e) => _gridHoverAsset(e, g.kind, g.id));
      hit.addEventListener('mouseleave', () => _gridTooltip(null));
      gAsset.appendChild(hit);
      gAsset.appendChild(node);
    }
  }
}

function _gridOpenAssetModal(kind, uid) {
  if (kind === 'producer' || kind === 'consumer') {
    // Reuse the rich offer/bid curve modal already used by the Generators
    // and Loads tabs — covers dispatch, bounds, reserve awards, offer curve.
    showOfferCurve(uid);
    return;
  }
  if (kind === 'shunt') {
    _gridShowShuntModal(uid);
    return;
  }
}

function _gridShowShuntModal(uid) {
  const sh = currentCase?.shunts?.[uid];
  if (!sh) return;
  const t = currentPeriod;
  const fmt = (v, d=4) => v == null ? '—' : Number(v).toFixed(d);
  const init = sh.initial_status || {};
  const initialStep = init.step;
  const range = sh.step_lb != null && sh.step_ub != null ? `[${sh.step_lb}, ${sh.step_ub}]` : '—';
  const fixedStep = sh.step_lb != null && sh.step_ub != null && sh.step_lb === sh.step_ub;

  // Per-period dispatched step
  const dispatchedStep = (sh.step_series || [])[t];
  const actualGs = dispatchedStep != null && sh.gs != null ? sh.gs * dispatchedStep : null;
  const actualBs = dispatchedStep != null && sh.bs != null ? sh.bs * dispatchedStep : null;
  const atMin = dispatchedStep != null && sh.step_lb != null && dispatchedStep === sh.step_lb && !fixedStep;
  const atMax = dispatchedStep != null && sh.step_ub != null && dispatchedStep === sh.step_ub && !fixedStep;
  const bindCls = atMin ? 'warn' : atMax ? 'penalty' : '';
  const bindNote = atMin ? ' (at Lower bound)' : atMax ? ' (at Upper bound)' : '';

  const dispatchRows = (sh.step_series || []).length
    ? `
      <h3 style="margin-top:12px">Dispatched state · Period ${t}</h3>
      <table><tbody>
        <tr><td style="text-align:left;color:var(--text-muted)">Step${fixedStep ? ' (fixed)' : ''}</td>
            <td class="${bindCls}">${dispatchedStep != null ? dispatchedStep : '—'}${bindNote}</td></tr>
        <tr><td style="text-align:left;color:var(--text-muted)">Effective G (MW @ 1 pu)</td><td>${fmt(actualGs)}</td></tr>
        <tr><td style="text-align:left;color:var(--text-muted)">Effective B (Mvar @ 1 pu)</td><td>${fmt(actualBs)}</td></tr>
      </tbody></table>`
    : `<div style="margin-top:8px;font-size:10px;color:var(--text-dim)">Per-period dispatched step not available in this case payload.</div>`;

  $('modal-title').textContent = `Shunt ${uid}${fixedStep ? ' · fixed' : ''}`;
  $('modal-body').innerHTML = `
    <h3>Static parameters</h3>
    <table><tbody>
      <tr><td style="text-align:left;color:var(--text-muted)">Bus</td><td>${sh.bus ?? '—'}</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">G<sub>s</sub> (MW @ 1 pu · per step)</td><td>${fmt(sh.gs)}</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">B<sub>s</sub> (Mvar @ 1 pu · per step)</td><td>${fmt(sh.bs)}</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">Step range</td><td>${range}${fixedStep ? ' (no decision)' : ''}</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">Initial step</td><td>${initialStep != null ? initialStep : '—'}</td></tr>
    </tbody></table>
    ${dispatchRows}`;
  $('modal-overlay').classList.add('show');
}

function _gridOpenBranchModal(uid) {
  const br = (currentCase?.periods?.branches || []).find(b => b.uid === uid);
  if (!br) return;
  const t = currentPeriod;
  const flow = (br.flow_mva || [])[t];
  const dcFlow = (br.dc_flow_mva || [])[t];
  const winFlow = (br.winner_flow_mva || [])[t];
  const limit = br.limit_mva;
  const loading = (flow != null && limit) ? (Math.abs(flow) / limit) : null;
  const loadPct = loading != null ? (loading * 100).toFixed(1) + ' %' : '—';
  const over = loading != null && loading >= 1.0;
  const nearMax = loading != null && loading >= 0.8 && loading < 1.0;
  const stateCls = over ? 'penalty' : nearMax ? 'warn' : 'ok';
  const stateTxt = over ? 'AT/ABOVE LIMIT' : nearMax ? 'NEAR LIMIT' : 'WITHIN LIMIT';
  const dcSlack = (br.dc_slack_mw || [])[t] ?? 0;
  const dcPen = (br.dc_penalty || [])[t] ?? 0;
  const fmt = (v, d=2) => v == null ? '—' : Number(v).toFixed(d);

  // Transformer-specific enrichment: tap ratio + angle shift + phase-shifter flag.
  const xf = currentCase?.xfmrs?.[uid];
  let title = `${(br.type || 'branch').toUpperCase()} ${uid} · Period ${t}`;
  let xfmrSection = '';
  if (xf) {
    const tm = (xf.tm_series || [])[t];
    const ta = (xf.ta_series || [])[t];
    const onStatus = (xf.on_status_series || [])[t];
    const tmLb = xf.tm_bounds?.[0], tmUb = xf.tm_bounds?.[1];
    const taLb = xf.ta_bounds?.[0], taUb = xf.ta_bounds?.[1];
    const fixedTm = tmLb != null && tmUb != null && tmLb === tmUb;
    const fixedTa = taLb != null && taUb != null && taLb === taUb;
    if (xf.is_phase_shifter) title = `PHASE SHIFTER ${uid} · Period ${t}`;
    else if (xf.has_tap_ratio) title = `TAP XFMR ${uid} · Period ${t}`;
    else title = `XFMR ${uid} · Period ${t}`;
    const tmBindCls = (!fixedTm && tm != null && tmLb != null && tmUb != null)
      ? (Math.abs(tm - tmLb) < 1e-6 ? 'warn' : Math.abs(tm - tmUb) < 1e-6 ? 'warn' : '')
      : '';
    const taBindCls = (!fixedTa && ta != null && taLb != null && taUb != null)
      ? (Math.abs(ta - taLb) < 1e-6 ? 'warn' : Math.abs(ta - taUb) < 1e-6 ? 'warn' : '')
      : '';
    const taDeg = ta != null ? (ta * 180 / Math.PI) : null;
    xfmrSection = `
      <h3 style="margin-top:12px">Dispatched tap · Period ${t}</h3>
      <table><tbody>
        <tr><td style="text-align:left;color:var(--text-muted)">Status</td>
            <td class="${onStatus === 0 ? 'penalty' : 'ok'}">${onStatus == null ? '—' : (onStatus ? 'connected' : 'out of service')}</td></tr>
        <tr><td style="text-align:left;color:var(--text-muted)">Tap ratio t<sub>m</sub>${fixedTm ? ' (fixed)' : ''}</td>
            <td class="${tmBindCls}">${fmt(tm, 4)}${(tmLb != null && tmUb != null) ? `  ·  [${fmt(tmLb, 4)}, ${fmt(tmUb, 4)}]` : ''}</td></tr>
        <tr><td style="text-align:left;color:var(--text-muted)">Phase shift t<sub>a</sub>${fixedTa ? ' (fixed)' : ''}</td>
            <td class="${taBindCls}">${fmt(ta, 4)} rad${taDeg != null ? `  (${taDeg.toFixed(2)}°)` : ''}${(taLb != null && taUb != null) ? `  ·  [${fmt(taLb, 4)}, ${fmt(taUb, 4)}]` : ''}</td></tr>
      </tbody></table>`;
  }

  $('modal-title').textContent = title;
  $('modal-body').innerHTML = `
    <h3>Topology</h3>
    <table><tbody>
      <tr><td style="text-align:left;color:var(--text-muted)">Type</td><td>${br.type || '—'}</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">From bus</td><td>${br.fr_bus}</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">To bus</td><td>${br.to_bus}</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">Thermal limit</td><td>${fmt(limit, 1)} MVA</td></tr>
    </tbody></table>
    <h3 style="margin-top:12px">Flow · Period ${t}</h3>
    <table><tbody>
      <tr><td style="text-align:left;color:var(--text-muted)">DC flow</td><td>${fmt(dcFlow, 2)} MVA</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">AC flow</td><td>${fmt(flow, 2)} MVA</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">Winner flow</td><td>${fmt(winFlow, 2)} MVA</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">Loading</td><td class="${stateCls}">${loadPct}</td></tr>
      <tr><td style="text-align:left;color:var(--text-muted)">State</td><td class="${stateCls}">${stateTxt}</td></tr>
      ${dcSlack > 0 ? `<tr><td style="text-align:left;color:var(--text-muted)">DC slack</td><td class="warn">${fmt(dcSlack, 2)} MW</td></tr>` : ''}
      ${dcPen > 0 ? `<tr><td style="text-align:left;color:var(--text-muted)">DC penalty</td><td class="warn">${'$' + fmt(dcPen, 2)}</td></tr>` : ''}
    </tbody></table>
    ${xfmrSection}`;
  $('modal-overlay').classList.add('show');
}

function _gridHoverBranch(e, uid) {
  const tt = $('grid-tooltip');
  const wrap = document.querySelector('.grid-canvas-wrap');
  const rect = wrap.getBoundingClientRect();
  const br = (currentCase?.periods?.branches || []).find(b => b.uid === uid);
  if (!br) return;
  const t = currentPeriod;
  const flow = (br.flow_mva || [])[t];
  const limit = br.limit_mva;
  const loading = (flow != null && limit) ? (Math.abs(flow) / limit) * 100 : null;
  const fmt = (v, d=2) => v == null ? '—' : Number(v).toFixed(d);
  tt.innerHTML = `
    <div class="uid">${uid}</div>
    <div class="row"><span class="lbl">type</span><span>${br.type || '—'}</span></div>
    <div class="row"><span class="lbl">from → to</span><span>${br.fr_bus} → ${br.to_bus}</span></div>
    <div class="row"><span class="lbl">flow</span><span>${fmt(flow, 1)} MVA</span></div>
    <div class="row"><span class="lbl">limit</span><span>${fmt(limit, 1)} MVA</span></div>
    <div class="row"><span class="lbl">loading</span><span>${loading != null ? loading.toFixed(1) + ' %' : '—'}</span></div>
  `;
  tt.classList.remove('hidden');
  const left = Math.min(rect.width - 220, e.clientX - rect.left + 12);
  const top = Math.min(rect.height - 120, e.clientY - rect.top + 12);
  tt.style.left = left + 'px';
  tt.style.top = top + 'px';
}

function _gridHoverAsset(e, kind, uid) {
  const tt = $('grid-tooltip');
  const wrap = document.querySelector('.grid-canvas-wrap');
  const rect = wrap.getBoundingClientRect();
  const t = currentPeriod;
  const fmt = (v, d=2) => v == null ? '—' : Number(v).toFixed(d);

  if (kind === 'shunt') {
    const sh = currentCase?.shunts?.[uid] || {};
    tt.innerHTML = `
      <div class="uid">${uid}</div>
      <div class="row"><span class="lbl">kind</span><span>shunt</span></div>
      <div class="row"><span class="lbl">Gs</span><span>${fmt(sh.gs, 4)}</span></div>
      <div class="row"><span class="lbl">Bs</span><span>${fmt(sh.bs, 4)}</span></div>
    `;
  } else {
    const d = (currentCase?.periods?.devices || []).find(x => x.uid === uid);
    if (!d) return;
    const p = (d.ac_p || [])[t];
    const pmin = (d.p_lb || [])[t];
    const pmax = (d.p_ub || [])[t];
    const on = (d.ac_on || [])[t];
    tt.innerHTML = `
      <div class="uid">${uid}</div>
      <div class="row"><span class="lbl">kind</span><span>${kind}</span></div>
      <div class="row"><span class="lbl">on</span><span>${on == null ? '—' : (on ? 'on' : 'off')}</span></div>
      <div class="row"><span class="lbl">P (MW)</span><span>${fmt(p, 1)}</span></div>
      <div class="row"><span class="lbl">[Pmin, Pmax]</span><span>[${fmt(pmin, 1)}, ${fmt(pmax, 1)}]</span></div>
    `;
  }
  tt.classList.remove('hidden');
  const left = Math.min(rect.width - 220, e.clientX - rect.left + 12);
  const top = Math.min(rect.height - 120, e.clientY - rect.top + 12);
  tt.style.left = left + 'px';
  tt.style.top = top + 'px';
}

function _gridGlyphFor(kind, x, y) {
  if (kind === 'producer') {
    const s = ASSET_SIZE + 0.5;
    return _svg('polygon', { 'class': 'asset producer', points: `${x},${y - s} ${x - s},${y + s} ${x + s},${y + s}` });
  }
  if (kind === 'consumer') {
    const s = ASSET_SIZE;
    return _svg('polygon', { 'class': 'asset consumer', points: `${x - s},${y - s} ${x + s},${y - s} ${x},${y + s}` });
  }
  if (kind === 'shunt') {
    const s = ASSET_SIZE - 0.5;
    return _svg('polygon', { 'class': 'asset shunt', points: `${x - s},${y} ${x},${y - s} ${x + s},${y} ${x},${y + s}` });
  }
  return null;
}

// ─── color modes + legend ───────────────────────────────────────────────
function _availableColorModes() {
  const t = currentPeriod;
  const buses = currentCase?.periods?.buses || [];
  const has = { voltage: false, dc_lmp: false, ac_lmp: false, net_p: false };
  for (const b of buses) {
    if ((b.ac_vm || [])[t] != null || (b.winner_vm || [])[t] != null) has.voltage = true;
    if ((b.dc_lmp || [])[t] != null) has.dc_lmp = true;
    if ((b.ac_lmp || [])[t] != null) has.ac_lmp = true;
    if ((b.ac_p_inj || [])[t] != null || (b.dc_p_inj || [])[t] != null) has.net_p = true;
  }
  return has;
}

function _pickEffectiveMode() {
  const avail = _availableColorModes();
  let mode = _gridState.mode;
  if (!avail[mode]) {
    // Fall back priority: voltage → ac_lmp → dc_lmp → net_p
    for (const m of ['voltage', 'ac_lmp', 'dc_lmp', 'net_p']) {
      if (avail[m]) { mode = m; break; }
    }
  }
  return { mode, avail };
}

function _busValueForMode(busRec, mode, t) {
  if (!busRec) return null;
  if (mode === 'voltage') return (busRec.ac_vm || [])[t] ?? (busRec.winner_vm || [])[t] ?? null;
  if (mode === 'dc_lmp')  return (busRec.dc_lmp || [])[t] ?? null;
  if (mode === 'ac_lmp')  return (busRec.ac_lmp || [])[t] ?? null;
  if (mode === 'net_p')   return (busRec.ac_p_inj || [])[t] ?? (busRec.dc_p_inj || [])[t] ?? null;
  return null;
}

// Returns a CSS color string for `value` given (mode, low, high).
function _colorFor(value, mode, lo, hi) {
  if (value == null) return '#30303a';
  if (mode === 'voltage') {
    // Diverging around 1.0 pu. Rose at ≤0.95, amber near bounds, light purple mid.
    if (value < 0.95) return '#f472b6';
    if (value < 0.97) return '#fbbf24';
    if (value > 1.05) return '#fbbf24';
    if (value > 1.03) return '#c084fc';
    return '#a78bfa';
  }
  // Sequential purple → rose ramp for LMP / net P / generic continuous
  const range = (hi - lo) || 1;
  const t = Math.max(0, Math.min(1, (value - lo) / range));
  // Mix two stops: #a78bfa (low) → #f472b6 (high) through #c084fc
  const c0 = [167, 139, 250], c1 = [192, 132, 252], c2 = [244, 114, 182];
  let r, g, b;
  if (t < 0.5) {
    const u = t * 2;
    r = c0[0] + (c1[0] - c0[0]) * u;
    g = c0[1] + (c1[1] - c0[1]) * u;
    b = c0[2] + (c1[2] - c0[2]) * u;
  } else {
    const u = (t - 0.5) * 2;
    r = c1[0] + (c2[0] - c1[0]) * u;
    g = c1[1] + (c2[1] - c1[1]) * u;
    b = c1[2] + (c2[2] - c1[2]) * u;
  }
  return `rgb(${Math.round(r)},${Math.round(g)},${Math.round(b)})`;
}

// Per-period: recolor bus fills, restyle branches, refresh legend + asset state.
function updateGridPeriod() {
  const svg = $('grid-svg');
  if (!svg || !svg.firstChild) return;
  const t = currentPeriod;
  const { mode, avail } = _pickEffectiveMode();
  // Sync the mode buttons visually (for when fallback kicks in).
  document.querySelectorAll('#color-mode-group .mode-btn').forEach(btn => {
    const m = btn.dataset.mode;
    btn.classList.toggle('active', m === mode);
    btn.disabled = !avail[m];
  });

  // Collect per-bus values for min/max legend.
  const busRecs = (currentCase?.periods?.buses || []).reduce((o, b) => { o[b.uid] = b; return o; }, {});
  const vals = [];
  for (const uid of Object.keys(_gridState.busPx)) {
    const v = _busValueForMode(busRecs[uid], mode, t);
    if (v != null) vals.push(v);
  }
  const lo = vals.length ? Math.min(...vals) : 0;
  const hi = vals.length ? Math.max(...vals) : 1;

  // Recolor buses + update rings for voltage bounds.
  const busGroups = svg.querySelectorAll('.bus-group');
  busGroups.forEach(g => {
    const uid = g.getAttribute('data-uid');
    const rec = busRecs[uid];
    const v = _busValueForMode(rec, mode, t);
    const circle = g.querySelector('.bus-circle');
    circle.setAttribute('fill', _colorFor(v, mode, lo, hi));
    // Voltage-bound rings (always checked from vm, not the active color mode)
    const ringBreach = g.querySelector('.bus-ring-breach');
    const ringNear = g.querySelector('.bus-ring-near');
    const vm = (rec?.ac_vm || [])[t];
    const vmLb = rec?.vm_lb ?? null;
    const vmUb = rec?.vm_ub ?? null;
    let state = null;
    if (vm != null && vmLb != null && vmUb != null) {
      if (vm <= vmLb || vm >= vmUb) state = 'breach';
      else if (vm <= vmLb + 0.01 || vm >= vmUb - 0.01) state = 'near';
    }
    ringBreach.style.display = state === 'breach' ? '' : 'none';
    ringNear.style.display   = state === 'near'   ? '' : 'none';
  });

  // Branch: thickness + class based on per-period flow/limit.
  const branchRecs = (currentCase?.periods?.branches || []);
  const branchByUid = branchRecs.reduce((o, b) => { o[b.uid] = b; return o; }, {});
  svg.querySelectorAll('.branch').forEach(el => {
    const uid = el.getAttribute('data-uid');
    const b = branchByUid[uid] || {};
    const flow = Math.abs((b.flow_mva || [])[t] ?? (b.dc_flow_mva || [])[t] ?? 0);
    const limit = b.limit_mva || null;
    const load = (limit && limit > 0) ? flow / limit : 0;
    // Thickness 0.6–3.5 px proportional to load (or flow fraction of max flow if no limit)
    let thickness = 1.0 + Math.min(load, 1) * 2.0;
    if (load > 1) thickness = 3.5;
    el.setAttribute('stroke-width', thickness);
    el.classList.remove('loaded', 'breach');
    if (load >= 1.0) el.classList.add('breach');
    else if (load >= 0.8) el.classList.add('loaded');
  });

  // Asset glyph on/off + at-bound state.
  _gridUpdateAssets(t);

  _gridUpdateLegend(mode, lo, hi);
}

function _gridUpdateAssets(t) {
  const svg = $('grid-svg');
  const devs = (currentCase?.periods?.devices || []).reduce((o, d) => { o[d.uid] = d; return o; }, {});
  svg.querySelectorAll('.asset').forEach(el => {
    const uid = el.getAttribute('data-uid');
    const d = devs[uid];
    if (!d) return;
    const on = (d.ac_on || [])[t] ?? 1;
    const p = Math.abs((d.ac_p || [])[t] ?? 0);
    const pmin = (d.p_lb || [])[t] ?? 0;
    const pmax = (d.p_ub || [])[t] ?? 0;
    el.classList.remove('off', 'at-max', 'at-min', 'curtail');
    if (!on) el.classList.add('off');
    else if (pmax > 0 && Math.abs(p - pmax) < 0.05 * pmax) el.classList.add('at-max');
    else if (pmin > 0 && Math.abs(p - pmin) < 0.05 * Math.max(pmin, 1)) el.classList.add('at-min');
    if (d.type === 'consumer' && pmax > 0 && p < pmax - 0.05 * pmax) el.classList.add('curtail');
  });
}

function _gridUpdateLegend(mode, lo, hi) {
  const el = $('grid-legend');
  if (!el) return;
  const labelMap = { voltage: 'V (pu)', dc_lmp: 'DC LMP ($/MWh)', ac_lmp: 'AC LMP ($/MWh)', net_p: 'P (MW)' };
  const fmtVal = mode === 'voltage'
    ? v => (v == null ? '—' : v.toFixed(3))
    : v => (v == null ? '—' : Number(v).toLocaleString(undefined, { maximumFractionDigits: 0 }));
  el.innerHTML = `
    <span>${labelMap[mode] || mode}</span>
    <span class="legend-value lo">${fmtVal(lo)}</span>
    <span class="legend-bar" style="background:${_legendGradient(mode, lo, hi)}"></span>
    <span class="legend-value hi">${fmtVal(hi)}</span>
  `;
}

function _legendGradient(mode, lo, hi) {
  if (mode === 'voltage') {
    return 'linear-gradient(90deg, #f472b6 0%, #fbbf24 16%, #a78bfa 50%, #c084fc 83%, #fbbf24 100%)';
  }
  return 'linear-gradient(90deg, #a78bfa 0%, #c084fc 50%, #f472b6 100%)';
}

// ─── interactions ───────────────────────────────────────────────────────
function _gridWireControls() {
  document.querySelectorAll('#color-mode-group .mode-btn').forEach(btn => {
    btn.onclick = () => {
      _gridState.mode = btn.dataset.mode;
      localStorage.setItem('grid.mode', _gridState.mode);
      updateGridPeriod();
    };
  });
  const lblCb = $('grid-show-labels');
  const assetCb = $('grid-show-assets');
  lblCb.onchange = () => { _gridState.showLabels = lblCb.checked; _gridRefreshLayers(); };
  assetCb.onchange = () => { _gridState.showAssets = assetCb.checked; _gridRefreshLayers(); };
}

function _gridRefreshLayers() {
  const svg = $('grid-svg');
  if (!svg) return;
  svg.querySelector('.layer-labels')?.setAttribute('style', _gridState.showLabels ? '' : 'display:none');
  svg.querySelector('.layer-assets')?.setAttribute('style', _gridState.showAssets ? '' : 'display:none');
}

function _gridWirePanZoom(svg) {
  const root = svg.querySelector('#grid-root');
  const apply = () => {
    root.setAttribute('transform', `translate(${_gridState.panX},${_gridState.panY}) scale(${_gridState.zoom})`);
  };
  _gridState.panX = 0; _gridState.panY = 0; _gridState.zoom = 1;
  apply();

  let dragging = false, sx = 0, sy = 0, startX = 0, startY = 0;
  svg.onmousedown = e => {
    if (e.button !== 0) return;
    dragging = true; sx = e.clientX; sy = e.clientY;
    startX = _gridState.panX; startY = _gridState.panY;
    svg.classList.add('panning');
  };
  window.addEventListener('mouseup', () => { dragging = false; svg.classList.remove('panning'); });
  window.addEventListener('mousemove', e => {
    if (!dragging) return;
    _gridState.panX = startX + (e.clientX - sx);
    _gridState.panY = startY + (e.clientY - sy);
    apply();
  });
  svg.onwheel = e => {
    e.preventDefault();
    const delta = -e.deltaY * 0.0015;
    const newZoom = Math.max(0.4, Math.min(4, _gridState.zoom * (1 + delta)));
    // Zoom toward cursor
    const rect = svg.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    const k = newZoom / _gridState.zoom;
    _gridState.panX = cx - (cx - _gridState.panX) * k;
    _gridState.panY = cy - (cy - _gridState.panY) * k;
    _gridState.zoom = newZoom;
    apply();
  };
  svg.ondblclick = () => {
    _gridState.panX = 0; _gridState.panY = 0; _gridState.zoom = 1; apply();
  };
}

function _gridHover(e, uid) {
  const tt = $('grid-tooltip');
  const wrap = document.querySelector('.grid-canvas-wrap');
  const rect = wrap.getBoundingClientRect();
  const bus = (currentCase?.periods?.buses || []).find(b => b.uid === uid);
  if (!bus) return;
  const t = currentPeriod;
  const vm = (bus.ac_vm || [])[t];
  const va = (bus.ac_va || [])[t];
  const dcLmp = (bus.dc_lmp || [])[t];
  const acLmp = (bus.ac_lmp || [])[t];
  const assets = currentCase?.grid_assets?.[uid] || {};
  const nProd = (assets.producers || []).length;
  const nCons = (assets.consumers || []).length;
  const nSh = (assets.shunts || []).length;
  const nXf = (assets.transformer_ends || []).length;
  const nDc = (assets.hvdc_ends || []).length;
  tt.innerHTML = `
    <div class="uid">${uid}</div>
    <div class="row"><span class="lbl">V (pu)</span><span>${vm != null ? vm.toFixed(4) : '—'}</span></div>
    <div class="row"><span class="lbl">θ (rad)</span><span>${va != null ? va.toFixed(4) : '—'}</span></div>
    <div class="row"><span class="lbl">DC LMP</span><span>${dcLmp != null ? '$' + dcLmp.toFixed(2) : '—'}</span></div>
    <div class="row"><span class="lbl">AC LMP</span><span>${acLmp != null ? '$' + acLmp.toFixed(2) : '—'}</span></div>
    <div class="row"><span class="lbl">Gen / Load</span><span>${nProd} / ${nCons}</span></div>
    ${(nSh + nXf + nDc) ? `<div class="row"><span class="lbl">Sh / Xf / DC</span><span>${nSh} / ${nXf} / ${nDc}</span></div>` : ''}
  `;
  tt.classList.remove('hidden');
  const left = Math.min(rect.width - 220, e.clientX - rect.left + 12);
  const top = Math.min(rect.height - 120, e.clientY - rect.top + 12);
  tt.style.left = left + 'px';
  tt.style.top = top + 'px';
}

function _gridTooltip(content) {
  const tt = $('grid-tooltip');
  if (!content) tt.classList.add('hidden');
}

function _gridOpenBusModal(uid) {
  const bus = (currentCase?.periods?.buses || []).find(b => b.uid === uid);
  if (!bus) return;
  const t = currentPeriod;
  const assets = currentCase?.grid_assets?.[uid] || {};
  const devs = currentCase?.periods?.devices || [];
  const devMap = devs.reduce((o, d) => { o[d.uid] = d; return o; }, {});

  const fmt = (v, d=2) => v == null ? '—' : Number(v).toFixed(d);

  const busRows = [
    ['Voltage (pu)',  fmt((bus.ac_vm || [])[t], 4)],
    ['  bounds',      `[${fmt(bus.vm_lb, 3)}, ${fmt(bus.vm_ub, 3)}]`],
    ['Angle (rad)',   fmt((bus.ac_va || [])[t], 4)],
    ['AC P inj',      fmt((bus.ac_p_inj || [])[t], 2) + ' MW'],
    ['AC Q inj',      fmt((bus.ac_q_inj || [])[t], 2) + ' Mvar'],
    ['DC P inj',      fmt((bus.dc_p_inj || [])[t], 2) + ' MW'],
  ];
  const busTable = `<table><tbody>${busRows.map(r => `<tr><td style="text-align:left;color:var(--text-muted)">${r[0]}</td><td>${r[1]}</td></tr>`).join('')}</tbody></table>`;

  // LMP breakdown — MEC (energy) + MCC (congestion) + MLC (losses) = LMP.
  // Shown DC vs AC side-by-side so the decomposition is comparable across
  // the two solver stages.
  const fmtPrice = v => v == null ? '—' : '$' + Number(v).toFixed(2);
  const fmtSignedPrice = v => {
    if (v == null) return '—';
    const sign = v >= 0 ? '+' : '−';
    return sign + '$' + Math.abs(v).toFixed(2);
  };
  const lmpRows = [
    ['MEC · energy',     'dc_mec',  'ac_mec',  fmtPrice],
    ['MCC · congestion', 'dc_mcc',  'ac_mcc',  fmtSignedPrice],
    ['MLC · losses',     'dc_mlc',  'ac_mlc',  fmtSignedPrice],
    ['LMP · total',      'dc_lmp',  'ac_lmp',  fmtPrice],
  ];
  const lmpTableRows = lmpRows.map(([label, dcKey, acKey, formatter], idx) => {
    const dcv = (bus[dcKey] || [])[t];
    const acv = (bus[acKey] || [])[t];
    const isTotal = label.startsWith('LMP');
    const rowCls = isTotal ? ' class="surge-row"' : '';
    return `<tr${rowCls}>
      <td style="text-align:left;color:var(--text-muted)">${label}</td>
      <td>${formatter(dcv)}</td>
      <td>${formatter(acv)}</td>
    </tr>`;
  }).join('');
  const lmpTable = `<h3 style="margin-top:12px">LMP breakdown · $/MWh</h3>
    <table>
      <thead><tr>
        <th style="text-align:left;color:var(--text-muted)">Component</th>
        <th>DC SCUC</th>
        <th>AC SCED</th>
      </tr></thead>
      <tbody>${lmpTableRows}</tbody>
    </table>`;

  let devHtml = '';
  const devices = [...(assets.producers || []), ...(assets.consumers || [])];
  if (devices.length) {
    const rows = devices.map(id => {
      const d = devMap[id];
      if (!d) return '';
      const p = (d.ac_p || [])[t];
      const pmin = (d.p_lb || [])[t];
      const pmax = (d.p_ub || [])[t];
      const on = (d.ac_on || [])[t];
      const bandHint = (pmin != null && pmax != null) ? `[${fmt(pmin, 1)}, ${fmt(pmax, 1)}]` : '—';
      return `<tr>
        <td style="text-align:left">${id}</td>
        <td style="text-align:left">${d.type}</td>
        <td>${on == null ? '—' : (on ? 'on' : 'off')}</td>
        <td>${fmt(p, 1)}</td>
        <td style="color:var(--text-muted)">${bandHint}</td>
      </tr>`;
    }).join('');
    devHtml = `
      <h3 style="margin-top:12px">Devices (${devices.length})</h3>
      <table>
        <thead><tr><th style="text-align:left">UID</th><th style="text-align:left">Type</th><th>On</th><th>P (MW)</th><th style="text-align:left">[Pmin, Pmax]</th></tr></thead>
        <tbody>${rows}</tbody>
      </table>`;
  }

  $('modal-title').textContent = `Bus ${uid} · Period ${t}`;
  $('modal-body').innerHTML = `<h3>State</h3>${busTable}${lmpTable}${devHtml}`;
  $('modal-overlay').classList.add('show');
}

// ─── Re-solve / Re-validate jobs ────────────────────────────────────────
let currentJob = null;
let currentJobEventSource = null;

function openResolveModal() {
  if (!currentCaseKey) return;
  const pol = currentCase?.policy || {};
  const modalBody = $('modal-body');
  $('modal-title').textContent = `Re-solve · ${currentCaseKey}`;
  modalBody.innerHTML = `
    <div style="font-size:11px;color:var(--text-muted);margin-bottom:10px">
      Override AdapterPolicy fields. Blank = default. Writes archive of the prior run before starting.
    </div>
    <form class="policy-form" id="resolve-form">
      <label>LP Solver
        <select name="lp_solver">
          <option value="">(default)</option>
          <option value="gurobi" ${pol.lp_solver==='gurobi'?'selected':''}>gurobi</option>
          <option value="highs" ${pol.lp_solver==='highs'?'selected':''}>highs</option>
        </select>
      </label>
      <label>Commitment Mode
        <select name="commitment_mode">
          <option value="">(default)</option>
          <option value="optimize" ${pol.commitment_mode==='optimize'?'selected':''}>optimize</option>
          <option value="fixed_initial" ${pol.commitment_mode==='fixed_initial'?'selected':''}>fixed_initial</option>
        </select>
      </label>
      <label>AC Reconcile Mode
        <select name="ac_reconcile_mode">
          <option value="">(default)</option>
          <option value="ac_dispatch" ${pol.ac_reconcile_mode==='ac_dispatch'?'selected':''}>ac_dispatch</option>
          <option value="none" ${pol.ac_reconcile_mode==='none'?'selected':''}>none</option>
        </select>
      </label>
      <label>AC NLP Solver
        <select name="nlp_solver">
          <option value="">(default)</option>
          <option value="ipopt" ${(pol.nlp_solver||pol.ac_nlp_solver)==='ipopt'?'selected':''}>ipopt</option>
          <option value="copt" ${(pol.nlp_solver||pol.ac_nlp_solver)==='copt'?'selected':''}>copt</option>
        </select>
      </label>
      <label>MIP Rel. Gap
        <input type="number" step="0.0001" min="0" max="1" name="commitment_mip_rel_gap" value="${pol.commitment_mip_rel_gap ?? ''}" placeholder="0.0001">
      </label>
      <label>MIP Time Limit (s)
        <input type="number" step="1" min="1" name="commitment_time_limit_secs" value="${pol.commitment_time_limit_secs ?? ''}" placeholder="600">
      </label>
      <label class="checkbox span2">
        <input type="checkbox" name="capture_solver_log" ${pol.capture_solver_log?'checked':''}>
        Capture full solver log (Rust tracing + Gurobi/Ipopt output)
      </label>
    </form>
    <div class="modal-actions">
      <button class="action-btn ghost" onclick="closeModal()">Cancel</button>
      <button class="action-btn" id="resolve-submit">Start Solve</button>
    </div>
  `;
  $('modal-overlay').classList.add('show');
  $('resolve-submit').onclick = () => {
    const form = $('resolve-form');
    const fd = new FormData(form);
    const overrides = {};
    for (const [k, v] of fd.entries()) {
      if (v === '' || v === null) continue;
      if (k === 'commitment_mip_rel_gap' || k === 'commitment_time_limit_secs') overrides[k] = Number(v);
      else if (k === 'capture_solver_log') overrides[k] = true;
      else overrides[k] = v;
    }
    closeModal();
    submitJob('solve', overrides);
  };
}

function submitJob(kind, policy) {
  const key = currentCaseKey;
  const url = `api/cases/${encodeURI(key)}/${kind === 'solve' ? 'solve' : 'validate'}`;
  const body = kind === 'solve' ? JSON.stringify({ policy }) : JSON.stringify({});
  openDrawer(kind.toUpperCase(), key, 'queued');
  $('job-log-stream').textContent = '';
  fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body })
    .then(r => { if (!r.ok) throw new Error(r.status); return r.json(); })
    .then(job => attachJob(job))
    .catch(err => {
      setJobStatus('failed');
      appendJobLog(`[error] submit failed: ${err.message}`);
    });
}

function attachJob(job) {
  currentJob = job;
  setJobStatus(job.status);
  if (currentJobEventSource) currentJobEventSource.close();
  const es = new EventSource(`api/jobs/${job.id}/stream`);
  currentJobEventSource = es;
  es.addEventListener('log', e => appendJobLog(e.data));
  es.addEventListener('status', e => {
    setJobStatus(e.data);
    if (e.data === 'succeeded' || e.data === 'failed') {
      es.close();
      currentJobEventSource = null;
      if (e.data === 'succeeded' && currentCaseKey === job.case_key) {
        delete caseCache[job.case_key];
        setTimeout(() => selectCase(job.case_key), 500);
      }
    }
  });
  es.onerror = () => {
    // Connection dropped (stream ended or network). Poll once to get final state.
    fetch(`api/jobs/${job.id}`).then(r => r.json()).then(j => setJobStatus(j.status)).catch(() => {});
  };
}

function openDrawer(kind, caseKey, status) {
  $('job-kind-label').textContent = kind;
  $('job-case-label').textContent = caseKey;
  setJobStatus(status);
  $('job-drawer').classList.remove('hidden');
}

function closeDrawer() {
  if (currentJobEventSource) { currentJobEventSource.close(); currentJobEventSource = null; }
  $('job-drawer').classList.add('hidden');
}

function setJobStatus(status) {
  const el = $('job-status-label');
  el.textContent = status;
  el.className = 'job-status ' + status;
}

function appendJobLog(line) {
  const el = $('job-log-stream');
  const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 10;
  el.textContent += line + '\n';
  if (atBottom) el.scrollTop = el.scrollHeight;
}

function wireActions() {
  $('btn-resolve').onclick = openResolveModal;
  $('btn-revalidate').onclick = () => {
    if (!currentCaseKey) return;
    submitJob('validate', null);
  };
  $('btn-archives').onclick = openArchivesModal;
  $('btn-job-close').onclick = closeDrawer;
}

function openArchivesModal() {
  if (!currentCaseKey) return;
  $('modal-title').textContent = `Archives · ${currentCaseKey}`;
  $('modal-body').innerHTML = '<div style="color:var(--text-muted);font-size:11px">Loading…</div>';
  $('modal-overlay').classList.add('show');
  fetch(`api/cases/${encodeURI(currentCaseKey)}/archives`)
    .then(r => r.json())
    .then(data => renderArchivesModal(data.archives || []))
    .catch(err => {
      $('modal-body').innerHTML = `<div class="banner rose">Failed to load: ${err.message}</div>`;
    });
}

function renderArchivesModal(archives) {
  if (archives.length === 0) {
    $('modal-body').innerHTML = `
      <div style="color:var(--text-muted);font-size:11.5px">No archived runs yet. Every re-solve moves the prior run into <code>scenario_NNN/archive/{iso_ts}/</code>; the newest 10 are kept.</div>
    `;
    return;
  }
  const sorted = archives.slice().sort((a, b) => b.timestamp.localeCompare(a.timestamp));
  const itemsHtml = sorted.map(a => {
    const status = a.status || 'none';
    return `<div class="archive-item" data-ts="${a.timestamp}">
      <span class="ts">${a.timestamp}</span>
      <span class="status ${status}">${status}</span>
    </div>`;
  }).join('');
  $('modal-body').innerHTML = `
    <div style="color:var(--text-muted);font-size:11px;margin-bottom:4px">
      Click an archive to inspect its run-report + solve.log. Newest first.
    </div>
    <div class="archive-list">${itemsHtml}</div>
    <div id="archive-detail" style="display:none">
      <h3 style="margin-top:12px">Run Report</h3>
      <div id="archive-report" class="archive-report"></div>
      <h3>Solve Log (tail)</h3>
      <div id="archive-log" class="archive-report" style="max-height:260px"></div>
    </div>
  `;
  document.querySelectorAll('.archive-item').forEach(el => {
    el.onclick = () => selectArchive(el.dataset.ts);
  });
}

function selectArchive(ts) {
  document.querySelectorAll('.archive-item').forEach(el => {
    el.classList.toggle('active', el.dataset.ts === ts);
  });
  $('archive-detail').style.display = '';
  $('archive-report').textContent = 'Loading…';
  $('archive-log').textContent = 'Loading…';
  const key = currentCaseKey;
  fetch(`api/cases/${encodeURI(key)}/archives/${encodeURIComponent(ts)}`)
    .then(r => r.json())
    .then(data => {
      $('archive-report').textContent = JSON.stringify(data.run_report, null, 2);
    })
    .catch(err => { $('archive-report').textContent = `error: ${err.message}`; });
  fetch(`api/cases/${encodeURI(key)}/archives/${encodeURIComponent(ts)}/log`)
    .then(r => { if (!r.ok) throw new Error('no solve.log'); return r.text(); })
    .then(text => {
      const lines = text.split('\n');
      const tail = lines.slice(-200).join('\n');
      $('archive-log').textContent = tail;
    })
    .catch(err => { $('archive-log').textContent = `(${err.message})`; });
}

// ─── Build badge in the top nav ─────────────────────────────────────────
function renderBuildBadge(health) {
  const el = $('build-badge');
  if (!el) return;
  if (!health || !health.surge_loaded) {
    el.className = 'build-badge offline';
    el.innerHTML = '<span class="dot"></span>SURGE offline';
    return;
  }
  const build = health.build || 'unknown';
  const sha = (health.git_sha || '').slice(0, 7);
  const dirty = health.git_dirty ? '<span class="dirty">·</span>' : '';
  el.className = 'build-badge ' + build;
  el.innerHTML = `<span class="dot"></span>SURGE ${build}${sha ? ' · ' + sha : ''}${dirty}`;
  el.title = `.so path: ${health.so_path || '?'}\nmtime: ${health.so_mtime || '?'}\nsize: ${health.so_size_bytes != null ? (health.so_size_bytes/1e6).toFixed(1) + ' MB' : '?'}\npid: ${health.pid || '?'}\ndirty: ${health.git_dirty ? 'yes' : 'no'}`;
}

// ─── Boot ────────────────────────────────────────────────────────────────
function boot() {
  wireTabs();
  wirePeriod();
  wireActions();
  fetch('api/health').then(r => r.json()).then(renderBuildBadge).catch(() => renderBuildBadge(null));
  fetch('api/cases').then(r => r.json()).then(data => {
    CASE_INDEX = data.cases || {};
    caseKeys = Object.keys(CASE_INDEX).sort();
    buildSidebar();
  }).catch(err => {
    $('case-list').innerHTML = `<div class="banner rose" style="margin:8px">API error: ${err.message}</div>`;
  });
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot);
} else {
  boot();
}
