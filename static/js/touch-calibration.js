"use strict";
const $ = (id) => document.getElementById(id);
const canvas = $('canvas');

const results = {
  started_at: new Date().toISOString(),
  stages: {},
  pixel_drift_max: 0,
  corners_total: 0,
  corners_hit: 0,
  edge_dead_strips: [],
  drag_max_distance: 0,
  summary: 'pending',
  finished_at: null,
};

let currentStage = 0;
const dragState = { active: false, startX: 0, startY: 0, currentX: 0, currentY: 0, targetX: 0, targetY: 0 };

const STAGE_DEFS = [
  { id: 'corner',  label: 'Corners' },
  { id: 'grid',    label: 'Grid' },
  { id: 'edges',   label: 'Edges' },
  { id: 'drag',    label: 'Drag' },
  { id: 'summary', label: 'Result' },
];

const STAGE_HANDLERS = {
  corner: {
    enter: () => cornerEnter(),
    onTap: (x, y) => cornerTap(x, y),
  },
  grid: {
    enter: () => gridEnter(),
    onTap: (x, y) => gridTap(x, y),
  },
  edges: {
    enter: () => edgesEnter(),
    onTap: (x, y) => edgesTap(x, y),
  },
  drag: {
    enter: () => dragEnter(),
    onTap: (x, y) => dragTap(x, y),
    onMove: (x, y) => dragMove(x, y),
  },
  summary: {
    enter: () => summaryEnter(),
    onTap: () => {},
  },
};

function rect() {
  const main = document.querySelector('main');
  return { w: main.clientWidth, h: main.clientHeight };
}
function rand(min, max) { return Math.random() * (max - min) + min; }
function clearChildren(el) { while (el.firstChild) el.removeChild(el.firstChild); }
function showHint(html) {
  let h = document.getElementById('hint');
  if (!h) { h = document.createElement('div'); h.id = 'hint'; h.className = 'hint'; canvas.appendChild(h); }
  h.innerHTML = html;
}
function hideHint() { const h = document.getElementById('hint'); if (h) h.remove(); }

function setStage(idx) {
  currentStage = idx;
  const dotEls = document.querySelectorAll('#progress .dot');
  dotEls.forEach((d, i) => {
    d.className = 'dot';
    if (i < idx) {
      const s = results.stages[STAGE_DEFS[i].id];
      d.classList.add(s && s.summary === 'fail' ? 'failed' : 'done');
    } else if (i === idx) {
      d.classList.add('active');
    }
  });
  $('stageLabel').innerHTML = '<b>' + (idx+1) + '/' + STAGE_DEFS.length + '</b> · ' + STAGE_DEFS[idx].label;
  STAGE_HANDLERS[STAGE_DEFS[idx].id].enter();
}

canvas.addEventListener('pointerdown', (ev) => {
  ev.preventDefault();
  const sid = STAGE_DEFS[currentStage].id;
  if (sid === 'drag') {
    if (Math.hypot(ev.clientX - dragState.targetX, ev.clientY - dragState.targetY) < 60) {
      dragState.active = true;
      dragState.startX = dragState.currentX = ev.clientX;
      dragState.startY = dragState.currentY = ev.clientY;
      STAGE_HANDLERS.drag.onMove(ev.clientX, ev.clientY);
    }
    return;
  }
  STAGE_HANDLERS[sid].onTap(ev.clientX, ev.clientY);
}, { passive: false });

canvas.addEventListener('pointermove', (ev) => {
  if (!dragState.active) return;
  dragState.currentX = ev.clientX;
  dragState.currentY = ev.clientY;
  STAGE_HANDLERS.drag.onMove(ev.clientX, ev.clientY);
}, { passive: false });

canvas.addEventListener('pointerup', (ev) => {
  if (STAGE_DEFS[currentStage].id !== 'drag' || !dragState.active) return;
  dragState.active = false;
  const dist = Math.hypot(ev.clientX - dragState.targetX, ev.clientY - dragState.targetY);
  results.stages.drag = {
    passes: dist < 80 ? 1 : 0,
    fails:  dist < 80 ? 0 : 1,
    drag_max_distance: dist,
  };
  results.drag_max_distance = dist;
  finishStage();
}, { passive: false });

