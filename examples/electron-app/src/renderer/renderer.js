// Exercises every method the addon exposes.
//
// The flow is built around one distinction that is easy to get wrong:
//   freeze() keeps the audio so you can cut it.
//   stop()   throws it away.
// So freeze is the primary action while recording, and stop is labelled
// "Discard" and asks first.

const BUCKETS = 600;
const POLL_MS = 200;

const $ = (id) => document.getElementById(id);

let status = null;
let peaks = [];
/** Selection in ms, relative to the oldest audio held. */
let selection = null;
let dragFrom = null;
let clipUrl = null;
let clipBytes = null;

const fmtMs = (ms) => `${(ms / 1000).toFixed(1)}s`;

function showError(message) {
  const el = $('error');
  el.hidden = !message;
  el.dataset.source = message?.startsWith('stream:') ? 'stream' : '';
  if (message) el.textContent = `${message}  (click to dismiss)`;
}

$('error').onclick = async () => {
  await window.audio.clearError();
  showError(null);
};

const guard = (fn) => async () => {
  try {
    showError(null);
    await fn();
    await refresh();
  } catch (e) {
    showError(e.message);
  }
};

// ---------------------------------------------------------------- permission

async function refreshPermission() {
  const p = await window.audio.permissionStatus();
  const el = $('permission');
  el.textContent = p === 'notRequired' ? 'no permission needed' : p;
  el.className = `pill ${p}`;
  $('open-settings').hidden = p === 'notRequired';
}

// --------------------------------------------------------------------- chrome

function renderChrome() {
  const state = status?.state ?? 'idle';
  const held = status?.filledMs ?? 0;
  const frozen = state === 'frozen';
  const running = state === 'running';

  $('start').hidden = state !== 'idle';
  $('freeze').hidden = !running;
  $('resume').hidden = !frozen;
  $('stop').hidden = state === 'idle';
  $('stop').textContent = frozen ? 'Discard' : 'Stop & discard';

  const canSelect = frozen && held > 0;
  for (const id of ['select-tail', 'select-tail-10', 'select-all']) {
    $(id).disabled = !canSelect;
  }

  $('guide').className = `guide ${state}`;
  $('guide').textContent = {
    idle: 'Press Start, play some audio, then Freeze to keep what you captured.',
    running: `Recording — ${fmtMs(held)} held. Press Freeze to keep it. "Stop & discard" throws it away.`,
    frozen:
      held > 0
        ? 'Frozen. Drag across the waveform to select a range, or use a preset, then Cut clip.'
        : 'Frozen, but nothing was captured — check the permission badge above.',
  }[state];

  $('wave-hint').textContent = running
    ? 'live — freeze before selecting'
    : frozen
      ? 'drag to select a range'
      : 'nothing captured';
}

function renderStats() {
  if (!status) return;
  const real = Math.max(0, status.filledMs - status.silenceInsertedMs);
  const cells = [
    ['state', status.state, `state-${status.state}`],
    ['held', fmtMs(status.filledMs)],
    ['of which real', fmtMs(real)],
    ['silence inserted', fmtMs(status.silenceInsertedMs)],
    ['retention', fmtMs(status.retentionMs)],
    ['output', `${status.sampleRate} Hz mono`],
    ['device rate', status.nativeSampleRate ? `${status.nativeSampleRate} Hz` : '—'],
    ['channels', status.channels ?? '—'],
    ['device', status.deviceName ?? '—'],
    ['restarts', status.restarts],
  ];

  $('stats').innerHTML = cells
    .map(
      ([k, v, cls]) =>
        `<div class="stat"><div class="k">${k}</div><div class="v ${cls ?? ''}">${v}</div></div>`,
    )
    .join('');

  // Latched, so it stays until dismissed — which is the point.
  if (status.error) showError(`stream: ${status.error}`);
  else if ($('error').dataset.source === 'stream') showError(null);
}

// ------------------------------------------------------------------ waveform

function drawWave() {
  const canvas = $('wave');
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  canvas.width = w * dpr;
  canvas.height = h * dpr;

  const ctx = canvas.getContext('2d');
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);

  const mid = h / 2;
  ctx.strokeStyle = '#262b36';
  ctx.beginPath();
  ctx.moveTo(0, mid);
  ctx.lineTo(w, mid);
  ctx.stroke();

  if (!peaks.length || !status?.filledMs) return;

  const buckets = peaks.length / 2;
  const barWidth = w / buckets;
  const frozen = status.state === 'frozen';

  // Unselected audio is dimmed when a selection exists, so what you are about
  // to cut is unmistakable.
  const sel = selection;
  for (let b = 0; b < buckets; b++) {
    const tMs = (b / buckets) * status.filledMs;
    const inSel = sel && tMs >= sel.start && tMs <= sel.end;
    ctx.fillStyle = inSel ? '#4ea1ff' : frozen ? (sel ? '#33465e' : '#ffb454') : '#4ea1ff';

    const lo = peaks[b * 2];
    const hi = peaks[b * 2 + 1];
    const y1 = mid - hi * mid;
    const y2 = mid - lo * mid;
    ctx.fillRect(b * barWidth, y1, Math.max(barWidth - 0.5, 0.5), Math.max(y2 - y1, 1));
  }

  if (sel) {
    const x1 = (sel.start / status.filledMs) * w;
    const x2 = (sel.end / status.filledMs) * w;
    ctx.strokeStyle = '#4ea1ff';
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(x1, 0);
    ctx.lineTo(x1, h);
    ctx.moveTo(x2, 0);
    ctx.lineTo(x2, h);
    ctx.stroke();
    ctx.lineWidth = 1;
  }
}

