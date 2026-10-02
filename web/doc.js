// Inbox API documentation: fills /doc with the real URLs, groups and producers.
'use strict';

(() => {
  const { $, el, icon, api, copy } = window.Inbox;

  // A copyable block: monospace text and a copy button.
  function block(text, opts) {
    const code = el('code', { text });
    const pre = el('pre', { class: opts && opts.inline ? 'code inline' : 'code' }, code);
    const btn = el('button', {
      class: 'copy-btn',
      type: 'button',
      title: 'Copier',
      'aria-label': 'Copier',
      onclick: () => copy(text, code),
    }, icon('content-copy'));
    return el('div', { class: 'code-block' + (opts && opts.inline ? ' inline' : '') }, pre, btn);
  }

  const put = (id, ...nodes) => $(id).replaceChildren(...nodes);
  const json = (v) => JSON.stringify(v, null, 2);
  const shellQuote = (s) => "'" + String(s).replace(/'/g, "'\\''") + "'";

  function render(data) {
    const port = data.server.api_port;
    const base = location.protocol + '//' + location.hostname + ':' + port;
    const groups = data.groups || [];
    const producers = data.producers || [];
    // Example group: one a producer may alert, otherwise the first declared.
    const exampleGroup = (producers[0] && producers[0].groups[0]) || (groups[0] && groups[0].id) || 'infra';

    $('server-name').textContent = data.server.name;
    document.title = 'API · ' + data.server.name;
    $('api-port').textContent = String(port);

    put('base-url', block(base, { inline: true }));
    put('token-export', block("export INBOX_TOKEN='<token du producteur>'"));

    put('send-url', block(base + '/api/v1/alerts', { inline: true }));
    const body = { group: exampleGroup, title: 'Disque plein sur db-1', message: 'Le volume /var/lib/postgresql est à 97 %.' };
    put('send-curl', block([
      'curl -X POST ' + shellQuote(base + '/api/v1/alerts') + ' \\',
      '  -H "Authorization: Bearer $INBOX_TOKEN" \\',
      "  -H 'Idempotency-Key: db-1-disk-2026-10-02T14:00' \\",
      "  -H 'Content-Type: application/json' \\",
      '  -d ' + shellQuote(JSON.stringify(body)),
    ].join('\n')));
    put('send-response', block(json({ alert_id: '5f0c2a9e8b1d4c3fa6e7d8c9b0a1f2e3', duplicate: false, targets: 2 })));

    put('status-url', block(base + '/api/v1/alerts/{alert_id}', { inline: true }));
    put('status-curl', block('curl -H "Authorization: Bearer $INBOX_TOKEN" \\\n  ' + shellQuote(base + '/api/v1/alerts/5f0c2a9e8b1d4c3fa6e7d8c9b0a1f2e3')));
    const now = Date.now();
    put('status-response', block(json({
      id: '5f0c2a9e8b1d4c3fa6e7d8c9b0a1f2e3',
      group: exampleGroup,
      title: body.title,
      message: body.message,
      source: 'producer:' + ((producers[0] && producers[0].name) || 'monitoring'),
      created_at: now - 4000,
      targets: 2,
      received: 1,
      expired: 0,
      deliveries: [
        { device_id: '…', device_name: 'Pixel 8', user_name: 'alice', action: 'alarm', attempts: 1, first_sent_at: now - 3990, last_sent_at: now - 3990, received_at: now - 3800, expired: false, device_revoked: false },
        { device_id: '…', device_name: 'Galaxy S24', user_name: 'bob', action: 'ring', attempts: 3, first_sent_at: now - 3990, last_sent_at: now - 990, received_at: null, expired: false, device_revoked: false },
      ],
    })));

    put('webhook-url', block(base + '/api/v1/webhooks/alertmanager/{group}', { inline: true }));
    put('webhook-groups', ...(groups.length
      ? groups.map((g) => el('div', { class: 'doc-url-row' },
        el('span', { class: 'chip', text: g.id }),
        block(base + '/api/v1/webhooks/alertmanager/' + g.id, { inline: true })))
      : [el('p', { class: 'muted', text: 'Aucun groupe déclaré.' })]));
    put('webhook-alertmanager', block([
      'route:',
      '  receiver: inbox',
      '  group_by: ["alertname"]',
      '  repeat_interval: 4h',
      '',
      'receivers:',
      '  - name: inbox',
      '    webhook_configs:',
      '      - url: ' + base + '/api/v1/webhooks/alertmanager/' + exampleGroup,
      '        send_resolved: false',
      '        max_alerts: 50',
      '        http_config:',
      '          authorization:',
      '            type: Bearer',
      '            credentials_file: /etc/alertmanager/inbox_token',
    ].join('\n')));
    put('webhook-grafana', block([
      'apiVersion: 1',
      'contactPoints:',
      '  - orgId: 1',
      '    name: inbox-' + exampleGroup,
      '    receivers:',
      '      - uid: inbox-' + exampleGroup,
      '        type: webhook',
      '        settings:',
      '          url: ' + base + '/api/v1/webhooks/alertmanager/' + exampleGroup,
      '          httpMethod: POST',
      '          authorization_scheme: Bearer',
      '          authorization_credentials: $__env{INBOX_TOKEN}',
      '          maxAlerts: 50',
      '        disableResolveMessage: true',
    ].join('\n')));

    put('health-rows', ...[
      ['/healthz', "« ok » tant que le processus répond"],
      ['/readyz', '« ready », ou 503 si le stockage est indisponible ou si le serveur s\'arrête'],
      ['/api/version', 'JSON : version, révision et version du protocole'],
    ].map(([path, what]) => el('tr', {},
      el('td', {}, block(base + path, { inline: true })),
      el('td', { text: what }))));

    put('groups-rows', ...groups.map((g) => el('tr', {},
      el('td', {}, el('code', { text: g.id })),
      el('td', { text: g.name }),
      el('td', { class: g.description ? null : 'muted', text: g.description || '—' }))));

    $('producers-empty').hidden = producers.length > 0;
    $('producers-rows').closest('table').hidden = producers.length === 0;
    const names = new Map(groups.map((g) => [g.id, g.name]));
    put('producers-rows', ...producers.map((p) => el('tr', {},
      el('td', {}, el('code', { text: p.name })),
      el('td', {}, el('div', { class: 'subs' }, ...p.groups.map((g) => el('span', { class: 'chip', title: names.get(g) || g, text: g })))),
      el('td', { text: p.webhook_send_resolved ? 'Envoyées' : 'Ignorées' }))));
  }

  async function load() {
    try {
      render(await api('GET', '/api/admin/overview'));
      $('global-error').hidden = true;
      // Re-apply the anchor once the content has its final height.
      if (location.hash) {
        const target = document.getElementById(location.hash.slice(1));
        if (target) target.scrollIntoView();
      }
    } catch (e) {
      if (e.code === 'host_not_allowed') return;
      const box = $('global-error');
      box.replaceChildren(icon('alert-circle-outline'), el('span', { text: 'Serveur injoignable : ' + e.message }));
      box.hidden = false;
    }
  }

  // Highlights the section being read in the side navigation.
  const links = new Map([...document.querySelectorAll('.doc-nav a')].map((a) => [a.getAttribute('href').slice(1), a]));
  if ('IntersectionObserver' in window) {
    const seen = new Map();
    const io = new IntersectionObserver((entries) => {
      for (const e of entries) seen.set(e.target.id, e.isIntersecting);
      const current = [...links.keys()].find((id) => seen.get(id));
      for (const [id, a] of links) a.classList.toggle('active', id === current);
    }, { rootMargin: '-64px 0px -55% 0px' });
    for (const id of links.keys()) {
      const section = document.getElementById(id);
      if (section) io.observe(section);
    }
  }

  load();
})();
