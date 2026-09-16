use crate::conf::Conf;
use crate::step::{Shell, Step};

pub const FORGEJO_VERSION: &str = "16.0.4";
pub const FORGEJO_SHA256: &str = "08c46742233dc045af05170fedffb3621021190cd616ccbda88bf8fac7b648d7";
pub const RUNNER_VERSION: &str = "13.1.0";
pub const RUNNER_SHA256: &str = "4a18a9a2aad9619e61aca5d81d2c69cc624b26af8605118bfc4729ff994d12d3";
pub const MAILPIT_VERSION: &str = "1.31.1";
pub const MAILPIT_SHA256: &str = "be6a1f9dcf0ac0d7157ee777eac2b5351352b367bcae3c0a0f4854da60546884";
pub const GARAGE_VERSION: &str = "2.1.0";
pub const GARAGE_SHA256: &str = "65a1bedc9b6d5a2df788b55e81cb0013129326478fd94cf83a844ab278e8055f";

pub fn tpl(body: &str, vars: &[(&str, &str)]) -> String {
    let mut out = body.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("@{k}@"), v);
    }
    out
}

fn shell(name: &str, why: &str, check: &str, apply: &str) -> Step {
    Step::Shell(Shell {
        name: name.to_string(),
        why: why.to_string(),
        check: check.trim().to_string(),
        apply: apply.trim().to_string(),
        root: true,
    })
}

pub fn fetch(url: &str, sha: &str, artifact: &str) -> String {
    tpl(
        r#"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "@url@" -o "$tmp/@artifact@"
echo "@sha@  $tmp/@artifact@" | sha256sum -c - >/dev/null
"#,
        &[("url", url), ("sha", sha), ("artifact", artifact)],
    )
}

pub fn build(c: &Conf) -> Vec<Step> {
    let mut steps = Vec::new();
    if c.flag("forge", true) {
        steps.extend(forge(c));
    }
    if c.flag("postgres", true) {
        steps.push(postgres(c));
    }
    if c.flag("valkey", true) {
        steps.push(valkey(c));
    }
    if c.flag("mailpit", true) {
        steps.push(mailpit(c));
    }
    if c.flag("garage", true) {
        steps.extend(garage(c));
    }
    steps
}

