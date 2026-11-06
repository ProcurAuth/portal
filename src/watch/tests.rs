//! Tests of the detectors and the engine. A fake clock gives the time.

use super::*;

const S: fn(u64) -> Duration = Duration::from_secs;

fn svc(id: &str, seen: Seen) -> Observed {
    Observed {
        id: id.into(),
        name: format!("Name of {id}"),
        seen,
    }
}

fn config() -> WatchConfig {
    WatchConfig {
        group: "infra".into(),
        service_unhealthy: true,
        unhealthy_after: S(120),
        sign_in_burst: true,
        sign_in_failures: 10,
        sign_in_window: S(300),
        repeat_after: S(3600),
        recovered: true,
    }
}

fn health() -> HealthDetector {
    HealthDetector::new(S(120), S(3600), true)
}

fn kinds(due: &[Due]) -> Vec<(Kind, &str)> {
    due.iter().map(|d| (d.kind, d.subject.as_str())).collect()
}

#[test]
fn the_tile_rules_give_the_state_of_a_service() {
    let mut service = Service {
        id: "a".into(),
        group: "Other".into(),
        name: "A".into(),
        description: String::new(),
        icon: None,
        href: "/".into(),
        new_tab: false,
        weight: 0,
        health_url: None,
        actions_url: None,
        exclude: Vec::new(),
        health: Health::Unknown,
        runtime: None,
    };
    assert_eq!(seen(&service), Seen::Unknown);
    service.health = Health::Up;
    assert_eq!(seen(&service), Seen::Healthy);
    service.health = Health::Down;
    assert_eq!(seen(&service), Seen::Unhealthy);
    service.health = Health::Up;
    service.runtime = Some(RuntimeHealth::Unhealthy);
    assert_eq!(seen(&service), Seen::Unhealthy);
    service.runtime = Some(RuntimeHealth::Starting);
    assert_eq!(seen(&service), Seen::Starting);
    service.health = Health::Unknown;
    service.runtime = Some(RuntimeHealth::Healthy);
    assert_eq!(seen(&service), Seen::Healthy);
    service.health = Health::Down;
    assert_eq!(seen(&service), Seen::Unhealthy, "down wins, as on the tile");
}

#[test]
fn a_service_alerts_after_the_threshold_and_not_before() {
    let mut d = health();
    let down = [svc("notes", Seen::Unhealthy)];
    assert!(d.observe(S(0), &down).is_empty());
    assert!(d.observe(S(60), &down).is_empty());
    assert!(d.observe(S(119), &down).is_empty());
    let due = d.observe(S(120), &down);
    assert_eq!(kinds(&due), [(Kind::ServiceUnhealthy, "notes")]);
    // Due again until the inbox accepts it.
    assert_eq!(d.observe(S(121), &down).len(), 1);
    d.confirm(&due[0], S(121));
    assert!(d.observe(S(122), &down).is_empty());
}

#[test]
fn a_healthy_or_starting_service_breaks_the_unhealthy_time() {
    for gap in [Seen::Healthy, Seen::Starting, Seen::Unknown] {
        let mut d = health();
        let down = [svc("notes", Seen::Unhealthy)];
        d.observe(S(0), &down);
        d.observe(S(100), &[svc("notes", gap)]);
        assert!(d.observe(S(130), &down).is_empty(), "the count restarts");
        assert!(d.observe(S(249), &down).is_empty());
        assert_eq!(d.observe(S(250), &down).len(), 1, "{gap:?}");
    }
}

#[test]
fn a_still_unhealthy_service_alerts_again_after_repeat_after_only() {
    let mut d = health();
    let down = [svc("notes", Seen::Unhealthy)];
    d.observe(S(0), &down);
    let first = d.observe(S(120), &down);
    d.confirm(&first[0], S(120));
    assert!(d.observe(S(3000), &down).is_empty());
    assert!(d.observe(S(3719), &down).is_empty());
    let again = d.observe(S(3720), &down);
    assert_eq!(kinds(&again), [(Kind::ServiceUnhealthy, "notes")]);
}

