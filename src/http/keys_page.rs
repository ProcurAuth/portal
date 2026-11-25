//! The keys of the tab Access and their addresses: `GET /auth/keys/new`,
//! `GET /auth/keys/edit`, `POST /auth/keys/create`, `POST /auth/keys/edit` and
//! `POST /auth/keys/revoke`. The list of the keys is a panel of the tab Access: `GET /auth/keys`
//! does not exist.
//!
//! Each address needs a session. The list shows the keys of the user. A user with `keys.all.read`
//! also sees the keys of all the users. A session that "Log in as" opens can read but never make,
//! change or revoke a key. The text of a new key is in the answer of the creation, one
//! time, with `Cache-Control: no-store`. The portal puts it in no address and in no log.

use super::*;
use crate::grants::ACCESS;
use crate::key_forms::Context;
use crate::keyauth;
use crate::keys::KeyKind;
use crate::portal_actions as portal;
use crate::store::{KeyCaps, KeyListItem, KeyScope, KeyWriteError};
use crate::ui::grant_editor::{self, EditorData};
use crate::ui::keys::{
    self as ui_keys, Cover, EditTarget, GrantLine, KeyLine, KeyState, KeysList, KeysView, NewKey,
    Pane,
};
use crate::users::KeyOwner;

/// The purpose of the anti-forgery tokens of the forms of the keys.
pub(super) const KEYS_TOKEN: &str = "keys";
/// The lines that the list of the tab Access holds at most.
const LIST_LIMIT: usize = 500;
/// The lines that one read of the database gives.
const PAGE_SIZE: usize = 100;
/// The largest body of the forms of the keys: 512 grants of 12 KiB and their limits.
pub(super) const MAX_FORM_BYTES: usize = 7 * 1024 * 1024;

/// The rights that the viewer has on the page.
struct Rights {
    read_own: bool,
    /// The kinds of key that the viewer may make: `keys.own.create` on the kind, and the file turns
    /// the kind on. `create` is true when there is one at least.
    create_kinds: Vec<KeyKind>,
    create: bool,
    edit: bool,
    revoke: bool,
    read_all: bool,
    revoke_all: bool,
    /// The viewer can make keys and cannot use them.
    cannot_use: bool,
}

fn rights_of(state: &AppState, viewer: &Viewer) -> Rights {
    let on: Vec<KeyKind> = state
        .config
        .keys
        .as_ref()
        .map(|keys| keys.kinds())
        .unwrap_or_default();
    let (read_own, create_kinds, edit, revoke, read_all, revoke_all, cannot_use) =
        match &viewer.grants {
            // The superadmin in its own right, and in the session of the superadmin as such.
            None => (true, on.clone(), true, true, true, true, false),
            Some(grants) => {
                let has = |action| portal::effective(grants, action);
                let create_kinds: Vec<KeyKind> = on
                    .iter()
                    .copied()
                    .filter(|kind| portal::effective_kind(grants, portal::KEYS_OWN_CREATE, *kind))
                    .collect();
                // The viewer can make a kind of key and cannot use it.
                let cannot_use = create_kinds
                    .iter()
                    .any(|kind| !portal::effective_kind(grants, portal::KEYS_OWN_USE, *kind));
                (
                    has(portal::KEYS_OWN_READ),
                    create_kinds,
                    has(portal::KEYS_OWN_EDIT),
                    has(portal::KEYS_OWN_REVOKE),
                    has(portal::KEYS_ALL_READ),
                    has(portal::KEYS_ALL_REVOKE),
                    cannot_use,
                )
            }
        };
    // A session that "Log in as" opens changes and revokes no key.
    let browsing_as_user = viewer.session.impersonated;
    let create_kinds = if browsing_as_user {
        Vec::new()
    } else {
        create_kinds
    };
    Rights {
        read_own,
        create: !create_kinds.is_empty(),
        create_kinds,
        edit: edit && !browsing_as_user,
        revoke: revoke && !browsing_as_user,
        read_all,
        revoke_all: revoke_all && !browsing_as_user,
        cannot_use,
    }
}

/// Whether the viewer fills a form that gives grants: the superadmin, and a user who makes or
/// changes keys. These forms use the options of the services.
pub(super) fn may_fill_forms(state: &AppState, viewer: &Viewer) -> bool {
    let rights = rights_of(state, viewer);
    viewer.grants.is_none() || rights.create || rights.edit
}

/// Whose keys the section "Mine" shows.
enum Me {
    Superadmin,
    Database(String),
    File(String),
}

fn me(state: &AppState, viewer: &Viewer) -> Me {
    match &viewer.session.principal {
        Principal::Superadmin => Me::Superadmin,
        Principal::User(login) if state.config.access.accounts.contains_key(login) => {
            Me::File(login.clone())
        }
        Principal::User(login) => Me::Database(login.clone()),
    }
}

/// The owner of a key as the store names it: `None` for the superadmin.
fn owner_login(me: &Me) -> Option<&str> {
    match me {
        Me::Superadmin => None,
        Me::Database(login) | Me::File(login) => Some(login),
    }
}

