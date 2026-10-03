# Garmin MCP

*Lire en : **Français** · [English](README.md)*

🌐 **Site :** <https://devfrp.github.io/mcp-garmin-for-ia/>

Connectez votre compte Garmin à Claude — avec **zéro serveur tiers**. Une seule
commande installe un petit connecteur sur votre propre machine (ou serveur
maison) ; votre email et votre mot de passe Garmin vont de votre navigateur à
cette machine puis à Garmin, et nulle part ailleurs. Personne — aucun
hébergeur, ni ce projet — ne peut les voir.

Fonctionne avec **Claude Code**, **Claude Desktop**, **claude.ai** (navigateur)
et tout client MCP. Outils : activités, détail d'activité, résumé quotidien,
sommeil, fréquence cardiaque, Body Battery, stress, VFC (HRV), profil.

## Installation

**Machine personnelle** (Linux & macOS) :

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh
```

**Serveur maison / LXC Proxmox / VPS** — au choix :

```sh
# Accès LAN + Tailscale (serveur maison) :
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --server

# Écoute uniquement sur l'IP Tailscale (obligatoire sur un VPS avec IP publique) :
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --server --tailscale

# URL HTTPS permanente, PRIVÉE à ton tailnet — pour Claude Code ou tout
# client que tu fais tourner toi-même sur une machine de ton tailnet.
# Ne marche PAS pour « Ajouter un connecteur personnalisé » de claude.ai
# (voir l'avertissement ci-dessous — il lui faut --funnel) :
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --tailscale-serve

# URL HTTPS permanente, PUBLIQUE — nécessaire pour « Ajouter un connecteur
# personnalisé » de claude.ai / Claude Desktop. Lis l'avertissement ci-dessous
# avant d'utiliser ceci.
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --funnel
```

Le script installe tout sous l'utilisateur courant, installe Tailscale si
besoin (`--tailscale`/`--tailscale-serve`/`--funnel`), enregistre le
connecteur en service qui démarre avec la machine (unité systemd système en
root, unité user + lingering sinon, launchd sous macOS) et affiche les liens
de connexion. `--no-sudo` interdit toute élévation de privilèges (implicite
en root). **Peut être relancé sans risque** à tout moment (pour changer
d'options ou mettre à jour) : il ne supprime jamais ta connexion Garmin ni ne
déconnecte Tailscale, il ne fait que réinstaller le code du connecteur et
redémarrer le service.

> [!WARNING]
> `--funnel` rend l'endpoint MCP et la surface OAuth de ton connecteur
> joignables depuis **tout Internet**, pas seulement ton tailnet — quiconque
> obtient le token d'accès peut l'appeler (connexion, santé et déconnexion
> restent privées, et les chemins inconnus renvoient 404, mais le token est le
> seul verrou sur l'endpoint MCP lui-même). Utilise plutôt
> `--tailscale-serve` si tu n'as besoin de l'URL que pour un client qui tourne
> directement sur une de tes machines du tailnet (Claude Code, un script
> local, etc).
>
> **`--tailscale-serve` ne fonctionne *pas* avec « Ajouter un connecteur
> personnalisé » de claude.ai ou de Claude Desktop**, même depuis un appareil
> de ton tailnet : cette fonctionnalité est vérifiée et appelée depuis les
> serveurs d'Anthropic, qui ne sont jamais sur ton tailnet. Pour ce flux
> précis, il te faut une adresse publique — `--funnel` ou le quick tunnel
> Cloudflare sont les seules options.

## Connexion à Garmin

Ouvrez la page affichée par l'installeur — `http://127.0.0.1:8765/setup` en
local, ou `http://<ip-du-serveur>:8765/setup` depuis votre LAN/tailnet — et
connectez-vous avec votre compte Garmin (MFA supporté). Les endpoints de
connexion ne répondent que sur ces adresses privées, jamais sur l'adresse
HTTPS stable (Funnel ou Tailscale Serve) ni sur un quick tunnel Cloudflare.

## Brancher un client Claude

Il y a deux manières bien différentes de brancher un client, et elles
n'acceptent pas le même genre d'URL :

| Client | Quoi utiliser |
|---|---|
| **Claude Code** — `claude mcp add -s user --transport http garmin "<URL>"` | Toute URL joignable **directement depuis la machine qui fait tourner Claude Code** : l'URL à token du LAN/Tailscale, voire une adresse HTTPS `--tailscale-serve` si cette machine est sur ton tailnet. |
| **Claude Desktop** — entrée MCP locale dans `claude_desktop_config.json` | Pareil — l'URL à token affichée par la page de configuration (`http://…:8765/garmin/?token=…`), joignable directement depuis cette machine. |
| **claude.ai** (navigateur) **ou** la fenêtre **« Ajouter un connecteur personnalisé »** de Claude Desktop | Une URL HTTPS **publique** uniquement — `--funnel` ou le quick tunnel Cloudflare. Cette fonctionnalité est vérifiée et appelée depuis les serveurs d'Anthropic, elle ne pourra donc jamais joindre une adresse `--tailscale-serve`, peu importe la machine ou le réseau utilisés. |

