use crate::conf::Conf;

pub const HELPER: &str = "/usr/local/lib/baserri/sweep";

#[derive(Debug, Clone)]
pub struct Target {
    pub name: &'static str,
    pub why: &'static str,
    pub measure: &'static str,
    pub sweep: &'static str,
}

pub const TARGETS: &[Target] = &[
    Target {
        name: "journal",
        why: "the systemd journal grows until it is capped",
        measure: r#"used=$(size_of /var/log/journal); cap=$(to_bytes "$JOURNAL_CAP"); [ "$used" -gt "$cap" ] && echo $((used - cap)) || echo 0"#,
        sweep: r#"journalctl --vacuum-size="$JOURNAL_CAP" >/dev/null 2>&1 || true"#,
    },
    Target {
        name: "apt-cache",
        why: "downloaded .debs are never needed twice",
        measure: "size_of /var/cache/apt/archives",
        sweep: "apt-get clean >/dev/null 2>&1 || true",
    },
    Target {
        name: "apt-orphans",
        why: "old kernels and abandoned dependencies",
        measure: r#"apt-get -s autoremove 2>/dev/null | awk '/disk space will be freed/ {print $(NF-4), $(NF-3)}' | to_bytes_pair"#,
        sweep: "apt-get -y autoremove --purge >/dev/null 2>&1 || true",
    },
    Target {
        name: "runner-workspace",
        why: "every CI job leaves a checkout and a build tree behind",
        measure: "size_of_older /var/lib/forgejo-runner/workspace",
        sweep: "delete_older /var/lib/forgejo-runner/workspace",
    },
    Target {
        name: "runner-cache",
        why: "the actions cache is a cache",
        measure: "size_of_older /var/lib/forgejo-runner/cache",
        sweep: "delete_older /var/lib/forgejo-runner/cache",
    },
    Target {
        name: "cargo-registry",
        why: "a host-mode runner building Rust downloads every crate source forever",
        measure: "size_of /var/lib/forgejo-runner/.cargo/registry/cache /var/lib/forgejo-runner/.cargo/registry/src",
        sweep: "rm -rf /var/lib/forgejo-runner/.cargo/registry/cache /var/lib/forgejo-runner/.cargo/registry/src",
    },
    Target {
        name: "forgejo-actions",
        why: "workflow logs and artifacts, which the forge never prunes on its own",
        measure: "size_of_older /var/lib/forgejo/data/actions_log /var/lib/forgejo/data/actions_artifacts",
        sweep: "delete_older /var/lib/forgejo/data/actions_log /var/lib/forgejo/data/actions_artifacts",
    },
    Target {
        name: "old-logs",
        why: "rotated logs nobody will read",
        measure: r#"find /var/log -type f \( -name '*.gz' -o -name '*.1' -o -name '*.old' \) -mtime "+$KEEP_DAYS" -printf '%s\n' 2>/dev/null | awk '{s+=$1} END {print s+0}'"#,
        sweep: r#"find /var/log -type f \( -name '*.gz' -o -name '*.1' -o -name '*.old' \) -mtime "+$KEEP_DAYS" -delete 2>/dev/null || true"#,
    },
    Target {
        name: "tmp",
        why: "half-finished downloads and extracted tarballs",
        measure: "size_of_older /tmp /var/tmp",
        sweep: "delete_older /tmp /var/tmp",
    },
    Target {
        name: "docker",
        why: "only present if you turned the container runtime back on",
        measure: r#"command -v docker >/dev/null 2>&1 && docker system df --format '{{.Reclaimable}}' 2>/dev/null | head -1 | to_bytes_loose || echo 0"#,
        sweep: r#"command -v docker >/dev/null 2>&1 && docker system prune -af --filter "until=$((KEEP_DAYS*24))h" >/dev/null 2>&1 || true"#,
    },
];

pub fn target(name: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|t| t.name == name)
}

pub fn names() -> Vec<&'static str> {
    TARGETS.iter().map(|t| t.name).collect()
}

