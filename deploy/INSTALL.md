# Installer Hestia sur le Raspberry Pi

Durée : environ 45 minutes. Il te faut le deuxième Pi 4B, la carte SD de 64 Go, un câble
Ethernet et le PC. Home Assistant continue de tourner sur l'autre Pi pendant toute la
migration : rien n'est coupé avant l'étape 5.

| Étape | Quoi | Durée |
|---|---|---|
| 1 | Préparer la carte SD | 10 min |
| 2 | Copier et installer | 10 min |
| 3 | Vérifier sur le réseau local | 5 min |
| 4 | Freebox, Apple Home, accès IA | 15 min |
| 5 | Basculer l'accès extérieur | 5 min |

---

## 1. Préparer la carte SD

1. Sur le PC, ouvre **Raspberry Pi Imager**.
2. Appareil : **Raspberry Pi 4**. Système : **Raspberry Pi OS (other) → Raspberry Pi OS Lite (64-bit)**.
   Stockage : la carte de 64 Go.
3. Dans **Modifier les réglages** :
   - nom d'hôte : `hestia` ;
   - utilisateur : le tien, noté `<utilisateur>` ci-dessous, avec un mot de passe ;
   - **ne configure pas le Wi-Fi** : le Pi sera en Ethernet ;
   - fuseau horaire `Europe/Paris`, clavier `fr` ;
   - onglet Services : **activer SSH** (authentification par mot de passe).
4. Écris la carte, mets-la dans le deuxième Pi, branche l'Ethernet puis l'alimentation.
5. Dans Freebox OS → **Paramètres de la Freebox → DHCP → Baux statiques**, donne une adresse fixe
   au Pi « hestia » (par exemple `192.168.1.20`). La redirection de ports de l'étape 5 en dépend.

## 2. Copier et installer

Sur le PC, dans PowerShell :

```powershell
scp -r <dossier-du-projet>\deploy <utilisateur>@hestia.local:~/hestia
ssh <utilisateur>@hestia.local
```

Si `hestia.local` ne répond pas, remplace-le par l'adresse IP du Pi.

Sur le Pi :

```bash
sudo bash ~/hestia/install.sh
```

Le script installe le programme, crée l'utilisateur système `hestia`, démarre le service et
installe Caddy sans le démarrer. Il se termine par « Installation terminée ».

Crée ensuite ton compte pour le tableau de bord (10 caractères minimum) :

```bash
hestia user add <identifiant>
```

> La commande `hestia …`, installée par le script, utilise automatiquement la configuration
> du service (`/etc/hestia/hestia.toml`) et les bons droits.

## 3. Vérifier sur le réseau local

1. Depuis le PC ou le téléphone sur le Wi-Fi, ouvre **http://hestia.local:8080**, ou l'IP du Pi suivie de `:8080`,
   puis connecte-toi.
2. En une dizaine de secondes, tu dois voir :
   - les lampes **Chambre** et **Cuisine**. L'**Ambilight** apparaît dès qu'elle répond ;
   - **PC fixe** « En ligne » et **Serveur Minecraft**, « En ligne » si le serveur tourne ;
   - la carte **Raspberry Pi** : température CPU, charge, mémoire, disque, alimentation.
3. Allume et éteins une lampe depuis le tableau de bord.
4. Les journaux en direct : `journalctl -u hestia -f` (Ctrl+C pour quitter).

## 4. Freebox, Apple Home et accès IA

> **Le plus simple : la page Réglages** du tableau de bord (bouton « Réglages » en haut à droite).
> Elle propose l’autorisation de la Freebox en un clic, le QR code Apple Home à scanner avec
> l’iPhone, la création des jetons IA et la gestion des comptes. Les commandes ci-dessous font
> la même chose depuis le terminal.

### Freebox

```bash
hestia freebox register
```

Valide la demande « Hestia » sur la Freebox (écran de la box ou appli Freebox) dans les
2 minutes. La carte Freebox apparaît ensuite toute seule en moins de 30 secondes.

Pour que l'interrupteur Wi-Fi fonctionne, dans Freebox OS → **Paramètres de la Freebox →
Gestion des accès → Applications → Hestia**, coche **Modification des réglages de la
Freebox**, puis lance `sudo systemctl restart hestia`.

Les graphiques de débit et de températures se remplissent ensuite au fil des heures.

### Apple Home

```bash
hestia homekit status
```

Note le code d'appairage (`XXX-XX-XXX`). Sur l'iPhone, connecté au Wi-Fi de la maison :

1. App **Maison → + → Ajouter un accessoire → Plus d'options…**
2. Choisis **Hestia**, saisis le code, puis accepte l'avertissement « accessoire non certifié ».
3. Range chaque lampe dans sa pièce.

Quand les lampes répondent bien, supprime l'ancien pont de Home Assistant (« HASS Bridge »)
dans l'app Maison, sinon chaque lampe apparaît en double.

Sans concentrateur Apple (Apple TV, HomePod, iPad), l'app Maison ne pilote les lampes que depuis
le Wi-Fi de la maison. De l'extérieur, passe par le tableau de bord web.

### Accès IA (MCP)

```bash
hestia mcp token add claude --control
```

Le jeton n'est affiché qu'une seule fois, avec la commande à lancer sur le PC pour Claude Code.
Avant la bascule (étape 5), remplace l'adresse `https://maison.example.fr/mcp` de cette commande par
`http://hestia.local:8080/mcp`.

