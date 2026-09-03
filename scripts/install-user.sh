#!/usr/bin/env bash
set -euo pipefail

skip_build=false
skip_start=false
for argument in "$@"; do
  case "$argument" in
    --skip-build) skip_build=true ;;
    --no-start) skip_start=true ;;
    -h|--help)
      echo "Usage: $0 [--skip-build] [--no-start]"
      exit 0
      ;;
    *)
      echo "Unknown option: $argument" >&2
      exit 2
      ;;
  esac
done

if [[ -z "${HOME:-}" || "$HOME" != /* ]]; then
  echo "HOME must be an absolute path." >&2
  exit 1
fi
if [[ "$HOME" =~ [[:space:]\\\"] ]]; then
  echo "HOME paths containing whitespace, backslashes, or quotes are not supported." >&2
  exit 1
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
bin_dir="${CAROBAGUARD_INSTALL_BIN_DIR:-$HOME/.local/bin}"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
config_dir="$config_home/carobaguard"
data_dir="$data_home/carobaguard"
unit_dir="$config_home/systemd/user"
env_file="$config_dir/carobaguard.env"
unit_file="$unit_dir/carobaguard.service"
binary="$repo_dir/target/release/carobaguard"

for install_path in "$bin_dir" "$config_home" "$data_home"; do
  if [[ "$install_path" != /* || "$install_path" =~ [[:space:]\\\"] ]]; then
    echo "Install paths must be absolute and cannot contain whitespace, backslashes, or quotes: $install_path" >&2
    exit 1
  fi
done

if ! $skip_build; then
  command -v cargo >/dev/null 2>&1 || {
    echo "Rust/Cargo was not found. Install Rust 1.85+ first." >&2
    exit 1
  }
  echo "Building CarobaGuard release..."
  cargo build --locked --release --manifest-path "$repo_dir/Cargo.toml"
fi

if [[ ! -x "$binary" ]]; then
  echo "Release binary not found at $binary." >&2
  exit 1
fi

umask 077
mkdir -p -- "$bin_dir" "$config_dir" "$data_dir" "$unit_dir"
install -m 0755 -- "$binary" "$bin_dir/carobaguard"

new_password=""
if [[ ! -e "$env_file" ]]; then
  if command -v openssl >/dev/null 2>&1; then
    new_password="$(openssl rand -hex 18)"
  else
    new_password="$(od -An -N18 -tx1 /dev/urandom | tr -d ' \n')"
  fi
  {
    printf 'CAROBAGUARD_HOST="127.0.0.1"\n'
    printf 'CAROBAGUARD_PORT="8090"\n'
    printf 'CAROBAGUARD_DATA_DIR="%s"\n' "$data_dir"
    printf 'CAROBAGUARD_ADMIN_USERNAME="admin"\n'
    printf 'CAROBAGUARD_ADMIN_PASSWORD="%s"\n' "$new_password"
    printf 'CAROBAGUARD_COOKIE_SECURE="false"\n'
  } >"$env_file"
  chmod 0600 "$env_file"
fi

cat >"$unit_file" <<EOF
[Unit]
Description=CarobaGuard lightweight Linux control plane
Documentation=https://github.com/SamuelCaroba/carobaguard
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=$env_file
Environment="PATH=$HOME/.opencode/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"
ExecStart="$bin_dir/carobaguard"
WorkingDirectory=$HOME
Restart=on-failure
RestartSec=3
TimeoutStopSec=20
KillMode=control-group
NoNewPrivileges=true
PrivateTmp=true
UMask=0077

[Install]
WantedBy=default.target
EOF
chmod 0644 "$unit_file"

started=false
if ! $skip_start && command -v systemctl >/dev/null 2>&1; then
  if systemctl --user daemon-reload \
    && systemctl --user enable carobaguard.service >/dev/null \
    && systemctl --user restart carobaguard.service; then
    started=true
  fi
fi

echo
echo "CarobaGuard installed at $bin_dir/carobaguard"
echo "Configuration: $env_file"
if [[ -n "$new_password" ]]; then
  echo "Initial username: admin"
  echo "Initial password: $new_password"
  echo "Store it now; this message is not shown on upgrades."
fi
if $started; then
  echo "Service started. Open http://127.0.0.1:8090"
else
  echo "Service was not started. Run:"
  echo "  systemctl --user daemon-reload && systemctl --user enable --now carobaguard"
  echo "Or run manually:"
  echo "  set -a; source '$env_file'; set +a; exec '$bin_dir/carobaguard'"
fi
if ! command -v opencode >/dev/null 2>&1; then
  echo "OpenCode was not found; the dashboard works, but AI features remain unavailable."
fi
