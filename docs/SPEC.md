# Inbox — cahier des charges

Document de référence des décisions prises au lancement du projet (octobre 2026).
Le code fait foi pour les détails ; ce document fixe le *quoi* et le *pourquoi*.

## 1. Objet

Inbox est un système d'alerte (inbox) auto-hébergé. Un serveur unique reçoit des
alertes et les fait sonner sur des téléphones Android abonnés. Une alerte peut
être vitale : **chaque appareil abonné doit la recevoir, et une seule fois**,
y compris s'il était éteint, en mode avion ou si l'app était coupée au moment de
l'émission.

## 2. Périmètre

| Inclus | Exclu (v1) |
|---|---|
| Binaire serveur Rust unique, front admin embarqué | iOS (impossible sans APNs, donc sans dépendance à Apple) |
| App Android (réception uniquement) | Émission d'alerte depuis l'app |
| Image conteneur, docker compose, manifestes k3s | Haute disponibilité / multi-nœuds |
| API d'émission + webhook Alertmanager / Grafana | Escalade automatique entre personnes |
| Télémétrie OpenTelemetry (traces, métriques, logs) | Plugin Grafana (les contact points ne sont pas extensibles) |

## 3. Exigences fonctionnelles

### 3.1 Connexion d'un téléphone
- L'utilisateur saisit l'adresse du serveur (IP:port, nom de domaine, URL
  `http(s)://`), le **mot de passe serveur partagé** et un **nom d'appareil**.
- Une fois connecté, l'app affiche les groupes déclarés sur le serveur.
- Pour chaque groupe, l'utilisateur choisit une **action** (ou ne s'abonne pas). Ce
  choix est **local à chaque appareil** (décision du 2026-10-02, protocole v2) : le
  serveur sait seulement quels groupes un appareil suit, jamais son niveau ; un même
  pseudo peut avoir plusieurs appareils réglés différemment :

| Action | Comportement Android |
|---|---|
| `alarm` | Sonnerie d'alarme en boucle, volume alarme au maximum, sort du silencieux / vibreur, passe le « Ne pas déranger », écran plein écran ; un gros bouton central (à glisser hors du cercle, comme les alarmes Samsung) coupe le son sur le téléphone |
| `ring` | Un son de notification **même en silencieux ou vibreur** |
| `notify` | Notification standard, respecte le mode du téléphone |

- Les alertes ne portent pas de sévérité : c'est l'abonné qui décide de la
  réaction, groupe par groupe.
- L'app ne peut **pas** émettre d'alerte : le serveur n'expose aucune route
  d'émission à une identité d'appareil.
- Les alertes sont **à sens unique** (décision du 2026-10-02) : pas d'acquittement,
  le téléphone ne renvoie rien à l'humain ni à l'émetteur. Seul un accusé de
  réception technique et automatique part du téléphone une fois l'alerte stockée :
  sans lui le serveur ne pourrait ni retransmettre ni rattraper un téléphone
  éteint, et la garantie de livraison tomberait.
- L'écran d'accueil affiche les dernières alertes reçues : par défaut les 5 plus
  récentes de moins de 24 h, les deux réglables. Menu burger vers les serveurs
  et les réglages ; thème clair / sombre du téléphone par défaut.
- Plusieurs serveurs Inbox peuvent être ajoutés et gérés dans l'app (une
  connexion et des abonnements par serveur).
- L'utilisateur s'identifie par un **pseudo** (décision du 2026-10-02) : un pseudo
  par défaut dans les réglages de l'app, modifiable serveur par serveur à tout
  moment. Le serveur l'enregistre avec l'appareil et dans chaque connexion, ce qui
  permet de suivre les sessions par identifiant dans le front admin.

### 3.2 Alerte
- Une alerte se compose d'un **groupe** et d'un **titre** (une ligne, 128 caractères
  max), tous deux obligatoires : ni description ni détails.
