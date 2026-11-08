//! JNI bridge for `app.portalinbox.core.Native`.
//!
//! Every entry point catches panics (the Android build uses `panic = "unwind"`)
//! and turns them, like misuse, into a Java `IllegalStateException`. Expected
//! protocol failures (bad code, wrong server key) are returned as JSON instead.

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

/// Reads an enrollment code. Returns `{"code": {...}}` or `{"error": {...}}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_portalinbox_core_Native_enrollParse<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    code: JString<'l>,
) -> jstring {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let code = Zeroizing::new(string(env, &code)?);
        new_string(env, &bridge::enroll_parse(&code).to_string())
    })
}

/// Returns `{"handle": <long>, "send": "<message 1, standard base64>"}` or `{"error": {...}}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_portalinbox_core_Native_enrollStart<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    code: JString<'l>,
    device_name: JString<'l>,
    app_version: JString<'l>,
) -> jstring {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let code = Zeroizing::new(string(env, &code)?);
        let name = string(env, &device_name)?;
        let version = string(env, &app_version)?;
        let out = match bridge::enroll_start(&code, &name, &version) {
            Ok((handle, msg1)) => {
                let ptr = Box::into_raw(handle) as jlong;
                json!({ "handle": ptr, "send": inbox_core::b64::encode(&msg1) })
            }
            Err(e) => bridge::error_json(&e),
        };
        new_string(env, &out.to_string())
    })
}

/// Consumes Noise message 2. Returns `{"done": {...}}` or `{"error": {...}}`.
/// The result holds the device private key: Kotlin must seal it at once.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_portalinbox_core_Native_enrollFinish<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    msg2: JByteArray<'l>,
) -> jstring {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let h = enroll_ref(handle)?;
        let msg2 = bytes(env, &msg2)?;
        let out = Zeroizing::new(bridge::enroll_finish(h, &msg2).to_string());
        new_string(env, &out)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_portalinbox_core_Native_enrollFree<'l>(
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
pub extern "system" fn Java_app_portalinbox_core_Native_sessionStart<'l>(
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
pub extern "system" fn Java_app_portalinbox_core_Native_sessionFinish<'l>(
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
pub extern "system" fn Java_app_portalinbox_core_Native_sessionEncrypt<'l>(
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
pub extern "system" fn Java_app_portalinbox_core_Native_sessionDecrypt<'l>(
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
pub extern "system" fn Java_app_portalinbox_core_Native_sessionFree<'l>(
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

/// The effect hash of an operation. Returns `{"effect": "sha256:..."}` or `{"error": {...}}`.
/// `op_json` is the JSON text of `op`: the parser refuses a float and a duplicate member name.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_portalinbox_core_Native_effectCompute<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    service: JString<'l>,
    action: JString<'l>,
    resource: JString<'l>,
    op_json: JString<'l>,
) -> jstring {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let service = string(env, &service)?;
        let action = string(env, &action)?;
        let resource = string(env, &resource)?;
        let op = string(env, &op_json)?;
        new_string(env, &bridge::effect_compute(&service, &action, &resource, &op).to_string())
    })
}

/// The view of an approval with the values of `op`. Returns `{"view": {...}}` or `{"error": {...}}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_portalinbox_core_Native_viewResolve<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    op_json: JString<'l>,
    view_json: JString<'l>,
) -> jstring {
    guard(&mut env, std::ptr::null_mut(), |env| {
        let op = string(env, &op_json)?;
        let view = string(env, &view_json)?;
        new_string(env, &bridge::view_resolve(&op, &view).to_string())
    })
}

/// Library and protocol versions, for the about screen.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_portalinbox_core_Native_protocolVersion<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jni::sys::jint {
    inbox_core::PROTOCOL_VERSION as jni::sys::jint
}

#[cfg(test)]
mod tests {
    /// The JNI names come from the package of the Kotlin class. A name that does not match makes
    /// the app crash when it loads the library. This test reads `Native.kt` and checks each
    /// `external fun` against the exported symbols of this file.
    #[test]
    fn each_native_function_of_the_app_has_its_jni_symbol() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let kotlin = std::fs::read_to_string(
            root.join("../../android/app/src/main/java/app/portalinbox/core/Native.kt"),
        )
        .expect("Native.kt");
        assert!(kotlin.contains("package app.portalinbox.core\n"));
        let source = std::fs::read_to_string(root.join("src/lib.rs")).expect("lib.rs");
        let names: Vec<&str> = kotlin
            .lines()
            .filter_map(|l| l.split("external fun ").nth(1))
            .filter_map(|l| l.split('(').next())
            .collect();
        assert_eq!(names.len(), 12, "{names:?}");
        for name in names {
            let symbol = format!("fn Java_app_portalinbox_core_Native_{name}<");
            assert!(source.contains(&symbol), "{symbol}");
        }
        let exported = source
            .lines()
            .filter(|l| l.starts_with("pub extern \"system\" fn Java_app_portalinbox_core_Native_"))
            .count();
        assert_eq!(exported, 12, "each exported symbol has the package of the app");
    }
}