canvas.addEventListener('pointercancel', () => { dragState.active = false; }, { passive: true });

// === STAGE: Corner ===
let cornerState = null;
function cornerEnter() {
  canvas.className = 'stage-canvas target';
  clearChildren(canvas);
  const r = rect();
  const corners = [
    { x: 0,   y: 0,   label: 'TL' },
    { x: r.w, y: 0,   label: 'TR' },
    { x: r.w, y: r.h, label: 'BR' },
    { x: 0,   y: r.h, label: 'BL' },
  ];
  results.stages.corner = { passes: 0, fails: 0, hits: [], misses: [], total: 4 };
  results.corners_total = 4;
  results.corners_hit = 0;
  cornerState = { i: 0, corners };
  showHint('Tap each <b>corner</b> as it lights up');
  cornerShowNext();
}
function cornerShowNext() {
  clearChildren(canvas);
  if (cornerState.i >= cornerState.corners.length) { finishStage(); return; }
  const c = cornerState.corners[cornerState.i];
  const bar = document.createElement('div');
  bar.className = 'edge-bar armed';
  bar.style.left = (c.x - 50) + 'px';
  bar.style.top  = (c.y - 50) + 'px';
  bar.style.width = '100px';
  bar.style.height = '100px';
  bar.textContent = c.label;
  canvas.appendChild(bar);
  cornerState.active = { el: bar, expected: c, idx: cornerState.i };
  cornerState.timeout = setTimeout(() => {
    if (cornerState.active && cornerState.active.idx === cornerState.i) {
      bar.className = 'edge-bar missed';
      results.stages.corner.misses.push({ expected: c, reason: 'timeout' });
      results.stages.corner.fails++;
      cornerState.active = null;
      cornerState.i++;
      setTimeout(cornerShowNext, 800);
    }
  }, 4000);
}
function cornerTap(x, y) {
  const a = cornerState.active;
  if (!a) return;
  const dx = x - a.expected.x;
  const dy = y - a.expected.y;
  if (Math.abs(dx) <= 60 && Math.abs(dy) <= 60) {
    a.el.className = 'edge-bar hit';
    results.stages.corner.passes++;
    results.corners_hit++;
    results.stages.corner.hits.push({ x, y, expected: a.expected, drift_px: Math.hypot(dx, dy) });
    cornerState.active = null;
    if (cornerState.timeout) clearTimeout(cornerState.timeout);
    cornerState.i++;
    setTimeout(cornerShowNext, 600);
  } else {
    const dist = Math.hypot(dx, dy);
    if (dist > 30) results.pixel_drift_max = Math.max(results.pixel_drift_max, dist);
  }
}

