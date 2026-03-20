#!/bin/bash
set -euo pipefail

# Build, install, and restart the scrolltune service

cd "$(dirname "$0")"

echo "Building..."
cargo build --release

echo "Stopping service..."
sudo systemctl stop scrolltune 2>/dev/null || true

echo "Installing binary..."
sudo cp target/release/scrolltune /usr/local/bin/scrolltune

echo "Installing config..."
sudo cp scrolltune.toml /etc/scrolltune.toml

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