pub fn script(keep_days: u64, journal_cap: &str) -> String {
    let mut s = format!(
        r#"#!/bin/bash
set -uo pipefail
export LC_ALL=C
KEEP_DAYS={keep_days}
JOURNAL_CAP="{journal_cap}"
APPLY=0

size_of() {{ du -sb "$@" 2>/dev/null | awk '{{s+=$1}} END {{print s+0}}'; }}
size_of_older() {{
  local total=0 d
  for d in "$@"; do
    [ -d "$d" ] || continue
    local n
    n=$(find "$d" -mindepth 1 -maxdepth 1 -mtime "+$KEEP_DAYS" -exec du -sb {{}} + 2>/dev/null | awk '{{s+=$1}} END {{print s+0}}')
    total=$((total + n))
  done
  echo "$total"
}}
delete_older() {{
  local d
  for d in "$@"; do
    [ -d "$d" ] || continue
    find "$d" -mindepth 1 -maxdepth 1 -mtime "+$KEEP_DAYS" -exec rm -rf {{}} + 2>/dev/null || true
  done
}}
to_bytes() {{
  local v="${{1:-0}}"
  case "$v" in
    *G|*g) echo $(( ${{v%[Gg]}} * 1024 * 1024 * 1024 )) ;;
    *M|*m) echo $(( ${{v%[Mm]}} * 1024 * 1024 )) ;;
    *K|*k) echo $(( ${{v%[Kk]}} * 1024 )) ;;
    *) echo "${{v:-0}}" ;;
  esac
}}
to_bytes_pair() {{ read -r n unit || true; case "${{unit:-B}}" in MB) echo $(( ${{n%%.*}} * 1000000 )) ;; kB) echo $(( ${{n%%.*}} * 1000 )) ;; GB) echo $(( ${{n%%.*}} * 1000000000 )) ;; *) echo "${{n:-0}}" ;; esac; }}
to_bytes_loose() {{ read -r v || true; to_bytes "$(echo "${{v:-0}}" | tr -d 'B ' | tr -d '()')"; }}
emit() {{ printf '%s\t%s\n' "$1" "${{2:-0}}"; }}

run_one() {{
  case "$1" in
"#
    );
    for t in TARGETS {
        s.push_str(&format!(
            "    {})\n      if [ \"$APPLY\" = 1 ]; then before=$({}); {}; after=$({}); emit {} $((before > after ? before - after : 0)); else emit {} \"$({})\"; fi\n      ;;\n",
            t.name, t.measure, t.sweep, t.measure, t.name, t.name, t.measure
        ));
    }
    s.push_str(
        r#"    *) echo "unknown target: $1" >&2; exit 2 ;;
  esac
}

what=""
for arg in "$@"; do
  case "$arg" in
    --apply) APPLY=1 ;;
    -*) echo "unknown flag: $arg" >&2; exit 2 ;;
    *) what="$arg" ;;
  esac
done
: "${what:=all}"

if [ "$what" = all ]; then
  for t in ALL_TARGETS; do run_one "$t"; done
else
  run_one "$what"
fi
"#,
    );
    s.replace("ALL_TARGETS", &names().join(" "))
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn parse_report(text: &str) -> Vec<(String, u64)> {
    text.lines()
        .filter_map(|l| {
            let (name, bytes) = l.split_once('\t')?;
            Some((name.trim().to_string(), bytes.trim().parse().ok()?))
        })
        .collect()
}

pub fn format_report(rows: &[(String, u64)], applied: bool) -> String {
    let total: u64 = rows.iter().map(|(_, b)| b).sum();
    if rows.is_empty() {
        return "the sweep reported nothing — is the helper installed?".into();
    }
    let mut body = String::new();
    for (name, bytes) in rows {
        if *bytes == 0 {
            continue;
        }
        body.push_str(&format!("{name:<18} {}\n", human(*bytes)));
    }
    if body.is_empty() {
        return if applied {
            "swept, nothing to reclaim".into()
        } else {
            "nothing to reclaim".into()
        };
    }
    let heading = if applied { "freed" } else { "reclaimable" };
    format!("<b>{heading} {}</b>\n<pre>{body}</pre>", human(total))
}

pub fn keep_days(c: &Conf) -> u64 {
    c.num("cleanup.keep_days", 7)
}