/// The grants of an owner, to compute what a key gets. `None`: the superadmin, who has every right.
fn owner_grants(state: &AppState, login: Option<&str>) -> Option<Vec<Grant>> {
    let login = login?;
    Some(
        keyauth::person_grants(state, login)
            .map(|(grants, _)| grants)
            .unwrap_or_default(),
    )
}

/// How the rights of the owner cover a grant of a key.
fn cover_of(owner: Option<&[Grant]>, grant: &Grant) -> Cover {
    let Some(held) = owner else {
        return Cover::Full;
    };
    let same = |g: &&Grant| g.action == grant.action && g.service == grant.service;
    if held
        .iter()
        .filter(same)
        .any(|g| grant.path.starts_with(&g.path))
    {
        Cover::Full
    } else if held
        .iter()
        .filter(same)
        .any(|g| g.path.len() > grant.path.len() && g.path.starts_with(&grant.path))
    {
        Cover::Limited
    } else {
        Cover::None
    }
}

/// Whether the side of the owner asks for the approval of the action of `grant`: a grant of the
/// owner with `confirm` covers the node of the grant. The superadmin has no side.
fn imposed_by_owner(owner: Option<&[Grant]>, grant: &Grant) -> bool {
    owner.is_some_and(|held| {
        held.iter().any(|g| {
            g.confirm
                && g.action == grant.action
                && g.service == grant.service
                && grant.path.starts_with(&g.path)
        })
    })
}

/// The editor of the grants of a form of a key of this kind. The owner can give what the owner
/// holds, so each service where the owner holds a grant is in the editor. An API key holds the
/// alert actions of the portal features and no service. An MCP key holds services. `ticked` are the
/// grants that the form has at the start.
fn editor_for(
    state: &AppState,
    kind: KeyKind,
    owner: Option<&[Grant]>,
    ticked: Vec<Grant>,
) -> EditorData {
    let catalog = current_catalog(state);
    let services = if kind == KeyKind::Mcp {
        // An external service is a link: no key form offers it.
        service_grants(state, &catalog)
            .into_iter()
            .filter(|s| !s.external)
            .collect()
    } else {
        Vec::new()
    };
    let features = portal::key_document(&state.config.portal_features(), kind)
        .map(|document| feature_actions(state, document));
    // The rights of the owner on an external service are not for a key: the editor does not carry them.
    let owner: Option<Vec<Grant>> = owner.map(|held| {
        held.iter()
            .filter(|g| catalog.service(&g.service).is_none_or(|s| !s.external))
            .cloned()
            .collect()
    });
    grant_editor::build(
        &services,
        features.as_ref(),
        owner.as_deref(),
        |grant| ui_keys::is_offered(kind, grant),
        ticked,
        true,
    )
}

/// The state of a key at `now`: the first that applies in the order expired, too old,
/// active.
fn state_of(
    state: &AppState,
    login: Option<&str>,
    kind: Option<KeyKind>,
    created_at: i64,
    expires_at: Option<i64>,
    now: i64,
) -> KeyState {
    if expires_at.is_some_and(|at| at <= now) {
        return KeyState::Expired;
    }
    let max = login
        .and_then(|l| keyauth::person_grants(state, l))
        .and_then(|(grants, _)| match kind {
            Some(kind) => portal::max_key_lifetime(&grants, kind),
            None => portal::max_key_lifetime_any(&grants),
        });
    if max
        .is_some_and(|max| now.saturating_sub(created_at) > i64::try_from(max).unwrap_or(i64::MAX))
    {
        return KeyState::TooOld;
    }
    KeyState::Active
}

fn grant_lines(grants: &[Grant], owner: Option<&[Grant]>) -> Vec<GrantLine> {
    grants
        .iter()
        .map(|g| GrantLine {
            grant: g.clone(),
            cover: cover_of(owner, g),
            imposed: imposed_by_owner(owner, g),
        })
        .collect()
}