#[test]
fn the_recovery_is_sent_once_and_only_after_an_alert() {
    let mut d = health();
    let down = [svc("notes", Seen::Unhealthy)];
    let up = [svc("notes", Seen::Healthy)];
    // Healthy without an alert before: nothing.
    assert!(d.observe(S(0), &up).is_empty());
    d.observe(S(10), &down);
    // Short outage, no alert, no recovery.
    assert!(d.observe(S(20), &up).is_empty());
    d.observe(S(30), &down);
    let alert = d.observe(S(150), &down);
    d.confirm(&alert[0], S(150));
    // Starting after the alert is not a recovery.
    assert!(d
        .observe(S(160), &[svc("notes", Seen::Starting)])
        .is_empty());
    let back = d.observe(S(170), &up);
    assert_eq!(kinds(&back), [(Kind::ServiceRecovered, "notes")]);
    assert_eq!(d.observe(S(171), &up).len(), 1, "due until accepted");
    d.confirm(&back[0], S(171));
    assert!(d.observe(S(172), &up).is_empty());
    assert!(d.observe(S(900), &up).is_empty());
}

#[test]
fn recovery_can_be_off() {
    let mut d = HealthDetector::new(S(120), S(3600), false);
    let down = [svc("notes", Seen::Unhealthy)];
    d.observe(S(0), &down);
    let alert = d.observe(S(120), &down);
    d.confirm(&alert[0], S(120));
    assert!(d.observe(S(130), &[svc("notes", Seen::Healthy)]).is_empty());
    // The next outage after repeat_after alerts again, with no stale state.
    d.observe(S(140), &down);
    assert!(d.observe(S(3000), &down).is_empty());
    assert_eq!(d.observe(S(3720), &down).len(), 1);
}

#[test]
fn a_flapping_service_makes_one_alert_and_one_recovery_for_each_repeat_time() {
    let mut d = health();
    let (down, up) = (
        [svc("notes", Seen::Unhealthy)],
        [svc("notes", Seen::Healthy)],
    );
    let mut sent = Vec::new();
    // It is down for 3 minutes and up for 1 minute, for 50 minutes.
    let mut t = 0;
    while t < 3000 {
        let state = if (t / 60) % 4 == 3 { &up } else { &down };
        for due in d.observe(S(t), state) {
            d.confirm(&due, S(t));
            sent.push(due.kind);
        }
        t += 5;
    }
    // Alert at 120 s, recovery at 180 s. The later outages are inside repeat_after.
    assert_eq!(sent, [Kind::ServiceUnhealthy, Kind::ServiceRecovered]);
}

#[test]
fn services_are_independent_and_a_removed_service_is_forgotten() {
    let mut d = health();
    let both = |a, b| [svc("a", a), svc("b", b)];
    d.observe(S(0), &both(Seen::Unhealthy, Seen::Healthy));
    d.observe(S(60), &both(Seen::Unhealthy, Seen::Unhealthy));
    let due = d.observe(S(120), &both(Seen::Unhealthy, Seen::Unhealthy));
    assert_eq!(kinds(&due), [(Kind::ServiceUnhealthy, "a")]);
    d.confirm(&due[0], S(120));
    // The service b is removed, then comes back: its count restarts.
    d.observe(S(121), &[svc("a", Seen::Unhealthy)]);
    d.observe(S(122), &both(Seen::Unhealthy, Seen::Unhealthy));
    assert!(d
        .observe(S(200), &both(Seen::Unhealthy, Seen::Unhealthy))
        .is_empty());
    let due = d.observe(S(242), &both(Seen::Unhealthy, Seen::Unhealthy));
    assert_eq!(kinds(&due), [(Kind::ServiceUnhealthy, "b")]);
    d.confirm(&due[0], S(242));
    // Removing a service that was alerted gives no recovery.
    d.observe(S(300), &[svc("b", Seen::Unhealthy)]);
    assert!(d
        .observe(S(301), &both(Seen::Healthy, Seen::Unhealthy))
        .is_empty());
}

