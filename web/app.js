// Inbox admin front. Vanilla JS, no dependencies.
//
// The interface has no password: access control is the network exposure of the
// admin listener (localhost, private network or an authenticating proxy).
'use strict';

(() => {
  const POLL_MS = 3000;

  // ------------------------------------------------------------ API

  function apiError(status, data) {
    const e = new Error((data && data.error && data.error.message) || ('HTTP ' + status));
    e.status = status;
    e.code = data && data.error && data.error.code;
    return e;
  }

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
      const e = apiError(res.status, data);
      if (e.code === 'host_not_allowed') showHostError();
      throw e;
    }
    show('host-error', false);
    return data;
  }

  function showHostError() {
    $('host-error-host').textContent = location.host;
    show('host-error', true);
  }

  // ------------------------------------------------------------ DOM helpers

  const $ = (id) => document.getElementById(id);

  function el(tag, attrs, ...children) {
    const e = document.createElement(tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v === undefined || v === null || v === false) continue;
        if (k === 'class') e.className = v;
        else if (k === 'text') e.textContent = v;
        else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
        else e.setAttribute(k, v === true ? '' : v);
      }
    }
    for (const c of children) {
      if (c === null || c === undefined || c === false) continue;
      e.append(c instanceof Node ? c : document.createTextNode(String(c)));
    }
    return e;
  }

  function show(id, visible) {
    $(id).classList.toggle('hidden', !visible);
  }

  const dateFmt = new Intl.DateTimeFormat('fr-FR', { dateStyle: 'short', timeStyle: 'medium' });

  function absTime(ms) {
    return ms ? dateFmt.format(new Date(ms)) : '–';
  }

  function duration(ms) {
    if (ms === null || ms === undefined || ms < 0) return '–';
    const s = Math.round(ms / 1000);
    if (s < 60) return s + ' s';
    const m = Math.floor(s / 60);
    if (m < 60) return m + ' min';
    const h = Math.floor(m / 60);
    if (h < 48) return h + ' h ' + String(m % 60).padStart(2, '0');
    return Math.floor(h / 24) + ' j ' + (h % 24) + ' h';
  }

  function ago(ms, now) {
    if (!ms) return '–';
    return 'il y a ' + duration((now || Date.now()) - ms);
  }

  function timeCell(ms, now) {
    return el('span', { title: absTime(ms) }, ms ? ago(ms, now) : '–');
  }

  const STATUS = {
    online: 'En ligne',
    offline: 'Hors ligne',
    unreachable: 'Injoignable',
    revoked: 'Révoqué',
  };
  const ACTIONS = { alarm: 'Alarme', ring: 'Sonnerie', notify: 'Notification' };

  function showError(msg) {
    const box = $('global-error');
    box.textContent = msg || '';
    show('global-error', !!msg);
  }

  // Modal confirmation. Escape or "Annuler" resolve to false.
  function confirmDialog(text) {
    const d = $('confirm-dialog');
    $('confirm-text').textContent = text;
    return new Promise((resolve) => {
      d.addEventListener('close', () => resolve(d.returnValue === 'ok'), { once: true });
      d.returnValue = 'cancel';
      d.showModal();
      $('confirm-ok').focus();
    });
  }

  // ------------------------------------------------------------ state & rendering

  let groups = [];
  let activeTab = 'devices';
  let pollTimer = null;
  let selectedAlert = null;
  let serverOffset = 0; // server time - local time

  function renderOverview(data) {
    const now = data.server.time_ms;
    serverOffset = now - Date.now();
    $('server-name').textContent = data.server.name;
    $('server-version').textContent = 'v' + data.server.version;
    document.title = 'Inbox – ' + data.server.name;

    const st = data.stats;
    $('st-online').textContent = st.online;
    $('st-enrolled').textContent = st.enrolled;
    $('st-unreachable').textContent = st.unreachable;
    $('st-pending').textContent = st.pending_deliveries;
    $('st-oldest').textContent = st.pending_deliveries > 0 ? duration(st.oldest_pending_ms) : '–';
    $('st-unreachable-box').classList.toggle('alarm', st.unreachable > 0);
    $('st-pending-box').classList.toggle('alarm', st.pending_deliveries > 0 && st.oldest_pending_ms > 60000);

    groups = data.groups;
    renderGroupSelect();

    const body = $('devices-body');
    body.replaceChildren();
    for (const row of data.devices) {
      const d = row.device;
      const live = row.live;
      const ip = live ? live.ip : (d.last_ip || d.enrolled_ip);
      const version = live ? live.app_version : (d.app_version || '–');
      const when = live
        ? el('span', { title: 'Connecté le ' + absTime(live.connected_at) }, 'depuis ' + duration(now - live.connected_at))
        : el('span', { title: absTime(d.last_seen_at) }, d.last_seen_at ? 'vu ' + ago(d.last_seen_at, now) : 'jamais connecté');
      const subs = el('div');
      if (d.subscriptions.length === 0) subs.append(el('span', { class: 'muted' }, 'aucun'));
      for (const s of d.subscriptions) {
        const g = groups.find((x) => x.id === s.group);
        subs.append(el('span', { class: 'pill sub ' + s.action, title: ACTIONS[s.action] }, (g ? g.name : s.group) + ' · ' + ACTIONS[s.action]));
      }
      const revoke = d.revoked_at ? null : el('button', {
        class: 'btn btn-ghost btn-small',
        type: 'button',
        onclick: () => revokeDevice(d),
      }, 'Révoquer');
      body.append(el('tr', { class: row.status === 'revoked' ? 'revoked' : null },
        el('td', null, el('strong', null, d.name), el('div', { class: 'muted small mono', title: 'Enrôlé le ' + absTime(d.enrolled_at) + ' depuis ' + d.enrolled_ip }, d.id.slice(0, 12))),
        el('td', null, el('span', { class: 'pill ' + row.status }, STATUS[row.status] || row.status)),
        el('td', { class: 'mono' }, ip || '–'),
        el('td', { class: 'mono' }, version),
        el('td', null, when),
        el('td', null, subs),
        el('td', { class: 'num' }, d.revoked_at ? '–' : (d.pending > 0 ? el('span', { class: 'pill bad' }, d.pending) : '0')),
        el('td', null, revoke),
      ));
    }
    show('devices-empty', data.devices.length === 0);
  }

  function renderGroupSelect() {
    const sel = $('send-group');
    const current = sel.value;
    const ids = groups.map((g) => g.id).join(',');
    if (sel.dataset.ids !== ids) {
      sel.replaceChildren(...groups.map((g) => el('option', { value: g.id }, g.name + ' (' + g.id + ')')));
      sel.dataset.ids = ids;
      if (groups.some((g) => g.id === current)) sel.value = current;
    }
    const g = groups.find((x) => x.id === sel.value);
    $('send-group-desc').textContent = g ? g.description : '';
  }

  async function revokeDevice(d) {
    const ok = await confirmDialog('Révoquer « ' + d.name + ' » ? L’appareil sera déconnecté et ne recevra plus aucune alerte. Il devra être ré-enrôlé avec le mot de passe.');
    if (!ok) return;
    try {
      await api('POST', '/api/admin/devices/' + encodeURIComponent(d.id) + '/revoke', {});
      await refresh();
    } catch (e) {
      if (e.code !== 'host_not_allowed') showError('Révocation impossible : ' + e.message);
    }
  }

  function renderAlerts(data) {
    const body = $('alerts-body');
    body.replaceChildren();
    for (const a of data.alerts) {
      const g = groups.find((x) => x.id === a.group);
      const label = a.title || a.message || '(sans texte)';
      const missing = a.targets - a.received;
      const tr = el('tr', {
        class: selectedAlert === a.id ? 'selected' : null,
        onclick: () => { selectedAlert = a.id; loadAlertDetail(); renderAlerts(data); },
      },
        el('td', { title: absTime(a.created_at) }, absTime(a.created_at)),
        el('td', null, g ? g.name : a.group),
        el('td', null, label.length > 80 ? label.slice(0, 80) + '…' : label),
        el('td', { class: 'muted small' }, a.source),
        el('td', { class: 'num' }, a.targets === 0 ? el('span', { class: 'pill bad' }, '0') : a.targets),
        el('td', { class: 'num' }, missing > 0 ? el('span', { class: 'pill bad', title: missing + ' en attente' }, a.received) : a.received),
        el('td', { class: 'num' }, a.displayed),
        el('td', { class: 'num' }, a.acknowledged),
      );
      body.append(tr);
    }
    show('alerts-empty', data.alerts.length === 0);
  }

  async function loadAlertDetail() {
    const box = $('alert-detail');
    if (!selectedAlert) {
      show('alert-detail', false);
      box.parentElement.classList.remove('with-detail');
      return;
    }
    try {
      const a = await api('GET', '/api/admin/alerts/' + encodeURIComponent(selectedAlert));
      const g = groups.find((x) => x.id === a.group);
      const close = el('button', { class: 'btn btn-ghost btn-small', type: 'button', onclick: () => { selectedAlert = null; loadAlertDetail(); } }, 'Fermer');
      const items = [
        el('div', { class: 'delivery-head' }, el('h2', null, a.title || '(sans titre)'), close),
        el('div', { class: 'muted small' }, (g ? g.name : a.group) + ' · ' + a.source + ' · ' + absTime(a.created_at)),
        a.message ? el('div', { class: 'msg' }, a.message) : null,
        el('div', { class: 'muted small' }, a.targets + ' cible(s) · ' + a.received + ' reçue(s) · ' + a.displayed + ' affichée(s) · ' + a.acknowledged + ' acquittée(s)'),
      ];
      if (a.deliveries.length === 0) {
        items.push(el('p', { class: 'error' }, 'Aucun appareil n’était abonné à ce groupe : personne n’a été alerté.'));
      }
      for (const d of a.deliveries) {
        const state = d.acknowledged_at ? ['ok', 'Acquittée'] : d.received_at ? ['ok', 'Reçue'] : d.device_revoked ? ['revoked', 'Appareil révoqué'] : ['bad', 'En attente'];
        const step = (label, ms) => el('li', { class: ms ? 'done' : null }, el('span', null, label), el('span', { title: absTime(ms) }, ms ? '+' + duration(ms - a.created_at) : '–'));
        items.push(el('div', { class: 'delivery' },
          el('div', { class: 'delivery-head' }, el('strong', null, d.device_name), el('span', { class: 'pill ' + state[0] }, state[1])),
          el('div', { class: 'muted small' }, ACTIONS[d.action] + ' · ' + d.attempts + ' envoi(s)'),
          el('ul', { class: 'timeline' },
            step('Premier envoi', d.first_sent_at),
            step('Dernier envoi', d.last_sent_at),
            step('Reçue', d.received_at),
            step('Affichée', d.displayed_at),
            step('Acquittée', d.acknowledged_at),
          ),
        ));
      }
      box.replaceChildren(...items.filter(Boolean));
      show('alert-detail', true);
      box.parentElement.classList.add('with-detail');
    } catch (e) {
      if (e.status === 404) { selectedAlert = null; show('alert-detail', false); }
      else if (e.code !== 'host_not_allowed') showError('Détail indisponible : ' + e.message);
    }
  }

  function renderConnections(data) {
    const body = $('connections-body');
    body.replaceChildren();
    const now = Date.now() + serverOffset;
    for (const c of data.connections) {
      body.append(el('tr', null,
        el('td', null, c.device_name),
        el('td', { class: 'mono' }, c.ip),
        el('td', { class: 'mono' }, c.app_version),
        el('td', null, absTime(c.connected_at)),
        el('td', null, c.disconnected_at ? absTime(c.disconnected_at) : el('span', { class: 'pill online' }, 'en cours')),
        el('td', null, duration((c.disconnected_at || now) - c.connected_at)),
        el('td', { class: 'muted small' }, c.close_reason || ''),
      ));
    }
    show('connections-empty', data.connections.length === 0);
  }

  async function refresh() {
    try {
      const overview = await api('GET', '/api/admin/overview');
      renderOverview(overview);
      if (activeTab === 'alerts') {
        renderAlerts(await api('GET', '/api/admin/alerts?limit=50'));
        if (selectedAlert) await loadAlertDetail();
      } else if (activeTab === 'connections') {
        renderConnections(await api('GET', '/api/admin/connections?limit=100'));
      }
      setConnState(true);
      showError('');
    } catch (e) {
      setConnState(false);
      if (e.code === 'host_not_allowed') showError('');
      else showError('Serveur injoignable ou en erreur : ' + e.message);
    }
  }

  function setConnState(ok) {
    const p = $('conn-state');
    p.className = 'pill ' + (ok ? 'ok' : 'bad');
    p.textContent = ok ? 'à jour' : 'hors ligne';
  }

  function startPolling() {
    stopPolling();
    refresh();
    pollTimer = setInterval(refresh, POLL_MS);
  }

  function stopPolling() {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = null;
  }

  // ------------------------------------------------------------ views

  function selectTab(name) {
    activeTab = name;
    document.querySelectorAll('.tab').forEach((t) => {
      const on = t.dataset.tab === name;
      t.classList.toggle('active', on);
      t.setAttribute('aria-selected', on ? 'true' : 'false');
    });
    document.querySelectorAll('.panel').forEach((p) => p.classList.toggle('hidden', p.dataset.panel !== name));
    refresh();
  }

  // Idempotency key: one per alert being composed, kept across retries of the same
  // submission so a network error followed by a retry never pages twice.
  let pendingKey = null;
  let pendingFingerprint = null;

  async function sendAlert(ev) {
    ev.preventDefault();
    const group = $('send-group').value;
    const title = $('send-title').value.trim();
    const message = $('send-message').value.trim();
    const fingerprint = JSON.stringify([group, title, message]);
    if (!pendingKey || fingerprint !== pendingFingerprint) {
      pendingKey = crypto.randomUUID();
      pendingFingerprint = fingerprint;
    }
    const g = groups.find((x) => x.id === group);
    const ok = await confirmDialog('Envoyer cette alerte au groupe « ' + (g ? g.name : group) + ' » ?');
    if (!ok) return;
    const btn = $('send-submit');
    const result = $('send-result');
    btn.disabled = true;
    try {
      const body = { group, idempotency_key: pendingKey };
      if (title) body.title = title;
      if (message) body.message = message;
      const r = await api('POST', '/api/admin/alerts', body);
      if (r.targets === 0) {
        result.className = 'result bad';
        result.textContent = 'ATTENTION : aucun appareil n’est abonné à ce groupe. Personne ne recevra cette alerte.';
      } else {
        result.className = 'result ok';
        result.textContent = (r.duplicate ? 'Déjà envoyée (doublon ignoré)' : 'Alerte acceptée') + ' : ' + r.targets + ' appareil(s) ciblé(s). Suivi dans l’onglet Alertes.';
        $('send-title').value = '';
        $('send-message').value = '';
      }
      pendingKey = null;
      pendingFingerprint = null;
      selectedAlert = r.alert_id;
    } catch (e) {
      result.className = 'result bad';
      result.textContent = 'Échec de l’envoi : ' + e.message + '. Vous pouvez réessayer sans risque de doublon.';
    } finally {
      show('send-result', true);
      btn.disabled = false;
    }
  }

  function init() {
    document.querySelectorAll('.tab').forEach((t) => t.addEventListener('click', () => selectTab(t.dataset.tab)));
    $('send-group').addEventListener('change', renderGroupSelect);
    $('send-form').addEventListener('submit', sendAlert);
    $('confirm-ok').addEventListener('click', () => $('confirm-dialog').close('ok'));
    $('confirm-cancel').addEventListener('click', () => $('confirm-dialog').close('cancel'));
    startPolling();
  }

  document.addEventListener('DOMContentLoaded', init);
})();
