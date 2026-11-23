#!/usr/bin/env bash
# Construye RiotKVM.app (firma ad-hoc) + riotkvm (CLI) y los empaqueta en un .dmg y un .tar.gz del CLI.
# Uso: packaging/macos/make-dmg.sh <version> <arch: x86_64|arm64> <dir con riotkvm y riotkvm-gui> <dir de salida>
set -euo pipefail
VERSION="$1"; ARCH="$2"; BIN="$3"; OUT="$4"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
STAGE="$(mktemp -d)"
APP="$STAGE/RiotKVM.app"

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT"
cp "$BIN/riotkvm-gui" "$APP/Contents/MacOS/riotkvm-gui"
cp "$HERE/RiotKVM.icns" "$APP/Contents/Resources/RiotKVM.icns"
sed "s/@VERSION@/$VERSION/g" "$HERE/Info.plist.in" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
chmod +x "$APP/Contents/MacOS/riotkvm-gui"

# Firma ad-hoc (sin Developer ID): macOS 13+ exige al menos esto en Apple Silicon para ejecutar. Gatekeeper avisará igualmente.
codesign --force --deep --sign - "$APP"
cp "$BIN/riotkvm" "$STAGE/riotkvm"
codesign --force --sign - "$STAGE/riotkvm"
cp "$REPO/client/README.md" "$STAGE/README.md"
cat > "$STAGE/LEEME-macOS.txt" <<'TXT'
RiotKVM sin Developer ID: macOS mostrará "no se puede abrir porque no se puede verificar al desarrollador".
 - Arrastra RiotKVM.app a Aplicaciones, clic derecho > Abrir > Abrir (una sola vez), o bien:
     xattr -cr /Applications/RiotKVM.app
 - La primera vez pedirá permiso de Cámara y de Bluetooth: acéptalos (Ajustes del Sistema > Privacidad y seguridad).
 - El CLI es el fichero "riotkvm" (cópialo a /usr/local/bin). Si lo ejecutas desde la Terminal y macOS pide Bluetooth, concédeselo a la Terminal.
   Si Gatekeeper lo bloquea: xattr -d com.apple.quarantine riotkvm
TXT
[ -f "$REPO/LICENSE" ] && cp "$REPO/LICENSE" "$STAGE/LICENSE"

DMG="$OUT/riotkvm-$VERSION-macos-$ARCH.dmg"
hdiutil create -volname "RiotKVM $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"

# CLI suelto
CLI="$OUT/riotkvm-cli-$VERSION-macos-$ARCH.tar.gz"
tar -C "$STAGE" -czf "$CLI" riotkvm README.md $( [ -f "$STAGE/LICENSE" ] && echo LICENSE )
echo "$DMG"; echo "$CLI"
