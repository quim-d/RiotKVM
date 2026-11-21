#!/usr/bin/env bash
# Construye QKVM.app (firma ad-hoc) + dkvm (CLI) y los empaqueta en un .dmg y un .tar.gz del CLI.
# Uso: packaging/macos/make-dmg.sh <version> <arch: x86_64|arm64> <dir con dkvm y dkvm-gui> <dir de salida>
set -euo pipefail
VERSION="$1"; ARCH="$2"; BIN="$3"; OUT="$4"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
STAGE="$(mktemp -d)"
APP="$STAGE/QKVM.app"

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT"
cp "$BIN/dkvm-gui" "$APP/Contents/MacOS/dkvm-gui"
sed "s/@VERSION@/$VERSION/g" "$HERE/Info.plist.in" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
chmod +x "$APP/Contents/MacOS/dkvm-gui"

# Firma ad-hoc (sin Developer ID): macOS 13+ exige al menos esto en Apple Silicon para ejecutar. Gatekeeper avisará igualmente.
codesign --force --deep --sign - "$APP"
cp "$BIN/dkvm" "$STAGE/dkvm"
codesign --force --sign - "$STAGE/dkvm"
cp "$REPO/client/README.md" "$STAGE/README.md"
cat > "$STAGE/LEEME-macOS.txt" <<'TXT'
QKVM sin Developer ID: macOS mostrará "no se puede abrir porque no se puede verificar al desarrollador".
 - Arrastra QKVM.app a Aplicaciones, clic derecho > Abrir > Abrir (una sola vez), o bien:
     xattr -cr /Applications/QKVM.app
 - La primera vez pedirá permiso de Cámara y de Bluetooth: acéptalos (Ajustes del Sistema > Privacidad y seguridad).
 - El CLI es el fichero "dkvm" (cópialo a /usr/local/bin). Si lo ejecutas desde la Terminal y macOS pide Bluetooth, concédeselo a la Terminal.
   Si Gatekeeper lo bloquea: xattr -d com.apple.quarantine dkvm
TXT
[ -f "$REPO/LICENSE" ] && cp "$REPO/LICENSE" "$STAGE/LICENSE"

DMG="$OUT/quim-dkvm-$VERSION-macos-$ARCH.dmg"
hdiutil create -volname "QKVM $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"

# CLI suelto
CLI="$OUT/quim-dkvm-cli-$VERSION-macos-$ARCH.tar.gz"
tar -C "$STAGE" -czf "$CLI" dkvm README.md $( [ -f "$STAGE/LICENSE" ] && echo LICENSE )
echo "$DMG"; echo "$CLI"
