// Inbox admin dashboard. Vanilla JS, no dependencies.
//
// The interface has no password: access control is the network exposure of the
// admin listener (localhost, private network or an authenticating proxy).
// Alerts are one-way: the only delivery state is "received" by each device.
// The server only knows which groups a device follows; how the phone reacts to
// them (alarm, sound, notification) is a setting of the phone itself.
'use strict';

(() => {
  const { $, el, icon, api, toast, copy } = window.Inbox;
  const md = window.InboxMarkdown;

  const ALERT_WINDOW_MS = 24 * 3600 * 1000;
  const ALERT_MIN = 3;
  const ALERT_MAX = 50;
  const ALERT_PHONE = 10;
  const CARD_GAP = 8;
  const CARD_MIN = 58; // a card without details nor missing line
  const CONNECTION_LIMIT = 50;
  const REFRESH_MS = 3000;
  const DESCRIPTION_MAX = 128;

  // ------------------------------------------------------------ helpers

  const show = (id, visible) => { $(id).hidden = !visible; };
  const plural = (n, word) => n + ' ' + word + (n === 1 ? '' : 's');

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

  // Screen readers hear new alerts and devices that become unreachable.
  function announce(text) {
    const box = $('announcer');
    box.append(el('p', { text }));
    while (box.childElementCount > 4) box.firstElementChild.remove();
  }

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
    online: ['Online', 'ok'],
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
  let alerts = [];
  const groupNames = new Map();
  const renderKeys = {};
  // User filter shared by Devices and Connections: null = everyone,
  // '' = devices without a user name.
  const NO_USER = '';
  let userFilter = null;
  let timer = null;
  let lastUpdate = 0;
  let offlineSince = 0;
  let alertLimit = ALERT_MIN;
  // Last ping seen per device, to pulse the status dot when a new one arrives.
  const lastPing = new Map();
  // Announcements: alerts and device states already known (null before the first load).
  let seenAlerts = null;
  let knownStatus = null;

  const groupName = (id) => groupNames.get(id) || id;
  const matchesUser = (userName) => userFilter === null || (userName || NO_USER) === userFilter;
  const resendAfter = () => (overview ? overview.server.resend_after_ms : 30000);

  // Replaces the children of `id` only when `key` changed, so polling never
  // steals focus or resets a hovered element.
  function renderIfChanged(id, key, build) {
    if (renderKeys[id] === key) return false;
    renderKeys[id] = key;
    $(id).replaceChildren(...build());
    return true;
  }

  // ------------------------------------------------------------ first load skeletons

  function skeletons() {
    const line = (w) => el('span', { class: 'skeleton skel-line ' + w });
    $('alerts-list').replaceChildren(...[0, 1, 2].map(() => el('div', { class: 'skel-card', 'aria-hidden': 'true' }, line('w40'), line('w80'))));
    $('devices-list').replaceChildren(...[0, 1].map(() => el('div', { class: 'skel-row', 'aria-hidden': 'true' }, line('w40'), line('w60'))));
    $('connections-list').replaceChildren(...[0, 1, 2].map(() => el('div', { class: 'skel-row', 'aria-hidden': 'true' }, line('w60'), line('w40'))));
    $('coverage-list').replaceChildren(...[0, 1, 2].map(() => el('div', { class: 'cov', 'aria-hidden': 'true' },
      el('div', { class: 'cov-main' }, line('w60'), line('w40')))));
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

  // ------------------------------------------------------------ server unreachable

  function setOffline(err) {
    if (!offlineSince) offlineSince = Date.now();
    $('app').classList.add('stale');
    const pill = $('status-pill');
    pill.className = 'status-pill offline';
    pill.title = err.message;
    $('status-text').textContent = 'Server offline · since ' + clock(offlineSince);
    const data = lastUpdate ? 'Showing the data received at ' + clock(lastUpdate) + '. ' : '';
    $('offline-text').textContent = (err.offline
      ? "Can't reach the Inbox server since " + clock(offlineSince) + '. '
      : 'The Inbox server answered with an error: ' + err.message + '. ')
      + data + 'Retrying every ' + REFRESH_MS / 1000 + ' s.';
    show('offline-banner', true);
  }

  function setOnline() {
    offlineSince = 0;
    $('app').classList.remove('stale');
    show('offline-banner', false);
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

    const st = data.stats;
    const pill = $('status-pill');
    const late = st.pending_deliveries > 0 && st.oldest_pending_ms > resendAfter() * 2;
    pill.className = 'status-pill' + (st.enrolled === 0 ? ' idle' : st.unreachable > 0 || late ? ' warn' : st.online < st.enrolled ? ' partial' : '');
    let text;
    if (st.enrolled === 0) text = 'No device';
    else if (st.unreachable > 0) text = st.unreachable + ' unreachable';
    else if (late) text = 'Delivery late';
    else text = st.online + '/' + st.enrolled + ' online';
    $('status-text').textContent = text;
    pill.title = st.online + ' online out of ' + st.enrolled + ' enrolled. ' + (st.pending_deliveries > 0
      ? st.pending_deliveries + (st.pending_deliveries === 1 ? ' delivery' : ' deliveries') + ' pending, oldest for ' + duration(st.oldest_pending_ms) + '.'
      : 'No pending delivery.');

    announceDevices();
    fillGroupSelect();
    updateTargets();
    renderUserFilter();
    renderDevices();
    renderOnboarding();
  }

  function announceDevices() {
    const now = new Map(overview.devices.map((r) => [r.device.id, r.status]));
    if (knownStatus) {
      for (const r of overview.devices) {
        if (r.status === 'unreachable' && knownStatus.get(r.device.id) !== 'unreachable') {
          announce(whoText(r.device.user_name, r.device.name) + ' is unreachable.');
        }
      }
    }
    knownStatus = now;
  }

  // ------------------------------------------------------------ coverage per group

  function renderCoverage() {
    if (!overview) return;
    // Newest alert of each group that reached at least one device (24 h window).
    const lastDelivered = new Map();
    for (const a of alerts) {
      if (a.received > 0 && !lastDelivered.has(a.group)) lastDelivered.set(a.group, a.created_at);
    }
    const items = overview.groups.map((g) => {
      const subs = overview.devices.filter((r) => r.status !== 'revoked' && r.device.subscriptions.includes(g.id));
      const online = subs.filter((r) => r.status === 'online').length;
      const cls = subs.length > 0 && online === subs.length ? 'ok' : online > 0 ? 'partial' : 'none';
      return { id: g.id, name: g.name, total: subs.length, online, cls, last: lastDelivered.get(g.id) || 0 };
    });
    renderIfChanged('coverage-list', JSON.stringify(items), () => items.map((c) => {
      const sub = c.total === 0
        ? 'no device follows it'
        : c.last ? 'last delivered ' + stamp(c.last) : 'no alert in 24 h';
      const title = c.online + ' of ' + plural(c.total, 'subscribed device') + ' online';
      return el('div', { class: 'cov ' + c.cls, title },
        el('span', { class: 'cov-dot', 'aria-hidden': 'true' }),
        el('div', { class: 'cov-main' },
          el('span', { class: 'cov-name', text: c.name }),
          el('span', { class: 'cov-sub', text: sub })),
        el('span', { class: 'cov-count', 'aria-label': title }, c.online + '/' + c.total, el('small', { text: ' online' })));
    }));
    $('coverage').hidden = items.length === 0;
  }

  // ------------------------------------------------------------ alerts

  const desktop = () => window.innerWidth >= 900;

  // Upper bound of the alerts the column can show: every card at its smallest.
  // fitAlerts() then drops the cards that do not fit entirely.
  function computeAlertLimit() {
    if (!desktop()) return ALERT_PHONE;
    const h = $('alerts-body').clientHeight;
    return Math.min(ALERT_MAX, Math.max(ALERT_MIN, Math.floor((h + CARD_GAP) / (CARD_MIN + CARD_GAP))));
  }

  // Desktop: keep only whole cards (at least ALERT_MIN), so the feed never scrolls.
  function fitAlerts() {
    const cards = [...$('alerts-list').children];
    for (const c of cards) c.hidden = false;
    if (desktop()) {
      // Hiding a card never moves the ones above it: cut at the first overflow.
      const bottom = $('alerts-body').getBoundingClientRect().bottom;
      const cut = cards.findIndex((c, i) => i >= ALERT_MIN && c.getBoundingClientRect().bottom > bottom + 1);
      if (cut >= 0) cards.slice(cut).forEach((c) => { c.hidden = true; });
    }
    const shown = cards.filter((c) => !c.hidden).length;
    $('alerts-summary').textContent = shown ? 'last ' + shown + ' · 24 h' : 'last 24 h';
  }

  async function refreshAlerts() {
    alertLimit = computeAlertLimit();
    const since = Math.floor(serverNow() - ALERT_WINDOW_MS);
    const data = await api('GET', 'api/admin/alerts?limit=' + alertLimit + '&since_ms=' + since);
    alerts = data.alerts || [];
    announceAlerts();
    show('alerts-empty', alerts.length === 0);
    // The pending → not received transition depends on time: part of the key.
    const key = JSON.stringify(alerts) + '|' + alerts.map((a) => deliveryState(a).kind).join();
    renderIfChanged('alerts-list', key, () => alerts.map(alertCard));
    fitAlerts();
    renderCoverage();
  }

  function announceAlerts() {
    if (seenAlerts) {
      for (const a of alerts.slice().reverse()) {
        if (!seenAlerts.has(a.id)) announce('New alert in ' + groupName(a.group) + ': ' + (a.description || md.plain(a.details) || 'no description') + '.');
      }
    }
    seenAlerts = new Set([...(seenAlerts || []), ...alerts.map((a) => a.id)]);
  }

  // Delivery state of an alert summary.
  //   received:     every target has it
  //   pending:      still in flight (younger than two resend periods)
  //   notreceived:  older and some active device still lacks it
  //   expired:      past the delivery TTL, no longer sent
  //   revoked:      only revoked devices lack it
  //   norecipient:  nobody followed the group
  function deliveryState(a) {
    const targets = a.targets || 0;
    const received = a.received || 0;
    const expired = Math.min(a.expired || 0, targets - received);
    const lacking = targets - received - expired;
    const missing = a.missing || [];
    if (targets === 0) return { kind: 'norecipient', sev: 'warning', badge: 'warning', label: 'No recipient', targets, received, expired, missing };
    if (expired > 0) return { kind: 'expired', sev: 'neutral', badge: 'neutral', label: 'Expired', targets, received, expired, missing };
    if (lacking === 0) return { kind: 'received', sev: 'ok', badge: 'ok', label: 'Received', targets, received, expired, missing };
    if (missing.length === 0) return { kind: 'revoked', sev: 'neutral', badge: 'neutral', label: lacking + ' revoked', targets, received, expired, missing };
    if (serverNow() - a.created_at < resendAfter() * 2) return { kind: 'pending', sev: 'warning', badge: 'warning', label: 'Pending', targets, received, expired, missing };
    return { kind: 'notreceived', sev: 'danger', badge: 'danger', label: lacking + ' not received', targets, received, expired, missing };
  }

  const pseudo = (u) => u || 'no user name';

  function alertCard(a) {
    const s = deliveryState(a);
    const description = a.description || groupName(a.group);
    const preview = md.plain(a.details);
    const titles = {
      norecipient: 'No device followed this group',
      expired: 'Not received by every device before expiry: no longer sent',
      pending: 'Being delivered: devices have not confirmed yet',
      notreceived: 'Some devices still have not received this alert',
      revoked: 'Only revoked devices did not receive it',
      received: 'Received by every device',
    };

    const fill = el('span');
    fill.style.width = (s.targets ? Math.round((100 * s.received) / s.targets) : 0) + '%';
    const barCls = s.kind === 'notreceived' ? ' failed' : s.received < s.targets ? ' partial' : '';
    const bar = el('span', { class: 'bar' + barCls, 'aria-hidden': 'true' }, fill);

    let missingLine = null;
    if (s.missing.length && s.kind !== 'received') {
      const cls = s.kind === 'pending' ? ' pending' : s.kind === 'expired' ? ' expired' : '';
      const word = s.kind === 'expired' ? 'never received: ' : 'missing: ';
      missingLine = el('span', { class: 'missing' + cls, text: word + s.missing.map(pseudo).join(', ') });
    }

    return el(
      'div',
      {
        class: 'alert-card',
        tabindex: '0',
        role: 'button',
        'aria-label': description + ', ' + groupName(a.group) + ', ' + s.label,
        onclick: () => openDetail(a.id),
        onkeydown: (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); openDetail(a.id); } },
      },
      el('span', { class: 'sev ' + s.sev }),
      el('div', { class: 'alert-clock', title: absTime(a.created_at) },
        el('span', { class: 'hms', text: stamp(a.created_at) }),
        el('span', { class: 'rel', 'data-age': a.created_at, text: age(a.created_at) })),
      el('div', { class: 'alert-main' },
        el('div', { class: 'alert-head' },
          el('span', { class: 'chip', text: groupName(a.group) }),
          el('span', { class: 'alert-title', text: description })),
        preview ? el('p', { class: 'alert-message', text: preview }) : null,
        el('div', { class: 'alert-meta' }, el('span', { text: sourceLabel(a.source) }), missingLine)),
      el('div', { class: 'alert-side' },
        el('span', { class: 'badge ' + s.badge, title: titles[s.kind] }, s.label),
        s.targets > 0
          ? el('span', { class: 'receipt', title: s.received + ' received out of ' + s.targets + (s.expired ? ', ' + s.expired + ' expired' : '') },
            bar, s.received + '/' + s.targets)
          : null),
    );
  }

  // Why a device has not received an alert, from the live device list.
  function explain(x, d) {
    const dev = overview && overview.devices.find((r) => r.device.id === x.device_id);
    const sent = x.first_sent_at
      ? plural(x.attempts, 'attempt') + ', last sent ' + ago(x.last_sent_at || x.first_sent_at) + '.'
      : 'Not sent yet.';
    if (x.device_revoked || (dev && dev.status === 'revoked')) {
      return { badge: el('span', { class: 'badge neutral' }, 'Revoked'), why: 'Device revoked: it will not receive this alert. ' + sent, bad: false };
    }
    if (dev && dev.status === 'unreachable') {
      const seen = dev.device.last_seen_at;
      return {
        badge: el('span', { class: 'badge danger' }, icon('alert-circle-outline'), 'Unreachable'),
        why: (seen ? 'Unreachable since ' + stamp(seen) + ' (last seen ' + ago(seen) + ')' : 'Never connected') + '. ' + sent,
        bad: true,
      };
    }
    if (dev && dev.status === 'offline') {
      return {
        badge: el('span', { class: 'badge warning' }, icon('clock-outline'), 'Waiting'),
        why: 'Offline, last seen ' + ago(dev.device.last_seen_at) + ': delivered when it reconnects. ' + sent,
        bad: false,
      };
    }
    if (serverNow() - d.created_at < resendAfter() * 2) {
      return { badge: el('span', { class: 'badge warning' }, icon('clock-outline'), 'Pending'), why: 'Online, receipt expected shortly. ' + sent, bad: false };
    }
    return { badge: el('span', { class: 'badge danger' }, icon('clock-outline'), 'Not received'), why: 'Online but no receipt yet. ' + sent, bad: true };
  }

  async function openDetail(id) {
    let d;
    try {
      d = await api('GET', 'api/admin/alerts/' + encodeURIComponent(id));
    } catch (e) {
      toast(e.message);
      return;
    }
    $('detail-title').textContent = d.description || groupName(d.group);
    $('detail-meta').replaceChildren(
      el('span', { class: 'chip', text: groupName(d.group) }),
      el('span', { class: 'mono', text: absTime(d.created_at) }),
      el('span', { text: sourceLabel(d.source) }),
      el('span', { class: 'mono', text: (d.received || 0) + '/' + (d.targets || 0) + ' received' + (d.expired ? ' · ' + d.expired + ' expired' : '') }),
    );
    $('detail-details').replaceChildren(d.details ? md.render(d.details) : el('p', { class: 'muted', text: 'No details.' }));
    const rows = (d.deliveries || []).map((x) => {
      let side;
      let why;
      let bad = false;
      if (x.received_at) {
        side = el('span', { class: 'badge ok case', title: absTime(x.received_at) }, icon('check-circle-outline'), 'Received in ' + duration(x.received_at - d.created_at));
        why = 'Received at ' + stamp(x.received_at) + (x.attempts > 1 ? ' after ' + x.attempts + ' attempts' : '');
      } else if (x.expired) {
        side = el('span', { class: 'badge neutral', title: 'Not received before the alert expired: no longer sent' }, icon('clock-outline'), 'Expired');
        why = 'Never received before expiry. ' + (x.first_sent_at ? plural(x.attempts, 'attempt') + ', last sent ' + ago(x.last_sent_at || x.first_sent_at) : 'Never sent');
      } else {
        const e = explain(x, d);
        side = e.badge;
        why = e.why;
        bad = e.bad;
      }
      return el('div', { class: 'row' },
        el('div', { class: 'row-main' },
          el('div', { class: 'row-title' }, who(x.user_name, x.device_name)),
          el('div', { class: 'why' + (bad ? ' bad' : ''), text: why })),
        el('div', { class: 'row-side' }, side));
    });
    if (rows.length === 0) rows.push(el('div', { class: 'row' }, el('span', { class: 'muted', text: 'No device followed this group.' })));
    $('detail-deliveries').replaceChildren(...rows);
    $('detail-dialog').showModal();
  }

  // ------------------------------------------------------------ send

  function subscribersOf(group) {
    if (!overview) return null;
    return overview.devices.filter((d) => d.status !== 'revoked' && d.device.subscriptions.includes(group)).length;
  }

  function fillGroupSelect() {
    const sel = $('send-group');
    const current = sel.value;
    const opts = overview.groups.map((g) => [g.id, g.name + ' · ' + plural(subscribersOf(g.id), 'device')]);
    const key = JSON.stringify(opts);
    if (sel.dataset.key === key) return;
    sel.dataset.key = key;
    sel.replaceChildren(
      el('option', { value: '', text: 'Choose a group…' }),
      ...overview.groups.map((g, i) => el('option', { value: g.id, title: g.description || null, text: opts[i][1] })),
    );
    sel.value = opts.some(([id]) => id === current) ? current : '';
    sel.disabled = false;
  }

  function updateTargets() {
    const group = $('send-group').value;
    const n = group ? subscribersOf(group) : null;
    const t = $('send-hint');
    t.classList.toggle('warn-text', n === 0);
    t.textContent = !group ? 'Choose the group to page' : n === 0 ? 'No device follows this group' : plural(n, 'device') + ' will be paged';
    $('send-targets').textContent = overview ? plural(overview.groups.length, 'group') : '';
    $('send-btn').disabled = !overview || !group;
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

  function toggleSend(open) {
    $('send-toggle').setAttribute('aria-expanded', open ? 'true' : 'false');
    $('send-form').hidden = !open;
    if (open) $('send-group').focus();
    else $('send-result').hidden = true;
    // The feed lost or regained height: fetch the matching number of alerts.
    scheduleRelayout();
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
    if (!body.group) {
      $('send-group').focus();
      return;
    }
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
      result.className = 'banner ' + (r.targets === 0 ? 'warning' : 'ok');
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
      refreshAlerts().catch(() => {});
    } catch (e) {
      result.className = 'banner danger';
      result.replaceChildren(icon('alert-circle-outline'), el('span', { text: 'Sending failed: ' + e.message + '. Retry: the alert will not be duplicated.' }));
      result.hidden = false;
    } finally {
      updateTargets();
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
    refreshConnections().catch(() => {});
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
        if (dot) {
          dot.classList.add('pulse');
          // A timer, not animationend: with reduced motion the static ring
          // must still show for a moment.
          setTimeout(() => dot.classList.remove('pulse'), 1200);
        }
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

  // ------------------------------------------------------------ onboarding

  // Device API base as phones reach it: same host, API listener port.
  function apiBase() {
    return location.protocol + '//' + location.hostname + ':' + overview.server.api_port;
  }
  const phoneLink = () => 'inbox://add?url=' + encodeURIComponent(apiBase());

  function renderOnboarding() {
    const base = apiBase();
    $('api-address').textContent = base;
    $('phone-link').textContent = phoneLink();
    $('phone-link').href = phoneLink();
    $('phone-link-btn').hidden = overview.devices.length === 0;
  }

  // ------------------------------------------------------------ connections

  async function refreshConnections() {
    const filter = userFilter !== null ? '&user=' + encodeURIComponent(userFilter) : '';
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
        el('div', { class: 'row-side' }, open ? el('span', { class: 'badge ok', text: 'Live' }) : null));
    }));
  }

  // ------------------------------------------------------------ polling

  async function tick() {
    clearTimeout(timer);
    try {
      await refreshOverview();
      await Promise.all([refreshAlerts(), refreshConnections()]);
      lastUpdate = Date.now();
      $('app').classList.remove('loading');
      setOnline();
    } catch (e) {
      if (e.code === 'host_not_allowed') setOnline();
      else setOffline(e);
    }
    tickClock();
    timer = setTimeout(tick, REFRESH_MS);
  }

  let relayoutTimer = null;
  function scheduleRelayout() {
    clearTimeout(relayoutTimer);
    relayoutTimer = setTimeout(() => {
      if (!overview || offlineSince) return;
      if (computeAlertLimit() !== alertLimit) refreshAlerts().catch(() => {});
      else fitAlerts();
    }, 250);
  }

  // ------------------------------------------------------------ wiring

  $('detail-close').addEventListener('click', () => $('detail-dialog').close());
  $('detail-dialog').addEventListener('click', (ev) => { if (ev.target === ev.currentTarget) ev.currentTarget.close(); });
  $('send-toggle').addEventListener('click', () => toggleSend($('send-form').hidden));
  $('send-cancel').addEventListener('click', () => { toggleSend(false); $('send-toggle').focus(); });
  $('send-form').addEventListener('submit', sendAlert);
  $('send-group').addEventListener('change', updateTargets);
  $('send-description').addEventListener('input', updateCounter);
  $('tab-write').addEventListener('click', () => selectTab(false));
  $('tab-preview').addEventListener('click', () => selectTab(true));
  $('api-address-copy').addEventListener('click', () => copy(apiBase(), $('api-address')));
  $('phone-link-copy').addEventListener('click', () => copy(phoneLink(), $('phone-link')));
  $('phone-link-btn').addEventListener('click', () => copy(phoneLink()));
  window.addEventListener('resize', scheduleRelayout);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) tick(); });
  setInterval(tickClock, 1000);

  skeletons();
  updateCounter();
  tick();
})();