- Sources : API HTTP avec token d'émetteur (déclaré en HCL, limité à des groupes),
  formulaire du front admin, webhook compatible Alertmanager (utilisé aussi par le
  contact point « Webhook » de Grafana).
- Clé d'idempotence recommandée (en-tête `Idempotency-Key`) ; sans elle, une requête
  identique reçue dans la même minute est dédupliquée. Un émetteur qui réessaie ne
  crée donc pas de doublon.
- Intégration avec n'importe quel outil d'alerting (décision du 2026-10-02) :
  endpoint générique `/api/v1/notify/<groupe>` (texte ou JSON quelconque), token
  possible dans l'URL (`?token=`) pour les outils sans en-têtes ; la page `/doc` du
  front donne les recettes (Grafana, Alertmanager et outils courants).
- Le front admin fonctionne derrière un ou plusieurs reverse proxys, à la racine
  ou sous n'importe quel sous-chemin, sans configuration supplémentaire.

### 3.3 Front admin (supervision)
- Appareils : nom, statut (en ligne / hors ligne / injoignable / révoqué), IP
  réelle (derrière proxys de confiance), version de l'app, connecté depuis,
  dernier signe de vie, abonnements, alertes en attente.
- Historique des connexions et des alertes, détail de livraison par appareil
  (envoyée, reçue, nombre d'envois).
- Accueil : les dernières alertes (5 de moins de 24 h par défaut, réglable),
  menu burger, thème du système par défaut, charte graphique de S3-Browser.
- Révocation d'un appareil, envoi manuel d'une alerte.
- Aucune configuration modifiable : la configuration est déclarative (HCL).
- **Pas de mot de passe** (décision du 2026-10-01) : l'accès au front admin est
  contrôlé par l'endroit où il écoute (localhost, réseau privé) ou par un reverse
  proxy authentifiant placé devant.

## 4. Garantie de livraison

Principe : transport « au moins une fois », déduplication sur le téléphone,
donc **affichage exactement une fois**.

1. L'API ne répond « acceptée » qu'une fois l'alerte **et** une ligne de livraison
   par appareil abonné écrites sur disque (SQLite WAL, `synchronous = FULL`), dans
   une seule transaction.
2. Les appareils ciblés sont ceux abonnés à l'instant de l'acceptation, qu'ils
   soient en ligne ou non.
3. Le serveur renvoie toute livraison non confirmée « reçue » : en continu
   pendant la session (`resend_after`) et à chaque reconnexion.
4. Le téléphone enregistre l'alerte durablement **avant** de confirmer la
   réception, déduplique sur l'identifiant d'alerte (128 bits aléatoires), et
   réaffiche au redémarrage ce qui a été reçu mais pas affiché.
5. Une alerte non reçue par un téléphone reste en mémoire et est renvoyée pendant
   `storage.delivery_ttl` (7 jours, décision du 2026-10-02), puis expire pour ce
   téléphone et apparaît « expirée » dans le front admin. L'historique est purgé
   après `storage.alert_retention`, qui doit être ≥ `delivery_ttl`.
6. Visibilité des trous de couverture : appareil abonné injoignable signalé dans
   le front et par la métrique `inbox_devices_unreachable` ; côté téléphone,
   notification permanente de connexion et avertissement après
   `offline_warning_after`.
7. Reconnexion immédiate sur retour réseau, au boot, et après un redémarrage du
   serveur (code 1012).

Limites assumées : nœud unique (rien n'est perdu si le serveur tombe, mais rien
n'est livré tant qu'il ne redémarre pas) ; « Forcer l'arrêt » et certains
gestionnaires de batterie constructeurs coupent l'app (l'app guide vers
l'exemption batterie, le serveur signale l'appareil injoignable).

## 5. Sécurité

Objectif : sur HTTP en clair, le mot de passe ne transite jamais et une capture
réseau ne permet aucune attaque hors ligne.

- **Enrôlement** : OPAQUE (PAKE). Le mot de passe ne quitte pas le téléphone ;
  sans l'enregistrement OPAQUE du serveur, un attaquant actif ne peut ni se faire
  passer pour le serveur ni enrôler un appareil.
