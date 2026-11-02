#!/usr/bin/env bash
# Enforces the dependency rules in docs/design/subsystems-and-interfaces.md §4.
# Each rule: <crate> must not reach <forbidden> through normal dependencies.
set -euo pipefail

fail=0
forbid() {
  local crate="$1" forbidden="$2" why="$3"
  if cargo tree -p "$crate" -e normal --prefix none --format '{p}' 2>/dev/null \
      | awk '{print $1}' | grep -qx "$forbidden"; then
    echo "::error::$crate must not depend on $forbidden ($why)"
    fail=1
  fi
}

forbid sim_models   sim_verify "models emit signals; verification observes them"
for c in sim_hpc sim_explorer sim_models sim_query; do
  forbid "$c" sim_assets "no glTF or image decoding outside the studio and saber-cli (D17)"
done
forbid sim_explorer sim_core      "the data side builds without the kernel or Bevy (§5.9)"
forbid sim_query    sim_core      "the data side builds without the kernel or Bevy (§5.9)"
for c in sim_schema sim_core sim_models sim_verify; do
  for gui in sim_studio sim_studio_ui sim_explorer sim_hpc sim_query; do
    forbid "$c" "$gui" "the four core libraries never depend on GUI, MPI or query crates"
  done
done

if [ "$fail" -ne 0 ]; then exit 1; fi
echo "dependency rules OK"
