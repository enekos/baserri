use crate::conf::Conf;
use crate::facts::Facts;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct Thresholds {
    pub disk_pct: u64,
    pub temp_c: f64,
    pub units: Vec<String>,
    pub interval_s: u64,
}

impl Default for Thresholds {
    fn default() -> Thresholds {
        Thresholds { disk_pct: 85, temp_c: 75.0, units: Vec::new(), interval_s: 600 }
    }
}

impl Thresholds {
    pub fn from_conf(c: &Conf) -> Thresholds {
        let d = Thresholds::default();
        Thresholds {
            disk_pct: c.num("alerts.disk_pct", d.disk_pct),
            temp_c: c
                .get("alerts.temp_c")
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.temp_c),
            units: c.list("alerts.units"),
            interval_s: c.num("alerts.interval_s", d.interval_s),
        }
    }
}

pub fn evaluate(f: &Facts, t: &Thresholds, dead_units: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let used = f.num("root_used_pct");
    if used >= t.disk_pct {
        out.push((
            "disk".into(),
            format!("root is {used}% full ({} GB left)", f.get("root_avail_gb")),
        ));
    }
    let temp: f64 = f.get("temp_c").parse().unwrap_or(0.0);
    if temp >= t.temp_c {
        out.push(("temp".into(), format!("{temp} C — throttling territory")));
    }
    let throttled = f.get("throttled");
    if throttled != "unknown" && throttled != "0x0" {
        out.push((
            "power".into(),
            format!("throttle flags {throttled} — check the PSU before trusting the disk"),
        ));
    }
    for unit in dead_units {
        out.push((format!("unit:{unit}"), format!("{unit} is not active")));
    }
    out
}

#[derive(Debug, Default)]
pub struct Watch {
    firing: BTreeSet<String>,
}

impl Watch {
    pub fn step(&mut self, current: Vec<(String, String)>) -> Vec<String> {
        let keys: BTreeSet<String> = current.iter().map(|(k, _)| k.clone()).collect();
        let mut messages = Vec::new();
        for (key, text) in &current {
            if !self.firing.contains(key) {
                messages.push(format!("\u{26a0} {text}"));
            }
        }
        for gone in self.firing.difference(&keys) {
            messages.push(format!("\u{2713} recovered: {gone}"));
        }
        self.firing = keys;
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(extra: &str) -> Facts {
        Facts::parse(&format!("root_used_pct=10\ntemp_c=45.0\nthrottled=0x0\nroot_avail_gb=100\n{extra}"))
    }

    #[test]
    fn a_healthy_box_fires_nothing() {
        assert!(evaluate(&facts(""), &Thresholds::default(), &[]).is_empty());
    }

    #[test]
    fn each_threshold_fires_on_its_own_key() {
        let f = facts("root_used_pct=91\ntemp_c=80.0\nthrottled=0x50000\n");
        let keys: Vec<String> = evaluate(&f, &Thresholds::default(), &[]).into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["disk", "temp", "power"]);
    }

    #[test]
    fn an_alert_is_sent_once_not_every_tick() {
        let mut w = Watch::default();
        let firing = vec![("disk".to_string(), "root is 91% full".to_string())];
        assert_eq!(w.step(firing.clone()).len(), 1);
        assert!(w.step(firing.clone()).is_empty());
        assert!(w.step(firing).is_empty());
    }

    #[test]
    fn clearing_an_alert_reports_a_recovery() {
        let mut w = Watch::default();
        w.step(vec![("disk".into(), "full".into())]);
        let msgs = w.step(vec![]);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("recovered: disk"));
        assert!(w.step(vec![]).is_empty());
    }

    #[test]
    fn a_second_alert_appearing_does_not_resend_the_first() {
        let mut w = Watch::default();
        w.step(vec![("disk".into(), "full".into())]);
        let msgs = w.step(vec![("disk".into(), "full".into()), ("temp".into(), "hot".into())]);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("hot"));
    }

    #[test]
    fn dead_units_become_alerts() {
        let out = evaluate(&facts(""), &Thresholds::default(), &["forgejo".to_string()]);
        assert_eq!(out[0].0, "unit:forgejo");
    }

    #[test]
    fn thresholds_come_from_the_config() {
        let t = Thresholds::from_conf(
            &Conf::parse("[alerts]\ndisk_pct = 70\ntemp_c = 65.5\nunits = forgejo, docker\ninterval_s = 60\n").unwrap(),
        );
        assert_eq!(t.disk_pct, 70);
        assert_eq!(t.temp_c, 65.5);
        assert_eq!(t.units, vec!["forgejo", "docker"]);
        assert_eq!(t.interval_s, 60);
    }
}
