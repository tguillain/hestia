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

# Le programme est essayé sur le Pi avant d'être publié : une version qui n'y démarre pas
# (instructions que le processeur n'a pas, par exemple) n'arrive jamais dans Réglages.
# HESTIA_SMOKE_HOST choisit le Pi, HESTIA_SMOKE=0 saute le test.
smoke_host="${HESTIA_SMOKE_HOST:-xakonekt@hestia.local}"
if [[ "${HESTIA_SMOKE:-1}" == 0 ]]; then
  echo "Test sur le Pi sauté (HESTIA_SMOKE=0)."
elif ssh -o BatchMode=yes -o ConnectTimeout=5 "$smoke_host" true 2>/dev/null; then
  echo "Test de démarrage sur $smoke_host…"
  remote="$(ssh -o BatchMode=yes "$smoke_host" 'mktemp -d /tmp/hestia-smoke.XXXXXX')"
  scp -q -o BatchMode=yes deploy/hestia "$smoke_host:$remote/hestia"
  if ! ssh -o BatchMode=yes "$smoke_host" bash -s "$remote" <<'SMOKE'
set -u
cd "$1"
printf 'data_dir = "data"\n[http]\nlisten = "127.0.0.1:0"\n[homekit]\nenabled = false\n[update]\ngithub = ""\n' > test.toml
chmod +x hestia
timeout 15 ./hestia -c test.toml serve > run.log 2>&1
status=$?
ok=true
# 124: still running when the 15 s were up, which is what a healthy start looks like.
[[ $status == 124 ]] || { echo "Le programme s'est arrêté (code $status)."; ok=false; }
grep -q 'hestia started' run.log || { echo "Le serveur n'a pas démarré."; ok=false; }
grep -q 'Matter controller ready' run.log || { echo "Le pilote Matter n'a pas démarré."; ok=false; }
$ok || tail -n 20 run.log
cd /tmp && [[ "$1" == /tmp/hestia-smoke.* ]] && rm -r -- "$1"
$ok
SMOKE
  then
    echo "Publication annulée : la version ne démarre pas sur le Pi." >&2
    exit 1
  fi
  echo "La version démarre sur le Pi."
else
  echo "Attention : $smoke_host injoignable en SSH sans mot de passe, version publiée sans test sur le Pi." >&2
fi

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