### claude.ai / « Ajouter un connecteur personnalisé » (OAuth)

Ce flux exige du HTTPS et OAuth, et — contrairement aux clients locaux
ci-dessus — la connexion à ton serveur est faite par l'infrastructure
d'Anthropic, pas par ton appareil. Il faut une adresse **publique** :

1. Obtenez une adresse HTTPS publique — au choix :
   - `--funnel` à l'installation : adresse permanente
     `https://<machine>.<tailnet>.ts.net`, survit aux reboots. ⚠️ Voir
     l'avertissement plus haut — ça expose l'endpoint MCP à tout Internet.
   - Le bouton **Create HTTPS link** de la page de configuration : un quick
     tunnel Cloudflare, également public, dont l'URL change à chaque
     redémarrage.
2. Dans claude.ai → Paramètres → Connecteurs → Ajouter un connecteur
   personnalisé (ou l'équivalent dans Claude Desktop), collez l'URL HTTPS
   suivie de `/garmin/` (sans token), p. ex. :
   `https://garmin.tailXXXX.ts.net/garmin/`
3. claude.ai ouvre la page d'autorisation du connecteur : collez votre token
   d'accès (affiché par `garmin-mcp url` ou la page de configuration) et
   validez. C'est fini — l'autorisation persiste.

Si claude.ai n'arrive pas à joindre le serveur juste après l'activation du
Funnel, le DNS `.ts.net` est peut-être en cours de propagation ; attendez
quelques minutes. Si ça persiste, renommez la machine dans la console
d'administration Tailscale (un nom DNS neuf résout immédiatement) et relancez
l'installeur.

## CLI

```sh
garmin-mcp               # démarre le connecteur (défaut : serve)
garmin-mcp login         # connexion depuis le terminal plutôt que la page
garmin-mcp url           # affiche votre URL MCP (--base <url> pour une base publique)
garmin-mcp tunnel        # quick tunnel Cloudflare depuis le terminal
garmin-mcp logout        # déconnecte Garmin (token MCP inchangé)
garmin-mcp rotate-token  # nouveau token — toutes les URLs/autorisations collées meurent
```

## Fonctionnement

```
Navigateur ──▶ connecteur garmin-mcp (votre machine) ──▶ SSO + API Garmin
                    │  tokens OAuth dans ~/.config/garmin-mcp/ (jamais le mdp)
                    ├──▶ endpoint MCP /garmin/  ◀── clients locaux, en direct (URL à
                    │                                token ou --tailscale-serve) :
                    │                                Claude Code, config locale de
                    │                                Desktop, tout client qui tourne
                    │                                sur une machine de ton tailnet
                    └──▶ entrée HTTPS publique       ◀── claude.ai / « connecteur
                         (Funnel ou quick tunnel)        personnalisé » — appelé depuis
                                                          les serveurs d'Anthropic
                                                          (OAuth 2.1 + PKCE)
```

- L'endpoint MCP accepte le token propre à l'installation en `?token=`, en
  préfixe de chemin (`/t/<token>/garmin/`) ou en header `Bearer` — le flux
  OAuth émet ce même token.
- Via l'entrée HTTPS publique (Funnel, quick tunnel), seuls l'endpoint MCP et
  la surface OAuth sont joignables ; connexion, health et déconnexion ne
  répondent que sur les adresses privées, et les chemins inconnus renvoient
  404.
- Funnel et le quick tunnel Cloudflare rendent cette entrée joignable depuis
  **tout Internet** — les scanners qui la sondent sont normaux, tout ce qu'ils
  touchent renvoie 404, mais c'est bien le token d'accès qui protège
  réellement l'endpoint MCP : traite-le comme un mot de passe.
- `--tailscale-serve` donne une adresse HTTPS séparée et privée pour les
  clients locaux directs (Claude Code, etc.) qui tournent sur ton tailnet —
  c'est une adresse différente de l'entrée publique ci-dessus, et elle n'est
  jamais joignable par « Ajouter un connecteur personnalisé » de claude.ai,
  qui a toujours besoin de l'adresse publique.

### LXC Proxmox

Le script tourne en root sans jamais appeler `sudo`, et installe une unité
systemd système qui démarre avec le CT. Tailscale a besoin de `/dev/net/tun` :
si le conteneur ne l'a pas, le script s'arrête et affiche les deux lignes
exactes à ajouter dans `/etc/pve/lxc/<CTID>.conf` sur l'hôte Proxmox. Les
conteneurs Debian nécessitent aussi `apt install python3-venv` au préalable.

## Licence

MIT