/// The services where the key has a grant on a node and no grant `access`.
fn without_access(grants: &[Grant]) -> Vec<String> {
    let mut out: Vec<String> = grants
        .iter()
        .filter(|g| g.action != ACCESS || !g.path.is_empty())
        // The portal features need no grant `access`: it only carries a budget.
        .filter(|g| g.service != portal::SERVICE)
        .filter(|g| {
            !grants
                .iter()
                .any(|a| a.action == ACCESS && a.service == g.service)
        })
        .map(|g| g.service.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A key of the file or of the database as a line, for the keys of the directory.
fn line_of_record(
    state: &AppState,
    key: &crate::users::KeyRecord,
    now: i64,
    can_edit: bool,
    owner: Option<Option<Vec<Grant>>>,
) -> KeyLine {
    let login = match &key.owner {
        KeyOwner::Superadmin => None,
        KeyOwner::User(l) | KeyOwner::File(l) => Some(l.as_str()),
    };
    let holder = owner.unwrap_or_else(|| owner_grants(state, login));
    let key_state = if key.is_from_file() {
        KeyState::Active
    } else {
        state_of(
            state,
            login,
            Some(key.kind),
            key.created_at,
            key.expires_at,
            now,
        )
    };
    let (last, count) = if key.is_from_file() {
        state
            .key_checks
            .file_key(&key.id)
            .map_or((None, 0), |(at, n)| (Some(at), n))
    } else {
        (None, 0)
    };
    let mut line = KeyLine {
        id: key.id.clone(),
        owner: login.map(str::to_string),
        mine: false,
        name: key.name.clone(),
        hint: key.hint.clone(),
        kind: Some(key.kind),
        state: key_state,
        file: key.is_from_file(),
        created_at: key.created_at,
        expires_at: key.expires_at,
        last_check_at: last,
        check_count: count,
        revoked_at: None,
        grants: grant_lines(&key.grants, holder.as_deref()),
        no_access: without_access(&key.grants),
        interaction: Some(key.interaction),
        can_change: false,
    };
    line.can_change = can_edit && !key.is_from_file() && key_state == KeyState::Active;
    line
}

/// A line of the list that the store gave: a key that exists, or the summary row of a revoked key.
fn line_of_item(state: &AppState, item: &KeyListItem, now: i64, can_edit: bool) -> KeyLine {
    if item.revoked_at.is_none() {
        if let Some(record) = state.users.key_by_id(&item.id) {
            // The directory has the key: it gives the state and the grants of this instance.
            let mut line = line_of_record(state, &record, now, can_edit, None);
            line.last_check_at = item.last_check_at;
            line.check_count = item.check_count;
            return line;
        }
    }
    let login = item.owner.as_deref();
    let holder = owner_grants(state, login);
    let key_state = if item.revoked_at.is_some() {
        KeyState::Revoked
    } else {
        state_of(
            state,
            login,
            item.kind,
            item.created_at,
            item.expires_at,
            now,
        )
    };
    KeyLine {
        id: item.id.clone(),
        owner: item.owner.clone(),
        mine: false,
        name: item.name.clone(),
        hint: item.hint.clone(),
        kind: item.kind,
        state: key_state,
        file: false,
        created_at: item.created_at,
        expires_at: item.expires_at,
        last_check_at: item.last_check_at,
        check_count: item.check_count,
        revoked_at: item.revoked_at,
        grants: grant_lines(&item.grants, holder.as_deref()),
        no_access: without_access(&item.grants),
        interaction: item.interaction,
        can_change: false,
    }
}

/// The query parameters, each one time at most. `None`: a parameter that is not known, or that
/// is repeated (HTTP 400).
fn query_parameters(uri: &str) -> Option<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    let Some((_, query)) = uri.split_once('?') else {
        return Some(out);
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = percent_decode_form(name).ok()?;
        let value = percent_decode_form(value).ok()?;
        if !matches!(name.as_str(), "copy" | "s" | "id" | "kind") || value.len() > 160 {
            return None;
        }
        if out.insert(name, value).is_some() {
            return None;
        }
    }
    Some(out)
}

fn frame_token(state: &AppState, viewer: &Viewer) -> Option<(String, String)> {
    let logout = sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token).ok()?;
    let keys = sign_action_token(&state.signing_secret, KEYS_TOKEN, &viewer.token).ok()?;
    Some((logout, keys))
}

fn bad_request() -> HttpResponse {
    text_response(StatusCode::BAD_REQUEST, "Bad Request\n")
}

fn not_found() -> HttpResponse {
    text_response(StatusCode::NOT_FOUND, "Not Found\n")
}

fn forbidden() -> HttpResponse {
    text_response(StatusCode::FORBIDDEN, "Forbidden\n")
}

/// Whether the viewer sees the list of the keys in the tab Access: the file has the block `keys`,
/// and the viewer may read the keys of the account or of all the accounts.
pub(super) fn list_visible(state: &AppState, viewer: &Viewer) -> bool {
    if state.config.keys.is_none() {
        return false;
    }
    let rights = rights_of(state, viewer);
    rights.read_own || rights.read_all
}

/// Whether a key of this owner is a key of the viewer.
fn is_mine(me: &Me, owner: Option<&str>) -> bool {
    owner == owner_login(me)
}

