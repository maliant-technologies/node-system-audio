// Exercises the macOS System Audio Recording permission path.
//
// This lives here because it cannot be tested from the consuming app's repo in
// isolation, and it cannot be asserted in a unit test either: the TCC prompt is
// a one-time, machine-global, user-driven event.
//
// Apple documents that a *denial is silent*: the stream opens, play() succeeds,
// no error is raised — and no frames arrive. Any consumer that treats "start()
// resolved" as "we are recording" is wrong on macOS, which is why the check
// below is written the way it is.
//
// CAVEAT WORTH KNOWING BEFORE YOU TRUST THE OUTPUT: run from a terminal, this
// usually reports success without ever prompting. TCC attributes a request to
// the *responsible process*, which for a CLI tool is the terminal application
// rather than node — and a terminal that already holds Screen Recording appears
// to satisfy the tap. So a pass here says the capture chain works; it does not
// say the permission flow was tested. That needs a signed bundle with its own
// identifier, i.e. the real application.
//
//   node examples/permission-check.mjs

import { SystemAudioBuffer, permissionStatus } from '../index.js';

const PROBE_MS = 1500;
const SETTINGS_DEEP_LINK =
  'x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

console.log(`platform         ${process.platform} (${process.arch})`);
console.log(`reported status  ${permissionStatus()}`);

if (permissionStatus() === 'notRequired') {
  console.log('\nLoopback needs no permission here. Nothing further to check.');
  process.exit(0);
}

if (permissionStatus() === 'unknown') {
  console.log(
    '\nBuilt without the `tcc-preflight` feature, so the grant cannot be read\n' +
      'ahead of time. Rebuild with `pnpm run build:preflight` to compare.\n' +
      'Probing by capture instead — this is the path production uses.',
  );
}

const buf = new SystemAudioBuffer({ retentionSeconds: 10, ceilingSeconds: 10 });

console.log(`\nStarting capture. If macOS has not asked before, it prompts now.`);
try {
  buf.start();
} catch (e) {
  console.error(`\nFAILED to open the device: ${e.message}`);
  console.error('That is a device problem, not a permission problem.');
  process.exit(1);
}

const running = buf.status();
console.log(`device           ${running.deviceName}`);
console.log(`native rate      ${running.nativeSampleRate} Hz, ${running.channels}ch`);
console.log(`\nProbing for ${PROBE_MS}ms — play something audible to be sure.`);

await sleep(PROBE_MS);

const s = buf.status();
buf.stop();

console.log(`\nfilledMs         ${s.filledMs}`);
console.log(`silenceInserted  ${s.silenceInsertedMs}ms`);
if (s.error) console.log(`stream error     ${s.error}`);

if (s.filledMs === 0) {
  console.log(`
RESULT  no frames arrived — permission is NOT granted.

Note that nothing threw. This is exactly the failure production has to detect,
and the only reliable signal is this one: filledMs stayed at zero.

  System Settings → Privacy & Security → Screen & System Audio Recording

  ${SETTINGS_DEEP_LINK}
`);
  process.exit(2);
}

const real = s.filledMs - s.silenceInsertedMs;
console.log(`
RESULT  capturing — ${s.filledMs}ms held, ${real}ms of it real device frames.

If silenceInserted is most of it, capture works but nothing was playing. The gap
filler stands in for undelivered frames so the window still tracks the wall
clock; on macOS it should stay near zero, because CoreAudio keeps delivering
silent frames rather than stopping.
`);
