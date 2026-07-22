# Garmin MCP

> 🇬🇧 [English version](README.md)

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

# Tout ça + une URL HTTPS publique permanente pour claude.ai (Tailscale Funnel) :
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --funnel
```

Le script installe tout sous l'utilisateur courant, installe Tailscale si
besoin (`--tailscale`/`--funnel`), enregistre le connecteur en service qui
démarre avec la machine (unité systemd système en root, unité user + lingering
sinon, launchd sous macOS) et affiche les liens de connexion. `--no-sudo`
interdit toute élévation de privilèges (implicite en root).

## Connexion à Garmin

Ouvrez la page affichée par l'installeur — `http://127.0.0.1:8765/setup` en
local, ou `http://<ip-du-serveur>:8765/setup` depuis votre LAN/tailnet — et
connectez-vous avec votre compte Garmin (MFA supporté). Les endpoints de
connexion ne répondent que sur les adresses privées, jamais sur les entrées
HTTPS publiques.

## Brancher un client Claude

| Client | Quoi utiliser |
|---|---|
| **Claude Code** | `claude mcp add -s user --transport http garmin "<URL MCP locale ou tailscale>"` |
| **Claude Desktop** / clients MCP locaux | L'URL MCP affichée par la page (`http://…:8765/garmin/?token=…`) |
| **claude.ai** (navigateur) | L'URL HTTPS publique **sans token** — voir ci-dessous |

### claude.ai (OAuth)

claude.ai exige du HTTPS et un flux OAuth pour les connecteurs personnalisés.
Le connecteur implémente les deux :

1. Obtenez une adresse HTTPS publique : `--funnel` à l'installation
   (`https://<machine>.<tailnet>.ts.net` permanente, survit aux reboots) ou le
   bouton **Create HTTPS link** de la page (quick tunnel Cloudflare, URL qui
   change à chaque redémarrage).
2. Dans claude.ai → Paramètres → Connecteurs → Ajouter un connecteur
   personnalisé, collez l'URL HTTPS suivie de `/garmin/` (sans token), p. ex. :
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
                    ├──▶ endpoint MCP /garmin/  ◀── Claude Code / Desktop (URL à token)
                    └──▶ entrée HTTPS publique  ◀── claude.ai (OAuth 2.1 + PKCE)
```

- L'endpoint MCP accepte le token propre à l'installation en `?token=`, en
  préfixe de chemin (`/t/<token>/garmin/`) ou en header `Bearer` — le flux
  OAuth émet ce même token.
- Via les entrées publiques (Funnel, quick tunnel), seuls l'endpoint MCP et la
  surface OAuth sont joignables ; connexion, health et déconnexion ne répondent
  que sur les adresses privées, et les chemins inconnus renvoient 404.
- Les scanners Internet qui sondent une adresse publique sont normaux ; tout ce
  qu'ils touchent renvoie 404.

### LXC Proxmox

Le script tourne en root sans jamais appeler `sudo`, et installe une unité
systemd système qui démarre avec le CT. Tailscale a besoin de `/dev/net/tun` :
si le conteneur ne l'a pas, le script s'arrête et affiche les deux lignes
exactes à ajouter dans `/etc/pve/lxc/<CTID>.conf` sur l'hôte Proxmox. Les
conteneurs Debian nécessitent aussi `apt install python3-venv` au préalable.

## Licence

MIT
