#!/usr/bin/env bash
# Installe ou met à jour Hestia sur Raspberry Pi OS 64 bits.
# Usage : sudo bash install.sh   (depuis le dossier qui contient ce script)
# Relançable sans risque : la configuration et les données existantes sont conservées.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"

if [[ $EUID -ne 0 ]]; then
  echo "Lance ce script avec sudo : sudo bash $0" >&2
  exit 1
fi
if [[ "$(uname -m)" != "aarch64" ]]; then
  echo "Il faut Raspberry Pi OS 64 bits (aarch64), trouvé : $(uname -m)" >&2
  exit 1
fi
for file in hestia launch hestia.service hestia.example.toml Caddyfile.example; do
  [[ -f "$here/$file" ]] || { echo "Fichier manquant : $here/$file" >&2; exit 1; }
done

# Your own hestia.toml / Caddyfile, when present next to this script, win over the examples.
pick() {
  if [[ -f "$here/$1" ]]; then echo "$here/$1"; else echo "$here/$2"; fi
}
config_source="$(pick hestia.toml hestia.example.toml)"
caddy_source="$(pick Caddyfile Caddyfile.example)"

echo "== Utilisateur et dossiers"
id hestia &>/dev/null || useradd --system --home-dir /var/lib/hestia --shell /usr/sbin/nologin hestia
install -d -o hestia -g hestia -m 0750 /var/lib/hestia
install -d -m 0755 /etc/hestia

echo "== Programme"
install -d -m 0755 /usr/local/lib/hestia
install -m 0755 "$here/hestia" /usr/local/lib/hestia/hestia
install -m 0755 "$here/launch" /usr/local/lib/hestia/launch
/usr/local/lib/hestia/hestia --version
# A version installed from the Settings page (or from GitHub) stays active when it is more
# recent than this folder's; an older one is set aside as "previous" for rolling back.
if [[ -x /var/lib/hestia/releases/hestia ]]; then
  packaged="$(/usr/local/lib/hestia/hestia --version | sed 's/^hestia //')"
  updated="$(/var/lib/hestia/releases/hestia --version 2>/dev/null | sed 's/^hestia //' || true)"
  newest="$(printf '%s\n%s\n' "$packaged" "$updated" | sort -V | tail -n 1)"
  if [[ -n "$updated" && "$updated" != "$packaged" && "$newest" == "$updated" ]]; then
    echo "La version $updated installée depuis Réglages est plus récente : elle reste active."
  else
    mv -f /var/lib/hestia/releases/hestia /var/lib/hestia/releases/hestia.previous
  fi
fi

echo "== Configuration"
if [[ -f /etc/hestia/hestia.toml ]]; then
  echo "/etc/hestia/hestia.toml existe déjà : conservé"
else
  install -m 0644 "$config_source" /etc/hestia/hestia.toml
  if [[ "$config_source" == *example* ]]; then
    echo "Configuration d'exemple installée : adapte /etc/hestia/hestia.toml à ta maison."
  fi
fi

echo "== Commande « hestia »"
cat > /usr/local/bin/hestia <<'WRAPPER'
#!/bin/sh
# Lance Hestia avec la configuration du service, en tant qu'utilisateur hestia.
exec sudo -u hestia /usr/local/lib/hestia/launch "$@"
WRAPPER
chmod 0755 /usr/local/bin/hestia
/usr/local/bin/hestia check-config

echo "== Journaux limités à 64 Mo (carte SD)"
install -d /etc/systemd/journald.conf.d
printf '[Journal]\nSystemMaxUse=64M\n' > /etc/systemd/journald.conf.d/hestia.conf
systemctl restart systemd-journald

echo "== Service hestia"
install -m 0644 "$here/hestia.service" /etc/systemd/system/hestia.service
systemctl daemon-reload
systemctl enable hestia
systemctl restart hestia
sleep 2
systemctl --no-pager --lines=0 status hestia || true

echo "== Caddy (installé mais arrêté jusqu'à la bascule des ports 80/443)"
if ! command -v caddy >/dev/null; then
  apt-get update
  apt-get install -y caddy
fi
first_caddy_install=false
if [[ -f /etc/caddy/Caddyfile && ! -f /etc/caddy/Caddyfile.origine ]]; then
  cp /etc/caddy/Caddyfile /etc/caddy/Caddyfile.origine
  first_caddy_install=true
fi
# The example only replaces Debian's default file, never a Caddyfile already set up.
if [[ "$caddy_source" != *example* || "$first_caddy_install" == true ]]; then
  install -m 0644 "$caddy_source" /etc/caddy/Caddyfile
  if [[ "$caddy_source" == *example* ]]; then
    echo "Caddyfile d'exemple installé : mets tes domaines dans /etc/caddy/Caddyfile."
  fi
fi
caddy validate --adapter caddyfile --config /etc/caddy/Caddyfile
if [[ "${1:-}" == "--bascule" ]]; then
  systemctl enable caddy
  systemctl restart caddy
  echo "Caddy démarré."
else
  systemctl disable --now caddy >/dev/null 2>&1 || true
  echo "Caddy prêt, pas encore démarré (voir l'étape « Bascule » du guide)."
fi

ip="$(hostname -I | awk '{print $1}')"
echo
echo "Installation terminée. Tableau de bord : http://$ip:8080"
echo "Prochaine étape : créer ton compte avec  hestia user add <identifiant>  (ou depuis la page Réglages)"
