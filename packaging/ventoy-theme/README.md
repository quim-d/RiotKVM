# Tema QKVM para Ventoy

Generado por `tools/gen_ventoy_theme.py` (no editar a mano). En un contenedor Debian con `python3-pil`,
`grub-common` y `fonts-jetbrains-mono`:

    docker run --rm -v "$PWD":/w -w /w debian:trixie-slim sh -c \
      'apt-get update -qq && apt-get install -y -qq python3-pil grub-common fonts-jetbrains-mono >/dev/null && python3 tools/gen_ventoy_theme.py --fonts'

- `qkvm/theme_<WxH>_<n>.txt` + `bg_<WxH>_<n>.png`: maquetas a 1920x1080 y 1024x768, una por cita del pie. Ventoy
  (`"resolution_fit": 1`) se queda con las de la resolución activa y elige una al azar en cada arranque. La
  1ª línea (`ventoy_left_top_color`) coloca la versión de Ventoy en gris sin pisar la línea de atajos.
- `qkvm/icons/qkvm-rescate.png`: icono de lo que haya en `/Tools`.
- `fonts/`: JetBrains Mono Regular/Bold en 16 y 24 px convertida a `.pf2` con `grub-mkfont` (ASCII, Latin-1/Latin
  Extended-A, puntuación, flechas, cajas y bloques). Licencia: SIL Open Font License 1.1 (`fonts/OFL.txt`), que
  permite redistribuirla embebida; el nombre «JetBrains Mono» no se usa para la versión convertida.
- `dkvm-flash` lo embebe (build.rs) y lo escribe en `/ventoy/theme/` de la SD (este README no se copia).