// === STAGE: Grid ===
let gridState = null;
function gridEnter() {
  canvas.className = 'stage-canvas grid-test';
  clearChildren(canvas);
  const r = rect();
  const cols = 5, rows = 4;
  const padding = 60;
  const cellW = (r.w - padding * 2) / cols;
  const cellH = (r.h - padding * 2) / rows;
  const targets = [];
  for (let row = 0; row < rows; row++) {
    for (let col = 0; col < cols; col++) {
      targets.push({
        col, row,
        x: padding + col * cellW,
        y: padding + row * cellH,
        w: cellW - 8,
        h: cellH - 8,
      });
    }
  }
  // Shuffle
  for (let i = targets.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [targets[i], targets[j]] = [targets[j], targets[i]];
  }
  results.stages.grid = { passes: 0, fails: 0, hits: [], misses: [], total: targets.length };
  gridState = { i: 0, targets, cells: [], timer: null };
  showHint('Tap each <b>highlighted</b> cell');
  gridShowNext();
}
function gridShowNext() {
  clearChildren(canvas);
  gridState.cells = [];
  gridState.targets.forEach((t, idx) => {
    const cell = document.createElement('div');
    cell.className = 'grid-cell idle';
    cell.style.left = t.x + 'px';
    cell.style.top  = t.y + 'px';
    cell.style.width  = t.w + 'px';
    cell.style.height = t.h + 'px';
    canvas.appendChild(cell);
    gridState.cells.push(cell);
  });
  if (gridState.i >= gridState.targets.length) { finishStage(); return; }
  const ac = gridState.cells[gridState.i];
  if (ac) {
    ac.className = 'grid-cell armed';
    ac.textContent = (gridState.i + 1);
  }
  gridState.active = { idx: gridState.i, el: ac, expected: gridState.targets[gridState.i] };
  if (gridState.timer) clearTimeout(gridState.timer);
  gridState.timer = setTimeout(() => {
    if (gridState.active && gridState.active.idx === gridState.i) {
      if (gridState.active.el) gridState.active.el.className = 'grid-cell missed';
      results.stages.grid.misses.push({ idx: gridState.i, reason: 'timeout' });
      results.stages.grid.fails++;
      gridState.active = null;
      gridState.i++;
      setTimeout(gridShowNext, 600);
    }
  }, 5000);
}
function gridTap(x, y) {
  if (!gridState.active) return;
  const e = gridState.active.expected;
  if (x >= e.x && x <= e.x + e.w && y >= e.y && y <= e.y + e.h) {
    gridState.active.el.className = 'grid-cell hit';
    results.stages.grid.passes++;
    results.stages.grid.hits.push({ x, y, expected: e });
    gridState.active = null;
    if (gridState.timer) clearTimeout(gridState.timer);
    gridState.i++;
    setTimeout(gridShowNext, 400);
  } else {
    const cdx = x - (e.x + e.w/2);
    const cdy = y - (e.y + e.h/2);
    const dist = Math.hypot(cdx, cdy);
    if (dist > 20) results.pixel_drift_max = Math.max(results.pixel_drift_max, dist);
  }
}

// === STAGE: Edges ===
let edgeState = null;
function edgesEnter() {
  canvas.className = 'stage-canvas target';
  clearChildren(canvas);
  const r = rect();
  const edges = [
    { id: 'top',    x: r.w/2 - 200, y: 8,            w: 400, h: 60 },
    { id: 'right',  x: r.w - 68,    y: r.h/2 - 100,   w: 60,  h: 200 },
    { id: 'bottom', x: r.w/2 - 200, y: r.h - 68,      w: 400, h: 60 },
    { id: 'left',   x: 8,           y: r.h/2 - 100,   w: 60,  h: 200 },
  ];
  results.stages.edges = { passes: 0, fails: 0, hits: [], misses: [], total: 4 };
  results.edge_dead_strips = [];
  edgeState = { i: 0, edges };
  showHint('Tap each edge bar to confirm the <b>edge sensors work</b>');
  edgeShowNext();
}
function edgeShowNext() {
  clearChildren(canvas);
  if (edgeState.i >= edgeState.edges.length) { finishStage(); return; }
  const e = edgeState.edges[edgeState.i];
  const bar = document.createElement('div');
  bar.className = 'edge-bar armed';
  bar.style.left = e.x + 'px';
  bar.style.top  = e.y + 'px';
  bar.style.width  = e.w + 'px';
  bar.style.height = e.h + 'px';
  bar.textContent = e.id.toUpperCase();
  canvas.appendChild(bar);
  edgeState.active = { el: bar, expected: e, idx: edgeState.i };
  edgeState.timeout = setTimeout(() => {
    if (edgeState.active && edgeState.active.idx === edgeState.i) {
      bar.className = 'edge-bar missed';
      results.stages.edges.misses.push({ edge: e.id, reason: 'timeout' });
      results.stages.edges.fails++;
      results.edge_dead_strips.push({ edge: e.id, max_offset: 'timeout' });
      edgeState.active = null;
      edgeState.i++;
      setTimeout(edgeShowNext, 700);
    }
  }, 4500);
}
function edgesTap(x, y) {
  const a = edgeState.active;
  if (!a) return;
  const e = a.expected;
  if (x >= e.x - 8 && x <= e.x + e.w + 8 && y >= e.y - 8 && y <= e.y + e.h + 8) {
    a.el.className = 'edge-bar hit';
    results.stages.edges.passes++;
    results.stages.edges.hits.push({ x, y, edge: e.id });
    edgeState.active = null;
    if (edgeState.timeout) clearTimeout(edgeState.timeout);
    edgeState.i++;
    setTimeout(edgeShowNext, 500);
  } else {
    const cdx = x - (e.x + e.w/2);
    const cdy = y - (e.y + e.h/2);
    const dist = Math.hypot(cdx, cdy);
    if (dist > 30) results.pixel_drift_max = Math.max(results.pixel_drift_max, dist);
  }
}

