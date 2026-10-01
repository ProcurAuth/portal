// Inbox admin front. Vanilla JS, no dependencies.
//
// The interface has no password: access control is the network exposure of the
// admin listener (localhost, private network or an authenticating proxy).
// Alerts are one-way: the only delivery state is "received" by each device.
'use strict';

(() => {
  // ------------------------------------------------------------ settings

  const SETTINGS_KEY = 'inbox.settings';
  const DEFAULTS = { limit: 5, windowMs: 86400000, theme: 'system', refreshMs: 3000 };
  const WINDOWS = [3600000, 21600000, 43200000, 86400000, 172800000, 604800000];
  const REFRESHES = [2000, 3000, 5000, 10000, 30000];

  function sanitize(s) {
    const out = { ...DEFAULTS };
    const limit = Number(s && s.limit);
    if (Number.isInteger(limit) && limit >= 1 && limit <= 50) out.limit = limit;
    if (s && WINDOWS.includes(Number(s.windowMs))) out.windowMs = Number(s.windowMs);
    if (s && ['system', 'light', 'dark'].includes(s.theme)) out.theme = s.theme;
    if (s && REFRESHES.includes(Number(s.refreshMs))) out.refreshMs = Number(s.refreshMs);
    return out;
  }

  function loadSettings() {
    try {
      return sanitize(JSON.parse(localStorage.getItem(SETTINGS_KEY) || '{}'));
    } catch (_) {
      return { ...DEFAULTS };
    }
  }

  function saveSettings() {
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    } catch (_) {
      // Private mode or blocked storage: settings last for this page only.
    }
  }

  let settings = loadSettings();

  function applyTheme() {
    if (settings.theme === 'system') delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = settings.theme;
  }

  // ------------------------------------------------------------ API

  // Admin API call. POSTs carry a JSON body and the X-Inbox-Request header: a
  // cross-site page cannot send either without a CORS preflight, which the
  // server never grants (anti-CSRF; the interface has no password).
  async function api(method, path, body) {
    const opts = { method, cache: 'no-store', headers: {} };
    if (method !== 'GET') {
      opts.headers['content-type'] = 'application/json';
      opts.headers['x-inbox-request'] = '1';
      opts.body = JSON.stringify(body === undefined ? {} : body);
    }
    const res = await fetch(path, opts);
    const data = await res.json().catch(() => ({}));
    if (!res.ok) {
      const err = new Error((data.error && data.error.message) || 'HTTP ' + res.status);
      err.status = res.status;
      err.code = data.error && data.error.code;
      if (err.code === 'host_not_allowed') {
        $('host-error-host').textContent = location.host;
        show('host-error', true);
      }
      throw err;
    }
    show('host-error', false);
    return data;
  }

  // ------------------------------------------------------------ DOM helpers

  const $ = (id) => document.getElementById(id);

  function el(tag, attrs, ...children) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === undefined || v === null || v === false) continue;
      if (k === 'class') e.className = v;
      else if (k === 'text') e.textContent = v;
      else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
      else e.setAttribute(k, v === true ? '' : v);
    }
    for (const c of children) {
      if (c === null || c === undefined || c === false) continue;
      e.append(c instanceof Node ? c : document.createTextNode(String(c)));
    }
    return e;
  }

  const icon = (name) => el('span', { class: 'mdi mdi-' + name, 'aria-hidden': 'true' });

  // "pseudo · device": the user pseudonym is the primary label. Devices enrolled
  // by older apps have none.
  function who(userName, deviceName) {
    const user = userName
      ? el('span', { class: 'who', text: userName })
      : el('span', { class: 'who none', text: '—' });
    return el('span', { class: 'who-line' }, user, el('span', { class: 'who-device', text: ' · ' + deviceName }));
  }
  const whoText = (userName, deviceName) => (userName || '—') + ' · ' + deviceName;

  // Filter chips: null = everyone, '' = devices without a pseudonym.
  const NO_USER = '';
  function renderUserFilter(id, entries, current, onPick) {
    const box = $(id);
    const users = new Map();
    for (const e of entries) {
      const k = e.user_name || NO_USER;
      users.set(k, (users.get(k) || 0) + 1);
    }
    if (users.size < 2 && current === null) {
      box.replaceChildren();
      return;
    }
    const chip = (value, label, count) => el('button', {
      class: 'filter-chip',
      type: 'button',
      'aria-pressed': current === value ? 'true' : 'false',
      onclick: () => onPick(current === value ? null : value),
    }, value === null ? null : icon('account-outline'), label, count === undefined ? null : el('span', { class: 'count', text: count }));
    const names = [...users.keys()].filter((k) => k !== NO_USER).sort((a, b) => a.localeCompare(b, 'fr'));
    box.replaceChildren(
      chip(null, 'Tous'),
      ...names.map((n) => chip(n, n, users.get(n))),
      users.has(NO_USER) ? chip(NO_USER, 'Sans pseudo', users.get(NO_USER)) : null,
    );
  }

  function show(id, visible) {
    $(id).hidden = !visible;
  }

  let toastTimer = null;
  function toast(text) {
    const t = $('toast');
    t.textContent = text;
    t.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { t.hidden = true; }, 3500);
  }

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

  // ------------------------------------------------------------ formatting

  let serverOffset = 0; // server clock - local clock
  const serverNow = () => Date.now() + serverOffset;
  const dateFmt = new Intl.DateTimeFormat('fr-FR', { dateStyle: 'medium', timeStyle: 'medium' });
  const absTime = (ms) => (ms ? dateFmt.format(new Date(ms)) : '–');

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

  function windowLabel(ms) {
    return ms >= 604800000 ? ms / 86400000 + ' j' : ms / 3600000 + ' h';
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

  // ------------------------------------------------------------ state

  let overview = null;
  const groupNames = new Map();
  const expanded = new Set();
  let lastAlertsKey = '';
  let currentView = 'alerts';
  let devicesUser = null;
  let connectionsUser = null;
  let timer = null;

  function groupName(id) {
    return groupNames.get(id) || id;
  }

  // ------------------------------------------------------------ overview / status

  async function refreshOverview() {
    const data = await api('GET', '/api/admin/overview');
    overview = data;
    serverOffset = data.server.time_ms - Date.now();
    groupNames.clear();
    for (const g of data.groups) groupNames.set(g.id, g.name);
    $('server-name').textContent = data.server.name;
    $('drawer-version').textContent = data.server.name + ' · v' + data.server.version;
    document.title = 'Inbox · ' + data.server.name;

    const st = data.stats;
    const pill = $('status-pill');
    const late = st.pending_deliveries > 0 && st.oldest_pending_ms > 60000;
    pill.classList.toggle('warn', st.unreachable > 0 || late);
    pill.classList.toggle('idle', st.enrolled === 0);
    let extra = '';
    if (st.unreachable > 0) extra = ' · ' + st.unreachable + ' injoignable' + (st.unreachable > 1 ? 's' : '');
    else if (late) extra = ' · envoi en attente';
    $('status-text').replaceChildren(
      st.online + '/' + st.enrolled,
      el('span', { class: 'pill-extra', text: ' en ligne' + extra }),
    );
    pill.title = st.pending_deliveries > 0
      ? st.pending_deliveries + ' livraison(s) en attente, la plus ancienne depuis ' + duration(st.oldest_pending_ms)
      : 'Aucune livraison en attente';
    const badge = $('nav-devices-badge');
    badge.textContent = st.unreachable;
    badge.hidden = st.unreachable === 0;

    fillGroupSelect();
    if (currentView === 'devices') renderDevices();
  }

  // ------------------------------------------------------------ alerts view

  async function refreshAlerts() {
    const since = serverNow() - settings.windowMs;
    const data = await api('GET', '/api/admin/alerts?limit=' + settings.limit + '&since_ms=' + Math.floor(since));
    // Harmless fallback if the server ignores since_ms: the list is newest first.
    const alerts = (data.alerts || []).filter((a) => a.created_at >= since).slice(0, settings.limit);
    renderAlerts(alerts);
  }

  function renderAlerts(alerts) {
    const scope = settings.limit + ' dernière' + (settings.limit > 1 ? 's' : '') + ' · ' + windowLabel(settings.windowMs);
    $('alerts-scope').textContent = settings.limit + ' plus récente' + (settings.limit > 1 ? 's' : '') + ' · ' + windowLabel(settings.windowMs);
    $('alerts-foot').textContent = scope;
    $('alerts-empty-text').textContent = 'Aucune alerte depuis ' + windowLabel(settings.windowMs) + '.';
    show('alerts-empty', alerts.length === 0);

    // Re-render only on change (relative times are refreshed in place).
    const key = JSON.stringify(alerts) + [...expanded].join();
    if (key !== lastAlertsKey) {
      lastAlertsKey = key;
      $('alerts-list').replaceChildren(...alerts.map(alertCard));
    }
    for (const t of document.querySelectorAll('[data-ago]')) t.textContent = ago(Number(t.dataset.ago));
  }

  function alertCard(a) {
    const targets = a.targets || 0;
    const received = a.received || 0;
    const missing = targets - received;
    const title = a.title || groupName(a.group);

    let state;
    if (targets === 0) state = el('span', { class: 'badge warning' }, icon('alert-circle-outline'), 'Aucun destinataire');
    else if (missing > 0) state = el('span', { class: 'badge danger' }, icon('clock-outline'), missing + ' non reçue' + (missing > 1 ? 's' : ''));
    else state = el('span', { class: 'badge success' }, icon('check-circle-outline'), 'Reçue');

    const pct = targets ? Math.round((100 * received) / targets) : 0;
    const fill = el('span');
    fill.style.width = pct + '%';
    const bar = el('span', { class: 'bar' + (missing > 0 ? ' partial' : ''), 'aria-hidden': 'true' }, fill);

    let message = null;
    let more = null;
    if (a.message) {
      const open = expanded.has(a.id);
      message = el('p', { class: 'alert-message' + (open ? ' expanded' : ''), text: a.message });
      if (a.message.length > 180 || a.message.split('\n').length > 3) {
        more = el('button', {
          class: 'more-btn',
          type: 'button',
          text: open ? 'Voir moins' : 'Voir plus',
          onclick: (ev) => {
            ev.stopPropagation();
            if (expanded.has(a.id)) expanded.delete(a.id);
            else expanded.add(a.id);
            lastAlertsKey = '';
            refreshAlerts().catch(showError);
          },
        });
      }
    }

    const card = el(
      'article',
      {
        class: 'panel alert-card' + (missing > 0 ? ' undelivered' : ''),
        tabindex: '0',
        'aria-label': title,
        onclick: () => openDetail(a.id),
        onkeydown: (ev) => { if (ev.key === 'Enter') openDetail(a.id); },
      },
      el('div', { class: 'alert-top' },
        el('span', { class: 'chip', text: groupName(a.group) }),
        state,
        el('span', { class: 'alert-time', title: absTime(a.created_at), 'data-ago': a.created_at, text: ago(a.created_at) })),
      el('div', { class: 'alert-title', text: title }),
      message,
      more,
      el('div', { class: 'alert-foot' },
        el('span', { class: 'source' }, icon('information-outline'), sourceLabel(a.source)),
        targets > 0 ? el('span', { class: 'progress' }, bar, received + '/' + targets + ' reçue' + (received > 1 ? 's' : '')) : null),
    );
    return card;
  }

  async function openDetail(id) {
    let d;
    try {
      d = await api('GET', '/api/admin/alerts/' + encodeURIComponent(id));
    } catch (e) {
      toast(e.message);
      return;
    }
    $('detail-group').textContent = groupName(d.group);
    $('detail-title').textContent = d.title || groupName(d.group);
    $('detail-meta').textContent = absTime(d.created_at) + ' · ' + sourceLabel(d.source) + ' · ' + (d.received || 0) + '/' + (d.targets || 0) + ' reçue(s)';
    $('detail-message').textContent = d.message || '';
    show('detail-message', !!d.message);
    const rows = (d.deliveries || []).map((x) => {
      const ok = !!x.received_at;
      const sub = [ACTIONS[x.action] || x.action];
      if (x.first_sent_at) sub.push('envoyée ' + ago(x.first_sent_at));
      if (x.attempts > 1) sub.push(x.attempts + ' tentatives');
      if (x.device_revoked) sub.push('appareil révoqué');
      const side = ok
        ? el('span', { class: 'badge success', title: absTime(x.received_at) }, icon('check-circle-outline'), 'Reçue en ' + duration(x.received_at - d.created_at))
        : el('span', { class: 'badge danger' }, icon('clock-outline'), x.first_sent_at ? 'En attente' : 'Pas encore envoyée');
      return el('div', { class: 'row' },
        el('div', { class: 'row-main' }, el('div', { class: 'row-title' }, who(x.user_name, x.device_name)), el('div', { class: 'row-sub', text: sub.join(' · ') })),
        el('div', { class: 'row-side' }, side));
    });
    if (rows.length === 0) rows.push(el('div', { class: 'row' }, el('span', { class: 'muted', text: "Aucun appareil n'était abonné à ce groupe." })));
    $('detail-deliveries').replaceChildren(...rows);
    $('detail-dialog').showModal();
  }

  // ------------------------------------------------------------ send view

  function fillGroupSelect() {
    const sel = $('send-group');
    const current = sel.value;
    const ids = overview ? overview.groups.map((g) => g.id) : [];
    if (sel.dataset.ids === ids.join()) return;
    sel.dataset.ids = ids.join();
    sel.replaceChildren(...overview.groups.map((g) => el('option', { value: g.id, text: g.name + (g.description ? ' — ' + g.description : '') })));
    if (ids.includes(current)) sel.value = current;
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
    const subscribers = overview
      ? overview.devices.filter((d) => d.status !== 'revoked' && d.device.subscriptions.some((s) => s.group === body.group)).length
      : null;
    const what = groupName(body.group) + (subscribers !== null ? ' (' + subscribers + ' appareil' + (subscribers > 1 ? 's' : '') + ' abonné' + (subscribers > 1 ? 's' : '') + ')' : '');
    if (!(await confirmDialog("Envoyer l'alerte au groupe " + what + ' ?', 'Envoyer'))) return;

    const btn = $('send-btn');
    btn.disabled = true;
    const result = $('send-result');
    try {
      const r = await api('POST', '/api/admin/alerts', { ...body, idempotency_key: pendingSend.key });
      pendingSend = null;
      result.className = 'banner ' + (r.targets === 0 ? 'warning' : 'success');
      result.replaceChildren(
        icon(r.targets === 0 ? 'alert-circle-outline' : 'check-circle-outline'),
        el('span', {
          text: r.targets === 0
            ? "Alerte enregistrée mais AUCUN appareil n'est abonné à ce groupe : personne ne sera prévenu."
            : 'Alerte envoyée à ' + r.targets + ' appareil' + (r.targets > 1 ? 's' : '') + (r.duplicate ? ' (déjà envoyée, non dupliquée).' : '.'),
        }),
      );
      result.hidden = false;
      $('send-title').value = '';
      $('send-message').value = '';
    } catch (e) {
      result.className = 'banner danger';
      result.replaceChildren(icon('alert-circle-outline'), el('span', { text: "Échec de l'envoi : " + e.message + ' — réessayez, l\'alerte ne sera pas dupliquée.' }));
      result.hidden = false;
    } finally {
      btn.disabled = false;
    }
  }

  // ------------------------------------------------------------ devices view

  function renderDevices() {
    if (!overview) return;
    const list = overview.devices;
    const st = overview.stats;
    $('devices-summary').textContent = st.enrolled + ' enrôlé' + (st.enrolled > 1 ? 's' : '') + ' · ' + st.online + ' en ligne' + (st.unreachable ? ' · ' + st.unreachable + ' injoignable' + (st.unreachable > 1 ? 's' : '') : '');
    show('devices-empty', list.length === 0);
    renderUserFilter('devices-filter', list.map((r) => r.device), devicesUser, (v) => { devicesUser = v; renderDevices(); });
    const shown = list
      .filter((r) => devicesUser === null || (r.device.user_name || NO_USER) === devicesUser)
      .sort((a, b) => (a.device.user_name || '\uffff').localeCompare(b.device.user_name || '\uffff', 'fr') || a.device.name.localeCompare(b.device.name, 'fr'));
    $('devices-list').replaceChildren(...shown.map((row) => {
      const d = row.device;
      const live = row.live;
      const [label, cls] = STATUS[row.status] || [row.status, 'neutral'];
      const kv = el('dl', { class: 'kv' });
      const add = (k, v) => kv.append(el('dt', { text: k }), el('dd', { text: v }));
      add('Adresse IP', (live && live.ip) || d.last_ip || '–');
      add(live ? 'Connecté' : 'Vu', live ? ago(live.connected_at) : ago(d.last_seen_at));
      add('Version', (live && live.app_version) || d.app_version || '–');
      if (d.pending && row.status !== 'revoked') add('En attente', d.pending + ' alerte' + (d.pending > 1 ? 's' : ''));
      const subs = d.subscriptions.length
        ? el('div', { class: 'subs' }, ...d.subscriptions.map((s) => el('span', { class: 'chip', title: ACTIONS[s.action] || s.action, text: groupName(s.group) + ' · ' + (ACTIONS[s.action] || s.action) })))
        : el('span', { class: 'muted', text: 'Aucun abonnement' });
      const actions = row.status === 'revoked' ? null : el('div', { class: 'device-actions' },
        el('button', {
          class: 'btn small danger',
          type: 'button',
          onclick: async () => {
            if (!(await confirmDialog("Révoquer l'appareil « " + whoText(d.user_name, d.name) + " » ? Il devra être ré-enrôlé pour recevoir des alertes.", 'Révoquer'))) return;
            try {
              await api('POST', '/api/admin/devices/' + encodeURIComponent(d.id) + '/revoke', {});
              toast('Appareil révoqué');
              await refreshOverview();
            } catch (e) {
              toast('Échec : ' + e.message);
            }
          },
        }, icon('delete-outline'), 'Révoquer'));
      return el('article', { class: 'panel device-card' + (row.status === 'revoked' ? ' revoked' : '') },
        el('div', { class: 'device-head' },
          icon(d.user_name ? 'account-outline' : 'cellphone'),
          el('div', { class: 'device-name' },
            el('div', { class: d.user_name ? 'who' : 'who none', text: d.user_name || 'Sans pseudo' }),
            el('div', { class: 'who-device', text: d.name })),
          el('span', { class: 'badge ' + cls, text: label })),
        kv, subs, actions);
    }));
  }

  // ------------------------------------------------------------ connections view

  async function refreshConnections() {
    const filter = connectionsUser ? '&user=' + encodeURIComponent(connectionsUser) : '';
    const data = await api('GET', '/api/admin/connections?limit=100' + filter);
    // Harmless client-side filter, also covers "without pseudonym".
    const list = (data.connections || []).filter((c) => connectionsUser === null || (c.user_name || NO_USER) === connectionsUser);
    const known = overview ? overview.devices.map((r) => r.device) : list;
    renderUserFilter('connections-filter', known, connectionsUser, (v) => { connectionsUser = v; refreshConnections().catch(showError); });
    show('connections-empty', list.length === 0);
    show('connections-list', list.length > 0);
    $('connections-list').replaceChildren(...list.map((c) => {
      const open = !c.disconnected_at;
      const sub = [c.ip, 'v' + c.app_version];
      if (!open) sub.push('durée ' + duration(c.disconnected_at - c.connected_at) + (c.close_reason ? ' · ' + c.close_reason : ''));
      return el('div', { class: 'row' },
        el('div', { class: 'row-main' }, el('div', { class: 'row-title' }, who(c.user_name, c.device_name)), el('div', { class: 'row-sub', text: sub.join(' · ') })),
        el('div', { class: 'row-side' },
          open ? el('span', { class: 'badge success', text: 'En cours' }) : null,
          el('div', { title: absTime(c.connected_at), text: ago(c.connected_at) })));
    }));
  }

  // ------------------------------------------------------------ settings view

  function fillSettings() {
    $('set-limit').value = settings.limit;
    $('set-window').value = String(settings.windowMs);
    $('set-refresh').value = String(settings.refreshMs);
    for (const r of document.querySelectorAll('input[name="theme"]')) r.checked = r.value === settings.theme;
  }

  function readSettings() {
    const theme = (document.querySelector('input[name="theme"]:checked') || {}).value;
    settings = sanitize({
      limit: Number($('set-limit').value),
      windowMs: Number($('set-window').value),
      refreshMs: Number($('set-refresh').value),
      theme,
    });
    saveSettings();
    applyTheme();
    lastAlertsKey = '';
    schedule();
    const saved = $('settings-saved');
    saved.hidden = false;
    clearTimeout(readSettings.t);
    readSettings.t = setTimeout(() => { saved.hidden = true; }, 1500);
  }

  // ------------------------------------------------------------ drawer & routing

  const drawer = $('drawer');
  const scrim = $('scrim');

  function openDrawer() {
    scrim.hidden = false;
    requestAnimationFrame(() => scrim.classList.add('open'));
    drawer.classList.add('open');
    drawer.setAttribute('aria-hidden', 'false');
    $('menu-btn').setAttribute('aria-expanded', 'true');
    (drawer.querySelector('a.active') || drawer.querySelector('a')).focus();
  }

  function closeDrawer() {
    if (!drawer.classList.contains('open')) return;
    drawer.classList.remove('open');
    drawer.setAttribute('aria-hidden', 'true');
    scrim.classList.remove('open');
    $('menu-btn').setAttribute('aria-expanded', 'false');
    setTimeout(() => { scrim.hidden = true; }, 200);
    $('menu-btn').focus();
  }

  drawer.addEventListener('keydown', (ev) => {
    if (ev.key === 'Escape') closeDrawer();
    if (ev.key !== 'Tab') return;
    const items = [...drawer.querySelectorAll('a, button')];
    const first = items[0];
    const last = items[items.length - 1];
    if (ev.shiftKey && document.activeElement === first) { ev.preventDefault(); last.focus(); }
    else if (!ev.shiftKey && document.activeElement === last) { ev.preventDefault(); first.focus(); }
  });

  const VIEWS = ['alerts', 'send', 'devices', 'connections', 'settings'];

  function route() {
    const view = VIEWS.includes(location.hash.slice(1)) ? location.hash.slice(1) : 'alerts';
    currentView = view;
    for (const v of VIEWS) show('view-' + v, v === view);
    for (const a of drawer.querySelectorAll('a[data-view]')) a.classList.toggle('active', a.dataset.view === view);
    if (view === 'settings') fillSettings();
    closeDrawer();
    tick();
  }

  // ------------------------------------------------------------ polling

  async function tick() {
    clearTimeout(timer);
    try {
      await refreshOverview();
      if (currentView === 'alerts') await refreshAlerts();
      if (currentView === 'connections') await refreshConnections();
      showError(null);
    } catch (e) {
      showError(e);
    }
    schedule();
  }

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(tick, settings.refreshMs);
  }

  // ------------------------------------------------------------ wiring

  $('menu-btn').addEventListener('click', openDrawer);
  $('drawer-close').addEventListener('click', closeDrawer);
  scrim.addEventListener('click', closeDrawer);
  $('status-pill').addEventListener('click', () => { location.hash = '#devices'; });
  $('alerts-refresh').addEventListener('click', () => { lastAlertsKey = ''; tick(); });
  $('detail-close').addEventListener('click', () => $('detail-dialog').close());
  $('detail-dialog').addEventListener('click', (ev) => { if (ev.target === ev.currentTarget) ev.currentTarget.close(); });
  $('send-form').addEventListener('submit', sendAlert);
  $('settings-form').addEventListener('change', readSettings);
  $('settings-form').addEventListener('submit', (ev) => ev.preventDefault());
  $('settings-reset').addEventListener('click', () => {
    settings = { ...DEFAULTS };
    fillSettings();
    readSettings();
  });
  window.addEventListener('hashchange', route);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) tick(); });

  applyTheme();
  route();
})();
