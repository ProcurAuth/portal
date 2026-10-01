//! WebAssembly build of the admin login (OPAQUE) and request signing.
//!
//! Raw C ABI, no wasm-bindgen. Byte buffers cross the boundary through
//! [`pm_alloc`] / [`pm_free`]; every function that produces bytes stores them in a
//! single output buffer read with [`pm_out_ptr`] / [`pm_out_len`] right after the
//! call. Return values: `>= 0` on success, negative error codes otherwise.
//!
//! The host must call [`pm_seed`] with 32 bytes from `crypto.getRandomValues`
//! before starting a login.

use std::collections::BTreeMap;
use std::sync::Mutex;

use inbox_core::login::{LoginClient, mac};
use inbox_core::rng::Rng;
use inbox_core::suite::Role;
use zeroize::Zeroize;

/// Not seeded yet.
pub const ERR_NO_SEED: i32 = -1;
/// Malformed input.
pub const ERR_INPUT: i32 = -2;
/// Wrong password or server without the credential record.
pub const ERR_AUTH: i32 = -3;
/// Unknown or already used login handle.
pub const ERR_HANDLE: i32 = -4;

struct State {
    rng: Option<Rng>,
    logins: BTreeMap<i32, LoginClient>,
    next_handle: i32,
    out: Vec<u8>,
}

static STATE: Mutex<State> =
    Mutex::new(State { rng: None, logins: BTreeMap::new(), next_handle: 1, out: Vec::new() });

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// # Safety
/// `ptr` must point to `len` readable bytes (or `len` must be 0).
unsafe fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len) } }
}

fn set_out(st: &mut State, bytes: Vec<u8>) {
    st.out.zeroize();
    st.out = bytes;
}

/// Allocates `len` bytes for the host to write into.
#[unsafe(no_mangle)]
pub extern "C" fn pm_alloc(len: usize) -> *mut u8 {
    let mut v = vec![0u8; len.max(1)];
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Frees a buffer from [`pm_alloc`], wiping it first.
///
/// # Safety
/// `ptr`/`len` must come from one `pm_alloc(len)` call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let mut v = unsafe { Vec::from_raw_parts(ptr, len.max(1), len.max(1)) };
    v.zeroize();
}

#[unsafe(no_mangle)]
pub extern "C" fn pm_out_ptr() -> *const u8 {
    state().out.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn pm_out_len() -> usize {
    state().out.len()
}

/// Wipes the output buffer once the host has copied it.
#[unsafe(no_mangle)]
pub extern "C" fn pm_out_clear() {
    let mut st = state();
    set_out(&mut st, Vec::new());
}

/// (Re)seeds the generator with 32 bytes at `ptr`.
///
/// # Safety
/// `ptr` must point to 32 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_seed(ptr: *const u8) -> i32 {
    let mut seed = [0u8; 32];
    seed.copy_from_slice(unsafe { slice(ptr, 32) });
    state().rng = Some(Rng::from_seed(seed));
    seed.zeroize();
    0
}

/// Starts an admin login. Returns a handle; output = serialized CredentialRequest.
///
/// # Safety
/// `pw` must point to `pw_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_login_start(pw: *const u8, pw_len: usize) -> i32 {
    let password = unsafe { slice(pw, pw_len) };
    let mut st = state();
    let Some(rng) = st.rng.as_mut() else { return ERR_NO_SEED };
    let Ok((client, request)) = LoginClient::start(Role::Admin, password, rng) else {
        return ERR_INPUT;
    };
    let handle = st.next_handle;
    st.next_handle = st.next_handle.wrapping_add(1).max(1);
    // At most a few concurrent logins: drop stale ones.
    if st.logins.len() >= 8 {
        st.logins.clear();
    }
    st.logins.insert(handle, client);
    set_out(&mut st, request);
    handle
}

/// Finishes a login with the server CredentialResponse.
/// Output = CredentialFinalization || request key (last 32 bytes).
///
/// # Safety
/// `resp` must point to `resp_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_login_finish(handle: i32, resp: *const u8, resp_len: usize) -> i32 {
    let response = unsafe { slice(resp, resp_len) };
    let mut st = state();
    let Some(client) = st.logins.remove(&handle) else { return ERR_HANDLE };
    let Some(rng) = st.rng.as_mut() else { return ERR_NO_SEED };
    let (mut finalization, key) = match client.finish(response, rng) {
        Ok(r) => r,
        Err(inbox_core::Error::AuthenticationFailed) => return ERR_AUTH,
        Err(_) => return ERR_INPUT,
    };
    let request_key = key.derive("admin/request-mac");
    finalization.extend_from_slice(request_key.as_ref());
    set_out(&mut st, finalization);
    0
}

/// Signs an admin request. Output = 32-byte HMAC tag.
///
/// # Safety
/// Every pointer must point to its length of readable bytes; `key` to 32 bytes.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn pm_sign(
    key: *const u8,
    method: *const u8,
    method_len: usize,
    path: *const u8,
    path_len: usize,
    ts: *const u8,
    ts_len: usize,
    nonce: *const u8,
    nonce_len: usize,
    body: *const u8,
    body_len: usize,
) -> i32 {
    let mut k = [0u8; 32];
    k.copy_from_slice(unsafe { slice(key, 32) });
    let parts: [&[u8]; 6] = unsafe {
        [
            b"inbox/v1/admin-request",
            slice(method, method_len),
            slice(path, path_len),
            slice(ts, ts_len),
            slice(nonce, nonce_len),
            slice(body, body_len),
        ]
    };
    let tag = mac(&k, &parts);
    k.zeroize();
    set_out(&mut state(), tag.to_vec());
    0
}