/// The keys of the tab Access: the keys of the viewer, or of all the accounts when the viewer may
/// read them. The error is the response. `None`: the list is not for this viewer.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
pub(super) async fn list(
    state: &Arc<AppState>,
    viewer: &Viewer,
) -> Result<Option<KeysList>, HttpResponse> {
    if !list_visible(state, viewer) {
        return Ok(None);
    }
    let rights = rights_of(state, viewer);
    let Some((_, csrf)) = frame_token(state, viewer) else {
        return Err(text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "authentication error\n",
        ));
    };
    let all = rights.read_all;
    let now = unix_now();
    let mine = me(state, viewer);
    // The keys of the file first, by login and then by name, then the keys of the database
    //.
    let mut file_keys: Vec<Arc<crate::users::KeyRecord>> = state
        .users
        .keys()
        .into_iter()
        .filter(|k| k.is_from_file())
        .filter(|k| {
            all || matches!(&k.owner, KeyOwner::File(login) if Some(login.as_str()) == owner_login(&mine))
        })
        .collect();
    file_keys.sort_by(|a, b| {
        let key = |k: &crate::users::KeyRecord| match &k.owner {
            KeyOwner::File(login) => (login.clone(), k.name.clone()),
            _ => (String::new(), k.name.clone()),
        };
        key(a).cmp(&key(b))
    });
    let mut lines: Vec<KeyLine> = file_keys
        .iter()
        .map(|key| line_of_record(state, key, now, false, None))
        .collect();
    // The keys of an account of the file are all in the file.
    let scope_owner = match (&mine, all) {
        (_, true) => Some(None),
        (Me::Superadmin, false) => Some(Some(None)),
        (Me::Database(login), false) => Some(Some(Some(login.clone()))),
        (Me::File(_), false) => None,
    };
    let mut truncated = false;
    if let Some(scope_owner) = scope_owner {
        let mut after: Option<(i64, String)> = None;
        loop {
            let (store, cursor, owner) =
                (Arc::clone(&state.store), after.clone(), scope_owner.clone());
            let listed = tokio::task::spawn_blocking(move || {
                let scope = match &owner {
                    None => KeyScope::All,
                    Some(None) => KeyScope::Superadmin,
                    Some(Some(login)) => KeyScope::Owner(login),
                };
                store.list_keys(
                    scope,
                    cursor.as_ref().map(|(at, id)| (*at, id.as_str())),
                    PAGE_SIZE,
                )
            })
            .await;
            let items = match listed {
                Ok(Ok(items)) => items,
                _ => {
                    return Err(text_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Service Unavailable\n",
                    ))
                }
            };
            let last = items.last().map(|item| (item.created_at, item.id.clone()));
            let full = items.len() == PAGE_SIZE;
            for item in &items {
                if lines.len() >= LIST_LIMIT {
                    truncated = true;
                    break;
                }
                lines.push(line_of_item(state, item, now, rights.edit));
            }
            if truncated || !full {
                break;
            }
            after = last;
        }
    }
    for line in &mut lines {
        line.mine = is_mine(&mine, line.owner.as_deref());
        line.can_change = line.can_change && line.mine && rights.edit;
    }
    let groups = state
        .config
        .inbox
        .as_ref()
        .map(|inbox| {
            inbox
                .groups
                .iter()
                .map(|(id, group)| (id.clone(), group.name.clone()))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(KeysList {
        lines,
        truncated,
        all,
        create_kinds: rights.create_kinds.clone(),
        can_revoke: rights.revoke,
        can_revoke_all: rights.revoke_all,
        impersonated: viewer.session.impersonated,
        cannot_use: rights.cannot_use,
        groups,
        csrf,
    }))
}

/// The states of the page that `handle_keys` draws.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Page {
    /// `GET /auth/keys/new`: the form of a new key.
    New,
    /// `GET /auth/keys/edit?id=<id>`: the form of the change of a key.
    Edit,
    /// The answer of the creation: the text of the key.
    Made,
}

/// `GET /auth/keys/new` and `GET /auth/keys/edit`. The page holds the form, with the
/// editor of the grants that the owner can give. The list of the keys is in the tab Access.
pub(super) async fn handle_keys(
    state: &Arc<AppState>,
    uri: &str,
    headers: &HeaderMap,
    client: &Client,
    page: Page,
) -> HttpResponse {
    if state.config.keys.is_none() {
        return not_found();
    }
    let Some(viewer) = current_viewer(state, headers, client) else {
        return redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH);
    };
    let Some(params) = query_parameters(uri) else {
        return bad_request();
    };
    draw(state, &viewer, params, page, None, None).await
}

/// A form that the server refused because a right has no `access` on its service. The page draws
/// the form again with the values that the person typed.
pub(super) struct Refused {
    pub service: String,
    pub name: String,
    pub expires_at: Option<String>,
    pub interaction: Option<crate::keys::Interaction>,
    pub agents: Option<crate::keys::AgentsMode>,
    pub grants: Vec<Grant>,
}

/// Whether the file has the block `inbox.agents`: only then the form of a key has the field `agents`.
fn agents_enabled(state: &AppState) -> bool {
    state
        .config
        .inbox
        .as_ref()
        .is_some_and(|inbox| inbox.agents.is_some())
}

/// The form that `raw` holds, when the check refuses it for `access`.
fn refused_for_access(
    raw: &key_forms::RawForm,
    ctx: &Context<'_>,
    kind: KeyKind,
) -> Option<Refused> {
    let (grants, service) =
        key_forms::grants_without_access(raw, ctx, key_forms::Holder::Key(kind))?;
    Some(Refused {
        service,
        name: raw.name.clone(),
        expires_at: raw.expires_at.clone(),
        interaction: raw
            .interaction
            .as_deref()
            .and_then(crate::keys::Interaction::from_name),
        agents: raw
            .agents
            .as_deref()
            .and_then(crate::keys::AgentsMode::from_name),
        grants,
    })
}

/// The text of a key that was just made: its name and its text, for the page of the creation.
pub(super) struct Made<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

