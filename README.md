# Rustral Studio

![Langage](https://img.shields.io/badge/langage-Rust-orange?logo=rust)
![Version](https://img.shields.io/badge/version-v0.8.0--dev-blue)
![Licence](https://img.shields.io/badge/licence-MIT-green)
[![Tests](https://github.com/astral-php/astral-dev/actions/workflows/ci.yml/badge.svg)](https://github.com/astral-php/astral-dev/actions/workflows/ci.yml)

## Présentation

**Rustral Studio** (anciennement « astral-dev ») est un environnement de développement local pour Windows, écrit en **Rust + Tauri 2** : un « Laragon personnel » conçu pour l'écosystème **Astral PHP**.

- **Rôle :** démarrer et surveiller le serveur web (Apache et/ou Caddy), PHP, MySQL, Redis et Mailpit, créer des projets et les exposer en `.test`, avec HTTPS local.
- **Objectif :** offrir aux projets Astral PHP (starter, blog, forum, shop…) un environnement prêt à l'emploi, sans configuration manuelle, qui installe lui-même ses composants avec vérification d'intégrité.
- **État :** version alpha (`0.8.0-dev`). L'installeur n'est pas signé par un certificat de code Windows : SmartScreen peut afficher un avertissement (« Informations complémentaires » puis « Exécuter quand même »).

## Fonctionnalités disponibles

- **Serveur web au choix :** Apache, Caddy, ou Caddy en façade d'Apache (pour les `.htaccess`), avec un serveur par projet dans ce dernier mode ; rechargement à chaud avec Caddy.
- **PHP :** versions NTS en FastCGI, une version de PHP par projet, pool de 1 à 8 processus par version.
- **Services :** MySQL 8.4, Redis (5 ou 8), Mailpit (navigateur ou fenêtre dédiée) ; démarrage et arrêt sans processus orphelin, journaux, ports de repli, alerte si un port est occupé par un autre programme ; tableau de bord avec la topologie réelle.
- **Composants :** catalogue de versions, téléchargement HTTPS limité à une liste blanche de domaines, somme SHA-256 vérifiée avant extraction, installation atomique ; import depuis Laragon en alternative.
- **Projets :** création par Composer ou projet PHP vide, fiche projet (adresse, serveur, version de PHP, HTTPS, cache, console Astral, dossier), activation en `.test` (fichier `hosts`, une seule élévation UAC), HTTPS local par projet (mkcert).
- **Redis :** statistiques, explorateur de clés, console en lecture seule par défaut, cache Redis par projet.
- **Mises à jour automatiques :** vérification depuis les Réglages, signature contrôlée avant toute installation, arrêt propre des services, relance automatique.
- **Interface :** thèmes clair et sombre, navigation au clavier, icône de zone de notification, instance unique, installeur par utilisateur (sans droits administrateur).
- **Données :** une nouvelle installation utilise `C:\rustral-studio` ; une installation existante continue d'utiliser `C:\astral-dev`, sans déplacement. La désinstallation conserve les projets et les données.

## Feuille de route

- **Prochaine étape — bases de données :** gestion de SQLite et de MySQL avec Adminer, accessible depuis la fiche projet (création d'une base MySQL et mise à jour du `.env`).
- **Stabilisation et version 1.0.0 :** tests complets sur machine propre et avec Laragon, revue de sécurité, documentation finale, versions candidates puis `1.0.0`.
- **Publication :** dépôt public `rustral-studio` avec ses Releases et mises à jour signées.
- **Après la 1.0.0 :** accès distant (ngrok), système de workers, installation de projets Laravel et Symfony, signature de code Windows, intégration continue complète, import de l'autorité mkcert dans Firefox, autres outils (Memcached, Node, Git, Python, Go).

## Licence

[MIT](LICENSE) © 2026 astral-php