fn forge(c: &Conf) -> Vec<Step> {
    let version = c.or("forgejo_version", FORGEJO_VERSION).to_string();
    let sha = c.or("forgejo_sha256", FORGEJO_SHA256).to_string();
    let rversion = c.or("runner_version", RUNNER_VERSION).to_string();
    let rsha = c.or("runner_sha256", RUNNER_SHA256).to_string();
    let hostname = c.or("hostname", "baserri").to_string();
    let url = c.or("forge_url", "http://localhost:3000/").to_string();
    let port = c.num("forge_port", 3000).to_string();
    let binary_url = format!(
        "https://codeberg.org/forgejo/forgejo/releases/download/v{version}/forgejo-{version}-linux-arm64"
    );
    let runner_url = format!(
        "https://code.forgejo.org/forgejo/runner/releases/download/v{rversion}/forgejo-runner-{rversion}-linux-arm64"
    );
    let vars: Vec<(&str, &str)> = vec![
        ("ver", &version),
        ("rver", &rversion),
        ("host", &hostname),
        ("url", &url),
        ("port", &port),
    ];

    vec![
        shell(
            "forgejo",
            "the canonical forge — one Go binary, SQLite, no container runtime",
            &tpl(
                r#"
test -x /usr/local/bin/forgejo || exit 10
/usr/local/bin/forgejo --version 2>/dev/null | grep -q "@ver@" || { echo "installed version is not @ver@"; exit 10; }
systemctl is-active --quiet forgejo || { echo "installed but not running"; exit 10; }
echo "@ver@ on :@port@"
"#,
                &vars,
            ),
            &format!(
                "{}\n{}",
                fetch(&binary_url, &sha, "forgejo"),
                tpl(
                    r#"
id -u git >/dev/null 2>&1 || adduser --system --shell /bin/bash --gecos 'Forgejo' --group --disabled-password --home /home/git git
install -d -o git -g git -m 0750 /var/lib/forgejo /var/lib/forgejo/custom /var/lib/forgejo/data /var/lib/forgejo/log
install -d -o root -g git -m 0770 /etc/forgejo
install -m 0755 "$tmp/forgejo" /usr/local/bin/forgejo
if ! test -f /etc/forgejo/app.ini; then
cat > /etc/forgejo/app.ini <<'INI'
APP_NAME = @host@
RUN_USER = git
RUN_MODE = prod
WORK_PATH = /var/lib/forgejo

[server]
PROTOCOL = http
HTTP_ADDR = 127.0.0.1
HTTP_PORT = @port@
ROOT_URL = @url@
DOMAIN = @host@
DISABLE_SSH = false
START_SSH_SERVER = false
LFS_START_SERVER = true
OFFLINE_MODE = true

[database]
DB_TYPE = sqlite3
PATH = /var/lib/forgejo/data/forgejo.db
SQLITE_JOURNAL_MODE = WAL

[repository]
ROOT = /var/lib/forgejo/data/forgejo-repositories

[security]
INSTALL_LOCK = true

[service]
DISABLE_REGISTRATION = true
REQUIRE_SIGNIN_VIEW = true

[actions]
ENABLED = true

[cron.update_checker]
ENABLED = false

[log]
ROOT_PATH = /var/lib/forgejo/log
MODE = console
LEVEL = info
INI
chown root:git /etc/forgejo/app.ini
chmod 0660 /etc/forgejo/app.ini
fi
cat > /etc/systemd/system/forgejo.service <<'UNIT'
[Unit]
Description=Forgejo
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=git
Group=git
WorkingDirectory=/var/lib/forgejo
ExecStart=/usr/local/bin/forgejo web --config /etc/forgejo/app.ini
Restart=always
RestartSec=5
Environment=USER=git HOME=/home/git GITEA_WORK_DIR=/var/lib/forgejo
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectHome=no

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now forgejo
sleep 2
systemctl is-active forgejo
"#,
                    &vars,
                )
            ),
        ),
        Step::Manual {
            name: "forgejo-admin".into(),
            why: "the first account needs a password only you should type".into(),
            check: r#"
test -f /var/lib/forgejo/data/forgejo.db || exit 10
n=$(sudo -u git /usr/local/bin/forgejo admin user list --config /etc/forgejo/app.ini 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
[ "${n:-0}" -gt 0 ] && echo "$n user(s)" || exit 10
"#
            .into(),
            instruction:
                "sudo -u git /usr/local/bin/forgejo admin user create --admin --username eneko --email you@example.com --config /etc/forgejo/app.ini"
                    .into(),
        },
        shell(
            "forgejo-runner",
            "CI in host mode — no images at all, which is why the arm64 image problem disappears",
            &tpl(
                r#"
test -x /usr/local/bin/forgejo-runner || exit 10
/usr/local/bin/forgejo-runner --version 2>/dev/null | grep -q "@rver@" || { echo "installed version is not @rver@"; exit 10; }
test -f /var/lib/forgejo-runner/.runner || { echo "binary present, not registered"; exit 10; }
systemctl is-active --quiet forgejo-runner || { echo "registered but not running"; exit 10; }
echo "@rver@ host-mode"
"#,
                &vars,
            ),
            &format!(
                "{}\n{}",
                fetch(&runner_url, &rsha, "forgejo-runner"),
                r#"
id -u forgejo-runner >/dev/null 2>&1 || useradd --system --create-home --home-dir /var/lib/forgejo-runner --shell /bin/bash forgejo-runner
install -d -o forgejo-runner -g forgejo-runner -m 0750 /var/lib/forgejo-runner
install -m 0755 "$tmp/forgejo-runner" /usr/local/bin/forgejo-runner
if ! test -f /var/lib/forgejo-runner/config.yml; then
cat > /var/lib/forgejo-runner/config.yml <<'YML'
log:
  level: info
runner:
  capacity: 2
  timeout: 1h
  labels:
    - "native:host"
cache:
  enabled: true
  dir: /var/lib/forgejo-runner/cache
container:
  network: host
host:
  workdir_parent: /var/lib/forgejo-runner/workspace
YML
chown forgejo-runner:forgejo-runner /var/lib/forgejo-runner/config.yml
fi
cat > /etc/systemd/system/forgejo-runner.service <<'UNIT'
[Unit]
Description=Forgejo Actions runner (host mode)
After=network-online.target forgejo.service
Wants=network-online.target

[Service]
Type=simple
User=forgejo-runner
WorkingDirectory=/var/lib/forgejo-runner
ExecStart=/usr/local/bin/forgejo-runner daemon --config /var/lib/forgejo-runner/config.yml
Restart=always
RestartSec=5
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/forgejo-runner
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LockPersonality=yes
MemoryMax=1200M

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
test -f /var/lib/forgejo-runner/.runner && systemctl enable --now forgejo-runner || true
echo "binary installed"
"#
            ),
        ),
        Step::Manual {
            name: "runner-register".into(),
            why: "registration needs a token minted in the forge UI".into(),
            check: "test -f /var/lib/forgejo-runner/.runner && echo registered || exit 10".into(),
            instruction: tpl(
                "take a token from @url@admin/actions/runners then: sudo -u forgejo-runner /usr/local/bin/forgejo-runner register --no-interactive --instance @url@ --token <TOKEN> --name @host@ --labels native:host --config /var/lib/forgejo-runner/config.yml  (then re-run apply to start it)",
                &vars,
            ),
        },
    ]
}

fn postgres(c: &Conf) -> Step {
    let db = c.or("pg_db", "dev").to_string();
    let user = c.or("pg_user", "dev").to_string();
    let pass = c.get("pg_password").unwrap_or_default().to_string();
    let vars: Vec<(&str, &str)> = vec![("db", &db), ("dbuser", &user), ("pass", &pass)];
    shell(
        "postgres",
        "one shared database for hobby repos instead of one per repo on the laptop",
        &tpl(
            r#"
command -v psql >/dev/null 2>&1 || exit 10
systemctl is-active --quiet postgresql || { echo "installed but not running"; exit 10; }
sudo -u postgres psql -tAc "select 1 from pg_database where datname='@db@'" 2>/dev/null | grep -q 1 || { echo "no @db@ database"; exit 10; }
echo "$(sudo -u postgres psql -tAc 'show server_version') with @db@"
"#,
            &vars,
        ),
        &tpl(
            r#"
apt-get install -y -qq postgresql
ver=$(ls /etc/postgresql | sort -n | tail -1)
apt-get install -y -qq "postgresql-${ver}-pgvector" 2>/dev/null || echo "pgvector not in apt for ${ver}, skipped"
systemctl enable --now postgresql
sudo -u postgres psql -tAc "select 1 from pg_roles where rolname='@dbuser@'" | grep -q 1 || \
  sudo -u postgres psql -qc "create role \"@dbuser@\" login password '@pass@'"
sudo -u postgres psql -tAc "select 1 from pg_database where datname='@db@'" | grep -q 1 || \
  sudo -u postgres createdb -O "@dbuser@" "@db@"
if [ -n "@pass@" ]; then
  conf="/etc/postgresql/${ver}/main/postgresql.conf"
  grep -q "^listen_addresses = '\*'" "$conf" || { sed -i "/^#*listen_addresses/d" "$conf"; printf "listen_addresses = '*'\n" >> "$conf"; }
  hba="/etc/postgresql/${ver}/main/pg_hba.conf"
  grep -q "100.64.0.0/10" "$hba" || printf 'host all all 100.64.0.0/10 scram-sha-256\n' >> "$hba"
  systemctl restart postgresql
else
  echo "no pg_password set, keeping postgres on localhost only"
fi
echo "postgres ${ver} ready"
"#,
            &vars,
        ),
    )
}

fn valkey(c: &Conf) -> Step {
    let pass = c.get("valkey_password").unwrap_or_default().to_string();
    let vars: Vec<(&str, &str)> = vec![("pass", &pass)];
    shell(
        "valkey",
        "the shared cache, from apt, with redis as the fallback on older Debian",
        r#"
systemctl is-active --quiet valkey-server 2>/dev/null && { echo valkey; exit 0; }
systemctl is-active --quiet redis-server 2>/dev/null && { echo "redis (valkey unavailable)"; exit 0; }
exit 10
"#,
        &tpl(
            r#"
if apt-get install -y -qq valkey-server 2>/dev/null; then
  svc=valkey-server; dir=/etc/valkey; conf=/etc/valkey/valkey.conf
else
  apt-get install -y -qq redis-server
  svc=redis-server; dir=/etc/redis; conf=/etc/redis/redis.conf
fi
if [ -n "@pass@" ]; then
  sed -i "/^#*requirepass/d;/^#*protected-mode/d;/^bind /d" "$conf"
  printf 'requirepass @pass@\nprotected-mode no\nbind 0.0.0.0\n' >> "$conf"
else
  echo "no valkey_password set, keeping it on localhost only"
fi
systemctl enable --now "$svc"
systemctl restart "$svc"
echo "$svc ready"
"#,
            &vars,
        ),
    )
}

fn mailpit(c: &Conf) -> Step {
    let version = c.or("mailpit_version", MAILPIT_VERSION).to_string();
    let sha = c.or("mailpit_sha256", MAILPIT_SHA256).to_string();
    let url = format!(
        "https://github.com/axllent/mailpit/releases/download/v{version}/mailpit-linux-arm64.tar.gz"
    );
    let vars: Vec<(&str, &str)> = vec![("ver", &version)];
    shell(
        "mailpit",
        "every app on the box gets an SMTP sink instead of a real mail provider",
        &tpl(
            r#"
test -x /usr/local/bin/mailpit || exit 10
/usr/local/bin/mailpit version 2>/dev/null | grep -q "@ver@" || { echo "installed version is not @ver@"; exit 10; }
systemctl is-active --quiet mailpit || { echo "installed but not running"; exit 10; }
echo "@ver@ on :1025 smtp, :8025 ui"
"#,
            &vars,
        ),
        &format!(
            "{}\n{}",
            fetch(&url, &sha, "mailpit.tar.gz"),
            r#"
tar -xzf "$tmp/mailpit.tar.gz" -C "$tmp" mailpit
id -u mailpit >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin mailpit
install -d -o mailpit -g mailpit -m 0750 /var/lib/mailpit
install -m 0755 "$tmp/mailpit" /usr/local/bin/mailpit
cat > /etc/systemd/system/mailpit.service <<'UNIT'
[Unit]
Description=Mailpit
After=network-online.target

[Service]
Type=simple
User=mailpit
ExecStart=/usr/local/bin/mailpit --db-file /var/lib/mailpit/mailpit.db --listen 0.0.0.0:8025 --smtp 0.0.0.0:1025
Restart=always
RestartSec=5
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/mailpit

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now mailpit
sleep 1
systemctl is-active mailpit
"#
        ),
    )
}

fn garage(c: &Conf) -> Vec<Step> {
    let version = c.or("garage_version", GARAGE_VERSION).to_string();
    let sha = c.or("garage_sha256", GARAGE_SHA256).to_string();
    let url = format!(
        "https://garagehq.deuxfleurs.fr/_releases/v{version}/aarch64-unknown-linux-musl/garage"
    );
    let vars: Vec<(&str, &str)> = vec![("ver", &version)];
    vec![
        shell(
            "garage",
            "S3-compatible object storage — MinIO stopped publishing binaries, this is one static Rust binary",
            &tpl(
                r#"
test -x /usr/local/bin/garage || exit 10
/usr/local/bin/garage --version 2>/dev/null | grep -q "@ver@" || { echo "installed version is not @ver@"; exit 10; }
systemctl is-active --quiet garage || { echo "installed but not running"; exit 10; }
echo "@ver@ on :3900"
"#,
                &vars,
            ),
            &format!(
                "{}\n{}",
                fetch(&url, &sha, "garage"),
                r#"
id -u garage >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin garage
install -d -o garage -g garage -m 0750 /var/lib/garage /var/lib/garage/meta /var/lib/garage/data
install -m 0755 "$tmp/garage" /usr/local/bin/garage
if ! test -f /etc/garage.toml; then
secret=$(openssl rand -hex 32)
admin=$(openssl rand -hex 32)
cat > /etc/garage.toml <<TOML
metadata_dir = "/var/lib/garage/meta"
data_dir = "/var/lib/garage/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "[::]:3901"
rpc_public_addr = "127.0.0.1:3901"
rpc_secret = "$secret"

[s3_api]
s3_region = "baserri"
api_bind_addr = "[::]:3900"
root_domain = ".s3.baserri.local"

[admin]
api_bind_addr = "[::]:3903"
admin_token = "$admin"
TOML
chown root:garage /etc/garage.toml
chmod 0640 /etc/garage.toml
fi
cat > /etc/systemd/system/garage.service <<'UNIT'
[Unit]
Description=Garage object storage
After=network-online.target

[Service]
Type=simple
User=garage
ExecStart=/usr/local/bin/garage server
Restart=always
RestartSec=5
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/garage

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now garage
sleep 2
systemctl is-active garage
"#
            ),
        ),
        Step::Manual {
            name: "garage-layout".into(),
            why: "a fresh garage holds no data until its single node is given a capacity".into(),
            check: "garage layout show 2>/dev/null | grep -q 'zone' && echo assigned || exit 10".into(),
            instruction:
                "garage status  (copy the node id), then: garage layout assign -z home -c 50G <NODE_ID> && garage layout apply --version 1"
                    .into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| s.name().to_string()).collect()
    }

    #[test]
    fn the_default_lane_is_the_whole_farmstead() {
        let steps = build(&Conf::default());
        assert_eq!(
            names(&steps),
            vec![
                "forgejo",
                "forgejo-admin",
                "forgejo-runner",
                "runner-register",
                "postgres",
                "valkey",
                "mailpit",
                "garage",
                "garage-layout"
            ]
        );
    }

    #[test]
    fn every_service_can_be_switched_off() {
        let off = "forge = false\npostgres = false\nvalkey = false\nmailpit = false\ngarage = false\n";
        assert!(build(&Conf::parse(off).unwrap()).is_empty());
    }

    #[test]
    fn every_downloaded_binary_is_checksum_verified() {
        for step in build(&Conf::default()) {
            let Step::Shell(s) = &step else { continue };
            if s.apply.contains("curl -fsSL") {
                assert!(
                    s.apply.contains("sha256sum -c -"),
                    "{} downloads without verifying a checksum",
                    s.name
                );
            }
        }
    }

    #[test]
    fn no_step_reaches_for_docker() {
        for step in build(&Conf::default()) {
            let Step::Shell(s) = &step else { continue };
            assert!(!s.apply.contains("docker "), "{} shells out to docker", s.name);
        }
    }

    #[test]
    fn the_runner_asks_for_host_mode_not_an_image() {
        let steps = build(&Conf::default());
        let Some(Step::Shell(s)) = steps.iter().find(|s| s.name() == "forgejo-runner") else {
            panic!("no runner step")
        };
        assert!(s.apply.contains("native:host"));
        assert!(!s.apply.contains("docker://"));
    }

    #[test]
    fn forgejo_binds_to_localhost_so_tailscale_serve_fronts_it() {
        let steps = build(&Conf::default());
        let Some(Step::Shell(s)) = steps.iter().find(|s| s.name() == "forgejo") else {
            panic!("no forgejo step")
        };
        assert!(s.apply.contains("HTTP_ADDR = 127.0.0.1"));
        assert!(s.apply.contains("INSTALL_LOCK = true"));
        assert!(s.apply.contains("DISABLE_REGISTRATION = true"));
    }

    #[test]
    fn versions_and_hashes_are_overridable() {
        let c = Conf::parse("forgejo_version = 17.0.0\nforgejo_sha256 = deadbeef\n").unwrap();
        let steps = build(&c);
        let Some(Step::Shell(s)) = steps.iter().find(|s| s.name() == "forgejo") else { panic!() };
        assert!(s.apply.contains("v17.0.0/forgejo-17.0.0-linux-arm64"));
        assert!(s.apply.contains("deadbeef"));
    }

    #[test]
    fn dev_services_stay_on_localhost_until_a_password_exists() {
        let steps = build(&Conf::default());
        for name in ["postgres", "valkey"] {
            let Some(Step::Shell(s)) = steps.iter().find(|s| s.name() == name) else { panic!() };
            assert!(s.apply.contains("localhost only"), "{name} exposes itself with no password");
        }
    }

    #[test]
    fn the_pinned_hashes_are_real_sha256s() {
        for h in [FORGEJO_SHA256, RUNNER_SHA256, MAILPIT_SHA256, GARAGE_SHA256] {
            assert_eq!(h.len(), 64);
            assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
