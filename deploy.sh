#!/bin/bash
set -euo pipefail

# Build, install, and restart the scrolltune service

overwrite_config=false
for arg in "$@"; do
    case "$arg" in
        --overwrite-config)
            overwrite_config=true
            ;;
        *)
            echo "Usage: $0 [--overwrite-config]" >&2
            exit 1
            ;;
    esac
done

cd "$(dirname "$0")"

echo "Building..."
cargo build --release

echo "Stopping service..."
sudo systemctl stop scrolltune 2>/dev/null || true

echo "Installing binary..."
sudo cp target/release/scrolltune /usr/local/bin/scrolltune

echo "Installing config..."
if [[ "$overwrite_config" == true ]]; then
    sudo cp scrolltune.toml /etc/scrolltune.toml
    echo "Overwriting config at /etc/scrolltune.toml"
elif [[ -e /etc/scrolltune.toml ]]; then
    echo "Preserving existing config at /etc/scrolltune.toml"
else
    sudo cp scrolltune.toml /etc/scrolltune.toml
    echo "Installing default config at /etc/scrolltune.toml"
fi

echo "Installing service..."
sudo cp -u scrolltune.service /etc/systemd/system/scrolltune.service
sudo systemctl daemon-reload

echo "Installing udev rule..."
sudo cp -u 99-scrolltune.rules /etc/udev/rules.d/99-scrolltune.rules
sudo udevadm control --reload-rules

echo "Starting service..."
sudo systemctl enable --now scrolltune

echo "Done."
sudo systemctl status scrolltune --no-pager | head -10