function setSelection(next) {
  selection = next;
  const usable = next && next.end - next.start >= 50 && status?.state === 'frozen';
  $('cut').disabled = !usable;
  $('selection').textContent = next
    ? `${fmtMs(next.start)} → ${fmtMs(next.end)}  ·  ${fmtMs(next.end - next.start)}`
    : '—';
  drawWave();
}

// --------------------------------------------------------------------- state

async function refresh() {
  const previous = status?.state;
  status = await window.audio.status();
  peaks = await window.audio.peaks(BUCKETS);

  // Entering frozen: preselect the tail, because "what just happened" is almost
  // always the thing you froze for — and it means Cut is usable immediately.
  if (status.state === 'frozen' && previous !== 'frozen' && status.filledMs > 0) {
    setSelection({ start: Math.max(0, status.filledMs - 5000), end: status.filledMs });
  }

  if (status.state !== 'frozen' && selection) setSelection(null);

  renderChrome();
  renderStats();
  drawWave();
}

// -------------------------------------------------------------------- events

function msFromEvent(event) {
  if (!status?.filledMs) return 0;
  const rect = $('wave').getBoundingClientRect();
  const ratio = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
  return Math.round(ratio * status.filledMs);
}

$('wave').addEventListener('mousedown', (e) => {
  if (status?.state !== 'frozen') return;
  dragFrom = msFromEvent(e);
});

$('wave').addEventListener('mousemove', (e) => {
  if (dragFrom === null) return;
  const to = msFromEvent(e);
  setSelection({ start: Math.min(dragFrom, to), end: Math.max(dragFrom, to) });
});

window.addEventListener('mouseup', () => {
  dragFrom = null;
});

$('start').onclick = guard(() => window.audio.start());
$('freeze').onclick = guard(() => window.audio.freeze());

$('resume').onclick = guard(async () => {
  if (clipBytes || confirm('Resume clears the buffer and starts fresh. Continue?')) {
    await window.audio.resume();
  }
});

$('stop').onclick = guard(async () => {
  if (status?.filledMs > 0 && !confirm('This discards the captured audio. Continue?')) return;
  await window.audio.stop();
});

$('retention').oninput = (e) => {
  $('retention-value').textContent = `${e.target.value}s`;
};
$('retention').onchange = guard(() => window.audio.setRetention(Number($('retention').value)));

const preset = (ms) => () => {
  if (!status?.filledMs) return;
  setSelection({ start: Math.max(0, status.filledMs - ms), end: status.filledMs });
};
$('select-tail').onclick = preset(5000);
$('select-tail-10').onclick = preset(10_000);
$('select-all').onclick = () => {
  if (status?.filledMs) setSelection({ start: 0, end: status.filledMs });
};

$('cut').onclick = guard(async () => {
  if (!selection) return;
  const bytes = await window.audio.read(selection.start, selection.end);

  const header = new TextDecoder().decode(bytes.slice(0, 4));
  if (header !== 'RIFF') throw new Error(`expected a RIFF header, got "${header}"`);

  clipBytes = bytes;
  if (clipUrl) URL.revokeObjectURL(clipUrl);
  clipUrl = URL.createObjectURL(new Blob([bytes], { type: 'audio/wav' }));

  const player = $('player');
  player.src = clipUrl;
  $('save').disabled = false;
  $('clip-info').textContent =
    `${fmtMs(selection.end - selection.start)} · ${(bytes.length / 1024).toFixed(1)} KB · ` +
    `${status.sampleRate} Hz mono 16-bit · RIFF ok`;
  $('clip-panel').classList.add('has-clip');

  // Play it straight away — the whole point is hearing whether it worked.
  player.play().catch(() => {
    /* autoplay refused; the controls still work */
  });
});

$('save').onclick = guard(async () => {
  if (!clipBytes) return;
  const saved = await window.audio.saveWav(clipBytes);
  if (saved) $('clip-info').textContent += ` · saved`;
});

$('open-settings').onclick = guard(() => window.audio.openSettings());
window.addEventListener('resize', drawWave);

(async () => {
  const info = await window.audio.appInfo();
  $('env').textContent =
    `${info.platform}/${info.arch} · electron ${info.electron} · ` +
    (info.packaged ? 'packaged' : 'dev — TCC sees the terminal, not this app');

  await refreshPermission();
  await refresh();

  // Only poll while capturing; a frozen buffer does not change, and repolling it
  // would fight the selection.
  setInterval(() => {
    if (status?.state === 'running') refresh().catch((e) => showError(e.message));
  }, POLL_MS);
  setInterval(() => refreshPermission().catch(() => {}), 3000);
})();
