<div align="center">
  <img src="assets/logo.png" alt="Aramaki Logo" width="200"/>

  # Aramaki

  **Chief Section 9 — Agent Orchestrator & Dynamic K8s Job Provisioner for Slack**

  [![Build](https://github.com/jzacharie/Amaraki/actions/workflows/build.yml/badge.svg)](https://github.com/jzacharie/Amaraki/actions/workflows/build.yml)
  [![Rust](https://img.shields.io/badge/Rust-2021-orange?logo=rust)](https://www.rust-lang.org/)
  [![Docker](https://img.shields.io/badge/Docker-ready-blue?logo=docker)](https://github.com/jzacharie/Amaraki/pkgs/container/amaraki)
  [![Kubernetes](https://img.shields.io/badge/Kubernetes-native-326CE5?logo=kubernetes)](https://kubernetes.io/)
  [![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
</div>

---

> 🇬🇧 [English](#english) | 🇫🇷 [Français](#français)

---

## English

### What is Aramaki?

**Aramaki** is a Slack-native AI agent orchestrator written in Rust. Named after the chief of Section 9 in *Ghost in the Shell*, it acts as a command bridge between Slack conversations and Kubernetes-powered AI agents.

When a user mentions `@aramaki` or sends a message in an active thread, Aramaki:

1. **Understands** the intent (email summary, code review, K8s diagnosis, incident response…)
2. **Confirms** the action with the user before executing
3. **Spawns** the right AI agent as a Kubernetes Job
4. **Reports back** the result in the Slack thread

It also supports **voice messages** — audio files sent to Slack are automatically transcribed via a Whisper-compatible ASR endpoint and converted to text instructions.

---

### Architecture

```
Slack ──► /slack/events ──► Gatekeeper ──► K8s Job (AI Agent)
                                │                  │
                           Intent analysis    Agent executes
                           Confirmation       with prompt
                           State machine      from ConfigMap
```

Key components:

| Module | Role |
|--------|------|
| `main.rs` | HTTP server (Axum), routing, background K8s sync loop |
| `gatekeeper.rs` | Intent detection, validation state machine, Whisper transcription |
| `k8s.rs` | Agent discovery (ConfigMaps), Job spawning, status sync |
| `metrics.rs` | In-memory agent stats, execution history, Prometheus export |
| `web.rs` | Web dashboard (HTML), REST APIs for stats and agent config |
| `auth.rs` | Session-based auth (cookie/header), access log middleware |
| `slack.rs` | Slack `chat.postMessage` notifier |

---

### Features

- 🎙️ **Voice-to-text** — Transcribes Slack audio/video files via Whisper API
- 🤖 **Smart routing** — Detects intent keywords and routes to the right agent
- ✅ **Human-in-the-loop** — Always asks for confirmation before launching a job
- 🔄 **K8s native** — Agents are declared as ConfigMaps, executed as K8s Jobs
- 📊 **Dashboard** — Web UI with live stats, agent list, execution history
- 📈 **Observability** — Prometheus `/metrics` + OpenTelemetry `/api/otel/v1/metrics`
- 🔒 **Auth** — Cookie-based session login, optional API key, configurable credentials
- 🐳 **Container-ready** — Multi-stage Dockerfile, GHCR auto-publish via GitHub Actions

---

### Quick Start

#### Prerequisites

- Rust 2021+ (`cargo`)
- Docker (optional, for containerized deployment)
- A Kubernetes cluster (optional, Aramaki runs in standalone mode without it)
- A Slack Bot Token (`SLACK_BOT_TOKEN`)

#### Run locally

```bash
# Clone the repo
git clone https://github.com/jzacharie/Amaraki.git
cd Amaraki

# Run in standalone mode (no K8s required)
ARAMAKI_AUTH_USER=admin \
ARAMAKI_AUTH_PASSWORD=section9 \
SLACK_BOT_TOKEN=xoxb-your-token \
cargo run --release
```

The server starts on `http://0.0.0.0:3000` by default.

#### Local CI

```bash
./local-ci.sh
```

Runs `cargo fmt`, `cargo check`, `cargo clippy`, `cargo build --release`, and optionally a local Docker build.

---

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` / `ARAMAKI_PORT` | `3000` | HTTP listen port |
| `ARAMAKI_HOST` | `0.0.0.0` | HTTP listen host |
| `POD_NAMESPACE` | `aramaki` | Kubernetes namespace |
| `AGENT_RUNNER_IMAGE` | `ghcr.io/jzacharie/opencode-agent:latest` | Docker image used for K8s Jobs |
| `SLACK_BOT_TOKEN` | *(none)* | Slack bot token (`xoxb-…`) |
| `WHISPER_URL` | `http://speaches.speaches.svc.cluster.local:8000/v1/audio/transcriptions` | Whisper ASR endpoint |
| `ARAMAKI_AUTH_USER` | `admin` | Dashboard login username |
| `ARAMAKI_AUTH_PASSWORD` | `section9` | Dashboard login password |
| `ARAMAKI_API_KEY` | *(none)* | Optional static API key for authenticated endpoints |
| `ARAMAKI_ALLOW_ANONYMOUS_METRICS` | `true` | Allow unauthenticated scraping of `/metrics` |

---

### Slack Setup

1. Create a Slack app at [api.slack.com/apps](https://api.slack.com/apps)
2. Enable **Event Subscriptions** and point the Request URL to `https://your-domain/slack/events`
3. Subscribe to `message.channels` and `app_mention` bot events
4. Add the `chat:write` OAuth scope and install the app to your workspace
5. Set `SLACK_BOT_TOKEN` with the `xoxb-…` token

**Trigger conditions:**
- Bot is mentioned (`app_mention` event)
- Message contains `amaraki` or `aramaki` (case-insensitive)
- Message is in an active validation thread
- Message contains an audio or video file

---

### Agent Discovery (Kubernetes)

Agents are declared as **ConfigMaps** in the target namespace. Each ConfigMap must contain an `agent.json` key:

```json
{
  "name": "agent-code-reviewer",
  "description": "Reviews code and proposes architectural improvements",
  "model": "openai/gpt-4o",
  "system_prompt": "You are an expert code reviewer...",
  "mcp_servers": [],
  "max_iterations": 10,
  "env": {}
}
```

At startup, Aramaki discovers all ConfigMaps in the namespace and registers the agents. If none are found, default built-in agents are seeded.

---

### API Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/health` | None | Health check |
| `GET` | `/metrics` | None* | Prometheus metrics |
| `GET` | `/api/otel/v1/metrics` | None* | OpenTelemetry metrics |
| `POST` | `/slack/events` | None | Slack event ingestion |
| `GET` | `/` / `/dashboard` | Session | Web dashboard |
| `GET` | `/api/dashboard/stats` | Session | Live dashboard stats |
| `GET` | `/api/agents/:name/config` | Session | Agent config export |
| `POST` | `/api/agents/test-trigger` | Session | Manual agent trigger |
| `POST` | `/api/agents/callback` | Session | Agent job result callback |
| `POST` | `/api/auth/login` | None | Login |
| `POST` | `/api/auth/logout` | Session | Logout |

*\* Public if `ARAMAKI_ALLOW_ANONYMOUS_METRICS=true`*

---

### Docker & CI/CD

The GitHub Actions workflow (`.github/workflows/build.yml`) automatically builds and pushes the Docker image to GHCR on every push to `main`.

```bash
# Pull the latest image
docker pull ghcr.io/jzacharie/amaraki:latest

# Run
docker run -p 3000:3000 \
  -e SLACK_BOT_TOKEN=xoxb-... \
  -e ARAMAKI_AUTH_PASSWORD=changeme \
  ghcr.io/jzacharie/amaraki:latest
```

---

---

## Français

### Qu'est-ce qu'Aramaki ?

**Aramaki** est un orchestrateur d'agents IA natif Slack, écrit en Rust. Nommé d'après le chef de la Section 9 dans *Ghost in the Shell*, il joue le rôle de pont de commandement entre les conversations Slack et les agents IA hébergés sur Kubernetes.

Lorsqu'un utilisateur mentionne `@aramaki` ou envoie un message dans un fil actif, Aramaki :

1. **Comprend** l'intention (synthèse d'e-mails, revue de code, diagnostic K8s, réponse à incident…)
2. **Confirme** l'action avec l'utilisateur avant toute exécution
3. **Crée** un Job Kubernetes avec le bon agent IA
4. **Retourne** le résultat dans le fil Slack

Il prend également en charge les **messages vocaux** — les fichiers audio envoyés sur Slack sont automatiquement transcrits via un endpoint Whisper ASR compatible OpenAI et convertis en instructions texte.

---

### Architecture

```
Slack ──► /slack/events ──► Gatekeeper ──► Job K8s (Agent IA)
                                │                  │
                        Analyse d'intention   L'agent exécute
                        Confirmation          avec le prompt
                        Machine d'état        issu du ConfigMap
```

Composants principaux :

| Module | Rôle |
|--------|------|
| `main.rs` | Serveur HTTP (Axum), routage, boucle de synchronisation K8s en arrière-plan |
| `gatekeeper.rs` | Détection d'intention, machine d'état de validation, transcription Whisper |
| `k8s.rs` | Découverte des agents (ConfigMaps), création de Jobs, synchronisation des statuts |
| `metrics.rs` | Statistiques en mémoire, historique d'exécution, export Prometheus |
| `web.rs` | Dashboard Web (HTML), APIs REST pour stats et config des agents |
| `auth.rs` | Auth par session (cookie/header), middleware de log des accès |
| `slack.rs` | Notifier Slack `chat.postMessage` |

---

### Fonctionnalités

- 🎙️ **Voix vers texte** — Transcription des fichiers audio/vidéo Slack via l'API Whisper
- 🤖 **Routage intelligent** — Détection des mots-clés d'intention et sélection de l'agent approprié
- ✅ **Validation humaine** — Demande systématique de confirmation avant le lancement d'un job
- 🔄 **K8s natif** — Les agents sont déclarés comme ConfigMaps, exécutés comme des Jobs K8s
- 📊 **Dashboard** — Interface Web avec statistiques en direct, liste des agents, historique d'exécution
- 📈 **Observabilité** — Prometheus `/metrics` + OpenTelemetry `/api/otel/v1/metrics`
- 🔒 **Auth** — Connexion par session (cookie), clé API optionnelle, credentials configurables
- 🐳 **Prêt pour les conteneurs** — Dockerfile multi-étapes, publication automatique sur GHCR via GitHub Actions

---

### Démarrage rapide

#### Prérequis

- Rust 2021+ (`cargo`)
- Docker (optionnel, pour le déploiement conteneurisé)
- Un cluster Kubernetes (optionnel, Aramaki fonctionne en mode autonome sans K8s)
- Un token de bot Slack (`SLACK_BOT_TOKEN`)

#### Lancement en local

```bash
# Cloner le dépôt
git clone https://github.com/jzacharie/Amaraki.git
cd Amaraki

# Lancer en mode autonome (sans K8s requis)
ARAMAKI_AUTH_USER=admin \
ARAMAKI_AUTH_PASSWORD=section9 \
SLACK_BOT_TOKEN=xoxb-votre-token \
cargo run --release
```

Le serveur démarre sur `http://0.0.0.0:3000` par défaut.

#### CI locale

```bash
./local-ci.sh
```

Exécute `cargo fmt`, `cargo check`, `cargo clippy`, `cargo build --release`, et optionnellement un build Docker local.

---

### Variables d'environnement

| Variable | Défaut | Description |
|----------|--------|-------------|
| `PORT` / `ARAMAKI_PORT` | `3000` | Port d'écoute HTTP |
| `ARAMAKI_HOST` | `0.0.0.0` | Hôte d'écoute HTTP |
| `POD_NAMESPACE` | `aramaki` | Namespace Kubernetes |
| `AGENT_RUNNER_IMAGE` | `ghcr.io/jzacharie/opencode-agent:latest` | Image Docker des Jobs K8s |
| `SLACK_BOT_TOKEN` | *(aucun)* | Token du bot Slack (`xoxb-…`) |
| `WHISPER_URL` | `http://speaches.speaches.svc.cluster.local:8000/v1/audio/transcriptions` | Endpoint ASR Whisper |
| `ARAMAKI_AUTH_USER` | `admin` | Identifiant de connexion au dashboard |
| `ARAMAKI_AUTH_PASSWORD` | `section9` | Mot de passe de connexion au dashboard |
| `ARAMAKI_API_KEY` | *(aucun)* | Clé API statique optionnelle pour les endpoints authentifiés |
| `ARAMAKI_ALLOW_ANONYMOUS_METRICS` | `true` | Autoriser le scraping non authentifié de `/metrics` |

---

### Configuration Slack

1. Créez une application Slack sur [api.slack.com/apps](https://api.slack.com/apps)
2. Activez les **Event Subscriptions** et configurez l'URL de requête sur `https://votre-domaine/slack/events`
3. Abonnez-vous aux événements bot `message.channels` et `app_mention`
4. Ajoutez le scope OAuth `chat:write` et installez l'application sur votre espace de travail
5. Définissez `SLACK_BOT_TOKEN` avec le token `xoxb-…`

**Conditions de déclenchement :**
- Le bot est mentionné (événement `app_mention`)
- Le message contient `amaraki` ou `aramaki` (insensible à la casse)
- Le message est dans un fil de validation actif
- Le message contient un fichier audio ou vidéo

---

### Découverte des agents (Kubernetes)

Les agents sont déclarés comme des **ConfigMaps** dans le namespace cible. Chaque ConfigMap doit contenir une clé `agent.json` :

```json
{
  "name": "agent-code-reviewer",
  "description": "Effectue des revues de code et propose des améliorations architecturales",
  "model": "openai/gpt-4o",
  "system_prompt": "Tu es un expert en revue de code...",
  "mcp_servers": [],
  "max_iterations": 10,
  "env": {}
}
```

Au démarrage, Aramaki découvre tous les ConfigMaps du namespace et enregistre les agents. Si aucun n'est trouvé, des agents intégrés par défaut sont chargés.

---

### Endpoints API

| Méthode | Chemin | Auth | Description |
|---------|--------|------|-------------|
| `GET` | `/health` | Aucune | Vérification de l'état du service |
| `GET` | `/metrics` | Aucune* | Métriques Prometheus |
| `GET` | `/api/otel/v1/metrics` | Aucune* | Métriques OpenTelemetry |
| `POST` | `/slack/events` | Aucune | Réception des événements Slack |
| `GET` | `/` / `/dashboard` | Session | Dashboard web |
| `GET` | `/api/dashboard/stats` | Session | Statistiques en direct |
| `GET` | `/api/agents/:name/config` | Session | Export de la config d'un agent |
| `POST` | `/api/agents/test-trigger` | Session | Déclenchement manuel d'un agent |
| `POST` | `/api/agents/callback` | Session | Callback de résultat d'un job agent |
| `POST` | `/api/auth/login` | Aucune | Connexion |
| `POST` | `/api/auth/logout` | Session | Déconnexion |

*\* Public si `ARAMAKI_ALLOW_ANONYMOUS_METRICS=true`*

---

### Docker & CI/CD

Le workflow GitHub Actions (`.github/workflows/build.yml`) construit et publie automatiquement l'image Docker sur GHCR à chaque push sur `main`.

```bash
# Récupérer la dernière image
docker pull ghcr.io/jzacharie/amaraki:latest

# Lancer
docker run -p 3000:3000 \
  -e SLACK_BOT_TOKEN=xoxb-... \
  -e ARAMAKI_AUTH_PASSWORD=changeme \
  ghcr.io/jzacharie/amaraki:latest
```

---

<div align="center">
  <sub>Built with ❤️ and Rust • Inspired by <em>Ghost in the Shell</em></sub>
</div>
