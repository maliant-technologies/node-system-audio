// Main process. The buffer lives here — not in a renderer — which is the point:
// it survives every window closing, and it never needs Chromium's loopback path
// (and therefore never asks macOS for the Screen Recording permission).

const { app, BrowserWindow, ipcMain, shell, dialog } = require('electron');
const path = require('node:path');

const { SystemAudioBuffer, permissionStatus } = require('node-system-audio');

const SETTINGS_PANE =
  'x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture';

/** @type {SystemAudioBuffer | null} */
let buffer = null;

function getBuffer() {
  if (!buffer) {
    buffer = new SystemAudioBuffer({ retentionSeconds: 60, ceilingSeconds: 600 });
  }
  return buffer;
}

function createWindow() {
  const win = new BrowserWindow({
    width: 1100,
    height: 820,
    title: 'node-system-audio',
    backgroundColor: '#0f1115',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  win.loadFile(path.join(__dirname, 'renderer', 'index.html'));
}

app.whenReady().then(() => {
  createWindow();
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

// Deliberately does NOT quit on macOS when windows close — mirroring the
// tray-resident shape a real consumer needs, where a running buffer outlives
// every window.
app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});

app.on('before-quit', () => {
  try {
    buffer?.stop();
  } catch {
    /* already stopped */
  }
});

// Errors are returned rather than thrown so the renderer can display the exact
// message instead of a generic "Error invoking remote method".
const handle = (channel, fn) =>
  ipcMain.handle(channel, async (_event, ...args) => {
    try {
      return { ok: true, value: await fn(...args) };
    } catch (e) {
      return { ok: false, error: e instanceof Error ? e.message : String(e) };
    }
  });

handle('buffer:start', () => {
  getBuffer().start();
  return getBuffer().status();
});

handle('buffer:stop', () => {
  getBuffer().stop();
  return getBuffer().status();
});

handle('buffer:freeze', () => {
  getBuffer().freeze();
  return getBuffer().status();
});

handle('buffer:resume', () => {
  getBuffer().resume();
  return getBuffer().status();
});

handle('buffer:status', () => getBuffer().status());

handle('buffer:clearError', () => {
  getBuffer().clearError();
  return getBuffer().status();
});

handle('buffer:setRetention', (seconds) => {
  getBuffer().setRetentionSeconds(seconds);
  return getBuffer().status();
});

handle('buffer:peaks', (buckets) => {
  // Float32Array does not survive structured clone through IPC as itself in all
  // Electron versions; a plain array is small enough at these bucket counts.
  return Array.from(getBuffer().peaks(buckets));
});

handle('buffer:read', (startMs, endMs) => {
  const wav = getBuffer().read(startMs, endMs);
  // Hand over a plain Uint8Array so the renderer can Blob it directly.
  return new Uint8Array(wav);
});

handle('permission:status', () => permissionStatus());

handle('permission:openSettings', async () => {
  if (process.platform !== 'darwin') return false;
  await shell.openExternal(SETTINGS_PANE);
  return true;
});

handle('app:info', () => ({
  platform: process.platform,
  arch: process.arch,
  electron: process.versions.electron,
  node: process.versions.node,
  bundleId: app.getName(),
  packaged: app.isPackaged,
}));

handle('app:saveWav', async (bytes) => {
  const { canceled, filePath } = await dialog.showSaveDialog({
    defaultPath: 'clip.wav',
    filters: [{ name: 'WAV', extensions: ['wav'] }],
  });
  if (canceled || !filePath) return null;
  require('node:fs').writeFileSync(filePath, Buffer.from(bytes));
  return filePath;
});
