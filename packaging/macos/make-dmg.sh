#!/usr/bin/env bash
# Construye RiotKVM.app (firma ad-hoc) + dkvm (CLI) + riotkvm-mcp (servidor MCP) y los empaqueta en un .dmg y un .tar.gz
# con los dos binarios de terminal.
# Uso: packaging/macos/make-dmg.sh <version> <arch: x86_64|arm64> <dir con dkvm, dkvm-gui y riotkvm-mcp> <dir de salida>
set -euo pipefail
VERSION="$1"; ARCH="$2"; BIN="$3"; OUT="$4"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
STAGE="$(mktemp -d)"
APP="$STAGE/RiotKVM.app"

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT"
cp "$BIN/dkvm-gui" "$APP/Contents/MacOS/dkvm-gui"
cp "$HERE/RiotKVM.icns" "$APP/Contents/Resources/RiotKVM.icns"
sed "s/@VERSION@/$VERSION/g" "$HERE/Info.plist.in" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
chmod +x "$APP/Contents/MacOS/dkvm-gui"

# Firma ad-hoc (sin Developer ID): macOS 13+ exige al menos esto en Apple Silicon para ejecutar. Gatekeeper avisará igualmente.
codesign --force --deep --sign - "$APP"
cp "$BIN/dkvm" "$STAGE/dkvm"
codesign --force --sign - "$STAGE/dkvm"
cp "$BIN/riotkvm-mcp" "$STAGE/riotkvm-mcp"
codesign --force --sign - "$STAGE/riotkvm-mcp"
cp "$REPO/client/README.md" "$STAGE/README.md"
cat > "$STAGE/LEEME-macOS.txt" <<'TXT'
RiotKVM sin Developer ID: macOS mostrará "no se puede abrir porque no se puede verificar al desarrollador".
 - Arrastra RiotKVM.app a Aplicaciones, clic derecho > Abrir > Abrir (una sola vez), o bien:
     xattr -cr /Applications/RiotKVM.app
 - La primera vez pedirá permiso de Cámara y de Bluetooth: acéptalos (Ajustes del Sistema > Privacidad y seguridad).
 - El CLI es el fichero "dkvm" (cópialo a /usr/local/bin). Si lo ejecutas desde la Terminal y macOS pide Bluetooth, concédeselo a la Terminal.
   Si Gatekeeper lo bloquea: xattr -d com.apple.quarantine dkvm
 - "riotkvm-mcp" es el servidor MCP para LLMs (cópialo a /usr/local/bin):
     claude mcp add riotkvm -- /usr/local/bin/riotkvm-mcp
   Necesita los mismos permisos (Bluetooth y Cámara) para la app que lo lanza (Terminal, Claude...).
TXT
[ -f "$REPO/LICENSE" ] && cp "$REPO/LICENSE" "$STAGE/LICENSE"

DMG="$OUT/quim-dkvm-$VERSION-macos-$ARCH.dmg"
hdiutil create -volname "RiotKVM $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"

# CLI suelto
CLI="$OUT/quim-dkvm-cli-$VERSION-macos-$ARCH.tar.gz"
tar -C "$STAGE" -czf "$CLI" dkvm riotkvm-mcp README.md $( [ -f "$STAGE/LICENSE" ] && echo LICENSE )
echo "$DMG"; echo "$CLI"
