use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Conf {
    values: BTreeMap<String, String>,
}

impl Conf {
    pub fn parse(text: &str) -> Result<Conf, String> {
        let mut values = BTreeMap::new();
        let mut section = String::new();
        for (n, raw) in text.lines().enumerate() {
            let line = strip_comment(raw);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let name = rest
                    .strip_suffix(']')
                    .ok_or_else(|| format!("line {}: unterminated section header", n + 1))?;
                section = name.trim().to_string();
                continue;
            }
            let eq = line
                .find('=')
                .ok_or_else(|| format!("line {}: expected `key = value`", n + 1))?;
            let key = line[..eq].trim();
            if key.is_empty() {
                return Err(format!("line {}: empty key", n + 1));
            }
            let val = unquote(line[eq + 1..].trim());
            let full = if section.is_empty() {
                key.to_string()
            } else {
                format!("{section}.{key}")
            };
            values.insert(full, val);
        }
        Ok(Conf { values })
    }

    pub fn load(path: &Path) -> Result<Conf, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Conf::parse(&text)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    pub fn req(&self, key: &str) -> Result<&str, String> {
        self.get(key)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("missing required config key `{key}`"))
    }

    pub fn or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).filter(|v| !v.is_empty()).unwrap_or(default)
    }

    pub fn flag(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some("true" | "yes" | "on" | "1") => true,
            Some("false" | "no" | "off" | "0") => false,
            _ => default,
        }
    }

    pub fn num(&self, key: &str, default: u64) -> u64 {
        self.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn section(&self, name: &str) -> Vec<(String, String)> {
        let prefix = format!("{name}.");
        self.values
            .iter()
            .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|k| (k.to_string(), v.clone())))
            .collect()
    }
}

fn unquote(v: &str) -> String {
    let b = v.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        return v[1..v.len() - 1].to_string();
    }
    v.to_string()
}

fn strip_comment(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with(';') {
        return "";
    }
    match line.find(" #") {
        Some(i) => &line[..i],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_qualify_keys() {
        let c = Conf::parse("host = pi.local\n[bot]\ntoken = abc\n").unwrap();
        assert_eq!(c.get("host"), Some("pi.local"));
        assert_eq!(c.get("bot.token"), Some("abc"));
        assert_eq!(c.get("token"), None);
    }

    #[test]
    fn comments_and_quotes() {
        let c = Conf::parse("# lead\na = 1 # trailing\nb = \"x = y\"\nc = 'z'\n").unwrap();
        assert_eq!(c.get("a"), Some("1"));
        assert_eq!(c.get("b"), Some("x = y"));
        assert_eq!(c.get("c"), Some("z"));
    }

    #[test]
    fn a_url_fragment_is_not_a_comment() {
        let c = Conf::parse("u = https://x/y#frag\n").unwrap();
        assert_eq!(c.get("u"), Some("https://x/y#frag"));
    }

    #[test]
    fn lists_flags_numbers() {
        let c = Conf::parse("ids = 1, 2 ,,3\nyes = on\nn = 42\n").unwrap();
        assert_eq!(c.list("ids"), vec!["1", "2", "3"]);
        assert!(c.flag("yes", false));
        assert!(!c.flag("nope", false));
        assert_eq!(c.num("n", 0), 42);
        assert_eq!(c.num("missing", 7), 7);
    }

    #[test]
    fn section_listing() {
        let c = Conf::parse("[jobs]\nbackup = a.sh\nsync = b.sh\n").unwrap();
        let mut got = c.section("jobs");
        got.sort();
        assert_eq!(got, vec![
            ("backup".to_string(), "a.sh".to_string()),
            ("sync".to_string(), "b.sh".to_string()),
        ]);
    }

    #[test]
    fn errors_point_at_the_line() {
        assert!(Conf::parse("ok = 1\nbroken\n").unwrap_err().contains("line 2"));
        assert!(Conf::parse("[unclosed\n").unwrap_err().contains("line 1"));
    }
}