// === STAGE: Drag ===
function dragEnter() {
  canvas.className = 'stage-canvas target';
  clearChildren(canvas);
  const r = rect();
  dragState.startX   = r.w * 0.2;
  dragState.startY   = r.h * 0.5;
  dragState.targetX  = r.w * 0.8;
  dragState.targetY  = r.h * 0.5;
  dragState.currentX = dragState.startX;
  dragState.currentY = dragState.startY;
  const start = document.createElement('div');
  start.id = 'dragStart';
  start.className = 'drag-handle start';
  const end = document.createElement('div');
  end.id = 'dragEnd';
  end.className = 'drag-handle end';
  canvas.appendChild(start);
  canvas.appendChild(end);
  dragMove(0, 0);
  showHint('Press the <b>green</b> circle and drag to the <b>yellow</b> circle');
}
function dragTap(x, y) {
  // intentionally no-op; drag uses pointerdown/up for state
}
function dragMove(x, y) {
  const s = document.getElementById('dragStart');
  const e = document.getElementById('dragEnd');
  if (s) { s.style.left = dragState.startX + 'px'; s.style.top = dragState.startY + 'px'; }
  if (e) { e.style.left = dragState.targetX + 'px'; e.style.top = dragState.targetY + 'px'; }
  const existing = document.getElementById('dragLine');
  if (existing) existing.remove();
  if (dragState.active) {
    const line = document.createElement('div');
    line.id = 'dragLine';
    line.className = 'drag-line';
    const dx = dragState.currentX - dragState.startX;
    const dy = dragState.currentY - dragState.startY;
    const len = Math.hypot(dx, dy);
    const angle = Math.atan2(dy, dx) * 180 / Math.PI;
    line.style.left = dragState.startX + 'px';
    line.style.top  = dragState.startY + 'px';
    line.style.width = len + 'px';
    line.style.transform = 'rotate(' + angle + 'deg)';
    canvas.appendChild(line);
  }
}