- **Sessions** : Noise IK entre la clé générée par le téléphone et la clé serveur
  épinglée à l'enrôlement — authentification mutuelle, chiffrement, confidentialité
  persistante. Le mot de passe n'est pas stocké sur le téléphone ; chaque appareil
  est révocable.
- **Front admin** : **sans mot de passe** (décision du 2026-10-01). Quiconque
  atteint `server.admin_listen` peut envoyer des alertes et révoquer des
  appareils : il doit donc écouter sur localhost / un réseau privé, ou être
  derrière un reverse proxy authentifiant (ex. Traefik forward-auth). Protections
  conservées contre les pages web hostiles ouvertes dans le navigateur de
  l'opérateur :
  - `Host` limité à `admin.allowed_hosts` (anti DNS rebinding) ;
  - tout `POST` exige `Content-Type: application/json` et `X-Inbox-Request: 1`
    (anti-CSRF : impossible depuis une autre origine sans pré-requête CORS, que
    Inbox n'accorde jamais) ;
  - requêtes `Sec-Fetch-Site: cross-site` refusées.
- Limitation des tentatives de mot de passe appareil par adresse (chaque tentative
  démarrée compte, car OPAQUE détecte le mauvais mot de passe côté téléphone),
  tokens d'émetteur comparés en temps constant, en-têtes de sécurité (CSP
  stricte), image `scratch` non-root.
- TLS est assuré par un reverse proxy (Traefik…) ; l'app accepte `https://`.

## 6. Exigences techniques

- **Rust**, un binaire statique (musl) unique ; front embarqué dans le binaire.
- **Standalone** : aucune dépendance à un autre serveur (pas de FCM, pas d'APNs,
  pas de base externe). La télémétrie est optionnelle et ne bloque jamais le service.
- **Configuration HCL déclarative et stricte** : tout paramètre est obligatoire,
  attribut inconnu refusé, contrôles sémantiques au démarrage, secrets uniquement
  via fichiers (`*_file`), aucune variable d'environnement. Rien ne démarre s'il
  manque un paramètre.
- **Stockage** SQLite (un seul thread écrivain, migrations embarquées et versionnées).
- **OpenTelemetry** : traces, métriques (`inbox_*`) et logs en OTLP/HTTP.
- **Peu de dépendances** : crates éprouvées pour la crypto (opaque-ke, snow), app
  Android sur les API de la plateforme + OkHttp, cœur Rust partagé via JNI écrit
  à la main, front en JavaScript natif sans npm.
- **Livraison** : un seul dépôt GitHub (`SCcagg5/Inbox`, privé) ; une release
  GitHub publie les binaires Linux amd64/arm64, l'APK signé et `SHA256SUMS` ;
  image conteneur `FROM scratch` construite localement, **jamais publiée sur un
  registre** (GHCR ou autre) ; `docker compose` prêt à l'emploi ; manifestes k3s
  inspirés du dépôt Home.

## 7. Architecture

```
crates/inbox-core     protocole + crypto sans I/O (OPAQUE, Noise, messages)
crates/inbox-server   binaire `inbox` (axum, SQLite, OTel, front embarqué)
crates/inbox-android  pont JNI vers inbox-core
web/                   front admin (HTML/CSS/JS natif, sans mot de passe)
android/               app Android (Kotlin, Views, SQLite plateforme, OkHttp)
deploy/                docker compose, k3s
```

## 8. Intégration Grafana

Les contact points de Grafana sont intégrés au code de Grafana : il n'existe pas
d'API de plugin pour en ajouter. L'intégration passe donc par le contact point
« Webhook » de Grafana (format compatible Alertmanager) vers
`POST /api/v1/webhooks/alertmanager/<groupe>` avec un token d'émetteur. Exemple
de provisioning : [`docs/grafana/contact-point.yaml`](grafana/contact-point.yaml).
