use crate::ssh::Host;
use std::collections::BTreeMap;

pub const PROBE: &str = r#"
emit() { printf '%s=%s\n' "$1" "$2"; }
have() { command -v "$1" >/dev/null 2>&1 && echo yes || echo no; }

emit model "$(tr -d '\0' < /proc/device-tree/model 2>/dev/null || echo unknown)"
emit arch "$(uname -m)"
emit kernel "$(uname -r)"
emit cpus "$(nproc)"
emit os "$(. /etc/os-release 2>/dev/null; echo "${PRETTY_NAME:-unknown}")"
emit mem_mb "$(awk '/MemTotal/ {printf "%d", $2/1024}' /proc/meminfo)"
emit swap_mb "$(awk '/SwapTotal/ {printf "%d", $2/1024}' /proc/meminfo)"
emit uptime_s "$(cut -d. -f1 /proc/uptime)"
emit user "$(id -un)"

root_src="$(findmnt -no SOURCE / 2>/dev/null || echo unknown)"
emit root_src "$root_src"
case "$root_src" in
  *mmcblk*) emit root_kind sd ;;
  *nvme*)   emit root_kind nvme ;;
  /dev/sd*) emit root_kind usb ;;
  *)        emit root_kind unknown ;;
esac
emit root_used_pct "$(df --output=pcent / | tail -1 | tr -dc '0-9')"
emit root_avail_gb "$(df --output=avail -BG / | tail -1 | tr -dc '0-9')"

if command -v vcgencmd >/dev/null 2>&1; then
  emit throttled "$(vcgencmd get_throttled 2>/dev/null | cut -d= -f2)"
  emit temp_c "$(vcgencmd measure_temp 2>/dev/null | tr -dc '0-9.')"
else
  emit throttled unknown
  emit temp_c "$(awk '{printf "%.1f", $1/1000}' /sys/class/thermal/thermal_zone0/temp 2>/dev/null || echo 0)"
fi

emit docker "$(have docker)"
emit compose "$(docker compose version >/dev/null 2>&1 && echo yes || echo no)"
emit tailscale "$(have tailscale)"
emit restic "$(have restic)"
emit curl "$(have curl)"
emit zram "$(test -e /dev/zram0 && echo yes || echo no)"
emit systemd "$(have systemctl)"
emit sudo_nopasswd "$(sudo -n true >/dev/null 2>&1 && echo yes || echo no)"
emit arolad "$(test -x /usr/local/bin/arolad && echo yes || echo no)"
"#;

#[derive(Debug, Default, Clone)]
pub struct Facts {
    map: BTreeMap<String, String>,
}

impl Facts {
    pub fn local() -> Result<Facts, String> {
        let script = format!("set -uo pipefail\nexport LC_ALL=C\n{PROBE}");
        let out = crate::run::cmd_stdin("bash", &["-s"], Some(&script))
            .map_err(|e| format!("probe failed: {e}"))?;
        Ok(Facts::parse(&out.out))
    }

    pub fn probe(host: &Host) -> Result<Facts, String> {
        let out = host.sh(PROBE).map_err(|e| format!("probe failed: {e}"))?;
        if !out.ok() {
            return Err(format!("probe failed on the host: {}", out.last_line()));
        }
        Ok(Facts::parse(&out.out))
    }

    pub fn parse(text: &str) -> Facts {
        let mut map = BTreeMap::new();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        Facts { map }
    }

    pub fn get(&self, key: &str) -> &str {
        self.map.get(key).map(String::as_str).unwrap_or("unknown")
    }

    pub fn num(&self, key: &str) -> u64 {
        self.map
            .get(key)
            .and_then(|v| v.parse::<f64>().ok())
            .map(|v| v as u64)
            .unwrap_or(0)
    }

    pub fn yes(&self, key: &str) -> bool {
        self.get(key) == "yes"
    }

    pub fn pairs(&self) -> impl Iterator<Item = (&String, &String)> {
        self.map.iter()
    }

    pub fn is_pi(&self) -> bool {
        self.get("model").to_lowercase().contains("raspberry pi")
    }

    pub fn pi_generation(&self) -> Option<u32> {
        let m = self.get("model").to_lowercase();
        ["5", "4", "3", "2"]
            .iter()
            .find(|g| m.contains(&format!("pi {g}")))
            .and_then(|g| g.parse().ok())
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.get("arch") != "aarch64" {
            w.push(format!(
                "arch is {} — arola targets aarch64; a 32-bit userland cannot run the arm64 images",
                self.get("arch")
            ));
        }
        match self.get("root_kind") {
            "sd" => w.push(
                "root is on the SD card — forge and Docker write load will kill it; move to USB3 SSD"
                    .into(),
            ),
            "unknown" => w.push(format!("could not classify root device {}", self.get("root_src"))),
            _ => {}
        }
        let throttled = self.get("throttled");
        if throttled != "unknown" && throttled != "0x0" {
            w.push(format!(
                "throttling flags {throttled} — bit 0 is undervoltage right now, bit 16 means it happened since boot; a brownout corrupts the SSD silently"
            ));
        }
        let temp = self.map.get("temp_c").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        if temp >= 70.0 {
            w.push(format!("{temp} C at idle — it will throttle under CI without active cooling"));
        }
        let mem = self.num("mem_mb");
        if mem > 0 && mem < 3500 {
            w.push(format!(
                "{mem} MB RAM — under 4 GB the dev-services lane (Postgres + Valkey + MinIO) does not fit"
            ));
        }
        if self.num("root_used_pct") >= 85 {
            w.push(format!("root is {}% full", self.get("root_used_pct")));
        }
        if !self.yes("sudo_nopasswd") {
            w.push("passwordless sudo is not available — every root step will fail with `sudo: a password is required`".into());
        }
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Facts {
        Facts::parse(
            "model=Raspberry Pi 4 Model B Rev 1.4\narch=aarch64\nmem_mb=3794\nroot_kind=usb\nthrottled=0x0\ntemp_c=44.5\nroot_used_pct=12\nsudo_nopasswd=yes\ndocker=no\n",
        )
    }

    #[test]
    fn a_healthy_4b_produces_no_warnings() {
        let f = sample();
        assert!(f.is_pi());
        assert_eq!(f.pi_generation(), Some(4));
        assert_eq!(f.num("mem_mb"), 3794);
        assert!(!f.yes("docker"));
        assert_eq!(f.warnings(), Vec::<String>::new());
    }

    #[test]
    fn four_gb_reports_as_3794_and_must_not_trip_the_ram_warning() {
        assert!(!sample().warnings().iter().any(|w| w.contains("RAM")));
    }

    #[test]
    fn sd_root_and_undervoltage_are_both_caught() {
        let f = Facts::parse("arch=aarch64\nroot_kind=sd\nthrottled=0x50005\nsudo_nopasswd=yes\n");
        let w = f.warnings();
        assert!(w.iter().any(|w| w.contains("SD card")));
        assert!(w.iter().any(|w| w.contains("0x50005")));
    }

    #[test]
    fn a_32_bit_userland_is_flagged() {
        let f = Facts::parse("arch=armv7l\nroot_kind=usb\nthrottled=0x0\nsudo_nopasswd=yes\n");
        assert!(f.warnings().iter().any(|w| w.contains("aarch64")));
    }

    #[test]
    fn unknown_keys_do_not_panic() {
        let f = Facts::default();
        assert_eq!(f.get("nothing"), "unknown");
        assert_eq!(f.num("nothing"), 0);
        assert!(!f.is_pi());
    }
}
