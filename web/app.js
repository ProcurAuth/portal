// Inbox admin dashboard. Vanilla JS, no dependencies.
//
// The interface has no password: access control is the network exposure of the
// admin listener (localhost, private network or an authenticating proxy).
// Alerts are one-way: the only delivery state is "received" by each device.
'use strict';

(() => {
  const { $, el, icon, api, toast } = window.Inbox;

  const ALERT_LIMIT = 5;
  const ALERT_WINDOW_MS = 24 * 3600 * 1000;
  const CONNECTION_LIMIT = 50;
  const REFRESH_MS = 3000;

  // ------------------------------------------------------------ helpers

  const show = (id, visible) => { $(id).hidden = !visible; };

  function showError(err) {
    const box = $('global-error');
    if (!err || err.code === 'host_not_allowed') {
      box.hidden = true;
      return;
    }
    box.replaceChildren(icon('alert-circle-outline'), el('span', { text: 'Serveur injoignable : ' + err.message }));
    box.hidden = false;
  }

  function confirmDialog(text, okLabel) {
    const d = $('confirm-dialog');
    $('confirm-text').textContent = text;
    $('confirm-ok').textContent = okLabel || 'Confirmer';
    return new Promise((resolve) => {
      d.addEventListener('close', () => resolve(d.returnValue === 'ok'), { once: true });
      d.returnValue = 'cancel';
      d.showModal();
      $('confirm-ok').focus();
    });
  }

  const plural = (n, word) => n + ' ' + word + (n > 1 ? 's' : '');

  // ------------------------------------------------------------ formatting

  let serverOffset = 0; // server clock - local clock
  const serverNow = () => Date.now() + serverOffset;
  const dateFmt = new Intl.DateTimeFormat('fr-FR', { dateStyle: 'medium', timeStyle: 'medium' });
  const clockFmt = new Intl.DateTimeFormat('fr-FR', { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });
  const dayFmt = new Intl.DateTimeFormat('fr-FR', { day: '2-digit', month: '2-digit' });
  const absTime = (ms) => (ms ? dateFmt.format(new Date(ms)) : '–');
  const clock = (ms) => (ms ? clockFmt.format(new Date(ms)) : '--:--:--');
  // Short relative age for dense rows: "12 s", "4 min", "3 h", "2 j".
  const age = (ms) => (ms ? duration(serverNow() - ms) : '–');
  const sameDay = (ms) => new Date(ms).toDateString() === new Date(serverNow()).toDateString();

  function duration(ms) {
    const s = Math.max(0, Math.round(ms / 1000));
    if (s < 60) return s + ' s';
    const m = Math.floor(s / 60);
    if (m < 60) return m + ' min';
    const h = Math.floor(m / 60);
    if (h < 48) return h + ' h' + (m % 60 ? ' ' + String(m % 60).padStart(2, '0') : '');
    return Math.floor(h / 24) + ' j';
  }

  function ago(ms) {
    if (!ms) return '–';
    const d = serverNow() - ms;
    return d < 45000 ? "à l'instant" : 'il y a ' + duration(d);
  }

  function sourceLabel(source) {
    if (source === 'admin') return 'Interface admin';
    const [kind, name] = String(source).split(':');
    if (kind === 'producer') return 'API · ' + name;
    if (kind === 'webhook') return 'Webhook · ' + name;
    return source;
  }

  const STATUS = {
    online: ['En ligne', 'success'],
    offline: ['Hors ligne', 'neutral'],
    unreachable: ['Injoignable', 'danger'],
    revoked: ['Révoqué', 'neutral'],
  };
  const ACTIONS = { alarm: 'Alarme', ring: 'Sonnerie', notify: 'Notification' };
  const CLOSE_REASONS = {
    peer_closed: "fermée par l'appareil",
    timeout: 'délai dépassé',
    replaced: 'remplacée par une nouvelle session',
    revoked: 'appareil révoqué',
    server_restart: 'redémarrage du serveur',
    protocol_error: 'erreur de protocole',
    storage_error: 'erreur de stockage',
    send_failed: "échec d'envoi",
  };

  // "pseudo · device": the user pseudonym is the primary label. Devices enrolled
  // by older apps have none.
  function who(userName, deviceName) {
    const user = userName
      ? el('span', { class: 'who', text: userName })
      : el('span', { class: 'who none', text: 'Sans pseudo' });
    return el('span', { class: 'who-line' }, user, el('span', { class: 'who-device', text: ' · ' + deviceName }));
  }
  const whoText = (userName, deviceName) => (userName || 'Sans pseudo') + ' · ' + deviceName;

  // ------------------------------------------------------------ state

  let overview = null;
  const groupNames = new Map();
  const expanded = new Set();
  const renderKeys = {};
  // Pseudonym filter shared by Appareils and Connexions: null = everyone,
  // '' = devices without a pseudonym.
  const NO_USER = '';
  let userFilter = null;
  let timer = null;

  const groupName = (id) => groupNames.get(id) || id;
  const matchesUser = (userName) => userFilter === null || (userName || NO_USER) === userFilter;

  // Replaces the children of `id` only when `key` changed, so polling never
  // steals focus or resets a hovered element.
  function renderIfChanged(id, key, build) {
    if (renderKeys[id] === key) return;
    renderKeys[id] = key;
    $(id).replaceChildren(...build());
  }

  // ------------------------------------------------------------ overview & status

  async function refreshOverview() {
    const data = await api('GET', 'api/admin/overview');
    overview = data;
    serverOffset = data.server.time_ms - Date.now();
    groupNames.clear();
    for (const g of data.groups) groupNames.set(g.id, g.name);
    $('server-name').textContent = data.server.name;
    $('server-version').textContent = 'v' + data.server.version;
    document.title = data.server.name + ' · Inbox';

    const st = data.stats;
    const pill = $('status-pill');
    const late = st.pending_deliveries > 0 && st.oldest_pending_ms > 60000;
    pill.classList.toggle('warn', st.unreachable > 0 || late);
    pill.classList.toggle('idle', st.enrolled === 0);
    let text;
    if (st.enrolled === 0) text = 'Aucun appareil';
    else if (st.unreachable > 0) text = plural(st.unreachable, 'injoignable');
    else if (late) text = 'Envoi en attente';
    else text = st.online + '/' + st.enrolled + ' en ligne';
    $('status-text').textContent = text;
    pill.title = st.online + ' en ligne sur ' + st.enrolled + ' enrôlé(s). ' + (st.pending_deliveries > 0
      ? st.pending_deliveries + ' livraison(s) en attente, la plus ancienne depuis ' + duration(st.oldest_pending_ms) + '.'
      : 'Aucune livraison en attente.');

    fillGroupSelect();
    updateTargets();
    renderUserFilter();
    renderDevices();
  }

  // ------------------------------------------------------------ alerts

  async function refreshAlerts() {
    const since = Math.floor(serverNow() - ALERT_WINDOW_MS);
    const data = await api('GET', 'api/admin/alerts?limit=' + ALERT_LIMIT + '&since_ms=' + since);
    const alerts = (data.alerts || []).filter((a) => a.created_at >= since).slice(0, ALERT_LIMIT);
    show('alerts-empty', alerts.length === 0);
    renderIfChanged('alerts-list', JSON.stringify(alerts) + [...expanded].join(), () => alerts.map(alertCard));
    for (const t of document.querySelectorAll('[data-age]')) t.textContent = age(Number(t.dataset.age));
  }

  function alertCard(a) {
    const targets = a.targets || 0;
    const received = a.received || 0;
    // Expired: never received within the server's delivery TTL, no longer sent.
    const expired = Math.min(a.expired || 0, targets - received);
    const missing = targets - received - expired;
    const title = a.title || groupName(a.group);

    let state;
    let sev;
    if (targets === 0) {
      state = el('span', { class: 'badge warning', title: "Aucun appareil n'était abonné au groupe" }, 'Aucun destinataire');
      sev = 'warning';
    } else if (missing > 0) {
      state = el('span', { class: 'badge danger' }, missing + ' non reçue' + (missing > 1 ? 's' : ''));
      sev = 'danger';
    } else if (expired === 0) {
      state = el('span', { class: 'badge success' }, 'Reçue');
      sev = 'ok';
    } else {
      state = el('span', { class: 'badge neutral', title: "Non reçue avant l'expiration : n'est plus envoyée" }, 'Expirée');
      sev = 'neutral';
    }

    const fill = el('span');
    fill.style.width = (targets ? Math.round((100 * received) / targets) : 0) + '%';
    const bar = el('span', { class: 'bar' + (missing > 0 || expired > 0 ? ' partial' : ''), 'aria-hidden': 'true' }, fill);

    const when = sameDay(a.created_at) ? clock(a.created_at) : dayFmt.format(new Date(a.created_at)) + ' ' + clock(a.created_at).slice(0, 5);
    return el(
      'div',
      {
        class: 'row clickable alert-row',
        tabindex: '0',
        role: 'button',
        'aria-label': title,
        onclick: () => openDetail(a.id),
        onkeydown: (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); openDetail(a.id); } },
      },
      el('span', { class: 'sev ' + sev }),
      el('div', { class: 'alert-clock', title: absTime(a.created_at) },
        el('span', { class: 'hms', text: when }),
        el('span', { class: 'rel', 'data-age': a.created_at, text: age(a.created_at) })),
      el('div', { class: 'row-main' },
        el('div', { class: 'alert-head' },
          el('span', { class: 'chip', text: groupName(a.group) }),
          el('span', { class: 'alert-title', text: title })),
        a.message ? el('p', { class: 'alert-message', text: a.message }) : null,
        el('div', { class: 'alert-meta' }, el('span', { text: sourceLabel(a.source) }))),
      el('div', { class: 'alert-side' },
        state,
        targets > 0
          ? el('span', { class: 'receipt', title: received + ' reçue(s) sur ' + targets + (expired ? ', ' + expired + ' expirée(s)' : '') },
            bar, received + '/' + targets)
          : null),
    );
  }

  async function openDetail(id) {
    let d;
    try {
      d = await api('GET', 'api/admin/alerts/' + encodeURIComponent(id));
    } catch (e) {
      toast(e.message);
      return;
    }
    $('detail-group').textContent = groupName(d.group);
    $('detail-title').textContent = d.title || groupName(d.group);
    $('detail-meta').textContent = absTime(d.created_at) + ' · ' + sourceLabel(d.source) + ' · ' + (d.received || 0) + '/' + (d.targets || 0) + ' reçue(s)'
      + (d.expired ? ' · ' + d.expired + ' expirée(s)' : '');
    $('detail-message').textContent = d.message || '';
    show('detail-message', !!d.message);
    const rows = (d.deliveries || []).map((x) => {
      const sub = [ACTIONS[x.action] || x.action];
      if (x.first_sent_at) sub.push('envoyée ' + ago(x.first_sent_at));
      if (x.attempts > 1) sub.push(x.attempts + ' tentatives');
      if (x.device_revoked) sub.push('appareil révoqué');
      let side;
      if (x.received_at) side = el('span', { class: 'badge success', title: absTime(x.received_at) }, icon('check-circle-outline'), 'Reçue en ' + duration(x.received_at - d.created_at));
      else if (x.expired) side = el('span', { class: 'badge neutral', title: "Non reçue avant l'expiration de l'alerte : elle n'est plus envoyée" }, icon('clock-outline'), 'Expirée (non reçue)');
      else side = el('span', { class: 'badge danger' }, icon('clock-outline'), x.first_sent_at ? 'En attente' : 'Pas encore envoyée');
      return el('div', { class: 'row' },
        el('div', { class: 'row-main' }, el('div', { class: 'row-title' }, who(x.user_name, x.device_name)), el('div', { class: 'row-sub', text: sub.join(' · ') })),
        el('div', { class: 'row-side' }, side));
    });
    if (rows.length === 0) rows.push(el('div', { class: 'row' }, el('span', { class: 'muted', text: "Aucun appareil n'était abonné à ce groupe." })));
    $('detail-deliveries').replaceChildren(...rows);
    $('detail-dialog').showModal();
  }

  // ------------------------------------------------------------ send

  function fillGroupSelect() {
    const sel = $('send-group');
    const current = sel.value;
    const ids = overview.groups.map((g) => g.id);
    if (sel.dataset.ids === ids.join()) return;
    sel.dataset.ids = ids.join();
    sel.replaceChildren(...overview.groups.map((g) => el('option', { value: g.id, title: g.description || null, text: g.name })));
    if (ids.includes(current)) sel.value = current;
  }

  function subscribersOf(group) {
    if (!overview) return null;
    return overview.devices.filter((d) => d.status !== 'revoked' && d.device.subscriptions.some((s) => s.group === group)).length;
  }

  function updateTargets() {
    const n = subscribersOf($('send-group').value);
    const t = $('send-hint');
    t.classList.toggle('warn-text', n === 0);
    t.textContent = n === null ? '' : n === 0 ? 'Aucun appareil abonné à ce groupe' : plural(n, 'appareil') + ' abonné' + (n > 1 ? 's' : '');
    $('send-targets').textContent = overview ? plural(overview.groups.length, 'groupe') : '';
  }

  let pendingSend = null; // { fingerprint, key }: the same content reuses its idempotency key

  async function sendAlert(ev) {
    ev.preventDefault();
    const body = {
      group: $('send-group').value,
      title: $('send-title').value.trim() || undefined,
      message: $('send-message').value.trim() || undefined,
    };
    if (!body.group) return;
    const fingerprint = JSON.stringify(body);
    if (!pendingSend || pendingSend.fingerprint !== fingerprint) {
      pendingSend = { fingerprint, key: crypto.randomUUID() };
    }
    const n = subscribersOf(body.group);
    const what = groupName(body.group) + (n !== null ? ' (' + plural(n, 'appareil') + ' abonné' + (n > 1 ? 's' : '') + ')' : '');
    if (!(await confirmDialog("Envoyer l'alerte au groupe " + what + ' ?', 'Envoyer'))) return;

    const btn = $('send-btn');
    btn.disabled = true;
    const result = $('send-result');
    try {
      const r = await api('POST', 'api/admin/alerts', { ...body, idempotency_key: pendingSend.key });
      pendingSend = null;
      result.className = 'banner ' + (r.targets === 0 ? 'warning' : 'success');
      result.replaceChildren(
        icon(r.targets === 0 ? 'alert-circle-outline' : 'check-circle-outline'),
        el('span', {
          text: r.targets === 0
            ? "Alerte enregistrée mais AUCUN appareil n'est abonné à ce groupe : personne ne sera prévenu."
            : 'Alerte envoyée à ' + plural(r.targets, 'appareil') + (r.duplicate ? ' (déjà envoyée, non dupliquée).' : '.'),
        }),
      );
      result.hidden = false;
      $('send-title').value = '';
      $('send-message').value = '';
      refreshAlerts().catch(showError);
    } catch (e) {
      result.className = 'banner danger';
      result.replaceChildren(icon('alert-circle-outline'), el('span', { text: "Échec de l'envoi : " + e.message + " — réessayez, l'alerte ne sera pas dupliquée." }));
      result.hidden = false;
    } finally {
      btn.disabled = false;
    }
  }

  // ------------------------------------------------------------ pseudonym filter

  function renderUserFilter() {
    const users = new Map();
    for (const r of overview.devices) {
      const k = r.device.user_name || NO_USER;
      users.set(k, (users.get(k) || 0) + 1);
    }
    if (userFilter !== null && !users.has(userFilter)) userFilter = null;
    const names = [...users.keys()].filter((k) => k !== NO_USER).sort((a, b) => a.localeCompare(b, 'fr'));
    const key = JSON.stringify([...users]) + '|' + userFilter;
    $('user-filter').hidden = users.size < 2;
    if (users.size < 2) {
      renderIfChanged('user-filter', key, () => []);
      return;
    }
    const chip = (value, label, count) => el('button', {
      class: 'filter-chip',
      type: 'button',
      'aria-pressed': userFilter === value ? 'true' : 'false',
      onclick: () => pickUser(userFilter === value ? null : value),
    }, value === null ? null : icon('account-outline'), label, count === undefined ? null : el('span', { class: 'count', text: count }));
    renderIfChanged('user-filter', key, () => [
      chip(null, 'Tous'),
      ...names.map((n) => chip(n, n, users.get(n))),
      users.has(NO_USER) ? chip(NO_USER, 'Sans pseudo', users.get(NO_USER)) : null,
    ].filter(Boolean));
  }

  function pickUser(value) {
    userFilter = value;
    renderUserFilter();
    renderDevices();
    refreshConnections().catch(showError);
  }

  // ------------------------------------------------------------ devices

  function renderDevices() {
    const st = overview.stats;
    $('devices-summary').textContent = st.online + '/' + st.enrolled + ' en ligne' + (st.unreachable ? ' · ' + st.unreachable + ' injoignable' + (st.unreachable > 1 ? 's' : '') : '');
    const list = overview.devices
      .filter((r) => matchesUser(r.device.user_name))
      .sort((a, b) => (a.device.user_name || '￿').localeCompare(b.device.user_name || '￿', 'fr') || a.device.name.localeCompare(b.device.name, 'fr'));
    show('devices-empty', overview.devices.length === 0);
    show('devices-list', list.length > 0);
    // Relative times change every tick: key on minutes so rows re-render rarely.
    const key = JSON.stringify(list) + Math.floor(serverNow() / 30000);
    renderIfChanged('devices-list', key, () => list.map(deviceRow));
  }

  function deviceRow(row) {
    const d = row.device;
    const live = row.live;
    const [label, cls] = STATUS[row.status] || [row.status, 'neutral'];
    const meta = [
      (live && live.ip) || d.last_ip || 'IP inconnue',
      'v' + ((live && live.app_version) || d.app_version || '?'),
      live ? 'connecté depuis ' + age(live.connected_at) : 'vu il y a ' + age(d.last_seen_at),
    ];
    if (d.pending && row.status !== 'revoked') meta.push(plural(d.pending, 'alerte') + ' en attente');
    const subs = d.subscriptions.length
      ? el('div', { class: 'subs' }, ...d.subscriptions.map((s) => el('span', { class: 'chip ' + s.action, text: groupName(s.group) + ' · ' + (ACTIONS[s.action] || s.action) })))
      : el('div', { class: 'subs muted', text: 'Aucun abonnement' });
    const revoke = row.status === 'revoked' ? null : el('button', {
      class: 'icon-btn danger',
      type: 'button',
      title: 'Révoquer',
      'aria-label': 'Révoquer ' + whoText(d.user_name, d.name),
      onclick: async () => {
        if (!(await confirmDialog("Révoquer l'appareil « " + whoText(d.user_name, d.name) + " » ? Il devra être ré-enrôlé pour recevoir des alertes.", 'Révoquer'))) return;
        try {
          await api('POST', 'api/admin/devices/' + encodeURIComponent(d.id) + '/revoke', {});
          toast('Appareil révoqué');
          await refreshOverview();
        } catch (e) {
          toast('Échec : ' + e.message);
        }
      },
    }, icon('delete-outline'));
    return el('div', { class: 'row device-row' + (row.status === 'revoked' ? ' revoked' : '') },
      el('span', { class: 'status-dot ' + cls, title: label }),
      el('div', { class: 'row-main' },
        el('div', { class: 'row-title' }, who(d.user_name, d.name)),
        el('div', { class: 'row-sub', text: meta.join(' · ') }),
        subs),
      el('div', { class: 'row-side' }, el('span', { class: 'badge ' + cls, text: label }), revoke));
  }

  // ------------------------------------------------------------ connections

  async function refreshConnections() {
    const filter = userFilter ? '&user=' + encodeURIComponent(userFilter) : '';
    const data = await api('GET', 'api/admin/connections?limit=' + CONNECTION_LIMIT + filter);
    const list = (data.connections || []).filter((c) => matchesUser(c.user_name));
    $('connections-summary').textContent = list.length + ' dernière' + (list.length > 1 ? 's' : '') + (userFilter === null ? '' : ' · ' + (userFilter || 'sans pseudo'));
    show('connections-empty', list.length === 0);
    show('connections-list', list.length > 0);
    const key = JSON.stringify(list) + Math.floor(serverNow() / 30000);
    renderIfChanged('connections-list', key, () => list.map((c) => {
      const open = !c.disconnected_at;
      const sub = [c.ip, 'v' + c.app_version];
      if (open) sub.push('depuis ' + age(c.connected_at));
      else sub.push(duration(c.disconnected_at - c.connected_at) + (c.close_reason ? ' · ' + (CLOSE_REASONS[c.close_reason] || c.close_reason) : ''));
      const when = sameDay(c.connected_at) ? clock(c.connected_at) : dayFmt.format(new Date(c.connected_at)) + ' ' + clock(c.connected_at).slice(0, 5);
      return el('div', { class: 'row' },
        el('span', { class: 'sev ' + (open ? 'ok' : '') }),
        el('span', { class: 'row-time conn-time', title: absTime(c.connected_at), text: when }),
        el('div', { class: 'row-main' }, el('div', { class: 'row-title' }, who(c.user_name, c.device_name)), el('div', { class: 'row-sub', text: sub.join(' · ') })),
        el('div', { class: 'row-side' }, open ? el('span', { class: 'badge success', text: 'En cours' }) : null));
    }));
  }

  // ------------------------------------------------------------ polling

  async function tick() {
    clearTimeout(timer);
    try {
      await refreshOverview();
      await Promise.all([refreshAlerts(), refreshConnections()]);
      showError(null);
    } catch (e) {
      showError(e);
    }
    timer = setTimeout(tick, REFRESH_MS);
  }

  // ------------------------------------------------------------ wiring

  $('detail-close').addEventListener('click', () => $('detail-dialog').close());
  $('detail-dialog').addEventListener('click', (ev) => { if (ev.target === ev.currentTarget) ev.currentTarget.close(); });
  $('send-form').addEventListener('submit', sendAlert);
  $('send-group').addEventListener('change', updateTargets);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) tick(); });

  tick();
})();
