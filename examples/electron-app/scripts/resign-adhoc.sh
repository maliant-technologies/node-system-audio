#!/usr/bin/env bash
# Re-sign the packaged bundle ad-hoc, inside-out.
#
# WHY THIS EXISTS
#
# The packager's default is a linker-signed ad-hoc signature whose *signing
# identifier* is "Electron" rather than the bundle id. TCC keys grants on the
# signing identity, so a permission granted to one build may not be recognised
# as belonging to the next.
#
# Setting `osxSign` in forge.config.js does NOT fix this — it re-signs the outer
# executable while leaving the Electron Framework on its original signature, and
# macOS then refuses to load them together:
#
#   Library not loaded: @rpath/Electron Framework.framework/Electron Framework
#   ... mapping process and mapped file (non-platform) have different Team IDs
#
# Signing has to happen inside-out: every nested framework, helper app and dylib
# first, then the outer bundle last, all with the same identity.
#
# A real product would sign with a Developer ID certificate and notarise, which
# @electron/osx-sign handles properly. This script exists only so the example can
# present a stable identity to TCC without one.
#
#   pnpm run package && pnpm run resign

set -euo pipefail

APP="${1:-out/SystemAudioExample-darwin-arm64/SystemAudioExample.app}"
BUNDLE_ID="dev.nodesystemaudio.example"

if [[ ! -d "$APP" ]]; then
  echo "no bundle at $APP — run 'pnpm run package' first" >&2
  exit 1
fi

echo "re-signing $APP"

# Nested code first, deepest last-modified order doesn't matter as long as
# containers are signed after their contents.
while IFS= read -r item; do
  codesign --force --sign - --timestamp=none "$item" 2>/dev/null || true
done < <(find "$APP/Contents/Frameworks" -depth \( -name "*.dylib" -o -name "*.node" -o -name "*.framework" -o -name "*.app" \) 2>/dev/null)

# Anything unpacked out of the asar (our addon lives here).
while IFS= read -r item; do
  codesign --force --sign - --timestamp=none "$item" 2>/dev/null || true
done < <(find "$APP/Contents/Resources" -name "*.node" 2>/dev/null)

# The outer bundle last, carrying the identifier TCC will see.
codesign --force --sign - --timestamp=none --identifier "$BUNDLE_ID" "$APP"

echo
codesign -dv "$APP" 2>&1 | grep -E "Identifier|Signature" || true
echo
echo "launch it and confirm it still starts — if dyld complains about Team IDs,"
echo "something in the bundle was missed and the default packaging is the safer bet."
