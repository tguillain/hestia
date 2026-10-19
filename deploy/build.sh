#!/usr/bin/env bash
# Compile Hestia pour le Raspberry Pi (ARM 64 bits, statique) et le signe pour la mise à
# jour depuis la page Réglages. Produit deploy/hestia et deploy/hestia.sig.
set -euo pipefail
cd "$(dirname "$0")/.."

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
export HESTIA_VERSION="${version}+$(date +%Y%m%d.%H%M)"

cargo build --release -p hestia --target aarch64-unknown-linux-musl
cargo build --release -p hestia
cp target/aarch64-unknown-linux-musl/release/hestia deploy/hestia

host=target/release/hestia
[[ -x "$host.exe" ]] && host="$host.exe"
"$host" release sign deploy/hestia
echo "Version $HESTIA_VERSION prête : deploy/hestia et deploy/hestia.sig"
