import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { SystemAudioBuffer, permissionStatus } from '../index.js';

const VALID_PERMISSIONS = ['granted', 'denied', 'unknown', 'notRequired'];

describe('permissionStatus', () => {
  it('answers with a defined status without touching the audio device', () => {
    assert.ok(VALID_PERMISSIONS.includes(permissionStatus()));
  });

  it('reports notRequired only where loopback genuinely needs no grant', () => {
    const needsGrant = process.platform === 'darwin';
    assert.equal(permissionStatus() === 'notRequired', !needsGrant);
  });
});

describe('construction', () => {
  it('defaults to a 15 minute window at 16 kHz', () => {
    const s = new SystemAudioBuffer().status();
    assert.equal(s.sampleRate, 16_000);
    assert.equal(s.retentionMs, 15 * 60 * 1000);
    assert.equal(s.state, 'idle');
    assert.equal(s.filledMs, 0);
  });

  it('accepts explicit options', () => {
    const s = new SystemAudioBuffer({
      retentionSeconds: 30,
      targetSampleRate: 8_000,
      ceilingSeconds: 60,
    }).status();
    assert.equal(s.sampleRate, 8_000);
    assert.equal(s.retentionMs, 30_000);
  });

  it('rejects a zero retention or sample rate rather than silently clamping', () => {
    assert.throws(() => new SystemAudioBuffer({ retentionSeconds: 0 }), /retentionSeconds/);
    assert.throws(() => new SystemAudioBuffer({ targetSampleRate: 0 }), /targetSampleRate/);
  });

  it('raises the ceiling to fit retention rather than truncating it', () => {
    const s = new SystemAudioBuffer({ retentionSeconds: 600, ceilingSeconds: 10 }).status();
    assert.equal(s.retentionMs, 600_000);
  });
});

describe('reads on an empty buffer', () => {
  it('still returns a valid, empty WAV', () => {
    const wav = new SystemAudioBuffer().read(0, 5_000);
    assert.equal(wav.subarray(0, 4).toString('ascii'), 'RIFF');
    assert.equal(wav.subarray(8, 12).toString('ascii'), 'WAVE');
    assert.equal(wav.length, 44, 'header only, no samples');
  });

  it('clamps an out-of-range range instead of throwing', () => {
    const buf = new SystemAudioBuffer();
    assert.equal(buf.read(999_999, 999_999_999).length, 44);
    assert.equal(buf.read(500, 100).length, 44, 'inverted range yields nothing');
  });

  it('produces a flat envelope of the requested width', () => {
    const peaks = new SystemAudioBuffer().peaks(128);
    assert.equal(peaks.length, 256, 'min and max per bucket');
    assert.ok(peaks.every((v) => v === 0));
  });

  it('rejects a zero bucket count', () => {
    assert.throws(() => new SystemAudioBuffer().peaks(0), /buckets/);
  });
});

describe('state machine', () => {
  it('refuses to freeze something that was never started', () => {
    assert.throws(() => new SystemAudioBuffer().freeze(), /not running/);
  });

  it('refuses to resume something that is not frozen', () => {
    assert.throws(() => new SystemAudioBuffer().resume(), /not frozen/);
  });

  it('treats stop on an idle buffer as a no-op', () => {
    const buf = new SystemAudioBuffer();
    buf.stop();
    assert.equal(buf.status().state, 'idle');
  });

  it('changes retention live without disturbing state', () => {
    const buf = new SystemAudioBuffer({ retentionSeconds: 900, ceilingSeconds: 1800 });
    buf.setRetentionSeconds(60);
    assert.equal(buf.status().retentionMs, 60_000);

    buf.setRetentionSeconds(1200);
    assert.equal(buf.status().retentionMs, 1_200_000, 'raising it again is allowed');
    assert.equal(buf.status().state, 'idle');
  });

  it('rejects a zero retention', () => {
    assert.throws(() => new SystemAudioBuffer().setRetentionSeconds(0), /retentionSeconds/);
  });
});