#[test]
fn a_clock_that_goes_back_gives_no_alert_and_no_panic() {
    let mut d = health();
    let down = [svc("notes", Seen::Unhealthy)];
    d.observe(S(1000), &down);
    let alert = d.observe(S(1120), &down);
    d.confirm(&alert[0], S(1120));
    // The clock goes back by an hour.
    assert!(d.observe(S(100), &down).is_empty());
    assert!(d.observe(S(219), &down).is_empty());
    // The alert time went back with the clock: a repeat needs a full repeat_after.
    assert!(d.observe(S(3000), &down).is_empty());
    assert_eq!(d.observe(S(3700), &down).len(), 1);
    // The unhealthy time also restarts.
    let mut d = health();
    d.observe(S(1000), &down);
    assert!(d.observe(S(50), &down).is_empty());
    assert!(d.observe(S(169), &down).is_empty());
    assert_eq!(d.observe(S(170), &down).len(), 1);
}

fn burst() -> BurstDetector {
    BurstDetector::new(10, S(300), S(3600))
}

#[test]
fn a_burst_needs_the_number_of_refusals_in_the_window() {
    let mut b = burst();
    assert!(!b.observe(S(0), 4));
    assert!(!b.observe(S(1), 5));
    assert!(b.observe(S(2), 1));
    assert!(b.observe(S(3), 0), "due until the inbox accepts it");
    b.confirm(S(3));
    assert!(!b.observe(S(4), 0));
}

#[test]
fn the_window_slides() {
    let mut b = burst();
    // Nine refusals at 0, one more at 299: inside the window.
    assert!(!b.observe(S(0), 9));
    assert!(b.observe(S(299), 1));
    let mut b = burst();
    assert!(!b.observe(S(0), 9));
    // At 300 the first refusals are out of the window.
    assert!(!b.observe(S(300), 1));
    // Slow refusals never add up: one each 40 s is 8 in a window.
    let mut b = burst();
    for i in 0..100 {
        assert!(!b.observe(S(i * 40), 1), "reading {i}");
    }
}

#[test]
fn a_second_burst_inside_repeat_after_is_silent_and_a_continuing_burst_alerts_again() {
    let mut b = burst();
    assert!(b.observe(S(0), 10));
    b.confirm(S(0));
    // The refusals that made the alert do not count again.
    assert!(!b.observe(S(30), 0));
    // A second burst 10 minutes later: silent.
    assert!(!b.observe(S(600), 12));
    assert!(!b.observe(S(1200), 0));
    // The burst continues: the window still holds refusals when repeat_after ends.
    for t in (3400..3600).step_by(20) {
        assert!(!b.observe(S(t), 1));
    }
    // 10 readings of 1 refusal are in the last 300 s at 3599.
    assert!(!b.observe(S(3599), 0));
    assert!(b.observe(S(3600), 0));
    b.confirm(S(3600));
    assert!(!b.observe(S(3601), 0));
}

#[test]
fn a_burst_that_stopped_makes_no_alert_when_repeat_after_ends() {
    let mut b = burst();
    assert!(b.observe(S(0), 10));
    b.confirm(S(0));
    assert!(!b.observe(S(100), 15));
    assert!(!b.observe(S(3600), 0), "the refusals are old");
}

#[test]
fn a_failed_send_keeps_the_burst_due_while_the_window_holds_it() {
    let mut b = burst();
    assert!(b.observe(S(0), 10));
    assert!(b.observe(S(100), 0));
    assert!(b.observe(S(299), 0));
    assert!(!b.observe(S(300), 0), "the burst left the window");
}

