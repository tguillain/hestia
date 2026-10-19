#!/usr/bin/env bash
# Publie une version de Hestia sur GitHub : compile et signe (build.sh), puis crée la
# release avec hestia-aarch64 et hestia-aarch64.sig. Le Pi la propose ensuite dans
# Réglages > Mise à jour et via l'outil MCP install_update.
#   deploy/release.sh "Ce qui change dans cette version"
set -euo pipefail
cd "$(dirname "$0")/.."

notes="${1:-}"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "Des modifications ne sont pas commitées : commite-les avant de publier." >&2
  exit 1
fi
git fetch --quiet origin
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse '@{upstream}' 2>/dev/null)" ]]; then
  echo "Le commit actuel n'est pas poussé sur GitHub : fais git push avant de publier." >&2
  exit 1
fi

gh=gh
if ! command -v gh >/dev/null 2>&1; then
  gh="/c/Program Files/GitHub CLI/gh.exe"
fi

deploy/build.sh

host=target/release/hestia
[[ -x "$host.exe" ]] && host="$host.exe"
version="$("$host" --version | tr -d '\r' | sed 's/^hestia //')"
tag="v${version/+/-}"

assets=target/release-assets
rm -rf "$assets"
mkdir -p "$assets"
cp deploy/hestia "$assets/hestia-aarch64"
cp deploy/hestia.sig "$assets/hestia-aarch64.sig"

"$gh" release create "$tag" "$assets/hestia-aarch64" "$assets/hestia-aarch64.sig" \
  --title "Hestia $version" \
  --notes "${notes:-Version $version.}" \
  --target "$(git rev-parse HEAD)"
echo "Version $version publiée ($tag)."