// === STAGE: Summary ===
function summaryEnter() {
  canvas.className = 'stage-canvas summary';
  clearChildren(canvas);
  hideHint();
  results.finished_at = new Date().toISOString();

  const totalPass = Object.values(results.stages).reduce((a, s) => a + (s.passes || 0), 0);
  const totalFail = Object.values(results.stages).reduce((a, s) => a + (s.fails || 0), 0);
  const hit_rate = totalPass + totalFail > 0 ? (totalPass / (totalPass + totalFail) * 100).toFixed(1) : '0';

  const cornerPass = (results.stages.corner?.passes || 0) >= 4;
  const gridPass   = (results.stages.grid?.passes || 0)   >= 20;
  const edgePass   = (results.stages.edges?.passes || 0)  >= 4;
  const dragPass   = (results.stages.drag?.passes || 0)   >= 1;
  const driftPass  = results.pixel_drift_max < 50;
  results.summary = (cornerPass && gridPass && edgePass && dragPass && driftPass) ? 'pass' : 'fail';

  const wrap = document.createElement('div');
  wrap.className = 'summary-wrap';
  const card = document.createElement('div');
  card.className = 'summary-card';
  card.innerHTML = `
    <h2 class="${results.summary}">${results.summary === 'pass' ? '✓ Calibration PASS' : '✗ Calibration FAIL'}</h2>
    <div class="summary-row"><span class="label">Corners</span><span class="value">${results.corners_hit}/${results.corners_total}</span></div>
    <div class="summary-row"><span class="label">Grid cells</span><span class="value">${results.stages.grid?.passes || 0}/${results.stages.grid?.total || 20}</span></div>
    <div class="summary-row"><span class="label">Edge strips</span><span class="value">${results.stages.edges?.passes || 0}/4</span></div>
    <div class="summary-row"><span class="label">Drag accuracy</span><span class="value">${(results.drag_max_distance || 0).toFixed(1)} px</span></div>
    <div class="summary-row"><span class="label">Pixel drift (max)</span><span class="value">${results.pixel_drift_max.toFixed(1)} px</span></div>
    <div class="summary-row"><span class="label">Hit rate</span><span class="value">${hit_rate}%</span></div>
    <div class="summary-row"><span class="label">Edge dead strips</span><span class="value">${results.edge_dead_strips.length ? results.edge_dead_strips.map(d => d.edge).join(', ') : 'none'}</span></div>
    ${results.summary === 'fail' ? '<ul class="failures" id="failList"></ul>' : ''}
    <div class="summary-actions">
      <button id="rerunBtn">Run again</button>
      <button id="sendBtn">Save result</button>
      <button class="primary" id="doneBtn">Done</button>
    </div>
  `;
  wrap.appendChild(card);
  canvas.appendChild(wrap);

  if (results.summary === 'fail') {
    const list = $('failList');
    [
      ['corner', results.stages.corner?.misses, m => m.expected?.label],
      ['grid',   results.stages.grid?.misses,   m => m.idx + 1],
      ['edges',  results.stages.edges?.misses,  m => m.edge],
    ].forEach(([stage, misses, fmt]) => {
      if (!misses) return;
      misses.forEach(m => {
        const li = document.createElement('li');
        li.innerHTML = `<b>${stage} ${fmt(m)}</b> ${m.reason}`;
        list.appendChild(li);
      });
    });
  }
  $('rerunBtn').addEventListener('click', () => {
    results.stages = {};
    results.pixel_drift_max = 0;
    results.corners_total = 0;
    results.corners_hit = 0;
    results.edge_dead_strips = [];
    results.drag_max_distance = 0;
    results.summary = 'pending';
    results.started_at = new Date().toISOString();
    results.finished_at = null;
    setStage(0);
  });
  $('sendBtn').addEventListener('click', saveResult);
  $('doneBtn').addEventListener('click', () => {
    if (window.opener) window.close();
    else window.location.href = '/';
  });
}

function finishStage() {
  hideHint();
  document.querySelectorAll('#progress .dot').forEach((d, i) => {
    if (i <= currentStage) {
      d.classList.add('done');
      d.classList.remove('active');
    }
  });
  setTimeout(() => {
    const next = currentStage + 1;
    if (next < STAGE_DEFS.length) setStage(next);
    else setStage(STAGE_DEFS.length - 1);
  }, 800);
}

async function saveResult() {
  const sendBtn = $('sendBtn');
  if (sendBtn) { sendBtn.disabled = true; sendBtn.textContent = 'Saving…'; }
  try {
    const r = await fetch('/api/touch-calibration.json', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(results),
    });
    if (!r.ok) throw new Error('HTTP ' + r.status);
    if (sendBtn) { sendBtn.textContent = '✓ Saved'; }
  } catch (e) {
    if (sendBtn) { sendBtn.textContent = '✗ Save failed'; sendBtn.disabled = false; }
  }
}

$('backBtn').addEventListener('click', () => {
  if (window.history.length > 1) window.history.back();
  else window.location.href = '/';
});

results.max_touch_points = (navigator.maxTouchPoints || 1);
results.user_agent = navigator.userAgent;
results.viewport = { w: window.innerWidth, h: window.innerHeight };

setStage(0);