/// Draws the page of a key for a viewer: the form of a new key, the form of the change of a key,
/// or the text of the key that was just made (`made`).
async fn draw(
    state: &Arc<AppState>,
    viewer: &Viewer,
    params: std::collections::HashMap<String, String>,
    page: Page,
    made: Option<Made<'_>>,
    refused: Option<Refused>,
) -> HttpResponse {
    let new = page == Page::New;
    let notice_code = params.get("s");
    let notice = match notice_code {
        None => None,
        Some(code) => match ui_keys::notice_for(code) {
            Some(notice) => Some(notice),
            None => return bad_request(),
        },
    };
    let copy = params.get("copy");
    if let Some(id) = copy {
        let valid = (1..=64).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !valid {
            return bad_request();
        }
    }
    let rights = rights_of(state, viewer);
    if !rights.read_own {
        return forbidden();
    }
    if new && !rights.create {
        return forbidden();
    }
    if page == Page::Edit && !rights.edit {
        return forbidden();
    }
    let Some((logout_token, csrf)) = frame_token(state, viewer) else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    };
    let now = unix_now();
    let mine = me(state, viewer);
    let my_grants: Option<Vec<Grant>> = match &mine {
        Me::Superadmin => None,
        Me::Database(login) | Me::File(login) => Some(viewer.grants.clone().unwrap_or_else(|| {
            keyauth::person_grants(state, login)
                .map(|(g, _)| g)
                .unwrap_or_default()
        })),
    };

    // The form of a key. The kind of a new key comes from the address (`kind`), or from the key that
    // the form copies, or it is the first kind that the viewer may make. A kind that the viewer may
    // not make is a bad request. The page of a change shows the kind of the key.
    let mut kind: Option<KeyKind> = None;
    if new {
        kind = match params.get("kind") {
            None => None,
            Some(name) => match KeyKind::from_name(name) {
                Some(asked) if rights.create_kinds.contains(&asked) => Some(asked),
                _ => return bad_request(),
            },
        };
    }
    let mut copy_name = String::new();
    let mut ticked: Vec<Grant> = Vec::new();
    // The interaction that the form shows: the key that the form copies or changes, or what the
    // person typed. Without these, the default of the kind.
    let mut interaction: Option<crate::keys::Interaction> = None;
    // The mode of the agents, in the same way. The form has the field only when the file has the block `inbox.agents`.
    let mut agents: Option<crate::keys::AgentsMode> = None;
    // The form that the server refused keeps what the person typed.
    let typed_expiry = refused.as_ref().and_then(|r| r.expires_at.clone());
    let mut edit: Option<EditTarget> = None;
    if let Some(id) = copy.filter(|_| new) {
        if let Some(source) = state.users.key_by_id(id) {
            let mine_key = match (&source.owner, &mine) {
                (KeyOwner::Superadmin, Me::Superadmin) => true,
                (KeyOwner::User(a), Me::Database(b)) => a == b,
                _ => false,
            };
            let same_kind = kind.is_none_or(|k| k == source.kind);
            if mine_key
                && !source.is_from_file()
                && same_kind
                && rights.create_kinds.contains(&source.kind)
            {
                // The form copies the grants and the limit values of the key.
                ticked = source.grants.clone();
                copy_name = source.name.clone();
                kind = Some(source.kind);
                interaction = Some(source.interaction);
                agents = Some(source.agents);
            }
        }
    }
    if page == Page::Edit {
        let Some(id) = params.get("id") else {
            return bad_request();
        };
        match editable_key(state, id, &mine, now) {
            Ok(key) => {
                ticked = key.grants.clone();
                kind = Some(key.kind);
                interaction = Some(key.interaction);
                agents = Some(key.agents);
                edit = Some(EditTarget {
                    id: key.id.clone(),
                    name: key.name.clone(),
                });
            }
            Err(code) => return refusal_redirect(code),
        }
    }
    if let Some(r) = &refused {
        ticked = r.grants.clone();
        interaction = r.interaction.or(interaction);
        agents = r.agents.or(agents);
        if let Some(target) = &mut edit {
            target.name = r.name.clone();
        } else {
            copy_name = r.name.clone();
        }
    }
    if new && kind.is_none() {
        // The API kind comes first, as in the selector.
        kind = rights
            .create_kinds
            .iter()
            .copied()
            .find(|k| *k == KeyKind::Api)
            .or_else(|| rights.create_kinds.first().copied());
    }
    let max_lifetime = kind.and_then(|kind| {
        my_grants
            .as_deref()
            .and_then(|grants| portal::max_key_lifetime(grants, kind))
    });
    let default_lifetime = state.config.key_limits.default_lifetime.as_secs();
    let first_lifetime = max_lifetime.map_or(default_lifetime, |max| max.min(default_lifetime));
    let editor = kind
        .filter(|_| made.is_none() && (new || edit.is_some()))
        .map(|kind| editor_for(state, kind, my_grants.as_deref(), ticked));
    let pane = match (&made, edit) {
        (Some(key), _) => Pane::Made(NewKey {
            text: key.text,
            name: key.name,
            superadmin: matches!(mine, Me::Superadmin),
        }),
        (None, Some(target)) => Pane::Edit(target),
        (None, None) => Pane::New,
    };
    let view = KeysView {
        create_kinds: rights.create_kinds.clone(),
        kind: kind.filter(|_| made.is_none()),
        field: notice_code.and_then(|code| ui_keys::field_of(code)),
        notice,
        csrf: &csrf,
        editor,
        pane,
        expires_at: typed_expiry
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| {
                ui_keys::instant_text(now + i64::try_from(first_lifetime).unwrap_or(i64::MAX))
            }),
        max_lifetime,
        copy_name,
        interaction: kind
            .map(|kind| interaction.unwrap_or(crate::keys::Interaction::default_for(kind))),
        agents: kind
            .filter(|_| agents_enabled(state))
            .map(|kind| agents.unwrap_or(crate::keys::AgentsMode::default_for(kind))),
        needs_access: refused.map(|r| r.service),
    };
    let frame = ui::Frame {
        logout_token: &logout_token,
        keys: list_visible(state, viewer),
        inbox: inbox_page::tab_visible(state, viewer),
        superadmin: is_superadmin(viewer),
        notify: inbox_page::notify_header(state, viewer).await,
        security: mfa_page::tab_visible(state, viewer),
        links: links_page::tab_visible(state, viewer),
        tab: state.tab_look(),
    };
    let status = if view.needs_access.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let mut response = html_response(status, ui_keys::keys_page(&frame, &view));
    if made.is_some() || view.needs_access.is_some() {
        response.headers_mut().insert(
            HeaderName::from_static("cache-control"),
            HeaderValue::from_static("no-store"),
        );
    }
    response
}

