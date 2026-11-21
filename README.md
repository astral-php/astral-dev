# astral-dev

![Langage](https://img.shields.io/badge/langage-Rust-orange?logo=rust)
![Version](https://img.shields.io/badge/version-0.4.2--dev-blue)
![Licence](https://img.shields.io/badge/licence-MIT-green)
[![CI](https://github.com/astral-php/astral-dev/actions/workflows/ci.yml/badge.svg)](https://github.com/astral-php/astral-dev/actions/workflows/ci.yml)

**astral-dev** est un environnement de développement local pour Windows, taillé pour les projets **Astral PHP** : un « Laragon personnel » en **Rust + Tauri 2**. Il démarre et surveille Apache, PHP, MySQL, Redis et Mailpit, crée des projets Astral, les expose en `.test`, et installe lui-même ses composants avec vérification SHA-256.

> Projet **hors** de la feuille de route Astral 2.2.0 ; suivi dans son propre dossier technique (workspace Astral : `astral-informations/astral-dev/`).

## État d'avancement

**Alpha** : `0.5.1-dev`. Fonctionne de bout en bout sur la machine de développement (mode autonome `C:\astral-dev`, installation sans Laragon vérifiée par test réel). Pas de signature de code : Windows SmartScreen peut avertir à l'installation.

## Fonctionnalités déjà intégrées

- **Services** : Apache (mod_php pour PHP 8.3, FastCGI pour les builds NTS 8.4/8.5), MySQL 8.4 (initialisation automatique), Redis 5.0.14.1, Mailpit ; démarrage/arrêt sans orphelin, journaux, ports de repli si un port est occupé, alerte « port occupé par un autre programme ».
- **Mode autonome** : racine `C:\astral-dev`, configuration propre (`astral-dev.toml`), domaines `.test`, cohabitation avec Laragon.
- **Installation de composants (v0.2)** : catalogue de versions, téléchargement HTTPS limité à une liste blanche de domaines, **SHA-256 vérifié avant extraction**, extraction sans zip-slip, installation atomique, extension PHP `redis` par version ; import depuis Laragon en alternative.
- **Projets Astral** : création par Composer (starter, shop, blog, forum), console Astral en direct, vhost + fichier hosts (élévation UAC à la demande), retrait d'exposition, HTTPS local guidé (mkcert installé depuis le catalogue, autorité locale approuvée sur demande, certificat par projet).
- **Redis** : statistiques, explorateur de clés (SCAN), console (lecture seule par défaut), mot de passe généré ; **cache Redis par projet** (`.env` + enregistrement du `CacheServiceProvider`, retour arrière).
- **Interface** : tableau de bord, pages Projets, Redis, Composants, Logs, Réglages ; icône de zone de notification ; instance unique ; installeur NSIS par utilisateur (sans droits administrateur).

## Fonctionnalités à venir (feuille de route)

- HTTPS : certificats par projet et `mkcert -install` guidé sont livrés ; reste l'import de l'autorité dans Firefox.
- Page Base de données ; Nginx (option) ; Memcached, Node, Git, Python, Go (prérequis d'`astral-deploy`).
- Mises à jour du catalogue à la demande depuis ce dépôt ; démarrage avec Windows ; signature de code ; intégration continue.

## Installation

1. Télécharger l'installeur `astral-dev_<version>_x64-setup.exe` (publié dans les *Releases* quand disponible) et le lancer : installation par utilisateur, sans droits administrateur.
2. Au premier lancement, **« Installer les composants (téléchargement) »** (ou « Importer depuis Laragon »). Les binaires sont installés dans `C:\astral-dev\bin`.
3. Désinstallation : *Paramètres Windows → Applications*. `C:\astral-dev` (projets, données MySQL) est conservé volontairement.

### Depuis les sources

Prérequis : Rust (rustup, toolchain MSVC), Visual Studio Build Tools (charge C++), Node.js, WebView2.

```bash
npm install
npm run dev                 # tauri dev
npx tauri build             # installeur dans src-tauri/target/release/bundle/nsis/
cd src-tauri && cargo test --lib
cargo test --lib standalone_real_download -- --ignored --nocapture   # installe tout depuis le réseau (≈ 400 Mo)
```

## Utilisation

1. **Tout démarrer** : Apache, MySQL, Redis, Mailpit.
2. **Projets Astral → Nouveau projet** (modèle `starter`, `shop`, `blog` ou `forum`), puis **Exposer** : `http://<nom>.test:<port>/`.
3. **Cache Redis** sur la fiche d'un projet : `.env` mis à jour, sauvegarde `.env.astral-dev.bak`, retour arrière possible.
4. Réglages (ports, version de PHP) : page **Réglages**, puis redémarrer astral-dev.

## Aperçu de l'architecture

```text
src/            interface (HTML/CSS/JS, sans framework) : dashboard, projets, Redis, composants, logs, réglages
src-tauri/
  catalog.json  catalogue embarqué (sources officielles, sommes SHA-256)
  src/
    supervisor.rs, services/   processus et configuration des services (ServiceSpec)
    catalog.rs, download.rs, install.rs, components.rs   téléchargement vérifié et installation atomique
    provision.rs               configs générées (httpd, php.ini, my.ini), import Laragon
    projects.rs, vhosts.rs, hosts.rs, mkcert.rs          projets, vhosts, hosts, certificats
    redis_client.rs, cache_setup.rs                      Redis (RESP maison) et cache par projet
```

Principes : pas de magie, pas de secret versionné, aucune télémétrie, écritures bornées (bloc balisé du fichier hosts, `.env` modifié sur demande uniquement, sauvegardes uniques).

## Contribution

Les contributions sont bienvenues : ouvrir une *issue* avant un changement important, garder `cargo test --lib` au vert, fichiers en UTF-8 sans BOM, `declare(strict_types=1)` pour tout PHP généré, pas de secret dans le dépôt. Les décisions d'architecture sont consignées sous forme d'ADR dans le dossier technique.

## Licence

[MIT](LICENSE) © 2026 astral-php
