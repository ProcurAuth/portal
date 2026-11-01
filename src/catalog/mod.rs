//! The service catalogue: what the portal lists, grouped and ordered.
//!
//! Services declare themselves through labels (see [`labels`]). This module holds no central
//! inventory. A [`Catalog`] is an immutable snapshot of the parsed services of every source.
//! The snapshot is cheap to share. The portal replaces it as a whole when discovery changes.
//!
//! Interface: [`parse_labels`] takes the labels of one workload and returns a [`Service`].
//! [`Catalog::build`] takes services and returns ordered groups. Everything else is plain data.

mod labels;

use std::collections::HashMap;

pub use labels::closed_prefix;
pub use labels::parse as parse_labels;

use crate::access::{ExcludeRule, Segments};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// No probe is configured, or no probe has answered yet.
    Unknown,
    Up,
    Down,
}

/// The result of the health check of the runtime: the Docker `HEALTHCHECK` of a container, or
/// the readiness probes of the pods. It adds detail to [`Health`]. It does not change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeHealth {
    /// The check passes.
    Healthy,
    /// The check fails.
    Unhealthy,
    /// The check has not passed yet. The workload starts.
    Starting,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    /// Stable identity: `portal.id`, or else the slug of `group/name`. Two workloads with the same
    /// id are one service (replicas, `global` containers). The portal keeps the first one seen.
    pub id: String,
    pub group: String,
    pub name: String,
    pub description: String,
    /// Icon reference as declared (`notes`, `docker.png`, ...). The icon library resolves it.
    pub icon: Option<String>,
    /// A path on this host (`/grafana/`) or an absolute http(s) URL. The parser validates it.
    pub href: String,
    pub new_tab: bool,
    pub weight: i32,
    pub health_url: Option<String>,
    /// Where the service publishes its actions (`/.well-known/procurauth/actions`). The value comes from
    /// the label `portal.actions`. If that label is absent, the origin of the health URL is used.
    /// `None` means that the service has only `access`.
    pub actions_url: Option<String>,
    /// Paths of the service that are reachable without a session (`portal.exclude`).
    /// The list is empty for almost every service.
    pub exclude: Vec<ExcludeRule>,
    pub health: Health,
    /// The health check of the runtime. `None` when the runtime has no check for this service,
    /// or when discovery does not ask the runtime.
    pub runtime: Option<RuntimeHealth>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub services: Vec<Service>,
}

impl Group {
    pub fn down_count(&self) -> usize {
        self.services
            .iter()
            .filter(|s| s.health == Health::Down)
            .count()
    }

    /// The services that run, but whose runtime health check fails. A down service is not in
    /// this count.
    pub fn unhealthy_count(&self) -> usize {
        self.services
            .iter()
            .filter(|s| s.health != Health::Down && s.runtime == Some(RuntimeHealth::Unhealthy))
            .count()
    }
}

/// A service that the gate sees. It has an id and the paths that it opens without a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateService {
    pub id: String,
    pub exclude: Vec<ExcludeRule>,
}

/// The services that own one path prefix. Usually, there is one service. An entry without a
/// service is a closed prefix: only the superadmin passes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateEntry {
    pub prefix: Vec<String>,
    pub services: Vec<GateService>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    groups: Vec<Group>,
    problems: Vec<String>,
    duplicates: usize,
    /// Path prefixes of the services with a local `href`, longest first. `/auth/verify` matches
    /// a request against these prefixes.
    gate: Vec<GateEntry>,
    /// The position in `gate` of each prefix: the search of the owner of a path looks for each
    /// prefix of the path in this table, longest first. It needs at most one search for each
    /// segment of the path, at any number of services (portal 8.3a).
    gate_index: HashMap<Segments, usize>,
    /// True when the snapshot holds every service of every source. The snapshot of the start is
    /// not complete. Discovery sets the value for each round.
    complete: bool,
}

impl Catalog {
    /// Groups and orders `services`. `problems` are discovery messages for the administrator.
    /// Examples are an unreachable source or a workload with unusable labels.
    ///
    /// The order is deterministic. Services are sorted by weight, then by name. Groups are sorted
    /// by their lightest service, then by name. Duplicate ids keep the first service in input order.
    /// The result is complete. Discovery marks a snapshot that misses a source.
    pub fn build(services: Vec<Service>, problems: Vec<String>) -> Self {
        Self::build_with_closed(services, problems, Vec::new())
    }

