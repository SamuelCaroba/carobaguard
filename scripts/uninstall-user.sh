#!/usr/bin/env bash
set -euo pipefail

purge=false
case "${1:-}" in
  "") ;;
  --purge) purge=true ;;
  -h|--help)
    echo "Usage: $0 [--purge]"
    exit 0
    ;;
  *)
    echo "Unknown option: ${1:-}" >&2
    exit 2
    ;;
esac

if [[ -z "${HOME:-}" || "$HOME" != /* ]]; then
  echo "HOME must be an absolute path." >&2
  exit 1
fi

bin_dir="${CAROBAGUARD_INSTALL_BIN_DIR:-$HOME/.local/bin}"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
config_dir="$config_home/carobaguard"
data_dir="$data_home/carobaguard"
unit_file="$config_home/systemd/user/carobaguard.service"

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user disable --now carobaguard.service >/dev/null 2>&1 || true
fi
rm -f -- "$unit_file" "$bin_dir/carobaguard"
if command -v systemctl >/dev/null 2>&1; then
  systemctl --user daemon-reload >/dev/null 2>&1 || true
fi

echo "CarobaGuard service and binary removed."
if $purge; then
  for target in "$config_dir" "$data_dir"; do
    case "$target" in
      "$HOME"/*/carobaguard) rm -rf -- "$target" ;;
      *)
        echo "Refusing unsafe purge target: $target" >&2
        exit 1
        ;;
    esac
  done
  echo "Configuration and local data removed. This cannot be undone."
else
  echo "Configuration and data were preserved. Use --purge to remove them explicitly."
fi
