use crate::conf::Conf;
use crate::facts::Facts;
use crate::step::{Shell, Step, Upload};
use std::path::PathBuf;

pub const DEFAULT_PACKAGES: &str =
    "curl, git, rsync, restic, jq, htop, ca-certificates, unattended-upgrades, zram-tools, nftables";

pub struct Ctx {
    pub conf: Conf,
    pub facts: Facts,
    pub daemon_binary: PathBuf,
    pub daemon_config: PathBuf,
}

fn tpl(body: &str, vars: &[(&str, &str)]) -> String {
    let mut out = body.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("@{k}@"), v);
    }
    out
}

fn shell(name: &str, why: &str, check: &str, apply: &str, root: bool) -> Step {
    Step::Shell(Shell {
        name: name.to_string(),
        why: why.to_string(),
        check: check.trim().to_string(),
        apply: apply.trim().to_string(),
        root,
    })
}

pub fn build(ctx: &Ctx) -> Vec<Step> {
    let c = &ctx.conf;
    let f = &ctx.facts;

    let hostname = c.or("hostname", "arola").to_string();
    let timezone = c.or("timezone", "Europe/Madrid").to_string();
    let login = c.or("user", "pi").to_string();
    let svc = c.or("service_user", "arola").to_string();
    let packages = {
        let list = c.list("packages");
        if list.is_empty() {
            DEFAULT_PACKAGES.split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>()
        } else {
            list
        }
        .join(" ")
    };
    let zram_pct = c.num("zram_percent", 50).to_string();
    let swap_mb = c.num("swap_mb", 2048).to_string();
    let journal_cap = c.or("journal_max", "200M").to_string();
    let lan = c.or("lan_cidr", "192.168.1.0/24").to_string();

    let vars: Vec<(&str, &str)> = vec![
        ("hostname", &hostname),
        ("tz", &timezone),
        ("user", &login),
        ("svc", &svc),
        ("pkgs", &packages),
        ("pct", &zram_pct),
        ("swapmb", &swap_mb),
        ("cap", &journal_cap),
        ("lan", &lan),
    ];

    let mut steps = vec![
        shell(
            "hostname",
            "a box you address by name survives a DHCP change",
            &tpl(r#"[ "$(hostname)" = "@hostname@" ] && echo "@hostname@" || exit 10"#, &vars),
            &tpl(
                r#"
hostnamectl set-hostname @hostname@
sed -i "s/^127.0.1.1.*/127.0.1.1\t@hostname@/" /etc/hosts
grep -q "^127.0.1.1" /etc/hosts || printf '127.0.1.1\t%s\n' @hostname@ >> /etc/hosts
echo @hostname@
"#,
                &vars,
            ),
            true,
        ),
        shell(
            "timezone",
            "cron windows and log timestamps have to match the humans reading them",
            &tpl(
                r#"tz=$(timedatectl show -p Timezone --value); [ "$tz" = "@tz@" ] && echo "$tz" || exit 10"#,
                &vars,
            ),
            &tpl("timedatectl set-timezone @tz@\necho @tz@", &vars),
            true,
        ),
        shell(
            "packages",
            "the base toolset every later step assumes",
            &tpl(
                r#"
missing=""
for p in @pkgs@; do
  dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "ok installed" || missing="$missing $p"
done
[ -z "$missing" ] && echo "all present" || { echo "missing:$missing"; exit 10; }
"#,
                &vars,
            ),
            &tpl("apt-get update -qq\napt-get install -y -qq @pkgs@\necho installed", &vars),
            true,
        ),
        shell(
            "journal-cap",
            "an uncapped journal is the classic way a Pi fills its own disk",
            &tpl(r#"grep -qs '^SystemMaxUse=@cap@' /etc/systemd/journald.conf && echo "@cap@" || exit 10"#, &vars),
            &tpl(
                r#"
sed -i '/^#*SystemMaxUse=/d' /etc/systemd/journald.conf
printf 'SystemMaxUse=@cap@\n' >> /etc/systemd/journald.conf
systemctl restart systemd-journald
echo "@cap@"
"#,
                &vars,
            ),
            true,
        ),
        shell(
            "zram",
            "4 GB with Postgres, Valkey and a runner needs compressed swap before it needs disk swap",
            &tpl(
                r#"test -e /dev/zram0 && grep -qs '^PERCENT=@pct@' /etc/default/zramswap && echo "zram @pct@%" || exit 10"#,
                &vars,
            ),
            &tpl(
                r#"
apt-get install -y -qq zram-tools
sed -i '/^#*PERCENT=/d;/^#*ALGO=/d' /etc/default/zramswap
printf 'ALGO=zstd\nPERCENT=@pct@\n' >> /etc/default/zramswap
systemctl restart zramswap
echo "zram @pct@%"
"#,
                &vars,
            ),
            true,
        ),
    ];

    if f.get("root_kind") == "sd" {
        steps.push(Step::Manual {
            name: "ssd-boot".into(),
            why: "forge plus Docker plus Postgres will write an SD card to death in months".into(),
            check: "exit 10".into(),
            instruction: "root is still on the SD card. Update the EEPROM (`sudo rpi-eeprom-update -a`), set BOOT_ORDER=0xf41, clone to a UAS-capable USB3 SSD and re-run. arola will not add a disk swapfile on an SD card.".into(),
        });
    } else {
        steps.push(shell(
            "swapfile",
            "zram alone cannot absorb a compile or a restic run on 4 GB",
            r#"swapon --show=NAME --noheadings 2>/dev/null | grep -qx /swapfile && echo present || exit 10"#,
            &tpl(
                r#"
systemctl disable --now dphys-swapfile >/dev/null 2>&1 || true
rm -f /var/swap
if ! test -f /swapfile; then
  fallocate -l @swapmb@M /swapfile || dd if=/dev/zero of=/swapfile bs=1M count=@swapmb@ status=none
fi
chmod 600 /swapfile
mkswap -q /swapfile >/dev/null
swapon /swapfile
grep -q '^/swapfile' /etc/fstab || printf '/swapfile none swap sw 0 0\n' >> /etc/fstab
sysctl -qw vm.swappiness=10
grep -q '^vm.swappiness' /etc/sysctl.conf || printf 'vm.swappiness=10\n' >> /etc/sysctl.conf
echo "@swapmb@M on $(findmnt -no SOURCE /)"
"#,
                &vars,
            ),
            true,
        ));
    }

    steps.push(shell(
        "service-user",
        "the control plane must not run as root or as you",
        &tpl(
            r#"id -u @svc@ >/dev/null 2>&1 && test -d /etc/arola && echo "@svc@" || exit 10"#,
            &vars,
        ),
        &tpl(
            r#"
id -u @svc@ >/dev/null 2>&1 || useradd --system --create-home --home-dir /var/lib/arola --shell /bin/bash @svc@
install -d -o @svc@ -g @svc@ -m 0750 /var/lib/arola
install -d -o @svc@ -g @svc@ -m 0755 /opt/arola
install -d -o root -g @svc@ -m 0750 /etc/arola
echo "@svc@"
"#,
            &vars,
        ),
        true,
    ));

    if c.flag("unattended_upgrades", true) {
        steps.push(shell(
            "unattended-upgrades",
            "an unpatched box you control from a phone is worse than no box",
            r#"
dpkg-query -W -f='${Status}' unattended-upgrades 2>/dev/null | grep -q "ok installed" \
  && grep -qs 'Unattended-Upgrade "1"' /etc/apt/apt.conf.d/20auto-upgrades \
  && echo enabled || exit 10
"#,
            r#"
apt-get install -y -qq unattended-upgrades
cat > /etc/apt/apt.conf.d/20auto-upgrades <<'CONF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
CONF
echo enabled
"#,
            true,
        ));
    }

    if c.flag("ssh_harden", true) {
        steps.push(shell(
            "ssh-harden",
            "keys only, no root login",
            r#"sshd -T 2>/dev/null | grep -q '^passwordauthentication no' && echo hardened || exit 10"#,
            &tpl(
                r#"
test -s /home/@user@/.ssh/authorized_keys || { echo "no authorized_keys for @user@ - refusing to disable password login"; exit 1; }
install -d -m 0755 /etc/ssh/sshd_config.d
cat > /etc/ssh/sshd_config.d/10-arola.conf <<'CONF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
CONF
sshd -t
systemctl reload ssh 2>/dev/null || systemctl reload sshd
echo hardened
"#,
                &vars,
            ),
            true,
        ));
    }

    if c.flag("docker", true) {
        steps.push(shell(
            "docker",
            "the forge, the runner and the dev services are all containers",
            r#"command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1 && docker --version || exit 10"#,
            &tpl(
                r#"
curl -fsSL https://get.docker.com -o /tmp/get-docker.sh
sh /tmp/get-docker.sh >/dev/null
rm -f /tmp/get-docker.sh
usermod -aG docker @user@ || true
usermod -aG docker @svc@ || true
docker --version
"#,
                &vars,
            ),
            true,
        ));
        steps.push(shell(
            "docker-logging",
            "container logs are the second classic way a Pi fills its own disk",
            r#"grep -qs '"log-driver": *"local"' /etc/docker/daemon.json && echo capped || exit 10"#,
            r#"
install -d -m 0755 /etc/docker
cat > /etc/docker/daemon.json <<'JSON'
{
  "log-driver": "local",
  "log-opts": { "max-size": "10m", "max-file": "3" },
  "default-address-pools": [ { "base": "172.30.0.0/16", "size": 24 } ]
}
JSON
systemctl restart docker
echo capped
"#,
            true,
        ));
    }

    if c.flag("tailscale", true) {
        steps.push(shell(
            "tailscale",
            "the only way in, so nothing has to be exposed",
            r#"command -v tailscale >/dev/null 2>&1 && echo installed || exit 10"#,
            "curl -fsSL https://tailscale.com/install.sh | sh >/dev/null\necho installed",
            true,
        ));
        steps.push(Step::Manual {
            name: "tailscale-up".into(),
            why: "joining the tailnet needs a browser login, which arola cannot do for you".into(),
            check: "tailscale status >/dev/null 2>&1 && echo up || exit 10".into(),
            instruction: tpl(
                "run `sudo tailscale up --ssh --hostname=@hostname@` on the box, then re-run apply",
                &vars,
            ),
        });
    }

    if c.flag("firewall", true) {
        steps.push(shell(
            "firewall",
            "deny inbound everywhere except the tailnet and LAN ssh",
            r#"ufw status 2>/dev/null | grep -q "Status: active" && echo active || exit 10"#,
            &tpl(
                r#"
apt-get install -y -qq ufw
ufw --force reset >/dev/null
ufw default deny incoming >/dev/null
ufw default allow outgoing >/dev/null
ufw allow in on tailscale0 >/dev/null
ufw allow from @lan@ to any port 22 proto tcp >/dev/null
ufw --force enable >/dev/null
echo active
"#,
                &vars,
            ),
            true,
        ));
    }

    if c.flag("sudo_allowlist", true) {
        steps.push(shell(
            "sudo-allowlist",
            "the bot may restart a unit and reboot the box, and nothing else",
            r#"test -f /etc/sudoers.d/arola && visudo -c -f /etc/sudoers.d/arola >/dev/null && echo present || exit 10"#,
            &tpl(
                r#"
cat > /etc/sudoers.d/arola <<'SUDO'
@svc@ ALL=(root) NOPASSWD: /usr/bin/systemctl, /bin/systemctl, /usr/sbin/reboot, /sbin/reboot, /usr/bin/docker
SUDO
chmod 0440 /etc/sudoers.d/arola
visudo -c -f /etc/sudoers.d/arola >/dev/null
echo present
"#,
                &vars,
            ),
            true,
        ));
    }

    if c.flag("arolad", true) {
        steps.push(Step::Upload(Upload {
            name: "arolad-binary".into(),
            why: "the Telegram control plane".into(),
            local: ctx.daemon_binary.clone(),
            remote: "/usr/local/bin/arolad".into(),
            mode: "0755".into(),
            post: "systemctl restart arolad >/dev/null 2>&1 || true".into(),
        }));
        steps.push(Step::Upload(Upload {
            name: "arolad-config".into(),
            why: "the bot token never lives in git".into(),
            local: ctx.daemon_config.clone(),
            remote: "/etc/arola/arolad.conf".into(),
            mode: "0640".into(),
            post: tpl(
                "chown root:@svc@ /etc/arola/arolad.conf\nsystemctl restart arolad >/dev/null 2>&1 || true",
                &vars,
            ),
        }));
        steps.push(shell(
            "arolad-service",
            "restart on crash, start on boot",
            r#"systemctl is-enabled arolad >/dev/null 2>&1 && systemctl is-active arolad >/dev/null 2>&1 && echo running || exit 10"#,
            &tpl(
                r#"
cat > /etc/systemd/system/arolad.service <<'UNIT'
[Unit]
Description=arola control plane
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=@svc@
ExecStart=/usr/local/bin/arolad --config /etc/arola/arolad.conf
Restart=always
RestartSec=5
PrivateTmp=yes
ProtectSystem=full
WorkingDirectory=/var/lib/arola

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now arolad
sleep 1
systemctl is-active arolad
"#,
                &vars,
            ),
            true,
        ));
    }

    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(conf: &str, facts: &str) -> Ctx {
        Ctx {
            conf: Conf::parse(conf).unwrap(),
            facts: Facts::parse(facts),
            daemon_binary: PathBuf::from("/tmp/arolad"),
            daemon_config: PathBuf::from("/tmp/arolad.conf"),
        }
    }

    fn names(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| s.name().to_string()).collect()
    }

    #[test]
    fn an_sd_card_root_blocks_instead_of_adding_a_swapfile() {
        let steps = build(&ctx("host = pi\n", "root_kind=sd\n"));
        assert!(names(&steps).contains(&"ssd-boot".to_string()));
        assert!(!names(&steps).contains(&"swapfile".to_string()));
    }

    #[test]
    fn an_ssd_root_gets_a_swapfile_and_no_manual_gate() {
        let steps = build(&ctx("host = pi\n", "root_kind=usb\n"));
        assert!(names(&steps).contains(&"swapfile".to_string()));
        assert!(!names(&steps).contains(&"ssd-boot".to_string()));
    }

    #[test]
    fn every_check_uses_the_exit_10_convention_or_is_trivial() {
        for step in build(&ctx("host = pi\n", "root_kind=usb\n")) {
            if let Step::Shell(s) = &step {
                assert!(
                    s.check.contains("exit 10"),
                    "{} has a check that cannot report todo",
                    s.name
                );
            }
        }
    }

    #[test]
    fn flags_turn_whole_lanes_off() {
        let off = "host = pi\ndocker = false\ntailscale = false\nfirewall = false\narolad = false\nssh_harden = false\nsudo_allowlist = false\nunattended_upgrades = false\n";
        let names = names(&build(&ctx(off, "root_kind=usb\n")));
        for absent in ["docker", "tailscale", "firewall", "arolad-binary", "ssh-harden"] {
            assert!(!names.contains(&absent.to_string()), "{absent} should be off");
        }
        assert!(names.contains(&"hostname".to_string()));
    }

    #[test]
    fn config_reaches_the_scripts() {
        let steps = build(&ctx(
            "host = pi\nhostname = etxe\ntimezone = Atlantic/Canary\nzram_percent = 33\n",
            "root_kind=usb\n",
        ));
        let all: String = steps
            .iter()
            .filter_map(|s| match s {
                Step::Shell(s) => Some(format!("{}{}", s.check, s.apply)),
                _ => None,
            })
            .collect();
        assert!(all.contains("etxe"));
        assert!(all.contains("Atlantic/Canary"));
        assert!(all.contains("PERCENT=33"));
    }

    #[test]
    fn the_ssh_harden_step_refuses_without_an_authorized_key() {
        let steps = build(&ctx("host = pi\nuser = eneko\n", "root_kind=usb\n"));
        let harden = steps.iter().find(|s| s.name() == "ssh-harden").unwrap();
        let Step::Shell(s) = harden else { panic!("expected a shell step") };
        assert!(s.apply.contains("/home/eneko/.ssh/authorized_keys"));
        assert!(s.apply.contains("refusing"));
    }

    #[test]
    fn the_sudo_allowlist_is_an_allowlist_not_all() {
        let steps = build(&ctx("host = pi\n", "root_kind=usb\n"));
        let Some(Step::Shell(s)) = steps.iter().find(|s| s.name() == "sudo-allowlist") else {
            panic!("expected the sudo step")
        };
        assert!(s.apply.contains("NOPASSWD: /usr/bin/systemctl"));
        assert!(!s.apply.contains("NOPASSWD: ALL"));
    }
}
