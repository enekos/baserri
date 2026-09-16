use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(BTreeMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(m) => m.get(key),
            _ => None,
        }
    }

    pub fn at(&self, index: usize) -> Option<&Value> {
        match self {
            Value::Arr(a) => a.get(index),
            _ => None,
        }
    }

    pub fn path(&self, dotted: &str) -> Option<&Value> {
        let mut cur = self;
        for part in dotted.split('.') {
            cur = match part.parse::<usize>() {
                Ok(i) => cur.at(i)?,
                Err(_) => cur.get(part)?,
            };
        }
        Some(cur)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Num(n) => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn str_at(&self, dotted: &str) -> Option<&str> {
        self.path(dotted).and_then(Value::as_str)
    }

    pub fn i64_at(&self, dotted: &str) -> Option<i64> {
        self.path(dotted).and_then(Value::as_i64)
    }
}

pub fn parse(text: &str) -> Result<Value, String> {
    let b = text.as_bytes();
    let mut i = 0;
    let v = parse_value(b, &mut i)?;
    skip_ws(b, &mut i);
    if i != b.len() {
        return Err(format!("trailing input at byte {i}"));
    }
    Ok(v)
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn parse_value(b: &[u8], i: &mut usize) -> Result<Value, String> {
    skip_ws(b, i);
    match b.get(*i) {
        None => Err("unexpected end of input".into()),
        Some(b'{') => parse_obj(b, i),
        Some(b'[') => parse_arr(b, i),
        Some(b'"') => parse_str(b, i).map(Value::Str),
        Some(b't') => lit(b, i, "true", Value::Bool(true)),
        Some(b'f') => lit(b, i, "false", Value::Bool(false)),
        Some(b'n') => lit(b, i, "null", Value::Null),
        Some(_) => parse_num(b, i),
    }
}

fn lit(b: &[u8], i: &mut usize, word: &str, v: Value) -> Result<Value, String> {
    if b[*i..].starts_with(word.as_bytes()) {
        *i += word.len();
        Ok(v)
    } else {
        Err(format!("invalid literal at byte {i}"))
    }
}

fn parse_num(b: &[u8], i: &mut usize) -> Result<Value, String> {
    let start = *i;
    while *i < b.len() && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
        *i += 1;
    }
    std::str::from_utf8(&b[start..*i])
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .map(Value::Num)
        .ok_or_else(|| format!("invalid number at byte {start}"))
}

fn parse_str(b: &[u8], i: &mut usize) -> Result<String, String> {
    *i += 1;
    let mut s = String::new();
    loop {
        let c = *b.get(*i).ok_or("unterminated string")?;
        *i += 1;
        match c {
            b'"' => return Ok(s),
            b'\\' => {
                let e = *b.get(*i).ok_or("unterminated escape")?;
                *i += 1;
                match e {
                    b'"' => s.push('"'),
                    b'\\' => s.push('\\'),
                    b'/' => s.push('/'),
                    b'b' => s.push('\u{8}'),
                    b'f' => s.push('\u{c}'),
                    b'n' => s.push('\n'),
                    b'r' => s.push('\r'),
                    b't' => s.push('\t'),
                    b'u' => {
                        let hi = hex4(b, i)?;
                        let ch = if (0xD800..0xDC00).contains(&hi) {
                            if b.get(*i) != Some(&b'\\') || b.get(*i + 1) != Some(&b'u') {
                                return Err("lone high surrogate".into());
                            }
                            *i += 2;
                            let lo = hex4(b, i)?;
                            if !(0xDC00..0xE000).contains(&lo) {
                                return Err("invalid low surrogate".into());
                            }
                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                        } else {
                            hi
                        };
                        s.push(char::from_u32(ch).ok_or("invalid code point")?);
                    }
                    _ => return Err("unknown escape".into()),
                }
            }
            _ => {
                let start = *i - 1;
                let len = utf8_len(c);
                let end = start + len;
                if end > b.len() {
                    return Err("truncated utf-8".into());
                }
                s.push_str(std::str::from_utf8(&b[start..end]).map_err(|_| "invalid utf-8")?);
                *i = end;
            }
        }
    }
}