describe('errors and recovery', () => {
  it('starts with no error and a zero restart count', () => {
    const s = new SystemAudioBuffer().status();
    assert.equal(s.error, undefined);
    assert.equal(s.restarts, 0);
  });

  it('exposes clearError as a no-op when nothing has failed', () => {
    const buf = new SystemAudioBuffer();
    buf.clearError();
    assert.equal(buf.status().error, undefined);
  });

  it('reports device facts only while capturing', () => {
    // An idle buffer has no device, so reporting a stale rate or name would be
    // a lie about what is being recorded.
    const s = new SystemAudioBuffer().status();
    assert.equal(s.nativeSampleRate, undefined);
    assert.equal(s.channels, undefined);
    assert.equal(s.deviceName, undefined);
  });
});

// Opens the real default output device. Skipped where CI has no audio device;
// on macOS a denied permission shows up as zero frames rather than an error,
// which is asserted on rather than worked around.
describe('live capture', { skip: process.env.CI ? 'no audio device in CI' : false }, () => {
  const settle = (ms) => new Promise((r) => setTimeout(r, ms));

  it('captures a wall-clock-accurate window and cuts a clip from it', async (t) => {
    const buf = new SystemAudioBuffer({ retentionSeconds: 30, ceilingSeconds: 60 });

    try {
      buf.start();
    } catch (e) {
      t.skip(`no capturable output device: ${e.message}`);
      return;
    }

    const running = buf.status();
    assert.equal(running.state, 'running');
    assert.ok(running.nativeSampleRate > 0, 'device rate reported once running');
    assert.ok(running.deviceName, 'device name reported once running');

    await settle(1200);
    buf.freeze();

    const frozen = buf.status();
    assert.equal(frozen.state, 'frozen');

    if (frozen.filledMs === 0) {
      assert.equal(
        process.platform,
        'darwin',
        'only macOS should ever yield zero frames — that is a denied permission',
      );
      t.diagnostic('no frames: System Audio Recording permission not granted');
      return;
    }

    // Silence-filled or not, the window must track elapsed time rather than
    // only the spans where audio happened to be playing.
    assert.ok(
      frozen.filledMs >= 800,
      `expected ~1200ms of wall clock, got ${frozen.filledMs}ms`,
    );

    const wav = buf.read(0, 500);
    assert.equal(wav.subarray(0, 4).toString('ascii'), 'RIFF');
    const declared = wav.readUInt32LE(40);
    assert.equal(declared, wav.length - 44, 'data chunk size matches payload');
    assert.ok(declared > 0, 'a clip cut from a filled buffer has samples');

    const peaks = buf.peaks(64);
    assert.equal(peaks.length, 128);
    assert.ok(peaks.every((v) => v >= -1 && v <= 1), 'envelope stays normalised');

    // Freezing is not a pause: resuming starts from empty so the timeline
    // cannot contain an invisible discontinuity.
    buf.resume();
    assert.equal(buf.status().state, 'running');
    assert.ok(buf.status().filledMs < frozen.filledMs, 'resume cleared the window');

    buf.stop();
    assert.equal(buf.status().state, 'idle');
    assert.equal(buf.status().filledMs, 0);
  });

  it('survives a real capture session without faulting or restarting', async (t) => {
    const buf = new SystemAudioBuffer({ retentionSeconds: 10 });
    try {
      buf.start();
    } catch {
      t.skip('no capturable output device');
      return;
    }

    await settle(1500);
    const s = buf.status();
    buf.stop();

    // Nothing was unplugged, so the supervisor should not have rebuilt anything.
    // A nonzero count here means it is restarting spuriously — which would show
    // up as gaps in the audio.
    assert.equal(s.restarts, 0, `unexpected restarts: ${s.error ?? 'no error'}`);
    assert.equal(s.error, undefined, `unexpected error: ${s.error}`);
  });

  it('clears state on stop so a fresh start is genuinely fresh', async (t) => {
    const buf = new SystemAudioBuffer({ retentionSeconds: 10 });
    try {
      buf.start();
    } catch {
      t.skip('no capturable output device');
      return;
    }

    await settle(600);
    buf.stop();

    const idle = buf.status();
    assert.equal(idle.state, 'idle');
    assert.equal(idle.filledMs, 0);
    assert.equal(idle.silenceInsertedMs, 0);
    assert.equal(idle.deviceName, undefined);
  });

  it('rejects start() while frozen, pointing at resume()', async (t) => {
    const buf = new SystemAudioBuffer({ retentionSeconds: 10 });
    try {
      buf.start();
    } catch {
      t.skip('no capturable output device');
      return;
    }
    await settle(100);
    buf.freeze();
    assert.throws(() => buf.start(), /resume/);
    buf.stop();
  });
});