/// What the three forms have in common: the checks before the body, and the body with the token
///. The answer is the response when a check fails.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn form_request(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    allowed: impl Fn(&Rights) -> bool,
) -> Result<(Viewer, Rights, std::collections::HashMap<String, String>), HttpResponse> {
    if state.config.keys.is_none() {
        return Err(not_found());
    }
    if !validate_post_origin(request.headers()) {
        state.metrics.users_origin_failure();
        return Err(text_response(
            StatusCode::FORBIDDEN,
            "Origin validation failed\n",
        ));
    }
    if !content_type_is_form(request.headers()) {
        return Err(text_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type\n",
        ));
    }
    // The session first: the portal reads no body for a request that has no session.
    let headers = request.headers().clone();
    let Some(first) = current_viewer(state, &headers, client) else {
        return Err(redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH));
    };
    if !allowed(&rights_of(state, &first)) {
        return Err(forbidden());
    }
    // At most 4 such bodies at the same time.
    let Ok(_permit) = Arc::clone(&state.key_body_gate).try_acquire_owned() else {
        return Err(text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable\n",
        ));
    };
    let body = match read_limited_body(
        request.into_body(),
        MAX_FORM_BYTES,
        state.config.server.body_read_timeout,
    )
    .await
    {
        Ok(body) => Zeroizing::new(body),
        Err(BodyReadError::TooLarge) => {
            return Err(text_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large\n",
            ))
        }
        Err(BodyReadError::Timeout) => {
            return Err(text_response(
                StatusCode::REQUEST_TIMEOUT,
                "Request Timeout\n",
            ))
        }
        Err(BodyReadError::Io) => return Err(bad_request()),
    };
    // The session can end while the body arrives. The action needs the session, the rights and the grants of now.
    let Some(viewer) = still_current(state, &headers, client, &first) else {
        return Err(redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH));
    };
    let rights = rights_of(state, &viewer);
    if !allowed(&rights) {
        return Err(forbidden());
    }
    let Ok(mut form) = parse_form(&body) else {
        return Err(bad_request());
    };
    let csrf = form.remove("csrf").unwrap_or_default();
    if !verify_action_token(&state.signing_secret, KEYS_TOKEN, &viewer.token, &csrf) {
        return Err(text_response(
            StatusCode::FORBIDDEN,
            "CSRF validation failed\n",
        ));
    }
    Ok((viewer, rights, form))
}

/// A refused creation goes back to the page of the new key, where the owner can correct the form.
fn new_key_refusal(code: &str, kind: KeyKind) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}?kind={}&s={code}", ui_keys::NEW_PATH, kind.name()),
    )
}

fn refusal_redirect(code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}?s={code}", ui_keys::PATH),
    )
}

/// The context of the check of a form: the rights of the owner, the documents and the time.
fn with_context<R>(
    state: &AppState,
    owner: Option<&[Grant]>,
    old_grants: Option<&[Grant]>,
    kind: KeyKind,
    use_context: impl FnOnce(&Context<'_>) -> R,
) -> R {
    let catalog = current_catalog(state);
    // A key never holds a right on an external service: for a key, the service does not exist.
    let known: std::collections::HashSet<String> = catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .filter(|s| !s.external)
        .map(|s| s.id.clone())
        .collect();
    let lookup = |service: &str| state.actions.declared(service);
    let exists = |service: &str| known.contains(service);
    let external = |service: &str| catalog.service(service).is_some_and(|s| s.external);
    let context = Context {
        now: unix_now(),
        owner_grants: owner,
        max_lifetime: owner.and_then(|grants| portal::max_key_lifetime(grants, kind)),
        document: &lookup,
        service_exists: &exists,
        is_external: &external,
        old_grants,
    };
    use_context(&context)
}

fn caps(state: &AppState, now: i64, owner: Option<&[Grant]>) -> KeyCaps {
    let limits = &state.config.key_limits;
    KeyCaps {
        max_per_user: limits.max_per_user,
        max_counter_rows: limits.max_counter_rows,
        max_directory_grants: limits.max_directory_grants,
        now,
        // A key older than the longest lifetime of its owner is not active.
        created_not_before: owner
            .and_then(portal::max_key_lifetime_any)
            .map(|max| now - i64::try_from(max).unwrap_or(i64::MAX)),
    }
}

/// The code of a failed write. `None`: a failure of the database.
fn write_code(error: &KeyWriteError) -> Option<&'static str> {
    match error {
        KeyWriteError::TooManyKeys => Some("too_many_keys"),
        KeyWriteError::Capacity => Some("capacity"),
        KeyWriteError::UnknownKey => Some("unknown_key"),
        KeyWriteError::UnknownOwner | KeyWriteError::Db(_) => None,
    }
}

