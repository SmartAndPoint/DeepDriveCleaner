//! A lenient read of `brew info --json=v2 --installed`. brew's JSON has no
//! schema: unknown fields are ignored, and a field of the wrong type reads
//! as absent, so one odd formula never makes the whole list unreadable. A
//! field the graph needs that is absent is said by [`super::Graph::new`].

use serde_json::Value;

/// The formulae and the casks brew lists as installed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed {
    pub formulae: Vec<Formula>,
    pub casks: Vec<Cask>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Formula {
    /// The short name, the Cellar rack's.
    pub name: Option<String>,
    /// `user/tap/foo` for a tap formula, the name itself for a core one.
    pub full_name: Option<String>,
    pub pinned: Option<bool>,
    /// `installed[]`: one entry per keg.
    pub kegs: Option<Vec<Keg>>,
    /// Declared: read for a keg whose tab records no runtime dependencies.
    pub dependencies: Vec<String>,
    pub recommended_dependencies: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keg {
    /// The folder's name under the rack: `Cellar/<name>/<version>`.
    pub version: Option<String>,
    /// The `full_name` of each runtime dependency the keg's tab records
    /// (recursive); `None` for a blank tab, or an entry without a name.
    pub runtime_dependencies: Option<Vec<String>>,
    /// An absent field reads as false, as brew's own JSON prints it for a
    /// keg whose record predates the field.
    pub installed_on_request: bool,
}

/// An installed cask, with the formulae its `depends_on formula:` names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cask {
    pub token: String,
    pub formulae: Vec<String>,
}

/// `None` when the output is not a JSON object.
pub fn parse(stdout: &str) -> Option<Installed> {
    let doc: Value = serde_json::from_str(stdout.trim()).ok()?;
    let doc = doc.as_object()?;
    let list = |key: &str| doc.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    Some(Installed {
        formulae: list("formulae").iter().map(formula).collect(),
        casks: list("casks").iter().map(cask).collect(),
    })
}

fn string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn formula(v: &Value) -> Formula {
    Formula {
        name: string(v, "name"),
        full_name: string(v, "full_name"),
        pinned: v.get("pinned").and_then(Value::as_bool),
        kegs: v.get("installed").and_then(Value::as_array).map(|a| a.iter().map(keg).collect()),
        dependencies: strings(v, "dependencies"),
        recommended_dependencies: strings(v, "recommended_dependencies"),
    }
}

fn keg(v: &Value) -> Keg {
    let runtime_dependencies = v.get("runtime_dependencies").and_then(Value::as_array).and_then(|deps| {
        deps.iter().map(|d| d.get("full_name").and_then(Value::as_str).map(str::to_owned)).collect::<Option<Vec<_>>>()
    });
    Keg {
        version: string(v, "version"),
        runtime_dependencies,
        installed_on_request: v.get("installed_on_request").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// `depends_on.formula` is a list in brew's JSON; a lone string is read too.
/// A cask without a token still counts: what it needs is kept.
fn cask(v: &Value) -> Cask {
    let token =
        string(v, "full_token").or_else(|| string(v, "token")).unwrap_or_else(|| "(a cask without a name)".to_owned());
    let formulae = match v.get("depends_on").and_then(|d| d.get("formula")) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect(),
        _ => Vec::new(),
    };
    Cask { token, formulae }
}
