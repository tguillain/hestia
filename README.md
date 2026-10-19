# Hestia

Hub domotique léger pour Raspberry Pi, écrit en Rust : un remplaçant de Home Assistant pour une
maison qui a quelques lampes, une box et un peu de supervision. Un seul programme statique
d'environ 5 Mo, une quinzaine de Mo de mémoire, tout en local.

Inspiré par [moli-os](https://github.com/rudy-io/moli-os) : un binaire unique, des pilotes
supervisés, des identifiants d'appareils stables et des IA qui aident sans jamais être dans la
boucle de contrôle.

## Ce que fait Hestia

- **Lampes Govee** pilotées en local (API LAN) : allumage, luminosité, couleur, blanc. Découverte
  automatique, balayage du réseau et ajout par adresse IP.
- **Apple Home** : pont HomeKit (HAP) natif, local, appairage par QR code.
- **Freebox** : débits, IP externe, températures, ventilateurs, interrupteur Wi-Fi.
- **Supervision** : serveurs Minecraft, présence des machines du réseau, santé du Raspberry Pi
  (température, charge, mémoire, disque, sous-tension).
- **Historique et graphiques** sur quelques jours, sans base de données externe.
- **Tableau de bord web** en français, temps réel, adapté au mobile, avec une page Réglages pour
  tout configurer sans terminal.
- **Accès IA (MCP)** : jetons en lecture seule ou avec commandes, pour Claude ou d'autres clients.
- **Mises à jour signées** depuis l'interface, avec retour à la version précédente.

## Installer

Le guide pas à pas est dans [deploy/INSTALL.md](deploy/INSTALL.md) : préparation de la carte SD,
installation, Freebox, Apple Home, accès extérieur avec Caddy et HTTPS.

La configuration d'exemple est [deploy/hestia.example.toml](deploy/hestia.example.toml).

## Compiler

Le projet est en Rust pur : la version pour le Pi se compile depuis Windows ou Linux, sans chaîne
de compilation ARM.

```bash
rustup target add aarch64-unknown-linux-musl
cargo test --workspace
bash deploy/build.sh
```

`deploy/build.sh` produit `deploy/hestia` (ARM 64 bits, statique) et sa signature
`deploy/hestia.sig`. Les mises à jour depuis l'interface n'acceptent que les programmes signés par
la clé dont la partie publique est dans `crates/hestia-api/release.pub`. Pour ton propre
déploiement, crée ta clé avec `cargo run -p hestia -- release keygen` et remplace ce fichier.

`deploy/release.sh "notes"` publie la version sur GitHub (release avec `hestia-aarch64` et sa
signature). Le Pi la propose alors dans Réglages → Mise à jour et via l'outil MCP
`install_update`. Le dépôt interrogé se règle dans la section `[update]` de `hestia.toml`.

## Organisation

| Crate | Rôle |
|---|---|
| `hestia-core` | Vocabulaire : appareils, points, valeurs, commandes. Aucune entrée/sortie. |
| `hestia-hub` | État, événements, commandes avec délai, supervision des pilotes. |
| `hestia-api` | Tableau de bord, API REST, temps réel, comptes, MCP, mises à jour. |
| `hestia-homekit` | Pont Apple Home (HAP : SRP, Ed25519, ChaCha20-Poly1305, mDNS). |
| `hestia-history` | Historique (redb) et graphiques. |
| `hestia-govee`, `hestia-freebox`, `hestia-minecraft`, `hestia-ping`, `hestia-system` | Pilotes. |
| `hestia` | Le programme : configuration, service, outils en ligne de commande. |

## Sécurité

- Connexion par mot de passe (Argon2id), sessions de 30 jours, blocage progressif après 5 échecs.
- Protection contre les requêtes venues d'un autre site, en-têtes de sécurité stricts.
- Jetons IA stockés sous forme d'empreinte seulement, révocables, en lecture seule par défaut.
- Service systemd confiné : il n'écrit que dans son dossier de données.
- Mises à jour vérifiées par signature Ed25519 avant toute installation.

## Licence

[Apache-2.0](LICENSE)
