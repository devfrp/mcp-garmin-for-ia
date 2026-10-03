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

# URL HTTPS permanente pour claude.ai, PRIVÉE — joignable uniquement depuis
# ton propre tailnet, jamais depuis Internet (recommandé par rapport à --funnel) :
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --tailscale-serve

# URL HTTPS permanente pour claude.ai, PUBLIQUE — joignable depuis tout
# Internet (Tailscale Funnel). Lis l'avertissement ci-dessous avant d'utiliser ceci.
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
> `--tailscale-serve`, sauf si tu as vraiment besoin que claude.ai (ou un
> autre client) joigne le connecteur depuis l'extérieur de ton tailnet.

## Connexion à Garmin

Ouvrez la page affichée par l'installeur — `http://127.0.0.1:8765/setup` en
local, ou `http://<ip-du-serveur>:8765/setup` depuis votre LAN/tailnet — et
connectez-vous avec votre compte Garmin (MFA supporté). Les endpoints de
connexion ne répondent que sur ces adresses privées, jamais sur l'adresse
HTTPS stable (Funnel ou Tailscale Serve) ni sur un quick tunnel Cloudflare.

## Brancher un client Claude

| Client | Quoi utiliser |
|---|---|
| **Claude Code** | `claude mcp add -s user --transport http garmin "<URL MCP locale ou tailscale>"` |
| **Claude Desktop** / clients MCP locaux | L'URL MCP affichée par la page (`http://…:8765/garmin/?token=…`) |
| **claude.ai** (navigateur) | L'URL HTTPS (Tailscale Serve/Funnel, ou tunnel Cloudflare) **sans token** — voir ci-dessous |

### claude.ai (OAuth)

claude.ai exige du HTTPS et un flux OAuth pour les connecteurs personnalisés.
Le connecteur implémente les deux :

1. Obtenez une adresse HTTPS — au choix :
   - `--tailscale-serve` à l'installation : adresse permanente
     `https://<machine>.<tailnet>.ts.net`, survit aux reboots, **joignable
     uniquement depuis ton propre tailnet** (recommandé).
   - `--funnel` à l'installation : même adresse permanente, mais **joignable
     depuis tout Internet**. ⚠️ Voir l'avertissement plus haut — préférez
     `--tailscale-serve` sauf besoin spécifique.
   - Le bouton **Create HTTPS link** de la page de configuration : un quick
     tunnel Cloudflare, également public, dont l'URL change à chaque
     redémarrage.
2. Dans claude.ai → Paramètres → Connecteurs → Ajouter un connecteur
   personnalisé, collez l'URL HTTPS suivie de `/garmin/` (sans token), p. ex. :
   `https://garmin.tailXXXX.ts.net/garmin/`
3. claude.ai ouvre la page d'autorisation du connecteur : collez votre token
   d'accès (affiché par `garmin-mcp url` ou la page de configuration) et
   validez. C'est fini — l'autorisation persiste.

Si claude.ai n'arrive pas à joindre le serveur juste après l'activation du
Funnel ou de Tailscale Serve, le DNS `.ts.net` est peut-être en cours de
propagation ; attendez quelques minutes. Si ça persiste, renommez la machine
dans la console d'administration Tailscale (un nom DNS neuf résout
immédiatement) et relancez l'installeur.

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
                    └──▶ entrée HTTPS (Serve, Funnel ou quick tunnel) ◀── claude.ai (OAuth 2.1 + PKCE)
```

- L'endpoint MCP accepte le token propre à l'installation en `?token=`, en
  préfixe de chemin (`/t/<token>/garmin/`) ou en header `Bearer` — le flux
  OAuth émet ce même token.
- Via l'entrée HTTPS (Tailscale Serve/Funnel, quick tunnel), seuls l'endpoint
  MCP et la surface OAuth sont joignables ; connexion, health et déconnexion
  ne répondent que sur les adresses privées, et les chemins inconnus
  renvoient 404.
- Tailscale Serve garde cette entrée HTTPS joignable uniquement depuis ton
  tailnet. Funnel et le quick tunnel Cloudflare la rendent au contraire
  joignable depuis **tout Internet** — les scanners qui la sondent sont
  normaux, tout ce qu'ils touchent renvoie 404, mais c'est bien le token
  d'accès qui protège réellement l'endpoint MCP : traite-le comme un mot de
  passe.

### LXC Proxmox

Le script tourne en root sans jamais appeler `sudo`, et installe une unité
systemd système qui démarre avec le CT. Tailscale a besoin de `/dev/net/tun` :
si le conteneur ne l'a pas, le script s'arrête et affiche les deux lignes
exactes à ajouter dans `/etc/pve/lxc/<CTID>.conf` sur l'hôte Proxmox. Les
conteneurs Debian nécessitent aussi `apt install python3-venv` au préalable.

## Licence

MIT