/// Reads the database again after a write, so this instance has the change at once. The other
/// instances take it at their next round.
fn refresh_after_write(state: &AppState) {
    blocking(|| crate::sync::directory_round(state));
}

/// `POST /auth/keys/create`.
pub(super) async fn handle_keys_create(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, rights, mut form) = match form_request(state, request, client, |r| r.create).await
    {
        Ok(v) => v,
        Err(response) => return response,
    };
    // The kind of the key is a field of the form. A kind that the viewer may not make, or that the
    // file turns off, is refused: the kind decides who verifies the key.
    let Some(kind) = form.remove("kind").as_deref().and_then(KeyKind::from_name) else {
        return bad_request();
    };
    if !rights.create_kinds.contains(&kind) {
        return forbidden();
    }
    let Ok(raw) = key_forms::read(form, false) else {
        return bad_request();
    };
    // The page sends the field `agents` only when the file has the block `inbox.agents`.
    if raw.agents.is_some() && !agents_enabled(state) {
        return bad_request();
    }
    let mine = me(state, &viewer);
    let owner = owner_grants(state, owner_login(&mine));
    let checked = with_context(state, owner.as_deref(), None, kind, |ctx| {
        key_forms::check(&raw, ctx, kind)
    });
    let form = match checked {
        Ok(form) => form,
        Err(key_forms::NEEDS_ACCESS) => {
            let refused = with_context(state, owner.as_deref(), None, kind, |ctx| {
                refused_for_access(&raw, ctx, kind)
            });
            if let Some(refused) = refused {
                let params = std::collections::HashMap::from([(
                    "kind".to_string(),
                    kind.name().to_string(),
                )]);
                return draw(state, &viewer, params, Page::New, None, Some(refused)).await;
            }
            return new_key_refusal(key_forms::NEEDS_ACCESS, kind);
        }
        Err(code) => return new_key_refusal(code, kind),
    };
    let now = unix_now();
    let (Ok(text), Ok(id)) = (crate::keys::generate(kind), crate::keys::new_id()) else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error\n");
    };
    let row = crate::store::KeyRow {
        id: id.clone(),
        owner: owner_login(&mine).map(str::to_string),
        name: form.name.clone(),
        digest: crate::keys::digest(&text),
        hint: crate::keys::hint(&text),
        created_at: now,
        expires_at: form.expires_at,
        last_check_at: None,
        check_count: 0,
        kind,
        interaction: form
            .interaction
            .unwrap_or(crate::keys::Interaction::default_for(kind)),
        agents: form
            .agents
            .unwrap_or(crate::keys::AgentsMode::default_for(kind)),
        grants: form.grants,
    };
    let caps = caps(state, now, owner.as_deref());
    let store = Arc::clone(&state.store);
    let written = tokio::task::spawn_blocking(move || store.insert_key(&row, &caps)).await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if let Some(code) = write_code(&error) {
                return new_key_refusal(code, kind);
            }
            log(
                state,
                LogLevel::Error,
                "key_insert_failed",
                &[("error", format!("{error:?}"))],
            );
            return text_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable\n");
        }
        Err(_) => return text_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable\n"),
    }
    refresh_after_write(state);
    log(
        state,
        LogLevel::Info,
        "key_created",
        &[
            ("key", id),
            ("owner", viewer.login.clone()),
            ("kind", kind.name().to_string()),
            (
                "expires_at",
                form.expires_at.map(|a| a.to_string()).unwrap_or_default(),
            ),
        ],
    );
    draw(
        state,
        &viewer,
        std::collections::HashMap::new(),
        Page::Made,
        Some(Made {
            name: &form.name,
            text: &text,
        }),
        None,
    )
    .await
}

/// The key of the user that the form names, when the user can change it: an active key of the
/// database that the user owns. The error is a refusal code of the key form.
fn editable_key(
    state: &AppState,
    id: &str,
    mine: &Me,
    now: i64,
) -> Result<Arc<crate::users::KeyRecord>, &'static str> {
    let Some(key) = state.users.key_by_id(id) else {
        return Err("unknown_key");
    };
    if key.is_from_file() {
        return Err("file_key");
    }
    let owned = match (&key.owner, mine) {
        (KeyOwner::Superadmin, Me::Superadmin) => true,
        (KeyOwner::User(a), Me::Database(b)) => a == b,
        _ => false,
    };
    if !owned {
        return Err("unknown_key");
    }
    let login = owner_login(mine);
    if state_of(
        state,
        login,
        Some(key.kind),
        key.created_at,
        key.expires_at,
        now,
    ) != KeyState::Active
    {
        return Err("unknown_key");
    }
    Ok(key)
}

