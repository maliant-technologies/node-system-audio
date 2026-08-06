// End-to-end walkthrough of the intended usage shape: run a rolling buffer,
// freeze it, look at the waveform, cut a clip out of the tail, write it to disk.
//
//   node examples/record-clip.mjs [seconds] [outfile]
//
// Play something audible while it runs, then open the file it prints.

import { writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { SystemAudioBuffer } from '../index.js';

const SECONDS = Number(process.argv[2] ?? 8);
// Written to the temp dir rather than the cwd, so running this from the repo
// root doesn't leave a file behind.
const OUT = process.argv[3] ?? join(tmpdir(), 'node-system-audio-clip.wav');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A generous ceiling costs address space, not memory — only the active
// retention is ever written to, so it can be raised later without reallocating.
const buf = new SystemAudioBuffer({ retentionSeconds: 60, ceilingSeconds: 600 });

buf.start();
const { deviceName, nativeSampleRate, channels } = buf.status();
console.log(`capturing ${deviceName} @ ${nativeSampleRate}Hz ${channels}ch`);
console.log(`rolling for ${SECONDS}s — play something...\n`);

for (let i = 0; i < SECONDS; i++) {
  await sleep(1000);
  const s = buf.status();
  process.stdout.write(`\r  held ${(s.filledMs / 1000).toFixed(1)}s`);
}

// Freeze stops capture and keeps the contents. Audio playing from here on is
// not recorded — the assumption is that you paused whatever was making it.
buf.freeze();
const frozen = buf.status();
console.log(`\n\nfrozen with ${(frozen.filledMs / 1000).toFixed(1)}s held`);

if (frozen.filledMs === 0) {
  console.error('nothing captured — see examples/permission-check.mjs');
  process.exit(1);
}

// Peaks are computed in Rust: this is 120 numbers rather than the whole buffer.
const peaks = buf.peaks(60);
const ramp = ' ▁▂▃▄▅▆▇█';
let wave = '';
for (let i = 0; i < peaks.length; i += 2) {
  const amplitude = Math.max(Math.abs(peaks[i]), Math.abs(peaks[i + 1]));
  wave += ramp[Math.min(ramp.length - 1, Math.round(amplitude * (ramp.length - 1)))];
}
console.log(`\n  ${wave}\n`);

// Cut the last 3 seconds. Offsets run from the oldest audio held, so the tail is
// the end of the window — which is where "what just happened" always lives.
const end = frozen.filledMs;
const start = Math.max(0, end - 3000);
const wav = buf.read(start, end);

writeFileSync(OUT, wav);
buf.stop();

console.log(
  `wrote ${OUT} — ${(wav.length / 1024).toFixed(1)} KB, ` +
    `${((end - start) / 1000).toFixed(1)}s at ${frozen.sampleRate}Hz mono 16-bit`,
);
console.log('this is the byte-for-byte shape a transcriber receives.');
