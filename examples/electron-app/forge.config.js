module.exports = {
  packagerConfig: {
    // The bundle identifier is the whole point of this harness. TCC keys grants
    // on it, so packaging under a distinct id gives macOS a fresh identity with
    // no inherited permission — which is what makes the prompt appear here and
    // not when running the same code from a terminal.
    appBundleId: 'dev.nodesystemaudio.example',

    // Native .node binaries cannot be loaded from inside an asar; they get
    // extracted to app.asar.unpacked at the same relative path.
    asar: {
      unpack: '**/*.node',
    },

    // pnpm links `file:` dependencies, and a symlink is not a file the packager
    // can copy into the bundle.
    derefSymlinks: true,

    // NOT setting `osxSign`. @electron/osx-sign re-signs the outer executable
    // but leaves the Electron Framework on its original signature, and macOS
    // then refuses to map the two together:
    //
    //   Library not loaded: @rpath/Electron Framework.framework/Electron Framework
    //   ... mapping process and mapped file (non-platform) have different Team IDs
    //
    // The packager's default linker-signed ad-hoc signature is internally
    // consistent across the bundle, so the app actually launches. See the
    // `resign` script in package.json for making the signing identifier match
    // the bundle id, which has to be done inside-out over the whole bundle.
    extendInfo: {
      // Required for system audio capture on macOS 14.2+. This string is what
      // the permission dialog shows. Without the key the OS has nothing to
      // present and capture cannot be granted.
      NSAudioCaptureUsageDescription:
        'This example records system audio so you can verify loopback capture works.',
      NSMicrophoneUsageDescription:
        'Not used by this example — present only to keep the audio entitlement set complete.',
    },
  },

  // Rebuilds/unpacks native modules found in the dependency tree.
  plugins: [
    {
      name: '@electron-forge/plugin-auto-unpack-natives',
      config: {},
    },
  ],
};
