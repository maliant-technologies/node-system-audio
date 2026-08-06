# node-system-audio

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Node](https://img.shields.io/badge/node-%3E%3D20-brightgreen.svg)](https://nodejs.org)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Windows%20%7C%20Linux-lightgrey.svg)](#platforms)

Native Node.js bindings for system audio capture — records **what the machine is playing**, not the microphone, into a rolling in-memory buffer you can reach back into after the fact.

The buffer lives in Rust. 15 minutes of mono 16 kHz is ~28 MB that never crosses into V8, is never serialised, and is never walked by a garbage collector.

## Contents

- [Installation](#installation)
- [Quick Start](#quick-start)
- [Platforms](#platforms)
- [API](#api)
- [Permissions](#permissions)
- [Device Changes](#device-changes)
- [Wall-Clock Accuracy](#wall-clock-accuracy)
- [Waveforms](#waveforms)
- [Examples](#examples)
- [Development](#development)
- [License](#license)

## Installation

```bash
npm install @maliant-technologies/node-system-audio
```

Prebuilt binaries ship as per-platform optional dependencies, so npm downloads only the one matching your machine. No build step, no Rust toolchain, no `node-gyp`.

N-API is ABI-stable, so the same binary works across Node and Electron versions without rebuilding.

## Quick Start

```js
import { SystemAudioBuffer } from '@maliant-technologies/node-system-audio';

const buf = new SystemAudioBuffer({ retentionSeconds: 900 });
buf.start();

// ... something interesting goes past ...

buf.freeze();
const { filledMs } = buf.status();
const wav = buf.read(filledMs - 30_000, filledMs);  // last 30s as a WAV
```

### Rolling capture with a live readout

```js
const buf = new SystemAudioBuffer({ retentionSeconds: 60, ceilingSeconds: 600 });
buf.start();

const timer = setInterval(() => {
  const s = buf.status();
  console.log(`${s.deviceName} @ ${s.nativeSampleRate}Hz — ${s.filledMs}ms held`);
}, 1000);

process.on('SIGINT', () => {
  clearInterval(timer);
  buf.stop();
});
```

### Cutting a clip

```js
buf.freeze();                              // capture stops, contents kept
const { filledMs, sampleRate } = buf.status();

const wav = buf.read(filledMs - 5_000, filledMs);
writeFileSync('clip.wav', wav);            // mono 16-bit PCM at sampleRate

buf.resume();                              // capture again, from empty
```

### In Electron

The buffer belongs in the main process, so it survives every window closing and never touches Chromium's loopback path.

```js
// main.js
const { SystemAudioBuffer } = require('@maliant-technologies/node-system-audio');
const buffer = new SystemAudioBuffer({ retentionSeconds: 900 });

ipcMain.handle('audio:start', () => { buffer.start(); return buffer.status(); });
ipcMain.handle('audio:freeze', () => { buffer.freeze(); return buffer.status(); });
ipcMain.handle('audio:peaks', (_e, n) => Array.from(buffer.peaks(n)));
ipcMain.handle('audio:read', (_e, a, b) => new Uint8Array(buffer.read(a, b)));
```

Add `asar: { unpack: '**/*.node' }` to your packager config so the addon can be loaded.

## Platforms

| OS | Mechanism | Permission | Prebuilt for |
|---|---|---|---|
| macOS 14.6+ | CoreAudio process tap | System Audio Recording | arm64, x64 |
| Windows 10+ | WASAPI loopback | none | x64, arm64 |
| Linux (glibc) | PipeWire / PulseAudio monitor source | none | x64, arm64 |
| Linux (musl) | PipeWire / PulseAudio monitor source | none | x64, arm64 |

There is no per-platform code in this crate. Capture is a single call — build an **input** stream on the default **output** device — and [cpal](https://github.com/RustAudio/cpal) selects the mechanism.

macOS below 14.6 has no loopback path at all.

> **Runtime verification so far is macOS arm64 only.** The other targets build in
> CI but nobody has sat in front of them. The part most worth knowing about is the
> silence gap-filler under [Wall-Clock Accuracy](#wall-clock-accuracy): it exists
> specifically for WASAPI behaviour and has only ever run on the platform it
> isn't for. If you hit something on Windows or Linux, please open an issue.

Using a CoreAudio tap rather than ScreenCaptureKit is the reason to do this natively instead of through Chromium's `getDisplayMedia`. Taps request the audio-only grant, leave the screen-recording indicator dark, and capture **pre-mixer** — a user with muted speakers still records clean audio.

## API

### `new SystemAudioBuffer(options?)`

| Option | Default | |
|---|---|---|
| `retentionSeconds` | `900` | History to keep |
| `targetSampleRate` | `16000` | Output rate |
| `ceilingSeconds` | `3600` | Allocation ceiling; retention can be raised to this later without reallocating |

### Methods

```ts
start(): void                              // begin capturing
stop(): void                               // stop and discard
freeze(): void                             // stop, keep contents
resume(): void                             // capture again, from empty
status(): BufferStatus
clearError(): void
setRetentionSeconds(n: number): void
read(startMs: number, endMs: number): Buffer        // mono 16-bit WAV
peaks(buckets: number): Float32Array                // [min, max] per bucket
permissionStatus(): 'granted' | 'denied' | 'unknown' | 'notRequired'
```

### `status()`

```ts
{
  state: 'idle' | 'running' | 'frozen',
  filledMs: number,            // wall-clock accurate
  retentionMs: number,
  sampleRate: number,
  silenceInsertedMs: number,   // synthesised to cover device gaps
  nativeSampleRate?: number,
  channels?: number,
  deviceName?: string,
  restarts: number,            // device faults recovered from
  error?: string               // latched
}
```

`read()` offsets run from the oldest audio held, so `0` is the left edge of a waveform drawn over the buffer. Out-of-range values clamp rather than throw. The result is a WAV container, not raw PCM, so format sniffers accept it and the bytes play in an `<audio>` element unchanged.

`resume()` clears the buffer rather than appending. Keeping the old contents would leave an invisible discontinuity mid-window, and every position in a waveform over it would be wrong.

## Permissions

Windows and Linux need none; `permissionStatus()` returns `notRequired`.

macOS gates process taps behind **System Audio Recording**, with no public API to request or check it. Per Apple's documentation:

1. The prompt fires by itself the first time capture starts.
2. **A denial is silent.** `start()` returns, no error is raised, and no frames arrive.

So the detection is: start, wait ~1s, check `filledMs`. If it is still zero, send the user to System Settings → Privacy & Security → Screen & System Audio Recording, via `x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture`.

Your app also needs `NSAudioCaptureUsageDescription` in its `Info.plist`.

> **Neither of those two points has been observed happening.** Both come from Apple's documentation and from [AudioCap](https://github.com/insidegui/AudioCap). Running from a terminal captures immediately with no prompt, because TCC attributes the request to the *responsible process* — the terminal — and a terminal holding Screen Recording appears to satisfy the tap. Verified: `TCCAccessPreflight` returns `0` for `kTCCServiceScreenCapture` and `2` for `kTCCServiceAudioCapture` on the same machine. TCC needs a distinct signed bundle identity to prompt about, so the prompt path is only reachable from a real packaged app.

### `tcc-preflight` feature

Optionally resolves `TCCAccessPreflight` from the private TCC framework to read the grant before starting. Off by default.

Measured on macOS 26.5: it returns `2` ("not determined") for `kTCCServiceAudioCapture` on a machine where capture demonstrably works. The symbol resolves; it just does not reflect the grant taps consult. Kept because it costs one `dlsym` and every failure path returns `unknown`, but do not build a UX that depends on it.

## Device Changes

Headphones get unplugged, docks disconnect, users switch outputs mid-session. A stream bound to a device that went away either errors or goes silent forever.

The worker supervises: on a stream fault or a change of default output device it reopens the stream, **keeping the buffer**, and rebuilds the resampler if the replacement runs at a different rate. `status().restarts` counts it. The interval between devices is filled with silence so positions stay correct.

## Wall-Clock Accuracy

WASAPI loopback delivers **no packets at all while the device is silent**. Left alone that drifts the buffer from the wall clock, and "the last 30 seconds" comes to mean 30 seconds of *sound* rather than 30 seconds of *elapsed time*.

The worker detects the shortfall and fills it with silence. `status().silenceInsertedMs` reports how much. macOS and Linux keep delivering silent frames, so it stays near zero there.

## Waveforms

`peaks(n)` returns `n * 2` floats — `[min, max]` per bucket, normalised to `-1..1`:

```js
const peaks = buf.peaks(600);
for (let b = 0; b < peaks.length; b += 2) {
  const [lo, hi] = [peaks[b], peaks[b + 1]];
  // draw a bar from lo to hi
}
```

Computed in Rust. Drawing from `read()` instead would ship the entire buffer across the boundary just to render a picture.

## Examples

```bash
node examples/permission-check.mjs   # probe the macOS permission path
node examples/record-clip.mjs 8      # record 8s, cut a clip, write clip.wav
```

`examples/electron-app` is a packaged Electron harness that exercises the whole API — record, freeze, drag a selection over the waveform, cut, play back, save. It also gives macOS a real bundle identity, which the CLI examples cannot.

```bash
cd examples/electron-app
pnpm install && pnpm run package && pnpm run open
```

## Development

```bash
pnpm install
pnpm run build              # release build for the host platform
pnpm run build:preflight    # with the TCC preflight feature
pnpm test                   # cargo test + node --test
pnpm run lint               # clippy, warnings denied
pnpm run audit              # cargo-deny
```

Requires Rust 1.85+. Linux also needs `libasound2-dev`, and `libpipewire-0.3-dev` for PipeWire.

## License

MIT
