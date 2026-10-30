//! A reader for the XML property lists `diskutil -plist` prints: dict,
//! array, string, integer, real, true, false, data and date. Small enough
//! that no crate is worth adding for it.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Dict(Vec<(String, Value)>),
    Array(Vec<Value>),
    String(String),
    Integer(i128),
    Bool(bool),
    /// `real`, `data` and `date`: kept as text, not used.
    Other(String),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_array(&self) -> &[Value] {
        match self {
            Value::Array(items) => items,
            _ => &[],
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Integer(n) => u64::try_from(*n).ok(),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("not a property list: {0}")]
pub struct PlistError(String);

/// Far deeper than any `diskutil` output: a deeper list is refused, not a stack overflow.
const MAX_DEPTH: usize = 64;

/// The top-level value of an XML property list.
pub fn parse(text: &str) -> Result<Value, PlistError> {
    let mut p = Parser { s: text, at: 0, depth: 0 };
    loop {
        p.skip_space();
        let rest = p.rest();
        if rest.starts_with("<?") || rest.starts_with("<!") {
            p.skip_past(">")?;
        } else if rest.starts_with("<plist") {
            p.skip_past(">")?;
            break;
        } else {
            return Err(p.error("expected <plist>"));
        }
    }
    p.value()
}

struct Parser<'a> {
    s: &'a str,
    at: usize,
    /// Dicts and arrays open around the current value.
    depth: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.at..]
    }

    fn error(&self, what: &str) -> PlistError {
        PlistError(format!("{what} at byte {}", self.at))
    }

    fn skip_space(&mut self) {
        let rest = self.rest();
        self.at += rest.len() - rest.trim_start().len();
    }

    fn skip_past(&mut self, end: &str) -> Result<(), PlistError> {
        match self.rest().find(end) {
            Some(i) => {
                self.at += i + end.len();
                Ok(())
            }
            None => Err(self.error(&format!("expected {end}"))),
        }
    }

    /// The next tag's name, consumed with its `>`; `/>` makes it empty.
    fn open_tag(&mut self) -> Result<(&'a str, bool), PlistError> {
        self.skip_space();
        let rest = self.rest();
        if !rest.starts_with('<') || rest.starts_with("</") {
            return Err(self.error("expected a tag"));
        }
        let end = rest.find('>').ok_or_else(|| self.error("unclosed tag"))?;
        let inner = &rest[1..end];
        self.at += end + 1;
        let empty = inner.ends_with('/');
        let name = inner.trim_end_matches('/').split_whitespace().next().unwrap_or("");
        Ok((name, empty))
    }

    /// Text up to `</name>`, consumed with the closing tag.
    fn text(&mut self, name: &str) -> Result<String, PlistError> {
        let close = format!("</{name}>");
        let rest = self.rest();
        let end = rest.find(&close).ok_or_else(|| self.error(&format!("expected {close}")))?;
        let text = unescape(&rest[..end]);
        self.at += end + close.len();
        Ok(text)
    }

    fn closes(&mut self, name: &str) -> bool {
        self.skip_space();
        let close = format!("</{name}>");
        if self.rest().starts_with(&close) {
            self.at += close.len();
            true
        } else {
            false
        }
    }

    fn descend(&mut self) -> Result<(), PlistError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH { Err(self.error("nested too deep")) } else { Ok(()) }
    }

    fn value(&mut self) -> Result<Value, PlistError> {
        let (name, empty) = self.open_tag()?;
        Ok(match (name, empty) {
            ("dict", true) => Value::Dict(Vec::new()),
            ("array", true) => Value::Array(Vec::new()),
            ("string", true) => Value::String(String::new()),
            ("true", _) => Value::Bool(true),
            ("false", _) => Value::Bool(false),
            ("dict", false) => {
                self.descend()?;
                let mut entries = Vec::new();
                while !self.closes("dict") {
                    let (key, _) = self.open_tag()?;
                    if key != "key" {
                        return Err(self.error("expected <key>"));
                    }
                    let key = self.text("key")?;
                    entries.push((key, self.value()?));
                }
                self.depth -= 1;
                Value::Dict(entries)
            }
            ("array", false) => {
                self.descend()?;
                let mut items = Vec::new();
                while !self.closes("array") {
                    items.push(self.value()?);
                }
                self.depth -= 1;
                Value::Array(items)
            }
            ("string", false) => Value::String(self.text("string")?),
            ("integer", false) => {
                let text = self.text("integer")?;
                Value::Integer(text.trim().parse().map_err(|_| self.error("bad integer"))?)
            }
            (other @ ("real" | "data" | "date"), false) => Value::Other(self.text(other)?),
            (other, _) => return Err(self.error(&format!("unexpected <{other}>"))),
        })
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_values_and_entities() {
        let v = parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Name</key>
	<string>Tom &amp; Jerry&#x27;s</string>
	<key>List</key>
	<array>
		<integer>-3</integer>
		<true/>
		<dict/>
		<string></string>
		<real>1.5</real>
	</array>
	<key>Empty</key>
	<array/>
</dict>
</plist>
"#,
        )
        .unwrap();
        assert_eq!(v.get("Name").and_then(Value::as_str), Some("Tom & Jerry's"));
        let list = v.get("List").unwrap().as_array();
        assert_eq!(list[0], Value::Integer(-3));
        assert_eq!(list[1], Value::Bool(true));
        assert_eq!(list[2], Value::Dict(Vec::new()));
        assert_eq!(list[3], Value::String(String::new()));
        assert!(v.get("Empty").unwrap().as_array().is_empty());
    }

    #[test]
    fn refuses_what_is_not_a_plist() {
        assert!(parse("Error: diskutil failed").is_err());
        assert!(parse("<plist><dict><key>A</key>").is_err());
    }

    #[test]
    fn reads_nesting_up_to_the_limit_and_refuses_deeper() {
        let arrays = |n: usize| format!("<plist>{}{}</plist>", "<array>".repeat(n), "</array>".repeat(n));
        let dicts =
            |n: usize| format!("<plist>{}<true/>{}</plist>", "<dict><key>k</key>".repeat(n), "</dict>".repeat(n));
        for nested in [arrays as fn(usize) -> String, dicts] {
            assert!(parse(&nested(MAX_DEPTH)).is_ok());
            assert!(parse(&nested(MAX_DEPTH + 1)).is_err());
        }
    }
}
