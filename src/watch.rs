use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Appeared { key: String, now: String },
    Changed { key: String, was: String, now: String },
    Gone { key: String, was: String },
}

impl Change {
    pub fn key(&self) -> &str {
        match self {
            Change::Appeared { key, .. } | Change::Changed { key, .. } | Change::Gone { key, .. } => key,
        }
    }
}

#[derive(Debug, Default)]
pub struct Transitions {
    seen: BTreeMap<String, String>,
    primed: bool,
}

impl Transitions {
    pub fn step(&mut self, current: Vec<(String, String)>) -> Vec<Change> {
        let now: BTreeMap<String, String> = current.into_iter().collect();
        let mut changes = Vec::new();
        for (key, value) in &now {
            match self.seen.get(key) {
                None => changes.push(Change::Appeared { key: key.clone(), now: value.clone() }),
                Some(was) if was != value => changes.push(Change::Changed {
                    key: key.clone(),
                    was: was.clone(),
                    now: value.clone(),
                }),
                Some(_) => {}
            }
        }
        for (key, was) in &self.seen {
            if !now.contains_key(key) {
                changes.push(Change::Gone { key: key.clone(), was: was.clone() });
            }
        }
        self.seen = now;
        if !self.primed {
            self.primed = true;
            return Vec::new();
        }
        changes
    }

    pub fn known(&self) -> &BTreeMap<String, String> {
        &self.seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn the_first_poll_is_silent_so_a_restart_does_not_replay_the_world() {
        let mut t = Transitions::default();
        let out = t.step(state(&[("pr:1", "green"), ("pr:2", "red")]));
        assert!(out.is_empty(), "priming must not notify");
        assert_eq!(t.known().len(), 2);
    }

    #[test]
    fn a_value_change_fires_once() {
        let mut t = Transitions::default();
        t.step(state(&[("pr:1", "green")]));
        assert_eq!(
            t.step(state(&[("pr:1", "red")])),
            vec![Change::Changed { key: "pr:1".into(), was: "green".into(), now: "red".into() }]
        );
        assert!(t.step(state(&[("pr:1", "red")])).is_empty());
    }

    #[test]
    fn a_new_key_appears_and_a_removed_key_goes() {
        let mut t = Transitions::default();
        t.step(state(&[("pr:1", "green")]));
        assert_eq!(
            t.step(state(&[("pr:1", "green"), ("pr:2", "draft")])),
            vec![Change::Appeared { key: "pr:2".into(), now: "draft".into() }]
        );
        assert_eq!(
            t.step(state(&[("pr:2", "draft")])),
            vec![Change::Gone { key: "pr:1".into(), was: "green".into() }]
        );
    }

    #[test]
    fn everything_vanishing_is_reported_not_swallowed() {
        let mut t = Transitions::default();
        t.step(state(&[("a", "1"), ("b", "2")]));
        let out = t.step(vec![]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|c| matches!(c, Change::Gone { .. })));
    }

    #[test]
    fn an_empty_first_poll_still_primes() {
        let mut t = Transitions::default();
        assert!(t.step(vec![]).is_empty());
        assert_eq!(
            t.step(state(&[("pr:1", "green")])),
            vec![Change::Appeared { key: "pr:1".into(), now: "green".into() }]
        );
    }
}
