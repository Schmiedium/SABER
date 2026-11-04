#!/usr/bin/env bash
# Rebuild the studio's symbol font, crates/sim_studio_ui/assets/fonts/DejaVuSans-Symbols.ttf.
#
# Feathers' Fira Sans and Fira Mono have no glyphs for most symbols the studio writes
# (✓ ✖ ⚠ ◐ ☰ ▸ ⚙ ℹ ...). The kit falls back to this font for them (`sim_studio_ui::fonts`).
# It is DejaVu Sans 2.37 cut down to the symbol and arrow blocks below, without hinting.
# License: crates/sim_studio_ui/assets/fonts/DejaVu-LICENSE.txt (Bitstream Vera and Arev terms
# with DejaVu's public-domain changes; modified copies keep a name without "Bitstream" or "Vera").
#
# Needs fontTools (`apt-get install python3-fonttools`) and DejaVu Sans 2.37 (`fonts-dejavu-core`).
# Usage: scripts/make_symbol_font.sh [path/to/DejaVuSans.ttf]
set -euo pipefail

src="${1:-/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf}"
out="crates/sim_studio_ui/assets/fonts/DejaVuSans-Symbols.ttf"

# Letterlike symbols, arrows, mathematical operators, miscellaneous technical, box drawing, block
# elements, geometric shapes, miscellaneous symbols, dingbats, supplemental arrows A and B,
# miscellaneous symbols and arrows.
blocks="U+2100-214F,U+2190-21FF,U+2200-22FF,U+2300-23FF,U+2500-257F,U+2580-259F,U+25A0-25FF"
blocks="$blocks,U+2600-26FF,U+2700-27BF,U+27F0-27FF,U+2900-297F,U+2B00-2BFF"

python3 -m fontTools.subset "$src" \
  --unicodes="$blocks" \
  --layout-features='*' \
  --name-IDs='*' \
  --no-hinting \
  --output-file="$out"
echo "wrote $out ($(wc -c <"$out") bytes) from $src ($(sha256sum "$src" | cut -d' ' -f1))"
