// Integrations page: how to send alerts to Inbox from Grafana, Alertmanager
// and the usual alerting tools. Built from the live server overview so every
// URL and group id is real. Tokens are never exposed by the server.
'use strict';

(() => {
  const { $, el, icon, api, copy } = window.Inbox;

  const TOKEN = '<TOKEN>';
  const shellQuote = (s) => "'" + String(s).replace(/'/g, "'\\''") + "'";

  // ------------------------------------------------------------ building blocks

  function code(text, opts) {
    const pre = el('pre', { text });
    const btn = el('button', {
      class: 'icon-btn copy',
      type: 'button',
      title: 'Copier',
      'aria-label': 'Copier',
      onclick: () => copy(text, pre),
    }, icon('content-copy'));
    const box = el('div', { class: 'code' + (opts && opts.inline ? ' inline' : '') }, pre, btn);
    if (opts && opts.label) return el('div', { class: 'code-wrap' }, el('div', { class: 'code-label', text: opts.label }), box);
    return box;
  }

  const p = (...children) => el('p', {}, ...children);
  const note = (...children) => el('p', { class: 'note' }, ...children);
  const warn = (...children) => el('p', { class: 'warn' }, ...children);
  const c = (text) => el('code', { text });
  const h3 = (text) => el('h3', { text });
  const steps = (...items) => el('ol', {}, ...items.map((i) => el('li', {}, ...(Array.isArray(i) ? i : [i]))));

  function table(head, rows) {
    return el('table', { class: 'kv' },
      el('thead', {}, el('tr', {}, ...head.map((h) => el('th', { text: h })))),
      el('tbody', {}, ...rows.map((r) => el('tr', {}, ...r.map((cell) => el('td', {}, cell))))));
  }

  const sections = [];
  function section(id, title, tag, ...body) {
    sections.push({ id, title });
    return el('section', { id, class: 'doc-section' },
      el('div', { class: 'panel-head' }, el('h2', { text: title }), tag ? el('span', { class: 'tag', text: tag }) : null),
      el('div', { class: 'doc-body' }, ...body));
  }

  // ------------------------------------------------------------ content

  function build(data) {
    const port = data.server.api_port;
    const base = location.protocol + '//' + location.hostname + (port ? ':' + port : '');
    const groups = data.groups || [];
    const g = (groups[0] && groups[0].id) || 'mon-groupe';
    const notify = base + '/api/v1/notify/' + g;
    const notifyTok = notify + '?token=' + TOKEN;
    const am = base + '/api/v1/webhooks/alertmanager/' + g;
    const alerts = base + '/api/v1/alerts';

    $('server-name').textContent = data.server.name;
    $('server-version').textContent = 'v' + data.server.version;
    document.title = 'Intégrations · ' + data.server.name;

    const out = [];

    out.push(section('start', 'Avant de commencer', null,
      p("Adresse de l'API (port ", c(String(port)), ' de ', c('server.listen'), ", distinct de cette interface) :"),
      code(base, { inline: true }),
      note("Elle est déduite de l'adresse de cette page. Derrière un reverse proxy, remplacez-la par l'adresse publique qui mène au port de l'API."),
      p('Chaque outil s’authentifie avec un ', el('strong', { text: 'token de producteur' }),
        ' : le contenu du fichier ', c('token_file'), ' de son bloc ', c('producer'), ' (généré par ', c('inbox token'), ').',
        ' Envoyez-le dans l’en-tête ', c('Authorization: Bearer <TOKEN>'), ', ou, pour les outils qui ne savent pas ajouter d’en-tête, dans l’URL : ', c('?token=<TOKEN>'), '.'),
      warn('Un token placé dans une URL peut finir dans les journaux d’un proxy : préférez l’en-tête quand l’outil le permet.'),
      p('Une alerte vise un ', el('strong', { text: 'groupe' }), ' ; seuls les groupes autorisés pour le producteur sont acceptés.'),
      groups.length
        ? table(['Identifiant', 'Nom', 'Description'], groups.map((x) => [x.id, x.name, x.description || '—']))
        : note('Aucun groupe déclaré.'),
      data.producers && data.producers.length
        ? table(['Producteur', 'Groupes autorisés', 'Résolutions'], data.producers.map((x) => [x.name, x.groups.join(', '), x.webhook_send_resolved ? 'envoyées' : 'ignorées']))
        : note('Aucun producteur déclaré : ajoutez un bloc producer dans la configuration du serveur.')));

    out.push(section('generic', 'Webhook universel', 'POST /api/v1/notify/{groupe}',
      p('La méthode qui marche avec tout outil capable d’appeler une URL. Le corps peut être :'),
      el('ul', {},
        el('li', {}, 'du JSON avec les champs facultatifs ', c('title'), ' et ', c('message'), ' ;'),
        el('li', {}, 'n’importe quel autre JSON : il devient le message, tel quel ;'),
        el('li', {}, 'du texte brut : c’est le message.')),
      p('Le titre peut aussi venir de ', c('?title=…'), ' ou de l’en-tête ', c('X-Title'), '. Réponse ', c('201'), ' (créée) ou ', c('200'), ' (doublon déjà reçu).'),
      code(notifyTok, { inline: true, label: 'URL à coller dans l’outil' }),
      code([
        'curl -fsS -X POST ' + shellQuote(notify) + ' \\',
        '  -H "Authorization: Bearer $INBOX_TOKEN" \\',
        "  -H 'Content-Type: application/json' \\",
        '  -d ' + shellQuote(JSON.stringify({ title: 'Disque plein', message: 'db-1 : /var à 97 %' })),
      ].join('\n'), { label: 'curl, JSON' }),
      code([
        'curl -fsS -X POST ' + shellQuote(notify + '?title=Sauvegarde') + ' \\',
        '  -H "Authorization: Bearer $INBOX_TOKEN" \\',
        "  --data-binary 'La sauvegarde nocturne a échoué'",
      ].join('\n'), { label: 'curl, texte brut' }),
      note('Sans en-tête ', c('Idempotency-Key'), ', deux requêtes identiques dans la même minute ne créent qu’une alerte : un outil qui réessaie ne réveille pas deux fois. Ajoutez une clé unique par événement pour maîtriser vous-même la déduplication.')));

    out.push(section('grafana', 'Grafana', 'Contact point Webhook',
      code(am, { inline: true, label: 'URL du contact point' }),
      steps(
        ['Alerting → ', el('strong', { text: 'Contact points' }), ' → ', el('strong', { text: 'Add contact point' }), '.'],
        ['Integration : ', el('strong', { text: 'Webhook' }), ', URL : celle ci-dessus.'],
        ['HTTP Method : ', c('POST'), '.'],
        ['Optional Webhook settings → Authorization Header : Scheme ', c('Bearer'), ', Credentials : le token.'],
        ['Enregistrez, testez avec ', el('strong', { text: 'Test' }), ', puis utilisez ce contact point dans une notification policy.']),
      note('Grafana envoie le format Alertmanager : une alerte Inbox par alerte Grafana, dédupliquée sur son empreinte. Les « resolved » ne sont envoyées que si le producteur a ', c('webhook_send_resolved = true'), '.'),
      code([
        'apiVersion: 1',
        'contactPoints:',
        '  - orgId: 1',
        '    name: Inbox',
        '    receivers:',
        '      - uid: inbox',
        '        type: webhook',
        '        settings:',
        '          url: ' + am,
        '          httpMethod: POST',
        '          authorization_scheme: Bearer',
        '          authorization_credentials: ' + TOKEN,
      ].join('\n'), { label: 'Provisioning (fichier YAML)' })));

    out.push(section('alertmanager', 'Prometheus Alertmanager', 'webhook_configs',
      code([
        'receivers:',
        '  - name: inbox',
        '    webhook_configs:',
        '      - url: ' + am,
        '        send_resolved: false',
        '        http_config:',
        '          authorization:',
        '            type: Bearer',
        '            credentials: ' + TOKEN,
        '',
        'route:',
        '  receiver: inbox',
      ].join('\n'), { label: 'alertmanager.yml' }),
      note('Une alerte Inbox par alerte du groupe Alertmanager ; les répétitions et les nouvelles tentatives ne réveillent pas deux fois.')));

    out.push(section('uptime-kuma', 'Uptime Kuma', 'Notification Webhook',
      code(notifyTok, { inline: true, label: 'Post URL' }),
      steps(
        ['Paramètres → Notifications → ', el('strong', { text: 'Configurer une notification' }), ', type ', el('strong', { text: 'Webhook' }), '.'],
        ['Post URL : celle ci-dessus.'],
        ['Request Body : ', el('strong', { text: 'Custom Body' }), ', avec le corps ci-dessous.'],
        ['Additional Headers (facultatif, à la place du ', c('?token='), ') : le JSON d’en-têtes ci-dessous.']),
      code('{ "title": "Uptime Kuma", "message": "{{msg}}" }', { label: 'Custom Body' }),
      code('{ "Authorization": "Bearer ' + TOKEN + '" }', { label: 'Additional Headers' })));

    out.push(section('zabbix', 'Zabbix', 'Type de média Webhook',
      steps(
        ['Alertes → Types de média → ', el('strong', { text: 'Créer un type de média' }), ', type ', el('strong', { text: 'Webhook' }), '.'],
        ['Paramètres : ', c('url'), ' = l’URL ci-dessous, ', c('token'), ' = le token, ', c('subject'), ' = ', c('{ALERT.SUBJECT}'), ', ', c('message'), ' = ', c('{ALERT.MESSAGE}'), '.'],
        ['Script : le code ci-dessous.'],
        ['Associez le média à un utilisateur et à une action de déclenchement.']),
      code(notify, { inline: true }),
      code([
        'var p = JSON.parse(value);',
        'var req = new HttpRequest();',
        "req.addHeader('Content-Type: application/json');",
        "req.addHeader('Authorization: Bearer ' + p.token);",
        'var resp = req.post(p.url, JSON.stringify({ title: p.subject, message: p.message }));',
        'if (req.getStatus() >= 300) {',
        "  throw 'Inbox : HTTP ' + req.getStatus() + ' ' + resp;",
        '}',
        "return 'OK';",
      ].join('\n'), { label: 'Script' })));

    out.push(section('gatus', 'Gatus', 'alerting.custom',
      code([
        'alerting:',
        '  custom:',
        '    url: "' + notify + '"',
        '    method: "POST"',
        '    headers:',
        '      Authorization: "Bearer ' + TOKEN + '"',
        '      Content-Type: "application/json"',
        '    body: |',
        '      { "title": "[ALERT_TRIGGERED_OR_RESOLVED] [ENDPOINT_NAME]",',
        '        "message": "[ALERT_DESCRIPTION]" }',
        '',
        'endpoints:',
        '  - name: api',
        '    url: "https://example.org/health"',
        '    alerts:',
        '      - type: custom',
      ].join('\n'), { label: 'config.yaml' })));

    out.push(section('healthchecks', 'Healthchecks.io', 'Intégration Webhook',
      steps(
        ['Intégrations → ', el('strong', { text: 'Webhook' }), '.'],
        ['Pour l’événement « down » : méthode ', c('POST'), ', l’URL ci-dessous, corps : ', c('$NAME est en panne ($STATUS)'), '.'],
        ['Laissez l’événement « up » vide, sauf si vous voulez aussi être prévenu du retour à la normale.']),
      code(notify + '?title=Healthchecks&token=' + TOKEN, { inline: true })));

    out.push(section('shell', 'Nagios, Icinga, Checkmk, Netdata', 'Commande de notification',
      p('Ces outils lancent une commande à chaque notification : appelez ', c('curl'), ' avec le message en texte brut.'),
      code([
        'define command {',
        '  command_name  notify-inbox',
        '  command_line  /usr/bin/curl -fsS -m 10 -X POST \\',
        '    -H "Authorization: Bearer ' + TOKEN + '" \\',
        '    --data-binary "$NOTIFICATIONTYPE$ $HOSTNAME$ $SERVICEDESC$ : $SERVICEOUTPUT$" \\',
        '    "' + notify + '?title=Nagios"',
        '}',
      ].join('\n'), { label: 'Nagios / Icinga 1 (commands.cfg)' }),
      code([
        '#!/bin/sh',
        '# ~/local/share/check_mk/notifications/inbox (chmod +x), puis règle de notification « inbox »',
        'curl -fsS -m 10 -X POST \\',
        '  -H "Authorization: Bearer ' + TOKEN + '" \\',
        '  --data-binary "$NOTIFY_HOSTNAME $NOTIFY_SERVICEDESC : $NOTIFY_SERVICEOUTPUT$NOTIFY_HOSTOUTPUT" \\',
        '  "' + notify + '?title=Checkmk"',
      ].join('\n'), { label: 'Checkmk (script de notification)' }),
      note('Netdata, Icinga 2 et les autres outils à base de scripts suivent le même principe : un ', c('curl'), ' vers l’URL universelle.')));

    out.push(section('home-assistant', 'Home Assistant', 'rest_command',
      code([
        'rest_command:',
        '  inbox:',
        '    url: "' + notify + '"',
        '    method: post',
        '    headers:',
        '      Authorization: "Bearer ' + TOKEN + '"',
        '    content_type: "application/json"',
        "    payload: '{\"title\": \"{{ title }}\", \"message\": \"{{ message }}\"}'",
      ].join('\n'), { label: 'configuration.yaml' }),
      note('Appel depuis une automatisation : service ', c('rest_command.inbox'), ' avec ', c('title'), ' et ', c('message'), '.')));

    out.push(section('workflow', 'n8n, Node-RED, Make, Zapier', 'Requête HTTP',
      steps(
        ['Nœud ', el('strong', { text: 'HTTP Request' }), ', méthode ', c('POST'), ', URL ci-dessous.'],
        ['En-tête ', c('Authorization'), ' = ', c('Bearer <TOKEN>'), ' (ou ', c('?token='), ' dans l’URL).'],
        ['Corps JSON : ', c('{"title": "…", "message": "…"}'), '.']),
      code(notify, { inline: true })));

    out.push(section('scripts', 'Scripts et cron', null,
      code([
        '#!/bin/sh',
        '# Prévenir si une sauvegarde échoue',
        'if ! /usr/local/bin/backup.sh; then',
        '  curl -fsS -m 10 -X POST ' + shellQuote(notify + '?title=Sauvegarde') + ' \\',
        '    -H "Authorization: Bearer $INBOX_TOKEN" \\',
        '    --data-binary "La sauvegarde de $(hostname) a échoué"',
        'fi',
      ].join('\n'), { label: 'Shell' }),
      code([
        'Invoke-RestMethod -Method Post -Uri "' + notify + '" `',
        '  -Headers @{ Authorization = "Bearer $env:INBOX_TOKEN" } `',
        '  -ContentType "application/json" `',
        "  -Body '{\"title\":\"Windows\",\"message\":\"Service arrêté\"}'",
      ].join('\n'), { label: 'PowerShell' })));

    out.push(section('reference', 'Référence', null,
      table(['Point d’entrée', 'Usage'], [
        ['POST /api/v1/notify/{groupe}', 'Webhook universel : JSON {title, message}, autre JSON ou texte brut.'],
        ['POST /api/v1/webhooks/alertmanager/{groupe}', 'Format Prometheus Alertmanager, utilisé aussi par Grafana.'],
        ['POST /api/v1/alerts', 'API JSON : {"group", "title"?, "message"?}.'],
        ['GET /api/v1/alerts/{id}', 'Réception par appareil d’une alerte donnée.'],
      ]),
      table(['Code', 'Signification'], [
        ['201', 'Alerte enregistrée sur disque et programmée pour chaque appareil abonné.'],
        ['200', 'Doublon : alerte déjà reçue, rien n’est renvoyé aux téléphones.'],
        ['400', 'Requête invalide (JSON, en-tête).'],
        ['401', 'Token absent ou invalide.'],
        ['403', 'Groupe non autorisé pour ce producteur.'],
        ['409', 'Même clé d’idempotence, contenu différent.'],
        ['413 / 422', 'Alerte trop longue, groupe inconnu ou caractères de contrôle.'],
        ['503', 'Stockage indisponible : réessayez.'],
      ]),
      note('Un téléphone hors ligne reçoit l’alerte à sa reconnexion. Une alerte qu’un téléphone n’a pas reçue dans le délai ', c('storage.delivery_ttl'), ' du serveur expire pour lui et n’est plus envoyée.')));

    const doc = $('doc');
    doc.replaceChildren(doc.firstElementChild, ...out);
    $('doc-nav').replaceChildren(
      el('span', { class: 'nav-label', text: 'Sommaire' }),
      ...sections.map((s) => el('a', { href: '#' + s.id, 'data-target': s.id, text: s.title })));
    watchActive();
    if (location.hash) {
      const target = document.getElementById(location.hash.slice(1));
      if (target) target.scrollIntoView();
    }
  }

  function watchActive() {
    if (!('IntersectionObserver' in window)) return;
    const links = new Map([...document.querySelectorAll('#doc-nav a')].map((a) => [a.dataset.target, a]));
    const io = new IntersectionObserver((entries) => {
      for (const e of entries) {
        if (!e.isIntersecting) continue;
        for (const a of links.values()) a.classList.remove('active');
        const a = links.get(e.target.id);
        if (a) a.classList.add('active');
      }
    }, { rootMargin: '-50px 0px -70% 0px' });
    for (const id of links.keys()) {
      const s = document.getElementById(id);
      if (s) io.observe(s);
    }
  }

  (async () => {
    try {
      build(await api('GET', 'api/admin/overview'));
    } catch (e) {
      if (e.code === 'host_not_allowed') return;
      const box = $('global-error');
      box.replaceChildren(icon('alert-circle-outline'), el('span', { text: 'Serveur injoignable : ' + e.message }));
      box.hidden = false;
    }
  })();
})();
