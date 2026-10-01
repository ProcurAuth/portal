//! JNI bridge for `app.inbox.core.Native`.
//!
//! Every entry point catches panics (the Android build uses `panic = "unwind"`)
//! and turns them, like misuse, into a Java `IllegalStateException`. Expected
//! protocol failures (wrong password, rejection) are returned as JSON instead.

mod bridge;

use std::panic::{AssertUnwindSafe, catch_unwind};

use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jbyteArray, jlong, jstring};
use serde_json::json;
use zeroize::Zeroizing;

use bridge::{Conn, Enroll};

const EXCEPTION: &str = "java/lang/IllegalStateException";

fn guard<'l, T, F>(env: &mut JNIEnv<'l>, fallback: T, f: F) -> T
where
    F: FnOnce(&mut JNIEnv<'l>) -> Result<T, String>,
{
    match catch_unwind(AssertUnwindSafe(|| f(env))) {
        Ok(Ok(v)) => v,
        Ok(Err(msg)) => {
            let _ = env.throw_new(EXCEPTION, msg);
            fallback
        }
        Err(_) => {
            let _ = env.throw_new(EXCEPTION, "inbox native panic");
            fallback
        }
    }
}

fn bytes(env: &JNIEnv<'_>, arr: &JByteArray<'_>) -> Result<Zeroizing<Vec<u8>>, String> {
    env.convert_byte_array(arr).map(Zeroizing::new).map_err(|e| e.to_string())
}

fn string(env: &mut JNIEnv<'_>, s: &JString<'_>) -> Result<String, String> {
    env.get_string(s).map(Into::into).map_err(|e| e.to_string())
}

fn new_string(env: &mut JNIEnv<'_>, s: &str) -> Result<jstring, String> {
    env.new_string(s).map(|j| j.into_raw()).map_err(|e| e.to_string())
}

fn new_bytes(env: &mut JNIEnv<'_>, b: &[u8]) -> Result<jbyteArray, String> {
    env.byte_array_from_slice(b).map(|j| j.into_raw()).map_err(|e| e.to_string())
}

/// # Safety contract
/// Handles come from `Box::into_raw` in this crate and Kotlin frees each exactly once.
fn enroll_ref<'a>(h: jlong) -> Result<&'a Enroll, String> {
    if h == 0 {
        return Err("null enrollment handle".into());
    }
    // SAFETY: see contract above.
    Ok(unsafe { &*(h as *const Enroll) })
}

fn conn_ref<'a>(h: jlong) -> Result<&'a Conn, String> {
    if h == 0 {
        return Err("null session handle".into());
    }
    // SAFETY: handles come from Box::into_raw below and are freed once by Kotlin.
    Ok(unsafe { &*(h as *const Conn) })
}

// ---------------------------------------------------------------- enrollment

/// Returns `{"handle": <long>, "send": "<first text frame>"}` or `{"error": {...}}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_enrollStart<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    password: JByteArray<'l>,
    device_name: JString<'l>,
    user_name: JString<'l>,
) -> jstring {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let password = bytes(env, &password)?;
        let name = string(env, &device_name)?;
        let user = string(env, &user_name)?;
        let out = match bridge::enroll_start(&password, &name, &user) {
            Ok((handle, msg)) => {
                let ptr = Box::into_raw(handle) as jlong;
                json!({ "handle": ptr, "send": msg })
            }
            Err(e) => bridge::error_json(&e),
        };
        new_string(env, &out.to_string())
    })
}

/// Returns `{"send": ...}`, `{"done": {...}}` or `{"error": {...}}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_enrollOnMessage<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    text: JString<'l>,
) -> jstring {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let h = enroll_ref(handle)?;
        let text = string(env, &text)?;
        let out = bridge::enroll_on_message(h, &text);
        new_string(env, &out.to_string())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_enrollFree<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
) {
    guard(&mut env, (), |_| {
        if handle != 0 {
            // SAFETY: handle created by enrollStart, freed exactly once.
            drop(unsafe { Box::from_raw(handle as *mut Enroll) });
        }
        Ok(())
    })
}

// ---------------------------------------------------------------- sessions

/// Starts the Noise handshake. Returns the handle; message 1 via `sessionMessage1`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_sessionStart<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    device_private: JByteArray<'l>,
    server_public: JByteArray<'l>,
    hello: JByteArray<'l>,
) -> jbyteArray {
    // Returns [8-byte big-endian handle || message 1] to keep one round trip.
    guard(&mut env, std::ptr::null_mut(), |env| {
        let sk = bytes(env, &device_private)?;
        let pk = bytes(env, &server_public)?;
        let hello = bytes(env, &hello)?;
        let (conn, msg1) = bridge::session_start(&sk, &pk, &hello).map_err(|e| e.to_string())?;
        let ptr = Box::into_raw(conn) as jlong;
        let mut out = Vec::with_capacity(8 + msg1.len());
        out.extend_from_slice(&ptr.to_be_bytes());
        out.extend_from_slice(&msg1);
        new_bytes(env, &out)
    })
}

/// Consumes handshake message 2, returns the accept payload (JSON bytes).
/// Throws if the server is not the pinned one.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_sessionFinish<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    msg2: JByteArray<'l>,
) -> jbyteArray {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let c = conn_ref(handle)?;
        let msg2 = bytes(env, &msg2)?;
        let payload = bridge::session_finish(c, &msg2).map_err(|e| e.to_string())?;
        new_bytes(env, &payload)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_sessionEncrypt<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    plaintext: JByteArray<'l>,
) -> jbyteArray {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let c = conn_ref(handle)?;
        let pt = bytes(env, &plaintext)?;
        let ct = bridge::session_encrypt(c, &pt).map_err(|e| e.to_string())?;
        new_bytes(env, &ct)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_sessionDecrypt<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    frame: JByteArray<'l>,
) -> jbyteArray {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let c = conn_ref(handle)?;
        let ct = env.convert_byte_array(&frame).map_err(|e| e.to_string())?;
        let pt = bridge::session_decrypt(c, &ct).map_err(|e| e.to_string())?;
        new_bytes(env, &pt)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_sessionFree<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
) {
    guard(&mut env, (), |_| {
        if handle != 0 {
            // SAFETY: handle created by sessionStart, freed exactly once.
            drop(unsafe { Box::from_raw(handle as *mut Conn) });
        }
        Ok(())
    })
}

/// Library and protocol versions, for the about screen.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_inbox_core_Native_protocolVersion<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jni::sys::jint {
    inbox_core::PROTOCOL_VERSION as jni::sys::jint
}
