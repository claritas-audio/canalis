//! Device rules (`DeviceRule`): what to do when a device appears and what to put back when
//! it goes. Pure; the session turns the `Action`s into scene recalls and metadata writes.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::DeviceRule;
use crate::model::{Defaults, VirtualKind};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Action {
    Scene(String),
    /// WirePlumber's configured default; `None` clears it, so WirePlumber picks by priority.
    Default {
        kind: VirtualKind,
        name: Option<String>,
    },
}

/// A default a rule set, and the configured one it replaced.
#[derive(Clone, Debug)]
struct Fired {
    kind: VirtualKind,
    set: String,
    before: Option<String>,
}

/// The rules that fired for devices still present, by device `node.name`.
#[derive(Default)]
pub(super) struct Rules {
    fired: BTreeMap<String, Vec<Fired>>,
}

impl Rules {
    /// What to do now that the devices went from `before` to `now`: put back what departed
    /// devices' rules replaced, then fire the rules of the devices that arrived.
    pub(super) fn changed(
        &mut self,
        rules: &[DeviceRule],
        before: &BTreeSet<String>,
        now: &BTreeSet<String>,
        defaults: &Defaults,
    ) -> Vec<Action> {
        let mut configured = Configured {
            sink: defaults.configured_sink.clone(),
            source: defaults.configured_source.clone(),
        };
        let mut actions = Vec::new();
        let gone: Vec<String> = self
            .fired
            .keys()
            .filter(|device| !now.contains(*device))
            .cloned()
            .collect();
        for device in gone {
            for fired in self.fired.remove(&device).unwrap_or_default() {
                if configured.get(fired.kind) == Some(&fired.set) {
                    configured.set(fired.kind, fired.before.clone());
                    actions.push(Action::Default {
                        kind: fired.kind,
                        name: fired.before,
                    });
                } else {
                    // Another rule replaced this one's default since: it now goes back
                    // to what this one replaced.
                    for other in self.fired.values_mut().flatten() {
                        if other.kind == fired.kind && other.before.as_ref() == Some(&fired.set) {
                            other.before = fired.before.clone();
                        }
                    }
                }
            }
        }
        for rule in rules
            .iter()
            .filter(|r| now.contains(&r.device) && !before.contains(&r.device))
        {
            if let Some(scene) = &rule.scene {
                actions.push(Action::Scene(scene.clone()));
            }
            let mut fired = Vec::new();
            for (kind, target) in [
                (VirtualKind::Sink, &rule.output),
                (VirtualKind::Source, &rule.input),
            ] {
                let Some(name) = target else { continue };
                let was = configured.get(kind).cloned();
                if was.as_ref() == Some(name) {
                    continue;
                }
                configured.set(kind, Some(name.clone()));
                actions.push(Action::Default {
                    kind,
                    name: Some(name.clone()),
                });
                fired.push(Fired {
                    kind,
                    set: name.clone(),
                    before: was,
                });
            }
            if rule.restore && !fired.is_empty() {
                self.fired.insert(rule.device.clone(), fired);
            }
        }
        actions
    }
}

/// The configured defaults as they will be once the actions so far are applied.
struct Configured {
    sink: Option<String>,
    source: Option<String>,
}

impl Configured {
    fn get(&self, kind: VirtualKind) -> Option<&String> {
        match kind {
            VirtualKind::Sink => self.sink.as_ref(),
            VirtualKind::Source => self.source.as_ref(),
        }
    }

    fn set(&mut self, kind: VirtualKind, name: Option<String>) {
        match kind {
            VirtualKind::Sink => self.sink = name,
            VirtualKind::Source => self.source = name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn configured(sink: Option<&str>, source: Option<&str>) -> Defaults {
        Defaults {
            configured_sink: sink.map(Into::into),
            configured_source: source.map(Into::into),
            ..Defaults::default()
        }
    }

    fn sink(name: Option<&str>) -> Action {
        Action::Default {
            kind: VirtualKind::Sink,
            name: name.map(Into::into),
        }
    }

    fn headphones_through(chain: &str) -> DeviceRule {
        DeviceRule {
            output: Some(chain.into()),
            ..DeviceRule::new("bt")
        }
    }

    #[test]
    fn a_device_arriving_sets_its_defaults_and_leaving_puts_them_back() {
        let mut rules = Rules::default();
        let list = [DeviceRule {
            input: Some("bt.mic".into()),
            scene: Some("Calls".into()),
            ..headphones_through("canalis.headphones-eq")
        }];
        let before = configured(Some("speakers"), None);
        let actions = rules.changed(
            &list,
            &set(&["speakers"]),
            &set(&["speakers", "bt"]),
            &before,
        );
        assert_eq!(
            actions,
            [
                Action::Scene("Calls".into()),
                sink(Some("canalis.headphones-eq")),
                Action::Default {
                    kind: VirtualKind::Source,
                    name: Some("bt.mic".into())
                },
            ]
        );

        let after = configured(Some("canalis.headphones-eq"), Some("bt.mic"));
        let actions = rules.changed(
            &list,
            &set(&["speakers", "bt"]),
            &set(&["speakers"]),
            &after,
        );
        assert_eq!(
            actions,
            [
                sink(Some("speakers")),
                Action::Default {
                    kind: VirtualKind::Source,
                    name: None
                },
            ]
        );
    }

    #[test]
    fn a_default_chosen_since_is_left_alone() {
        let mut rules = Rules::default();
        let list = [headphones_through("bt")];
        rules.changed(
            &list,
            &set(&[]),
            &set(&["bt"]),
            &configured(Some("speakers"), None),
        );
        let chosen = configured(Some("hdmi"), None);
        assert!(
            rules
                .changed(&list, &set(&["bt"]), &set(&[]), &chosen)
                .is_empty()
        );
    }

    #[test]
    fn devices_there_from_the_start_and_rules_without_restore_fire_once() {
        let mut rules = Rules::default();
        let mut list = [headphones_through("bt")];
        let start = configured(Some("speakers"), None);
        assert!(
            rules
                .changed(&list, &set(&["bt"]), &set(&["bt"]), &start)
                .is_empty()
        );

        list[0].restore = false;
        let actions = rules.changed(&list, &set(&[]), &set(&["bt"]), &start);
        assert_eq!(actions, [sink(Some("bt"))]);
        let now = configured(Some("bt"), None);
        assert!(
            rules
                .changed(&list, &set(&["bt"]), &set(&[]), &now)
                .is_empty()
        );
    }

    #[test]
    fn stacked_rules_unwind_to_the_first_default() {
        let mut rules = Rules::default();
        let list = [
            DeviceRule {
                output: Some("usb".into()),
                ..DeviceRule::new("usb")
            },
            headphones_through("bt"),
        ];
        rules.changed(
            &list,
            &set(&[]),
            &set(&["usb"]),
            &configured(Some("speakers"), None),
        );
        rules.changed(
            &list,
            &set(&["usb"]),
            &set(&["usb", "bt"]),
            &configured(Some("usb"), None),
        );
        // The USB headset leaves first, while the Bluetooth one is the default.
        let now = configured(Some("bt"), None);
        assert!(
            rules
                .changed(&list, &set(&["usb", "bt"]), &set(&["bt"]), &now)
                .is_empty()
        );
        let actions = rules.changed(&list, &set(&["bt"]), &set(&[]), &now);
        assert_eq!(actions, [sink(Some("speakers"))]);
    }
}
