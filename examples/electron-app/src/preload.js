const { contextBridge, ipcRenderer } = require('electron');

// Every call returns { ok, value } | { ok: false, error } from main; unwrap here
// so the renderer sees a normal promise rejection with the real message.
const call = async (channel, ...args) => {
  const res = await ipcRenderer.invoke(channel, ...args);
  if (!res.ok) throw new Error(res.error);
  return res.value;
};

contextBridge.exposeInMainWorld('audio', {
  start: () => call('buffer:start'),
  stop: () => call('buffer:stop'),
  freeze: () => call('buffer:freeze'),
  resume: () => call('buffer:resume'),
  status: () => call('buffer:status'),
  clearError: () => call('buffer:clearError'),
  setRetention: (seconds) => call('buffer:setRetention', seconds),
  peaks: (buckets) => call('buffer:peaks', buckets),
  read: (startMs, endMs) => call('buffer:read', startMs, endMs),
  permissionStatus: () => call('permission:status'),
  openSettings: () => call('permission:openSettings'),
  appInfo: () => call('app:info'),
  saveWav: (bytes) => call('app:saveWav', bytes),
});
