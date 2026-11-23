//! The quiet hours and the digest on the phone side, with the real engine and the real frame builder, on SQLite and on
//! PostgreSQL. The clock is the one of the inbox: each test moves it by hand. The pages and the frames of a real
//! phone are in `tests/prefs_e2e.rs`.

use super::*;
use crate::inbox::testing::DigestMemo;
use inbox_core::proto::{DigestMode, DigestPrefs, Prefs, QuietPrefs, ServerMsg};

/// Unix time in milliseconds of an RFC 3339 text.
fn at(text: &str) -> i64 {
    text.parse::<jiff::Timestamp>().unwrap().as_millisecond()
}

/// A request that arrives inside the quiet hours is delivered at once and flagged `quiet`. One that arrives outside the
/// quiet hours has no flag. The window is 00:00 to 00:30 in Paris, which is 23:00 to 23:30 UTC in winter.
#[test]
fn quiet_flag_set_in_window_request_still_delivered() {
    each_rig(true, "", |r, name| {
        r.device();
        let key = r.key_a();
        let prefs = Prefs {
            quiet: Some(QuietPrefs {
                from: "00:00".into(),
                to: "00:30".into(),
                tz: "Europe/Paris".into(),
                high_rings: false,
            }),
            digest: None,
        };
        eventually(|| runtime().block_on(r.items().set_prefs(OWNER, &prefs)));
        r.fix_clock(at("2027-01-15T23:10:00Z"));
        let inside = r.submit(&key, 1).id();
        r.fix_clock(at("2027-01-15T23:35:00Z"));
        let outside = r.submit(&key, 2).id();
        let frames = frames_of(r, DEVICE);
        let flag = |id: &str| {
            frames
                .iter()
                .find_map(|f| match f {
                    ServerMsg::Request(q) if q.request_id == id => Some(q.quiet),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("[{name}] the request {id} is not delivered"))
        };
        assert!(flag(&inside), "[{name}] inside the window");
        assert!(!flag(&outside), "[{name}] outside the window");
        // The server never holds an item back: both items are open, both are in the queue of the phone.
        assert_eq!(
            r.count("SELECT COUNT(*) FROM items WHERE state = 'pending'"),
            2,
            "[{name}]"
        );
    });
}

/// The digest frame has numbers and ids, never a body. With no open item, the session does not send the row.
#[test]
fn digest_row_becomes_a_frame_with_numbers_only() {
    each_rig(true, "", |r, name| {
        r.device();
        let key = r.key_a();
        let start = at("2027-01-15T08:00:00Z");
        r.fix_clock(start);
        let first = r.submit_with(&key, 1, json!({"lease_s": 86_400})).id();
        r.fix_clock(start + 60_000);
        let second = r.submit_with(&key, 2, json!({"lease_s": 86_400})).id();
        let prefs = Prefs {
            quiet: None,
            digest: Some(DigestPrefs {
                mode: DigestMode::Every,
                at: None,
                tz: None,
                every_h: Some(1),
            }),
        };
        eventually(|| runtime().block_on(r.items().set_prefs(OWNER, &prefs)));
        r.fix_clock(at("2027-01-15T09:01:00Z"));
        let file = crate::config::DigestConfig::Off;
        let mut memo = DigestMemo::default();
        let written = eventually(|| runtime().block_on(r.items().digest_pass(&file, &mut memo)));
        assert_eq!(written, 1, "[{name}]");
        let row =
            r.count("SELECT COUNT(*) FROM alerts WHERE kind = 'digest' AND request_id IS NULL");
        assert_eq!(row, 1, "[{name}]");
        let id = "d".repeat(32);
        let bytes = runtime()
            .block_on(crate::inbox::testing::digest_for_test(&r.app, OWNER, &id))
            .expect("the digest is built");
        let text = String::from_utf8(bytes.clone()).unwrap();
        for canary in CANARIES {
            assert!(!text.contains(canary), "[{name}] {canary} in {text}");
        }
        assert!(
            !text.contains("INSERT INTO analytics.events"),
            "[{name}] no command: {text}"
        );
        match ServerMsg::decode(&bytes).unwrap() {
            ServerMsg::Digest(d) => {
                assert_eq!(d.count, 2, "[{name}]");
                assert_eq!(d.oldest_created_at_ms, start, "[{name}]");
                assert_eq!(d.ids, vec![first.clone(), second.clone()], "[{name}]");
            }
            other => panic!("[{name}] {other:?}"),
        }
        // Nothing is open any more: the session withdraws the row instead of sending an empty digest.
        eventually(|| {
            runtime().block_on(r.items().cancel_where(None, Some(OWNER), "owner_deleted"))
        });
        assert!(
            runtime()
                .block_on(crate::inbox::testing::digest_for_test(&r.app, OWNER, &id))
                .is_none(),
            "[{name}]"
        );
    });
}
