// Integrations page: how to send alerts to Inbox from Grafana, Alertmanager
// and the usual alerting tools. Built from the live server overview so every
// URL and group id is real. Tokens are never exposed by the server.
'use strict';

(() => {
  const { $, el, icon, api, copy } = window.Inbox;

  const TOKEN = '<TOKEN>';
  const shellQuote = (s) => "'" + String(s).replace(/'/g, "'\\''") + "'";

  // ------------------------------------------------------------ syntax highlighting

  // One ordered rule list per language (first match wins at a given position).
  // Rules carry no capture group: they are joined into a single regex. Output is
  // DOM nodes and classes only: no innerHTML, no inline style (CSP).
  const URL = /["']?https?:\/\/[^\s'"\\]+["']?/;
  const RULES = {
    url: [['url', /https?:\/\/\S+/]],
    sh: [
      ['comment', /#.*/],
      ['url', URL],
      ['string', /'(?:[^']|'\\'')*'|"(?:\\.|[^"\\])*"/],
      ['keyword', /\b(?:curl|if|then|fi|define|command_name|command_line|Invoke-RestMethod)\b/],
      ['key', /(?:^|\s)--?[A-Za-z][\w-]*/],
      ['value', /\$[A-Za-z_][\w:]*|\$\{[^}]+\}/],
    ],
    yaml: [
      ['comment', /#.*/],
      ['url', URL],
      ['key', /"(?:\\.|[^"\\])*"(?=\s*:)/],
      ['string', /"(?:\\.|[^"\\])*"|'[^']*'/],
      ['key', /[\w.-]+(?=:(?:\s|$))/],
      ['value', /\b(?:true|false|null)\b|-?\b\d+\b/],
    ],
    json: [
      ['key', /"(?:\\.|[^"\\])*"(?=\s*:)/],
      ['string', /"(?:\\.|[^"\\])*"/],
      ['value', /-?\b\d+(?:\.\d+)?\b|\b(?:true|false|null)\b/],
    ],
    js: [
      ['comment', /\/\/.*/],
      ['string', /'(?:\\.|[^'\\])*'|"(?:\\.|[^"\\])*"/],
      ['keyword', /\b(?:var|new|if|else|throw|return|function)\b/],
      ['key', /\b[A-Za-z_]\w*(?=\()/],
      ['value', /\b\d+\b/],
    ],
  };
  const compiled = {};

  // <TOKEN> stands out wherever it sits: it is the one thing to replace.
  function mark(text) {
    return text.split(TOKEN).flatMap((part, i) =>
      i === 0 ? [part] : [el('span', { class: 'tk-ph', text: TOKEN }), part]);
  }

  function highlight(text, lang) {
    const rules = RULES[lang];
    if (!rules) return mark(text);
    const re = compiled[lang] || (compiled[lang] = new RegExp(rules.map((r) => '(' + r[1].source + ')').join('|'), 'gm'));
    re.lastIndex = 0;
    const out = [];
    let last = 0;
    for (let m = re.exec(text); m; m = re.exec(text)) {
      if (m[0] === '') { re.lastIndex++; continue; }
      if (m.index > last) out.push(...mark(text.slice(last, m.index)));
      const kind = rules[m.findIndex((g, i) => i > 0 && g !== undefined) - 1][0];
      out.push(el('span', { class: 'tk-' + kind }, ...mark(m[0])));
      last = re.lastIndex;
    }
    if (last < text.length) out.push(...mark(text.slice(last)));
    return out;
  }

  // ------------------------------------------------------------ building blocks

  // opts: lang (url, sh, yaml, json, js), label, inline.
  function code(text, opts) {
    const pre = el('pre', {}, ...highlight(text, opts && opts.lang));
    const btn = el('button', {
      class: 'icon-btn copy',
      type: 'button',
      title: 'Copy',
      'aria-label': 'Copy',
      onclick: () => copy(text, pre),
    }, icon('content-copy'));
    const box = el('div', { class: 'code' + (opts && opts.inline ? ' inline' : '') }, pre, btn);
    if (opts && opts.label) return el('div', { class: 'code-wrap' }, el('div', { class: 'code-label', text: opts.label }), box);
    return box;
  }

  const p = (...children) => el('p', {}, ...children.flat());
  const note = (...children) => el('p', { class: 'note' }, ...children);
  const warn = (...children) => el('p', { class: 'warn' }, ...children);
  const c = (text) => el('code', { text });
  const b = (text) => el('strong', { text });
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

  // The producer → group pair every recipe is written for. Only allowed pairs
  // are offered: a recipe never targets a group its token would get a 403 for.
  function pairs(data) {
    const names = new Map((data.groups || []).map((g) => [g.id, g.name]));
    const out = [];
    for (const pr of data.producers || []) {
      for (const g of pr.groups) out.push({ producer: pr.name, group: g, label: pr.name + ' → ' + (names.get(g) || g) });
    }
    return out;
  }

  function picker(data, list, current, onPick) {
    if (list.length === 0) {
      return el('div', { class: 'doc-pick' },
        el('p', { class: 'warn', text: 'No producer declared: add a producer block to the server configuration, then restart Inbox. The recipes below use a placeholder group.' }));
    }
    const sel = el('select', { id: 'doc-pair', 'aria-describedby': 'doc-pair-note' },
      ...(data.producers || []).map((pr) => el('optgroup', { label: pr.name },
        ...list.filter((x) => x.producer === pr.name).map((x) => {
          const o = el('option', { value: list.indexOf(x), text: x.label });
          if (x === current) o.selected = true;
          return o;
        }))));
    sel.addEventListener('change', () => onPick(list[Number(sel.value)]));
    return el('div', { class: 'doc-pick' },
      el('label', { for: 'doc-pair', text: 'Producer → group' }),
      sel,
      el('span', { id: 'doc-pair-note', class: 'note muted', text: 'Every URL and snippet below targets this group, with the token of this producer.' }));
  }

  function build(data, current) {
    sections.length = 0;
    const port = data.server.api_port;
    const base = location.protocol + '//' + location.hostname + (port ? ':' + port : '');
    const groups = data.groups || [];
    const g = current ? current.group : (groups[0] && groups[0].id) || 'my-group';
    const notify = base + '/api/v1/notify/' + g;
    const notifyTok = notify + '?token=' + TOKEN;
    const am = base + '/api/v1/webhooks/alertmanager/' + g;
    const alerts = base + '/api/v1/alerts';

    const out = [];

    out.push(section('start', 'Before you start', null,
      p('API address (port ', c(String(port)), ' of ', c('server.listen'), ', not the port of this console):'),
      code(base, { inline: true, lang: 'url' }),
      note('It is derived from the address of this page. Behind a reverse proxy, use the public address that leads to the API port instead.'),
      p('Each tool authenticates with a ', b('producer token'), ': the content of the ', c('token_file'), ' of its ', c('producer'), ' block (generated by ', c('inbox token'), ').',
        ' Send it in the ', c('Authorization: Bearer <TOKEN>'), ' header or, for tools that cannot add headers, in the URL: ', c('?token=<TOKEN>'), '.',
        current ? [' Below, ', c(TOKEN), ' is the token of ', b(current.producer), '.'] : null),
      warn('A token in a URL may end up in proxy logs: prefer the header when the tool supports it.'),
      p('An alert has a ', b('group'), ' and a ', b('title'), ': one line of at most 128 characters, shown in the notification and in this console.'),
      p('Only the groups allowed for the producer are accepted. Phones choose themselves how they react to each group.'),
      groups.length
        ? table(['Id', 'Name', 'Description'], groups.map((x) => [x.id, x.name, x.description || '—']))
        : note('No group declared.'),
      data.producers && data.producers.length
        ? table(['Producer', 'Allowed groups', 'Resolved alerts'], data.producers.map((x) => [x.name, x.groups.join(', '), x.webhook_send_resolved ? 'sent' : 'ignored']))
        : note('No producer declared: add a producer block to the server configuration.')));

    out.push(section('generic', 'Universal webhook', 'POST /api/v1/notify/{group}',
      p('Works with any tool that can call a URL. The body can be:'),
      el('ul', {},
        el('li', {}, 'JSON with a ', c('title'), ' field;'),
        el('li', {}, 'plain text, or any other JSON: it becomes the title (on one line, truncated to 128 characters).')),
      p('The title can also come from ', c('?title=…'), ' or the ', c('X-Title'), ' header, which win over the body. A request without any title is refused (', c('422'), '). Answers ', c('201'), ' (created) or ', c('200'), ' (duplicate already received).'),
      code(notifyTok, { inline: true, lang: 'url', label: 'URL to paste into the tool' }),
      code([
        'curl -fsS -X POST ' + shellQuote(notify) + ' \\',
        '  -H "Authorization: Bearer $INBOX_TOKEN" \\',
        "  -H 'Content-Type: application/json' \\",
        '  -d ' + shellQuote(JSON.stringify({ title: 'Disk full on db-1: /var at 97 %' })),
      ].join('\n'), { lang: 'sh', label: 'curl, JSON' }),
      code([
        'curl -fsS -X POST ' + shellQuote(notify) + ' \\',
        '  -H "Authorization: Bearer $INBOX_TOKEN" \\',
        "  --data-binary 'The nightly backup failed'",
      ].join('\n'), { lang: 'sh', label: 'curl, plain text' }),
      note('Without an ', c('Idempotency-Key'), ' header, two identical requests within the same minute create a single alert: a tool that retries does not page twice. Send a unique key per event to control deduplication yourself.')));

    out.push(section('grafana', 'Grafana', 'Webhook contact point',
      code(am, { inline: true, lang: 'url', label: 'Contact point URL' }),
      steps(
        ['Alerting → ', b('Contact points'), ' → ', b('Add contact point'), '.'],
        ['Integration: ', b('Webhook'), ', URL: the one above.'],
        ['HTTP Method: ', c('POST'), '.'],
        ['Optional Webhook settings → Authorization Header: Scheme ', c('Bearer'), ', Credentials: the token.'],
        ['Save, check with ', b('Test'), ', then use this contact point in a notification policy.']),
      note('Grafana sends the Alertmanager format: one Inbox alert per Grafana alert, deduplicated on its fingerprint. The title is ', c('[FIRING] <alertname>'), ' followed by the ', c('summary'), ' annotation when there is one. Resolved alerts are only sent if the producer has ', c('webhook_send_resolved = true'), '.'),
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
      ].join('\n'), { lang: 'yaml', label: 'Provisioning (YAML file)' })));

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
      ].join('\n'), { lang: 'yaml', label: 'alertmanager.yml' }),
      note('One Inbox alert per alert of the Alertmanager group; repeats and retries do not page twice. Title: ', c('[FIRING] <alertname>'), ' followed by the ', c('summary'), ' annotation when there is one.')));

    out.push(section('uptime-kuma', 'Uptime Kuma', 'Webhook notification',
      code(notifyTok, { inline: true, lang: 'url', label: 'Post URL' }),
      steps(
        ['Settings → Notifications → ', b('Setup Notification'), ', type ', b('Webhook'), '.'],
        ['Post URL: the one above.'],
        ['Request Body: ', b('Custom Body'), ', with the body below.'],
        ['Additional Headers (optional, instead of ', c('?token='), '): the header JSON below.']),
      code('{ "title": "Uptime Kuma: {{ msg }}" }', { lang: 'json', label: 'Custom Body' }),
      code('{ "Authorization": "Bearer ' + TOKEN + '" }', { lang: 'json', label: 'Additional Headers' })));

    out.push(section('zabbix', 'Zabbix', 'Webhook media type',
      steps(
        ['Alerts → Media types → ', b('Create media type'), ', type ', b('Webhook'), '.'],
        ['Parameters: ', c('url'), ' = the URL below, ', c('token'), ' = the token, ', c('subject'), ' = ', c('{ALERT.SUBJECT}'), '.'],
        ['Script: the code below.'],
        ['Assign the media to a user and to a trigger action.']),
      code(notify, { inline: true, lang: 'url' }),
      code([
        'var p = JSON.parse(value);',
        'var req = new HttpRequest();',
        "req.addHeader('Content-Type: application/json');",
        "req.addHeader('Authorization: Bearer ' + p.token);",
        'var resp = req.post(p.url, JSON.stringify({ title: p.subject }));',
        'if (req.getStatus() >= 300) {',
        "  throw 'Inbox: HTTP ' + req.getStatus() + ' ' + resp;",
        '}',
        "return 'OK';",
      ].join('\n'), { lang: 'js', label: 'Script' })));

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
        '      { "title": "[ALERT_TRIGGERED_OR_RESOLVED] [ENDPOINT_NAME]: [ALERT_DESCRIPTION]" }',
        '',
        'endpoints:',
        '  - name: api',
        '    url: "https://example.org/health"',
        '    alerts:',
        '      - type: custom',
      ].join('\n'), { lang: 'yaml', label: 'config.yaml' })));

    out.push(section('healthchecks', 'Healthchecks.io', 'Webhook integration',
      steps(
        ['Integrations → ', b('Webhook'), '.'],
        ['For the "down" event: method ', c('POST'), ', the URL below, body: ', c('$NAME is down ($STATUS)'), '.'],
        ['Leave the "up" event empty, unless you also want to be told when it recovers.']),
      code(notify + '?token=' + TOKEN, { inline: true, lang: 'url' })));

    out.push(section('shell', 'Nagios, Icinga, Checkmk, Netdata', 'Notification command',
      p('These tools run a command for each notification: call ', c('curl'), ' with a short text as the plain-text body: it becomes the title.'),
      code([
        'define command {',
        '  command_name  notify-inbox',
        '  command_line  /usr/bin/curl -fsS -m 10 -X POST \\',
        '    -H "Authorization: Bearer ' + TOKEN + '" \\',
        '    --data-binary "$NOTIFICATIONTYPE$ $HOSTNAME$ $SERVICEDESC$: $SERVICEOUTPUT$" \\',
        '    "' + notify + '"',
        '}',
      ].join('\n'), { lang: 'sh', label: 'Nagios / Icinga 1 (commands.cfg)' }),
      code([
        '#!/bin/sh',
        '# ~/local/share/check_mk/notifications/inbox (chmod +x), then a notification rule "inbox"',
        'curl -fsS -m 10 -X POST \\',
        '  -H "Authorization: Bearer ' + TOKEN + '" \\',
        '  --data-binary "$NOTIFY_HOSTNAME $NOTIFY_SERVICEDESC: $NOTIFY_SERVICEOUTPUT$NOTIFY_HOSTOUTPUT" \\',
        '  "' + notify + '"',
      ].join('\n'), { lang: 'sh', label: 'Checkmk (notification script)' }),
      note('Netdata, Icinga 2 and the other script-based tools work the same way: a ', c('curl'), ' to the universal URL.')));

    out.push(section('home-assistant', 'Home Assistant', 'rest_command',
      code([
        'rest_command:',
        '  inbox:',
        '    url: "' + notify + '"',
        '    method: post',
        '    headers:',
        '      Authorization: "Bearer ' + TOKEN + '"',
        '    content_type: "application/json"',
        "    payload: '{\"title\": \"{{ title }}\"}'",
      ].join('\n'), { lang: 'yaml', label: 'configuration.yaml' }),
      note('Call it from an automation: service ', c('rest_command.inbox'), ' with ', c('title'), '.')));

    out.push(section('workflow', 'n8n, Node-RED, Make, Zapier', 'HTTP request',
      steps(
        [b('HTTP Request'), ' node, method ', c('POST'), ', URL below.'],
        ['Header ', c('Authorization'), ' = ', c('Bearer <TOKEN>'), ' (or ', c('?token='), ' in the URL).'],
        ['JSON body: ', c('{"title": "…"}'), '.']),
      code(notify, { inline: true, lang: 'url' })));

    out.push(section('scripts', 'Scripts and cron', null,
      code([
        '#!/bin/sh',
        '# Page when a backup fails',
        'if ! /usr/local/bin/backup.sh; then',
        '  curl -fsS -m 10 -X POST ' + shellQuote(notify) + ' \\',
        '    -H "Authorization: Bearer $INBOX_TOKEN" \\',
        '    --data-binary "The backup of $(hostname) failed"',
        'fi',
      ].join('\n'), { lang: 'sh', label: 'Shell' }),
      code([
        'Invoke-RestMethod -Method Post -Uri "' + notify + '" `',
        '  -Headers @{ Authorization = "Bearer $env:INBOX_TOKEN" } `',
        '  -ContentType "application/json" `',
        "  -Body '{\"title\":\"Windows: service stopped\"}'",
      ].join('\n'), { lang: 'sh', label: 'PowerShell' })));

    out.push(section('reference', 'Reference', null,
      table(['Endpoint', 'Use'], [
        ['POST /api/v1/notify/{group}', 'Universal webhook: JSON {title}, ?title=, X-Title, or plain text as the title.'],
        ['POST /api/v1/webhooks/alertmanager/{group}', 'Prometheus Alertmanager format, also used by Grafana.'],
        ['POST ' + alerts.replace(base, ''), 'JSON API: {"group", "title"}.'],
        ['GET /api/v1/alerts/{id}', 'Delivery per device for one alert.'],
      ]),
      table(['Field', 'Content'], [
        ['group', 'Id of the group to page (see above).'],
        ['title', 'One line shown in the notification, at most 128 characters. Required.'],
      ]),
      table(['Code', 'Meaning'], [
        ['201', 'Alert written to disk and scheduled for every device following the group.'],
        ['200', 'Duplicate: alert already received, nothing is sent again to the phones.'],
        ['400', 'Invalid request (JSON, header).'],
        ['401', 'Missing or invalid token.'],
        ['403', 'Group not allowed for this producer.'],
        ['409', 'Same idempotency key, different content.'],
        ['413 / 422', 'Title too long, missing or with control characters; unknown group.'],
        ['503', 'Storage unavailable: retry.'],
      ]),
      note('An offline phone gets the alert when it reconnects. An alert a phone has not received within the server\'s ', c('storage.delivery_ttl'), ' expires for that phone and is no longer sent.')));

    return out;
  }

  let observer = null;

  function render(data, current) {
    const out = build(data, current);
    const doc = $('doc');
    const list = pairs(data);
    doc.replaceChildren(doc.firstElementChild, picker(data, list, current, (next) => {
      render(data, next);
      $('doc-pair').focus();
    }), ...out);
    $('doc-nav').replaceChildren(...sections.map((s) => el('a', { href: '#' + s.id, 'data-target': s.id, text: s.title })));
    watchActive();
  }

  function watchActive() {
    if (!('IntersectionObserver' in window)) return;
    const links = new Map([...document.querySelectorAll('#doc-nav a')].map((a) => [a.dataset.target, a]));
    if (observer) observer.disconnect();
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
    observer = io;
  }

  (async () => {
    try {
      const data = await api('GET', 'api/admin/overview');
      $('server-name').textContent = data.server.name;
      $('server-version').textContent = 'v' + data.server.version;
      // Default: the first producer's first allowed group.
      render(data, pairs(data)[0] || null);
      if (location.hash) {
        const target = document.getElementById(location.hash.slice(1));
        if (target) target.scrollIntoView();
      }
    } catch (e) {
      if (e.code === 'host_not_allowed') return;
      const box = $('global-error');
      box.replaceChildren(icon('alert-circle-outline'), el('span', {
        text: e.offline
          ? "Can't reach the Inbox server. Reload this page once it is running again."
          : 'The Inbox server answered with an error: ' + e.message + '.',
      }));
      box.hidden = false;
    }
  })();
})();