fn utf8_len(c: u8) -> usize {
    match c {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn hex4(b: &[u8], i: &mut usize) -> Result<u32, String> {
    let s = b.get(*i..*i + 4).ok_or("truncated \\u escape")?;
    *i += 4;
    u32::from_str_radix(std::str::from_utf8(s).map_err(|_| "bad \\u escape")?, 16)
        .map_err(|_| "bad \\u escape".to_string())
}

fn parse_arr(b: &[u8], i: &mut usize) -> Result<Value, String> {
    *i += 1;
    let mut items = Vec::new();
    skip_ws(b, i);
    if b.get(*i) == Some(&b']') {
        *i += 1;
        return Ok(Value::Arr(items));
    }
    loop {
        items.push(parse_value(b, i)?);
        skip_ws(b, i);
        match b.get(*i) {
            Some(b',') => *i += 1,
            Some(b']') => {
                *i += 1;
                return Ok(Value::Arr(items));
            }
            _ => return Err(format!("expected , or ] at byte {i}")),
        }
    }
}

fn parse_obj(b: &[u8], i: &mut usize) -> Result<Value, String> {
    *i += 1;
    let mut map = BTreeMap::new();
    skip_ws(b, i);
    if b.get(*i) == Some(&b'}') {
        *i += 1;
        return Ok(Value::Obj(map));
    }
    loop {
        skip_ws(b, i);
        if b.get(*i) != Some(&b'"') {
            return Err(format!("expected object key at byte {i}"));
        }
        let key = parse_str(b, i)?;
        skip_ws(b, i);
        if b.get(*i) != Some(&b':') {
            return Err(format!("expected : at byte {i}"));
        }
        *i += 1;
        map.insert(key, parse_value(b, i)?);
        skip_ws(b, i);
        match b.get(*i) {
            Some(b',') => *i += 1,
            Some(b'}') => {
                *i += 1;
                return Ok(Value::Obj(map));
            }
            _ => return Err(format!("expected , or }} at byte {i}")),
        }
    }
}

pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_telegram_shaped_payload() {
        let text = r#"{"ok":true,"result":[{"update_id":8,"message":{"chat":{"id":-100},"text":"/status","from":{"id":42}}}]}"#;
        let v = parse(text).unwrap();
        assert_eq!(v.path("ok").unwrap().as_bool(), Some(true));
        assert_eq!(v.i64_at("result.0.update_id"), Some(8));
        assert_eq!(v.str_at("result.0.message.text"), Some("/status"));
        assert_eq!(v.i64_at("result.0.message.chat.id"), Some(-100));
    }

    #[test]
    fn missing_paths_are_none_not_panics() {
        let v = parse(r#"{"a":{"b":1}}"#).unwrap();
        assert!(v.path("a.c").is_none());
        assert!(v.path("a.b.c").is_none());
        assert!(v.str_at("a.b").is_none());
    }

    #[test]
    fn escapes_and_unicode() {
        let v = parse(r#"{"s":"line\nquote\"slash\\ € 😀"}"#).unwrap();
        assert_eq!(v.str_at("s"), Some("line\nquote\"slash\\ \u{20ac} \u{1f600}"));
    }

    #[test]
    fn multibyte_text_survives_a_round_trip() {
        let original = "ikusmira: ñ, ç, 東京 — ok";
        let text = format!("{{\"t\":{}}}", escape(original));
        assert_eq!(parse(&text).unwrap().str_at("t"), Some(original));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("{").is_err());
        assert!(parse(r#"{"a":1}x"#).is_err());
        assert!(parse(r#"{"a":}"#).is_err());
        assert!(parse(r#"{"unterminated":"x}"#).is_err());
    }

    #[test]
    fn empty_containers() {
        assert_eq!(parse("[]").unwrap(), Value::Arr(vec![]));
        assert_eq!(parse("{}").unwrap(), Value::Obj(BTreeMap::new()));
    }
}
