// Inbox admin dashboard. Vanilla JS, no dependencies.
//
// The interface has no password: access control is the network exposure of the
// admin listener (localhost, private network or an authenticating proxy).
// Alerts are one-way: the only delivery state is "received" by each device.
// The server only knows which groups a device follows; how the phone reacts to
// them (alarm, sound, notification) is a setting of the phone itself.
'use strict';

(() => {
  const { $, el, icon, api, toast } = window.Inbox;
  const md = window.InboxMarkdown;

  const ALERT_LIMIT = 5;
  const ALERT_WINDOW_MS = 24 * 3600 * 1000;
  const CONNECTION_LIMIT = 50;
  const REFRESH_MS = 3000;
  const DESCRIPTION_MAX = 128;

  // ------------------------------------------------------------ helpers

  const show = (id, visible) => { $(id).hidden = !visible; };

  function showError(err) {
    const box = $('global-error');
    if (!err || err.code === 'host_not_allowed') {
      box.hidden = true;
      return;
    }
    box.replaceChildren(icon('alert-circle-outline'), el('span', { text: 'Server unreachable: ' + err.message }));
    box.hidden = false;
  }

  function confirmDialog(text, okLabel) {
    const d = $('confirm-dialog');
    $('confirm-text').textContent = text;
    $('confirm-ok').textContent = okLabel || 'Confirm';
    return new Promise((resolve) => {
      d.addEventListener('close', () => resolve(d.returnValue === 'ok'), { once: true });
      d.returnValue = 'cancel';
      d.showModal();
      $('confirm-ok').focus();
    });
  }

  const plural = (n, word) => n + ' ' + word + (n === 1 ? '' : 's');

  // ------------------------------------------------------------ formatting

  let serverOffset = 0; // server clock - local clock
  const serverNow = () => Date.now() + serverOffset;
  const dateFmt = new Intl.DateTimeFormat('en-GB', { dateStyle: 'medium', timeStyle: 'medium' });
  const clockFmt = new Intl.DateTimeFormat('en-GB', { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });
  const dayFmt = new Intl.DateTimeFormat('en-GB', { day: '2-digit', month: 'short' });
  const absTime = (ms) => (ms ? dateFmt.format(new Date(ms)) : '–');
  const clock = (ms) => (ms ? clockFmt.format(new Date(ms)) : '--:--:--');
  const sameDay = (ms) => new Date(ms).toDateString() === new Date(serverNow()).toDateString();
  const stamp = (ms) => (sameDay(ms) ? clock(ms) : dayFmt.format(new Date(ms)) + ' ' + clock(ms).slice(0, 5));

  // Compact duration for dense rows: "12s", "4m", "3h 05m", "2d".
  function duration(ms) {
    const s = Math.max(0, Math.floor(ms / 1000));
    if (s < 60) return s + 's';
    const m = Math.floor(s / 60);
    if (m < 60) return m + 'm';
    const h = Math.floor(m / 60);
    if (h < 48) return h + 'h' + (m % 60 ? ' ' + String(m % 60).padStart(2, '0') + 'm' : '');
    return Math.floor(h / 24) + 'd';
  }
  const age = (ms) => (ms ? duration(serverNow() - ms) : '–');
  const ago = (ms) => (ms ? (serverNow() - ms < 1000 ? 'just now' : duration(serverNow() - ms) + ' ago') : '–');

  function sourceLabel(source) {
    if (source === 'admin') return 'Admin console';
    const [kind, name] = String(source).split(':');
    if (kind === 'producer') return 'API · ' + name;
    if (kind === 'webhook') return 'Webhook · ' + name;
    if (kind === 'notify') return 'Notify · ' + name;
    return source;
  }

  const STATUS = {
    online: ['Online', 'success'],
    offline: ['Offline', 'neutral'],
    unreachable: ['Unreachable', 'danger'],
    revoked: ['Revoked', 'neutral'],
  };
  const CLOSE_REASONS = {
    peer_closed: 'closed by the device',
    timeout: 'timed out',
    replaced: 'replaced by a new session',
    revoked: 'device revoked',
    server_restart: 'server restart',
    protocol_error: 'protocol error',
    storage_error: 'storage error',
    send_failed: 'send failed',
  };


  // "user · device": the user name is the primary label.
  function who(userName, deviceName) {
    const user = userName
      ? el('span', { class: 'who', text: userName })
      : el('span', { class: 'who none', text: 'No user name' });
    return el('span', { class: 'who-line' }, user, el('span', { class: 'who-device', text: ' · ' + deviceName }));
  }
  const whoText = (userName, deviceName) => (userName || 'No user name') + ' · ' + deviceName;

  // ------------------------------------------------------------ state

  let overview = null;
  const groupNames = new Map();
  const renderKeys = {};
  // User filter shared by Devices and Connections: null = everyone,
  // '' = devices without a user name.
  const NO_USER = '';
  let userFilter = null;
  let timer = null;
  let lastUpdate = 0;
  // Last ping seen per device, to pulse the status dot when a new one arrives.
  const lastPing = new Map();

  const groupName = (id) => groupNames.get(id) || id;
  const matchesUser = (userName) => userFilter === null || (userName || NO_USER) === userFilter;

  // Replaces the children of `id` only when `key` changed, so polling never
  // steals focus or resets a hovered element.
  function renderIfChanged(id, key, build) {
    if (renderKeys[id] === key) return false;
    renderKeys[id] = key;
    $(id).replaceChildren(...build());
    return true;
  }

  // ------------------------------------------------------------ live ticker

  // Every second: relative times and "last ping" counters move without waiting
  // for the next poll.
  function tickClock() {
    for (const t of document.querySelectorAll('[data-age]')) t.textContent = age(Number(t.dataset.age));
    for (const t of document.querySelectorAll('[data-since]')) t.textContent = (t.dataset.prefix || '') + ago(Number(t.dataset.since));
    const f = $('freshness');
    if (lastUpdate) {
      const s = Math.floor((Date.now() - lastUpdate) / 1000);
      f.textContent = 'updated ' + (s < 1 ? 'just now' : s + 's ago');
      f.classList.toggle('stale', s > 10);
    }
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
    if (st.enrolled === 0) text = 'No device';
    else if (st.unreachable > 0) text = st.unreachable + ' unreachable';
    else if (late) text = 'Delivery pending';
    else text = st.online + '/' + st.enrolled + ' online';
    $('status-text').textContent = text;
    pill.title = st.online + ' online out of ' + st.enrolled + ' enrolled. ' + (st.pending_deliveries > 0
      ? st.pending_deliveries + (st.pending_deliveries === 1 ? ' delivery' : ' deliveries') + ' pending, oldest for ' + duration(st.oldest_pending_ms) + '.'
      : 'No pending delivery.');

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
    renderIfChanged('alerts-list', JSON.stringify(alerts), () => alerts.map(alertRow));
  }

  function alertRow(a) {
    const targets = a.targets || 0;
    const received = a.received || 0;
    // Expired: never received within the server's delivery TTL, no longer sent.
    const expired = Math.min(a.expired || 0, targets - received);
    const missing = targets - received - expired;
    const description = a.description || groupName(a.group);
    const preview = md.plain(a.details);

    let state;
    let sev;
    if (targets === 0) {
      state = el('span', { class: 'badge warning', title: 'No device followed this group' }, 'No recipient');
      sev = 'warning';
    } else if (missing > 0) {
      state = el('span', { class: 'badge danger' }, missing + ' not received');
      sev = 'danger';
    } else if (expired === 0) {
      state = el('span', { class: 'badge success' }, 'Received');
      sev = 'ok';
    } else {
      state = el('span', { class: 'badge neutral', title: 'Not received before expiry: no longer sent' }, 'Expired');
      sev = 'neutral';
    }

    const fill = el('span');
    fill.style.width = (targets ? Math.round((100 * received) / targets) : 0) + '%';
    const bar = el('span', { class: 'bar' + (missing > 0 || expired > 0 ? ' partial' : ''), 'aria-hidden': 'true' }, fill);

    return el(
      'div',
      {
        class: 'row clickable alert-row',
        tabindex: '0',
        role: 'button',
        'aria-label': description,
        onclick: () => openDetail(a.id),
        onkeydown: (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); openDetail(a.id); } },
      },
      el('span', { class: 'sev ' + sev }),
      el('div', { class: 'alert-clock', title: absTime(a.created_at) },
        el('span', { class: 'hms', text: stamp(a.created_at) }),
        el('span', { class: 'rel', 'data-age': a.created_at, text: age(a.created_at) })),
      el('div', { class: 'row-main' },
        el('div', { class: 'alert-head' },
          el('span', { class: 'chip', text: groupName(a.group) }),
          el('span', { class: 'alert-title', text: description })),
        preview ? el('p', { class: 'alert-message', text: preview }) : null,
        el('div', { class: 'alert-meta' }, el('span', { text: sourceLabel(a.source) }), expired ? el('span', { text: expired + ' expired' }) : null)),
      el('div', { class: 'alert-side' },
        state,
        targets > 0
          ? el('span', { class: 'receipt', title: received + ' received out of ' + targets + (expired ? ', ' + expired + ' expired' : '') },
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
    $('detail-title').textContent = d.description || groupName(d.group);
    $('detail-meta').textContent = absTime(d.created_at) + ' · ' + sourceLabel(d.source) + ' · ' + (d.received || 0) + '/' + (d.targets || 0) + ' received'
      + (d.expired ? ' · ' + d.expired + ' expired' : '');
    $('detail-details').replaceChildren(d.details ? md.render(d.details) : el('p', { class: 'muted', text: 'No details.' }));
    const rows = (d.deliveries || []).map((x) => {
      const sub = [];
      if (x.first_sent_at) sub.push('sent ' + ago(x.first_sent_at));
      else sub.push('not sent yet');
      if (x.attempts > 1) sub.push(x.attempts + ' attempts');
      if (x.device_revoked) sub.push('device revoked');
      let side;
      if (x.received_at) side = el('span', { class: 'badge success', title: absTime(x.received_at) }, icon('check-circle-outline'), 'Received in ' + duration(x.received_at - d.created_at));
      else if (x.expired) side = el('span', { class: 'badge neutral', title: 'Not received before the alert expired: no longer sent' }, icon('clock-outline'), 'Expired (not received)');
      else side = el('span', { class: 'badge danger' }, icon('clock-outline'), x.first_sent_at ? 'Pending' : 'Not sent yet');
      return el('div', { class: 'row' },
        el('div', { class: 'row-main' }, el('div', { class: 'row-title' }, who(x.user_name, x.device_name)), el('div', { class: 'row-sub', text: sub.join(' · ') })),
        el('div', { class: 'row-side' }, side));
    });
    if (rows.length === 0) rows.push(el('div', { class: 'row' }, el('span', { class: 'muted', text: 'No device followed this group.' })));
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
    return overview.devices.filter((d) => d.status !== 'revoked' && d.device.subscriptions.includes(group)).length;
  }

  function updateTargets() {
    const n = subscribersOf($('send-group').value);
    const t = $('send-hint');
    t.classList.toggle('warn-text', n === 0);
    t.textContent = n === null ? '' : n === 0 ? 'No device follows this group' : plural(n, 'device') + ' following';
    $('send-targets').textContent = overview ? plural(overview.groups.length, 'group') : '';
  }

  function updateCounter() {
    const n = [...$('send-description').value].length;
    const c = $('desc-count');
    c.textContent = n + '/' + DESCRIPTION_MAX;
    c.classList.toggle('near', n >= DESCRIPTION_MAX - 8);
  }

  function selectTab(preview) {
    $('tab-write').setAttribute('aria-selected', preview ? 'false' : 'true');
    $('tab-preview').setAttribute('aria-selected', preview ? 'true' : 'false');
    $('send-details').hidden = preview;
    $('details-preview').hidden = !preview;
    if (preview) {
      const src = $('send-details').value;
      $('details-preview').replaceChildren(src.trim() ? md.render(src) : el('p', { class: 'muted', text: 'Nothing to preview.' }));
    } else {
      $('send-details').focus();
    }
  }

  let pendingSend = null; // { fingerprint, key }: the same content reuses its idempotency key

  async function sendAlert(ev) {
    ev.preventDefault();
    const description = [...$('send-description').value.trim()].slice(0, DESCRIPTION_MAX).join('');
    const body = {
      group: $('send-group').value,
      description: description || undefined,
      details: $('send-details').value.trim() || undefined,
    };
    if (!body.group) return;
    const fingerprint = JSON.stringify(body);
    if (!pendingSend || pendingSend.fingerprint !== fingerprint) {
      pendingSend = { fingerprint, key: crypto.randomUUID() };
    }
    const n = subscribersOf(body.group);
    const what = groupName(body.group) + (n !== null ? ' (' + plural(n, 'device') + ')' : '');
    if (!(await confirmDialog('Send this alert to the group ' + what + '?', 'Send'))) return;

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
            ? 'Alert stored but NO device follows this group: nobody will be paged.'
            : 'Alert sent to ' + plural(r.targets, 'device') + (r.duplicate ? ' (already sent, not duplicated).' : '.'),
        }),
      );
      result.hidden = false;
      $('send-description').value = '';
      $('send-details').value = '';
      updateCounter();
      selectTab(false);
      refreshAlerts().catch(showError);
    } catch (e) {
      result.className = 'banner danger';
      result.replaceChildren(icon('alert-circle-outline'), el('span', { text: 'Sending failed: ' + e.message + '. Retry: the alert will not be duplicated.' }));
      result.hidden = false;
    } finally {
      btn.disabled = false;
    }
  }

  // ------------------------------------------------------------ user filter

  function renderUserFilter() {
    const users = new Map();
    for (const r of overview.devices) {
      const k = r.device.user_name || NO_USER;
      users.set(k, (users.get(k) || 0) + 1);
    }
    if (userFilter !== null && !users.has(userFilter)) userFilter = null;
    const names = [...users.keys()].filter((k) => k !== NO_USER).sort((a, b) => a.localeCompare(b, 'en'));
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
      chip(null, 'All'),
      ...names.map((n) => chip(n, n, users.get(n))),
      users.has(NO_USER) ? chip(NO_USER, 'No user name', users.get(NO_USER)) : null,
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
    $('devices-summary').textContent = st.online + '/' + st.enrolled + ' online' + (st.unreachable ? ' · ' + st.unreachable + ' unreachable' : '');
    const list = overview.devices
      .filter((r) => matchesUser(r.device.user_name))
      .sort((a, b) => (a.device.user_name || '￿').localeCompare(b.device.user_name || '￿', 'en') || a.device.name.localeCompare(b.device.name, 'en'));
    show('devices-empty', overview.devices.length === 0);
    show('devices-list', list.length > 0);
    const rendered = renderIfChanged('devices-list', JSON.stringify(list), () => list.map(deviceRow));
    // A new ping since the last poll: pulse the device's status dot.
    for (const r of list) {
      const seen = (r.live && r.live.last_seen_at) || 0;
      const before = lastPing.get(r.device.id);
      if (rendered && seen && before !== undefined && seen !== before) {
        const dot = document.querySelector('[data-dot="' + CSS.escape(r.device.id) + '"]');
        if (dot) dot.classList.add('pulse');
      }
      lastPing.set(r.device.id, seen);
    }
    tickClock();
  }

  function deviceRow(row) {
    const d = row.device;
    const live = row.live;
    const [label, cls] = STATUS[row.status] || [row.status, 'neutral'];
    const meta = [
      (live && live.ip) || d.last_ip || 'unknown IP',
      'v' + ((live && live.app_version) || d.app_version || '?'),
    ];
    if (d.pending && row.status !== 'revoked') meta.push(plural(d.pending, 'pending alert'));
    // Realtime presence: online devices tick "last ping …" every second.
    const presence = live
      ? el('span', { class: 'presence live' },
        el('span', { 'data-since': live.last_seen_at || live.connected_at, 'data-prefix': 'last ping ', text: 'last ping ' + ago(live.last_seen_at || live.connected_at) }),
        el('span', { class: 'sep', text: ' · ' }),
        el('span', { 'data-since': live.connected_at, 'data-prefix': 'connected ', text: 'connected ' + ago(live.connected_at) }))
      : el('span', { class: 'presence' },
        el('span', { 'data-since': d.last_seen_at, 'data-prefix': 'last seen ', text: 'last seen ' + ago(d.last_seen_at) }));
    const groups = d.subscriptions;
    const subs = groups.length
      ? el('div', { class: 'subs' }, ...groups.map((g) => el('span', { class: 'chip', text: groupName(g) })))
      : el('div', { class: 'subs muted', text: 'No group followed' });
    const revoke = row.status === 'revoked' ? null : el('button', {
      class: 'icon-btn danger',
      type: 'button',
      title: 'Revoke',
      'aria-label': 'Revoke ' + whoText(d.user_name, d.name),
      onclick: async () => {
        if (!(await confirmDialog('Revoke the device "' + whoText(d.user_name, d.name) + '"? It will have to enroll again to receive alerts.', 'Revoke'))) return;
        try {
          await api('POST', 'api/admin/devices/' + encodeURIComponent(d.id) + '/revoke', {});
          toast('Device revoked');
          await refreshOverview();
        } catch (e) {
          toast('Failed: ' + e.message);
        }
      },
    }, icon('delete-outline'));
    return el('div', { class: 'row device-row' + (row.status === 'revoked' ? ' revoked' : '') },
      el('span', { class: 'status-dot ' + cls, title: label, 'data-dot': d.id }),
      el('div', { class: 'row-main' },
        el('div', { class: 'row-title' }, who(d.user_name, d.name)),
        el('div', { class: 'row-sub' }, meta.join(' · ') + ' · ', presence),
        subs),
      el('div', { class: 'row-side' }, el('span', { class: 'badge ' + cls, text: label }), revoke));
  }

  // ------------------------------------------------------------ connections

  async function refreshConnections() {
    const filter = userFilter ? '&user=' + encodeURIComponent(userFilter) : '';
    const data = await api('GET', 'api/admin/connections?limit=' + CONNECTION_LIMIT + filter);
    const list = (data.connections || []).filter((c) => matchesUser(c.user_name));
    $('connections-summary').textContent = 'last ' + list.length + (userFilter === null ? '' : ' · ' + (userFilter || 'no user name'));
    show('connections-empty', list.length === 0);
    show('connections-list', list.length > 0);
    renderIfChanged('connections-list', JSON.stringify(list), () => list.map((c) => {
      const open = !c.disconnected_at;
      const sub = [c.ip, 'v' + c.app_version];
      if (!open) sub.push(duration(c.disconnected_at - c.connected_at) + (c.close_reason ? ' · ' + (CLOSE_REASONS[c.close_reason] || c.close_reason) : ''));
      return el('div', { class: 'row' },
        el('span', { class: 'sev ' + (open ? 'ok' : '') }),
        el('span', { class: 'row-time conn-time', title: absTime(c.connected_at), text: stamp(c.connected_at) }),
        el('div', { class: 'row-main' },
          el('div', { class: 'row-title' }, who(c.user_name, c.device_name)),
          el('div', { class: 'row-sub' }, sub.join(' · '),
            open ? el('span', { 'data-since': c.connected_at, 'data-prefix': ' · since ', text: ' · since ' + ago(c.connected_at) }) : null)),
        el('div', { class: 'row-side' }, open ? el('span', { class: 'badge success', text: 'Live' }) : null));
    }));
  }

  // ------------------------------------------------------------ polling

  async function tick() {
    clearTimeout(timer);
    try {
      await refreshOverview();
      await Promise.all([refreshAlerts(), refreshConnections()]);
      lastUpdate = Date.now();
      showError(null);
    } catch (e) {
      showError(e);
    }
    tickClock();
    timer = setTimeout(tick, REFRESH_MS);
  }

  // ------------------------------------------------------------ wiring

  $('detail-close').addEventListener('click', () => $('detail-dialog').close());
  $('detail-dialog').addEventListener('click', (ev) => { if (ev.target === ev.currentTarget) ev.currentTarget.close(); });
  $('send-form').addEventListener('submit', sendAlert);
  $('send-group').addEventListener('change', updateTargets);
  $('send-description').addEventListener('input', updateCounter);
  $('tab-write').addEventListener('click', () => selectTab(false));
  $('tab-preview').addEventListener('click', () => selectTab(true));
  document.addEventListener('animationend', (ev) => { if (ev.target.classList) ev.target.classList.remove('pulse'); });
  document.addEventListener('visibilitychange', () => { if (!document.hidden) tick(); });
  setInterval(tickClock, 1000);

  updateCounter();
  tick();
})();