    /// The same as [`Catalog::build`]. Each prefix of `closed` stays closed in the gate: only the
    /// superadmin passes it. A workload with unusable labels gives such a prefix.
    ///
    /// A duplicate id with another `href` also gives a closed prefix. The catalogue keeps the
    /// first service, but Traefik still routes the `href` of the second one.
    pub fn build_with_closed(
        services: Vec<Service>,
        mut problems: Vec<String>,
        mut closed: Vec<Segments>,
    ) -> Self {
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut duplicates = 0;
        let mut by_group: Vec<Group> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();

        for service in services {
            if let Some(first_href) = seen.get(&service.id) {
                duplicates += 1;
                if *first_href != service.href {
                    if let Some(prefix) = crate::access::href_prefix(&service.href) {
                        closed.push(prefix);
                    }
                    problems.push(format!(
                        "{}: the id is already used with the href {first_href}; {} stays closed",
                        service.id, service.href
                    ));
                }
                continue;
            }
            seen.insert(service.id.clone(), service.href.clone());
            let slot = *index.entry(service.group.clone()).or_insert_with(|| {
                by_group.push(Group {
                    name: service.group.clone(),
                    services: Vec::new(),
                });
                by_group.len() - 1
            });
            by_group[slot].services.push(service);
        }

        for group in &mut by_group {
            group
                .services
                .sort_by_cached_key(|s| (s.weight, s.name.to_lowercase(), s.id.clone()));
        }
        by_group.sort_by_cached_key(|g| {
            let lightest = g.services.iter().map(|s| s.weight).min().unwrap_or(0);
            (lightest, g.name.to_lowercase())
        });

        let gate = build_gate(&by_group, closed);
        let gate_index = index_gate(&gate);
        Self {
            groups: by_group,
            problems,
            duplicates,
            gate,
            gate_index,
            complete: true,
        }
    }

    /// Tells whether the snapshot holds every service of every source. The gate needs a complete
    /// snapshot to know that a path has no owner.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn set_complete(&mut self, complete: bool) {
        self.complete = complete;
    }

    /// The same catalogue without the services that `keep` refuses. Groups that become empty are
    /// removed. The gate index is not carried over. This catalogue is for display only, and
    /// `/auth/verify` uses the full one. `problems` keeps the discovery problems. They name
    /// workloads, so only the superadmin sees them.
    pub fn filtered(&self, keep: impl Fn(&Service) -> bool, problems: bool) -> Catalog {
        let groups = self
            .groups
            .iter()
            .filter_map(|group| {
                let services: Vec<Service> =
                    group.services.iter().filter(|s| keep(s)).cloned().collect();
                (!services.is_empty()).then(|| Group {
                    name: group.name.clone(),
                    services,
                })
            })
            .collect();
        Catalog {
            groups,
            problems: if problems {
                self.problems.clone()
            } else {
                Vec::new()
            },
            duplicates: self.duplicates,
            gate: Vec::new(),
            gate_index: HashMap::new(),
            complete: self.complete,
        }
    }

    /// The same catalogue with one more service that the portal itself gives, such as the service
    /// `inbox`. The service owns no path: the gate does not change. The service goes in the group
    /// with its name, or in a new group. An id that the catalogue has already stays as it is.
    pub fn with_internal(mut self, service: Service) -> Catalog {
        if self
            .groups
            .iter()
            .any(|g| g.services.iter().any(|s| s.id == service.id))
        {
            return self;
        }
        let slot = match self.groups.iter().position(|g| g.name == service.group) {
            Some(slot) => slot,
            None => {
                self.groups.push(Group {
                    name: service.group.clone(),
                    services: Vec::new(),
                });
                self.groups.len() - 1
            }
        };
        self.groups[slot].services.push(service);
        self.groups[slot]
            .services
            .sort_by_cached_key(|s| (s.weight, s.name.to_lowercase(), s.id.clone()));
        self.groups.sort_by_cached_key(|g| {
            let lightest = g.services.iter().map(|s| s.weight).min().unwrap_or(0);
            (lightest, g.name.to_lowercase())
        });
        self
    }

    /// The services that own the longest prefix of `segments`, if any.
    pub fn owner(&self, segments: &[String]) -> Option<&GateEntry> {
        (1..=segments.len())
            .rev()
            .find_map(|length| self.gate_index.get(&segments[..length]))
            .map(|position| &self.gate[*position])
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    pub fn service_count(&self) -> usize {
        self.groups.iter().map(|g| g.services.len()).sum()
    }

    pub fn duplicates(&self) -> usize {
        self.duplicates
    }
}