#[test]
fn the_burst_detector_survives_a_clock_that_goes_back() {
    let mut b = burst();
    assert!(!b.observe(S(1000), 5));
    assert!(!b.observe(S(10), 0));
    assert!(b.observe(S(20), 5), "the refusals count from the new time");
    b.confirm(S(20));
    assert!(!b.observe(S(5), 20));
    assert!(!b.observe(S(3000), 0));
}

#[test]
fn many_refusals_in_one_reading_do_not_overflow() {
    let mut b = burst();
    assert!(b.observe(S(0), u64::MAX));
    assert!(b.observe(S(1), u64::MAX));
}

#[test]
fn the_titles_are_one_line_of_plain_english() {
    let config = config();
    let service = |name: &str| Due {
        kind: Kind::ServiceUnhealthy,
        subject: "notes".into(),
        name: name.into(),
    };
    assert_eq!(
        title(&service("Notes"), &config),
        "Service Notes is unhealthy for 2 minutes"
    );
    let recovered = Due {
        kind: Kind::ServiceRecovered,
        ..service("Notes")
    };
    assert_eq!(title(&recovered, &config), "Service Notes is healthy again");
    let burst = Due {
        kind: Kind::SignInBurst,
        subject: String::new(),
        name: String::new(),
    };
    assert_eq!(title(&burst, &config), "10 refused sign-ins in 5 minutes");
    // The name is cleaned: no control character, no long run, a short length.
    let long = format!("A\n\tB   {}", "x".repeat(300));
    let t = title(&service(&long), &config);
    assert!(
        !t.chars().any(char::is_control) && t.chars().count() <= 128,
        "{t}"
    );
    assert!(t.starts_with("Service A B xxx"));
    assert_eq!(
        title(&service("  \n "), &config),
        "Service notes is unhealthy for 2 minutes"
    );
    for (secs, text) in [
        (10, "10 seconds"),
        (60, "1 minute"),
        (90, "90 seconds"),
        (3600, "1 hour"),
        (5400, "90 minutes"),
        (86_400, "1 day"),
        (172_800, "2 days"),
    ] {
        assert_eq!(words(S(secs)), text);
    }
}

#[test]
fn the_titles_never_hold_a_login_an_address_or_a_password() {
    // The burst title holds the configuration values only: the engine does not know them.
    let mut engine = Engine::new(config());
    let out = engine.tick(S(0), 1_000_000, 10, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "10 refused sign-ins in 5 minutes");
}

#[test]
fn the_keys_hold_shared_facts_only() {
    let due = Due {
        kind: Kind::ServiceUnhealthy,
        subject: "notes".into(),
        name: "Notes".into(),
    };
    // 1_000_000 / 3600 = 277.
    assert_eq!(key(&due, 1_000_000, S(3600)), "portal:unhealthy:notes:277");
    assert_eq!(
        key(&due, 1_000_000 + 100, S(3600)),
        "portal:unhealthy:notes:277"
    );
    assert_eq!(
        key(&due, 1_000_000 + 3600, S(3600)),
        "portal:unhealthy:notes:278"
    );
    let recovered = Due {
        kind: Kind::ServiceRecovered,
        ..due.clone()
    };
    assert_eq!(
        key(&recovered, 1_000_000, S(3600)),
        "portal:recovered:notes:277"
    );
    let burst = Due {
        kind: Kind::SignInBurst,
        subject: String::new(),
        name: String::new(),
    };
    assert_eq!(key(&burst, 1_000_000, S(60)), "portal:burst:16666");
    let long = Due {
        subject: "x".repeat(200),
        ..due
    };
    let k = key(&long, 1_000_000, S(3600));
    assert!(k.len() <= 128, "{k}");
    assert_eq!(k, key(&long, 1_000_000, S(3600)));
    assert!(k.starts_with("portal:unhealthy:"));
}

