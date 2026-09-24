#!/usr/bin/env bash
# Spike S4 teardown: remove the test key from the Windows agent, unregister the
# throwaway distro, and report what is left. Usage: ./teardown-target.sh <work-dir>
set -uo pipefail

WORK="${1:?usage: teardown-target.sh <work-dir>}"
DISTRO="amiri-s4-sshd"

echo "== remove test key from the Windows ssh-agent"
if [ -f "$WORK/s4_key.pub" ]; then
  ssh-add -d "$(cygpath -w "$WORK/s4_key.pub")" 2>&1 || true
fi
KEY_FPR=$(ssh-keygen -lf "$WORK/s4_key.pub" 2>/dev/null | awk '{print $2}')
if [ -n "$KEY_FPR" ] && ssh-add -l 2>/dev/null | grep -q "$KEY_FPR"; then
  echo "agent_still_has_test_key=true"
else
  echo "agent_still_has_test_key=false"
fi

echo "== unregister $DISTRO"
wsl.exe --terminate "$DISTRO" >/dev/null 2>&1 || true
wsl.exe --unregister "$DISTRO"
echo "unregister_exit=$?"
if wsl.exe -l -q 2>/dev/null | tr -d '\0\r' | grep -qx "$DISTRO"; then
  echo "distro_still_listed=true"
else
  echo "distro_still_listed=false"
fi
echo "vhdx_left=$(ls "$WORK/distro" 2>/dev/null | wc -l)"