pub fn journal_cap(c: &Conf) -> String {
    c.or("cleanup.journal_max", "200M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes_read_like_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1024), "1.0 KB");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn a_report_parses_and_totals() {
        let rows = parse_report("journal\t1048576\napt-cache\t2097152\ntmp\t0\n");
        assert_eq!(rows.len(), 3);
        let out = format_report(&rows, false);
        assert!(out.contains("reclaimable 3.0 MB"));
        assert!(out.contains("journal"));
        assert!(!out.contains("tmp"), "zero rows are noise");
    }

    #[test]
    fn nothing_to_reclaim_says_so_rather_than_printing_an_empty_table() {
        let rows = parse_report("journal\t0\ntmp\t0\n");
        assert_eq!(format_report(&rows, false), "nothing to reclaim");
        assert_eq!(format_report(&rows, true), "swept, nothing to reclaim");
    }

    #[test]
    fn a_silent_helper_is_reported_as_a_problem_not_as_success() {
        assert!(format_report(&[], false).contains("is the helper installed"));
    }

    #[test]
    fn garbage_lines_are_skipped() {
        assert!(parse_report("not a row\njournal\tnotanumber\n").is_empty());
    }

    #[test]
    fn every_target_is_in_the_generated_script() {
        let s = script(7, "200M");
        for name in names() {
            assert!(s.contains(&format!("    {name})")), "{name} has no case arm");
        }
        assert!(s.contains("for t in journal apt-cache"));
    }

    #[test]
    fn the_script_refuses_an_unknown_target() {
        let s = script(7, "200M");
        assert!(s.contains("unknown target"));
        assert!(s.contains("unknown flag"));
    }

    #[test]
    fn config_drives_the_retention_window() {
        let s = script(30, "50M");
        assert!(s.contains("KEEP_DAYS=30"));
        assert!(s.contains("JOURNAL_CAP=\"50M\""));
    }

    #[test]
    fn nothing_sweeps_without_the_apply_flag() {
        let s = script(7, "200M");
        assert!(s.contains("APPLY=0"));
        for t in TARGETS {
            let arm = s
                .split(&format!("    {})", t.name))
                .nth(1)
                .unwrap()
                .split(";;")
                .next()
                .unwrap()
                .to_string();
            assert!(arm.contains("if [ \"$APPLY\" = 1 ]"), "{} sweeps unconditionally", t.name);
        }
    }

    #[test]
    fn every_delete_is_depth_limited_to_a_known_directory() {
        let s = script(7, "200M");
        assert!(s.contains("find \"$d\" -mindepth 1 -maxdepth 1"));
        assert!(!s.contains("rm -rf /\n"));
        for t in TARGETS {
            assert!(
                !t.sweep.contains("$what") && !t.sweep.contains("$arg"),
                "{} interpolates an argument into a delete",
                t.name
            );
        }
    }

    #[test]
    fn the_generated_script_is_valid_bash() {
        let path = std::env::temp_dir().join(format!("baserri-sweep-{}.sh", std::process::id()));
        std::fs::write(&path, script(7, "200M")).unwrap();
        let out = crate::run::cmd("bash", &["-n", &path.display().to_string()]).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(out.ok(), "generated sweep script does not parse:\n{}", out.err);
    }

    #[test]
    fn a_dry_run_of_the_generated_script_reports_a_row_per_target() {
        let path = std::env::temp_dir().join(format!("baserri-sweep-run-{}.sh", std::process::id()));
        std::fs::write(&path, script(7, "200M")).unwrap();
        let out = crate::run::cmd("bash", &[&path.display().to_string(), "all"]).unwrap();
        std::fs::remove_file(&path).ok();
        let rows = parse_report(&out.out);
        assert_eq!(rows.len(), TARGETS.len(), "stdout was:\n{}\nstderr:\n{}", out.out, out.err);
    }

    #[test]
    fn an_unknown_target_exits_two_and_writes_nothing_to_stdout() {
        let path = std::env::temp_dir().join(format!("baserri-sweep-bad-{}.sh", std::process::id()));
        std::fs::write(&path, script(7, "200M")).unwrap();
        let out = crate::run::cmd("bash", &[&path.display().to_string(), "nope"]).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(out.code, 2);
        assert!(out.out.trim().is_empty());
    }

    #[test]
    fn target_lookup_is_an_allowlist() {
        assert!(target("journal").is_some());
        assert!(target("../../etc").is_none());
        assert!(target("journal; rm -rf /").is_none());
    }
}