- Sans `--control`, l'IA ne peut que lire : appareils et historique.
- Pour révoquer : `hestia mcp token remove claude`. Pour les lister : `hestia mcp token list`.

## 5. Basculer l'accès extérieur

Une fois tout validé en local :

1. Dans Freebox OS → **Paramètres de la Freebox → Gestion des ports**, modifie les deux
   redirections **80** et **443** (commentaire « Reverse Proxy ») : la destination devient le nouveau Pi.
   Ne touche pas à **25565**, qui pointe toujours vers le PC.
2. Sur le Pi :

   ```bash
   sudo bash ~/hestia/install.sh --bascule
   ```

   Ce script démarre Caddy, qui obtient les certificats Let's Encrypt de `maison.example.fr` et
   `jeu.example.fr` en environ une minute. Les enregistrements DNS chez OVH ne changent pas.
3. Depuis la 4G, sans Wi-Fi, vérifie **https://maison.example.fr** (connexion à Hestia) et
   **https://jeu.example.fr** (dashboard Minecraft). Journaux : `journalctl -u caddy -f`.
4. Après quelques jours sans souci, débranche l'ancien Pi. Garde sa carte SD au cas où.

### Retour arrière

Remets les redirections 80 et 443 vers `homeassistant` dans Freebox OS, puis lance
`sudo systemctl disable --now caddy` sur le nouveau Pi. Home Assistant reprend la main tel quel.

---

## Sauvegarde

Les fichiers importants sont dans `/var/lib/hestia` :
- `homekit.json` : l'identité du pont Apple Home. Si tu le perds, il faut ré-appairer.
- `freebox.json` : le jeton Freebox.
- les comptes, les jetons IA et l'historique.

La configuration est dans `/etc/hestia`.

```bash
sudo tar czf ~/hestia-sauvegarde-$(date +%F).tgz /etc/hestia /var/lib/hestia
```

Puis, depuis le PC : `scp <utilisateur>@hestia.local:~/hestia-sauvegarde-*.tgz .`

## Mise à jour

Les versions sont publiées sur GitHub (section `[update]` de `hestia.toml`). Le Pi regarde une fois par jour s’il y en a une nouvelle :
une pastille apparaît alors à côté de **Réglages**. Dans
**Réglages → Mise à jour**, **Chercher une mise à jour** compare avec la dernière version
publiée et propose **Installer la version …**. Une IA connectée en MCP avec un jeton
« commande » peut faire la même chose (outils `check_update` et `install_update`).

Hestia télécharge le programme et sa signature, vérifie la signature et que le programme
démarre, l’installe puis redémarre en quelques secondes. La page se recharge toute seule.

Pour publier une version depuis le PC (après `git push`) :

```bash
bash deploy/release.sh "Ce qui change"
```

Sans GitHub, on peut aussi choisir ensemble les fichiers `deploy/hestia` et `deploy/hestia.sig`
produits par `deploy/build.sh`, puis **Installer**.

Seuls les fichiers signés avec la clé de publication du PC (`%USERPROFILE%\.hestia\release.key`)
sont acceptés. Garde une copie de cette clé : sans elle, il faudra repasser par une
installation manuelle.

En cas de souci, **Revenir à la version précédente** dans la même section. Si Hestia ne
répond plus du tout :

```bash
sudo rm /var/lib/hestia/releases/hestia
sudo systemctl restart hestia
```

Il redémarre alors avec la version installée par `install.sh`.

Pour une installation manuelle (nouveaux fichiers de service par exemple), depuis le PC :

```powershell
scp -r <dossier-du-projet>\deploy <utilisateur>@hestia.local:~/hestia-maj
ssh -t <utilisateur>@hestia.local "sudo bash ~/hestia-maj/install.sh"
```

La configuration et les données sont conservées. Une fois la bascule faite (Caddy démarré),
ajoute `--bascule` à la fin de la deuxième commande, à l’intérieur des guillemets.

## Modifier la configuration

```bash
sudo nano /etc/hestia/hestia.toml
hestia check-config
sudo systemctl restart hestia
```

## Dépannage

- **Les lampes n'apparaissent pas.** Dans l'app Govee, ouvre chaque lampe → réglages → active le
  **Contrôle LAN**. Le Pi doit être sur le même réseau. Pour voir ce qui se passe :
  `journalctl -u hestia | grep -i lamp`.
- **« PC fixe » reste « Hors ligne » alors que le PC est allumé.** Teste
  `sudo -u hestia ping -c1 <ip-du-pc>`. Si la réponse est « Operation not permitted » :
  ```bash
  echo 'net.ipv4.ping_group_range = 0 2147483647' | sudo tee /etc/sysctl.d/99-ping.conf
  sudo sysctl --system
  ```
- **L'iPhone ne trouve pas le pont « Hestia ».** Vérifie que l'iPhone est sur le Wi-Fi de la
  maison, puis lance `hestia homekit status` et `sudo systemctl restart hestia`.
- **Caddy n'obtient pas de certificat.** Vérifie que les ports 80 et 443 de la Freebox pointent
  bien vers le nouveau Pi, puis regarde `journalctl -u caddy -n 50`.
- **Mot de passe oublié.** `hestia user passwd <identifiant>`.