/// `POST /auth/keys/edit`.
pub(super) async fn handle_keys_edit(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, _rights, mut form) = match form_request(state, request, client, |r| r.edit).await {
        Ok(v) => v,
        Err(response) => return response,
    };
    let Some(id) = form.remove("id") else {
        return bad_request();
    };
    let Ok(raw) = key_forms::read(form, true) else {
        return bad_request();
    };
    if raw.agents.is_some() && !agents_enabled(state) {
        return bad_request();
    }
    let mine = me(state, &viewer);
    let now = unix_now();
    let key = match editable_key(state, &id, &mine, now) {
        Ok(key) => key,
        Err(code) => return refusal_redirect(code),
    };
    let owner = owner_grants(state, owner_login(&mine));
    let old = key.grants.clone();
    let checked = with_context(state, owner.as_deref(), Some(&old), key.kind, |ctx| {
        key_forms::check(&raw, ctx, key.kind)
    });
    let form = match checked {
        Ok(form) => form,
        Err(key_forms::NEEDS_ACCESS) => {
            let refused = with_context(state, owner.as_deref(), Some(&old), key.kind, |ctx| {
                refused_for_access(&raw, ctx, key.kind)
            });
            if let Some(refused) = refused {
                let params = std::collections::HashMap::from([("id".to_string(), id.clone())]);
                return draw(state, &viewer, params, Page::Edit, None, Some(refused)).await;
            }
            return refusal_redirect(key_forms::NEEDS_ACCESS);
        }
        Err(code) => return refusal_redirect(code),
    };
    let caps = caps(state, now, owner.as_deref());
    let store = Arc::clone(&state.store);
    let login = owner_login(&mine).map(str::to_string);
    let written = {
        let (id, name, grants) = (id.clone(), form.name.clone(), form.grants.clone());
        let modes = crate::store::KeyModes {
            interaction: form.interaction.unwrap_or(key.interaction),
            agents: form.agents.unwrap_or(key.agents),
        };
        tokio::task::spawn_blocking(move || {
            store.update_key(&id, login.as_deref(), &name, modes, &grants, &caps)
        })
        .await
    };
    match written {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if let Some(code) = write_code(&error) {
                return refusal_redirect(code);
            }
            log(
                state,
                LogLevel::Error,
                "key_update_failed",
                &[("error", format!("{error:?}"))],
            );
            return text_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable\n");
        }
        Err(_) => return text_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable\n"),
    }
    refresh_after_write(state);
    log(
        state,
        LogLevel::Info,
        "key_edited",
        &[("key", id), ("owner", viewer.login.clone())],
    );
    refusal_redirect("edited")
}

/// `POST /auth/keys/revoke`.
pub(super) async fn handle_keys_revoke(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, rights, mut form) =
        match form_request(state, request, client, |r| r.revoke || r.revoke_all).await {
            Ok(v) => v,
            Err(response) => return response,
        };
    let Some(section) = form.remove("section") else {
        return bad_request();
    };
    // The form names each key `id.<id>` (value `1`), or one key `id`.
    let mut ids: Vec<String> = Vec::new();
    for (name, value) in &form {
        let id = match name.strip_prefix("id.") {
            Some(id) if value == "1" => id,
            None if name == "id" => value.as_str(),
            _ => return bad_request(),
        };
        let valid = (1..=64).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !valid {
            return bad_request();
        }
        ids.push(id.to_string());
    }
    if ids.is_empty() || ids.len() > 200 {
        return bad_request();
    }
    ids.sort();
    ids.dedup();
    let all = match section.as_str() {
        "mine" => false,
        "all" => true,
        _ => return bad_request(),
    };
    // The right of the section.
    if (all && !rights.revoke_all) || (!all && !rights.revoke) {
        return forbidden();
    }
    let mine = me(state, &viewer);
    let back = |code: &str| {
        redirect_response(
            StatusCode::SEE_OTHER,
            &format!("{}?s={code}", ui_keys::PATH),
        )
    };
    let mut code = "revoked";
    for id in ids {
        if state.users.key_by_id(&id).is_some_and(|k| k.is_from_file()) {
            code = "file_key";
            continue;
        }
        // The owner filter: in the section "Mine" the key must be the key of the user.
        let owner_filter: Option<Option<&str>> = if all { None } else { Some(owner_login(&mine)) };
        let now = unix_now();
        let store = Arc::clone(&state.store);
        let owned_filter: Option<Option<String>> = owner_filter.map(|o| o.map(str::to_string));
        let revoked = {
            let id = id.clone();
            tokio::task::spawn_blocking(move || {
                let filter = owned_filter.as_ref().map(|o| o.as_deref());
                match store.revoke_key(&id, filter, now) {
                    Ok(true) => Ok(true),
                    // A key that is revoked already, with its summary row, gives the same answer.
                    Ok(false) => store.has_key_history(&id, filter),
                    Err(error) => Err(error),
                }
            })
            .await
        };
        match revoked {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                code = "unknown_key";
                continue;
            }
            _ => return text_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable\n"),
        }
        refresh_after_write(state);
        // The approvals that wait for this key can never be used: the key is gone.
        state.inbox.cancel_key_items(&id).await;
        log(
            state,
            LogLevel::Info,
            "key_revoked",
            &[
                ("key", id),
                ("by", viewer.login.clone()),
                ("section", section.clone()),
            ],
        );
    }
    back(code)
}