/// The position of each prefix in the gate.
fn index_gate(gate: &[GateEntry]) -> HashMap<Segments, usize> {
    gate.iter()
        .enumerate()
        .map(|(position, entry)| (entry.prefix.clone(), position))
        .collect()
}

fn build_gate(groups: &[Group], closed: Vec<Segments>) -> Vec<GateEntry> {
    let mut entries: Vec<GateEntry> = Vec::new();
    // The position of each prefix in `entries`, so that the build looks at each prefix one time.
    let mut positions: HashMap<Segments, usize> = HashMap::new();
    for service in groups.iter().flat_map(|g| g.services.iter()) {
        let Some(prefix) = crate::access::href_prefix(&service.href) else {
            continue;
        };
        let gate_service = GateService {
            id: service.id.clone(),
            exclude: service.exclude.clone(),
        };
        match positions.get(&prefix) {
            Some(position) => entries[*position].services.push(gate_service),
            None => {
                positions.insert(prefix.clone(), entries.len());
                entries.push(GateEntry {
                    prefix,
                    services: vec![gate_service],
                });
            }
        }
    }
    // A closed prefix that a listed service also owns stays with that service.
    for prefix in closed {
        if !positions.contains_key(&prefix) {
            positions.insert(prefix.clone(), entries.len());
            entries.push(GateEntry {
                prefix,
                services: Vec::new(),
            });
        }
    }
    entries.sort_by(|a, b| {
        b.prefix
            .len()
            .cmp(&a.prefix.len())
            .then_with(|| a.prefix.cmp(&b.prefix))
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(group: &str, name: &str, weight: i32) -> Service {
        Service {
            id: labels::slug(&format!("{group}/{name}")),
            group: group.into(),
            name: name.into(),
            description: String::new(),
            icon: None,
            href: "/".into(),
            new_tab: false,
            weight,
            health_url: None,
            actions_url: None,
            exclude: Vec::new(),
            health: Health::Unknown,
            runtime: None,
        }
    }

    fn names(catalog: &Catalog) -> Vec<(String, Vec<String>)> {
        catalog
            .groups()
            .iter()
            .map(|g| {
                (
                    g.name.clone(),
                    g.services.iter().map(|s| s.name.clone()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn services_order_by_weight_then_name_and_groups_by_their_lightest_service() {
        let catalog = Catalog::build(
            vec![
                service("Monitoring", "Prometheus", 10),
                service("Infra", "Notes", 40),
                service("Infra", "Registry", 20),
                service("Infra", "DDNS", 20),
                service("Monitoring", "Grafana", 5),
            ],
            Vec::new(),
        );
        assert_eq!(
            names(&catalog),
            [
                (
                    "Monitoring".to_string(),
                    vec!["Grafana".to_string(), "Prometheus".to_string()]
                ),
                (
                    "Infra".to_string(),
                    vec![
                        "DDNS".to_string(),
                        "Registry".to_string(),
                        "Notes".to_string()
                    ]
                ),
            ]
        );
    }

    #[test]
    fn equal_weights_fall_back_to_alphabetical_groups() {
        let catalog = Catalog::build(vec![service("b", "x", 0), service("A", "y", 0)], Vec::new());
        assert_eq!(catalog.groups()[0].name, "A");
    }

    #[test]
    fn replicas_with_the_same_id_are_one_service() {
        let catalog = Catalog::build(
            vec![
                service("Monitoring", "Cadvisor", 20),
                service("Monitoring", "Cadvisor", 20),
                service("Monitoring", "Cadvisor", 20),
            ],
            Vec::new(),
        );
        assert_eq!(catalog.service_count(), 1);
        assert_eq!(catalog.duplicates(), 2);
    }

    /// Two workloads with one id and two different hrefs: the second one is dropped. Its prefix
    /// must stay closed, because Traefik still routes it.
    #[test]
    fn a_duplicate_id_with_another_href_keeps_the_dropped_prefix_closed() {
        let first = Service {
            id: "monitoring-grafana".into(),
            ..bound("Elsewhere", "/elsewhere/")
        };
        let second = Service {
            id: "monitoring-grafana".into(),
            ..bound("Grafana", "/grafana/")
        };
        let catalog = Catalog::build(vec![first, second], Vec::new());
        assert_eq!(catalog.service_count(), 1);
        assert_eq!(catalog.duplicates(), 1);
        let entry = catalog
            .owner(&segments(&["grafana", "d", "1"]))
            .expect("the dropped prefix stays in the gate");
        assert!(entry.services.is_empty(), "nobody owns it: it is closed");
        assert!(
            catalog.problems().iter().any(|p| p.contains("/grafana/")),
            "{:?}",
            catalog.problems()
        );
        let replicas = Catalog::build(
            vec![bound("Grafana", "/grafana/"), bound("Grafana", "/grafana/")],
            Vec::new(),
        );
        assert!(replicas.problems().is_empty(), "replicas are not a problem");
    }

    #[test]
    fn down_count_counts_only_down_services() {
        let mut up = service("Infra", "A", 0);
        up.health = Health::Up;
        let mut down = service("Infra", "B", 0);
        down.health = Health::Down;
        let catalog = Catalog::build(vec![up, down, service("Infra", "C", 0)], Vec::new());
        assert_eq!(catalog.groups()[0].down_count(), 1);
    }

    #[test]
    fn unhealthy_count_counts_only_services_that_run_with_a_failed_check() {
        let mut sick = service("Infra", "A", 0);
        sick.health = Health::Up;
        sick.runtime = Some(RuntimeHealth::Unhealthy);
        let mut down = service("Infra", "B", 0);
        down.health = Health::Down;
        down.runtime = Some(RuntimeHealth::Unhealthy);
        let mut starting = service("Infra", "C", 0);
        starting.health = Health::Up;
        starting.runtime = Some(RuntimeHealth::Starting);
        let catalog = Catalog::build(vec![sick, down, starting], Vec::new());
        assert_eq!(catalog.groups()[0].unhealthy_count(), 1);
        assert_eq!(catalog.groups()[0].down_count(), 1);
    }

    #[test]
    fn problems_travel_with_the_snapshot() {
        let catalog = Catalog::build(Vec::new(), vec!["docker: socket unreachable".into()]);
        assert_eq!(catalog.service_count(), 0);
        assert_eq!(catalog.problems(), ["docker: socket unreachable"]);
    }

    fn bound(name: &str, href: &str) -> Service {
        Service {
            href: href.into(),
            ..service("Infra", name, 0)
        }
    }

    fn segments(path: &[&str]) -> Vec<String> {
        path.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_gate_index_owns_the_longest_prefix_and_skips_what_it_cannot_gate() {
        let catalog = Catalog::build(
            vec![
                bound("Grafana", "/grafana/"),
                bound("Admin", "/grafana/admin/"),
                bound("Docs", "https://example.org/docs"),
                bound("Home", "/"),
            ],
            Vec::new(),
        );
        let owner = |path: &[&str]| {
            catalog
                .owner(&segments(path))
                .map(|e| e.services[0].id.clone())
        };
        assert_eq!(
            owner(&["grafana", "d", "x"]).as_deref(),
            Some("infra-grafana")
        );
        assert_eq!(
            owner(&["grafana", "admin", "x"]).as_deref(),
            Some("infra-admin")
        );
        assert_eq!(owner(&["grafana"]).as_deref(), Some("infra-grafana"));
        assert_eq!(owner(&["grafana-evil"]), None);
        assert_eq!(
            owner(&["docs"]),
            None,
            "an absolute URL is not on this host"
        );
        assert_eq!(
            owner(&[]),
            None,
            "a service at / would own everything: it is not indexed"
        );
    }

    #[test]
    fn two_services_on_one_prefix_share_an_entry() {
        let catalog = Catalog::build(
            vec![
                bound("One", "/shared/"),
                Service {
                    group: "Other".into(),
                    ..bound("Two", "/shared/")
                },
            ],
            Vec::new(),
        );
        let entry = catalog.owner(&segments(&["shared", "x"])).unwrap();
        assert_eq!(entry.services.len(), 2);
    }

    #[test]
    fn filtering_keeps_what_is_allowed_and_drops_empty_groups() {
        let catalog = Catalog::build(
            vec![
                bound("Open", "/open/"),
                Service {
                    group: "Secret".into(),
                    ..bound("Hidden", "/hidden/")
                },
            ],
            vec!["a problem".into()],
        );
        let shown = catalog.filtered(|s| s.group != "Secret", true);
        assert_eq!(shown.service_count(), 1);
        assert_eq!(
            shown.groups().len(),
            1,
            "the group with nothing left disappears"
        );
        assert_eq!(shown.problems(), ["a problem"]);
        assert!(
            catalog.filtered(|_| true, false).problems().is_empty(),
            "a user does not see the problems"
        );
        assert_eq!(catalog.service_count(), 2, "the original is untouched");
    }
}