#[test]
fn two_engines_build_the_same_alert_for_the_same_event() {
    // Two instances read the same event at slightly different times.
    let down = [svc("notes", Seen::Unhealthy)];
    let (mut a, mut b) = (Engine::new(config()), Engine::new(config()));
    let (mut first_a, mut first_b) = (None, None);
    for t in 0..=125 {
        let from_a = a.tick(S(t), 5_000_000 + t, 0, &down);
        let from_b = b.tick(S(t + 3), 5_000_000 + t + 3, 0, &down);
        first_a = first_a.or(from_a.into_iter().next());
        first_b = first_b.or(from_b.into_iter().next());
    }
    let (x, y) = (first_a.unwrap(), first_b.unwrap());
    assert_eq!((&x.title, &x.key), (&y.title, &y.key));
}

/// The audit finding portal/watch/group-alert-names-services-hidden-from-receivers: the alert of a
/// service and its recovery carry the id of the service, so the inbox shows them only to who may
/// open it. A burst names no service.
#[test]
fn an_alert_about_a_service_carries_the_id_of_the_service() {
    let mut engine = Engine::new(config());
    let down = [svc("notes", Seen::Unhealthy)];
    assert!(engine.tick(S(0), 0, 0, &down).is_empty());
    let alert = engine.tick(S(120), 120, 0, &down).remove(0);
    assert_eq!(alert.service().as_deref(), Some("notes"));
    engine.confirm(&alert, S(120));
    let back = engine
        .tick(S(130), 130, 0, &[svc("notes", Seen::Healthy)])
        .remove(0);
    assert_eq!(back.due.kind, Kind::ServiceRecovered);
    assert_eq!(back.service().as_deref(), Some("notes"));
    let burst = engine.tick(S(140), 140, 50, &[]).remove(0);
    assert_eq!(burst.due.kind, Kind::SignInBurst);
    assert_eq!(burst.service(), None);
}

#[test]
fn the_engine_follows_the_switches_of_the_configuration() {
    let down = [svc("notes", Seen::Unhealthy)];
    let mut off = config();
    off.service_unhealthy = false;
    let mut engine = Engine::new(off);
    assert!(engine.tick(S(0), 0, 0, &down).is_empty());
    assert!(engine.tick(S(500), 500, 0, &down).is_empty());
    assert_eq!(
        engine.tick(S(501), 501, 50, &down).len(),
        1,
        "the burst stays on"
    );

    let mut off = config();
    off.sign_in_burst = false;
    let mut engine = Engine::new(off);
    assert!(engine.tick(S(1), 1, 500, &[]).is_empty());
}

#[test]
fn the_engine_counts_the_new_refusals_of_the_counter() {
    let mut engine = Engine::new(config());
    assert!(engine.tick(S(0), 0, 4, &[]).is_empty());
    assert!(
        engine.tick(S(1), 1, 4, &[]).is_empty(),
        "the counter did not move"
    );
    assert!(engine.tick(S(2), 2, 8, &[]).is_empty());
    let out = engine.tick(S(3), 3, 10, &[]);
    assert_eq!(out.len(), 1);
    engine.confirm(&out[0], S(3));
    assert!(engine.tick(S(4), 4, 10, &[]).is_empty());
    // A counter that goes down (it does not) gives no refusal.
    assert!(engine.tick(S(5), 5, 0, &[]).is_empty());
}

#[test]
fn the_metrics_have_a_series_for_each_kind() {
    let stats = Stats::default();
    assert_eq!(stats.render(false), "");
    stats.sent(Kind::SignInBurst);
    stats.failed(Kind::ServiceUnhealthy);
    stats.failed(Kind::ServiceUnhealthy);
    let text = stats.render(true);
    assert!(text.contains("portal_watch_sent_total{kind=\"sign_in_burst\"} 1"));
    assert!(text.contains("portal_watch_sent_total{kind=\"service_unhealthy\"} 0"));
    assert!(text.contains("portal_watch_sent_total{kind=\"service_recovered\"} 0"));
    assert!(text.contains("portal_watch_failed_total{kind=\"service_unhealthy\"} 2"));
}
