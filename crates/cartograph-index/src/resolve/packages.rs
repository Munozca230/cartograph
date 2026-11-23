//! Package → source (bridges of kind `package`): in a workspace, a member that depends on a
//! package another member builds uses that member's source.
//!
//! Coordinates come from the manifests: Maven `pom.xml` (its own `groupId:artifactId`, the
//! groupId and version inherited from `<parent>`, `${…}` properties; every `<dependency>`
//! outside `<dependencyManagement>`, `<build>` and `<profiles>`), Gradle build files
//! (`group`, the project's folder or `rootProject.name`, `group:artifact:version`
//! strings and map notation) and `package.json` (name, version, every kind of
//! dependency). A dependency whose coordinate another member produces is a bridge
//! (`package/maven` or `package/npm`, exact), and makes the consumer's member depend on the
//! producer's: Java imports across members resolve only along those declared dependencies
//! (else they are guesses), TypeScript bare imports of a member's package resolve to its
//! source. A different version than the producer's is a `version_skew`.

use super::Index;
use cartograph_core::{Bridge, BridgeEndpoint, BridgeKind, BridgeRole, Confidence};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Endpoint detail of a dependency on another version than the member builds:
/// `version_skew <the producer's version>`.
pub const VERSION_SKEW: &str = "version_skew";

/// A package coordinate, where a manifest writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coord {
    /// `maven` or `npm`.
    pub eco: &'static str,
    /// `group:artifact` or the npm name.
    pub name: String,
    pub version: Option<String>,
    pub line: u32,
}

impl Coord {
    pub fn key(&self) -> String {
        format!("{}:{}", self.eco, self.name)
    }
}

/// What one manifest builds and uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestCoords {
    pub produces: Option<Coord>,
    pub deps: Vec<Coord>,
}

/// One manifest of a member.
#[derive(Debug, Clone)]
struct Manifest {
    path: String,
    member: String,
    coords: ManifestCoords,
}

/// Which members' code an import may reach (see [`Packages::eligible`]); empty for a single
/// repo, where everything is.
#[derive(Debug, Clone, Default)]
pub struct Eligibility {
    closure: HashMap<String, BTreeSet<String>>,
    known: BTreeSet<String>,
    producers: BTreeSet<String>,
}

impl Eligibility {
    /// See [`Packages::eligible`].
    pub fn eligible(&self, importer: &str, target: &str) -> bool {
        importer == target
            || !self.known.contains(importer)
            || !self.producers.contains(target)
            || self
                .closure
                .get(importer)
                .is_some_and(|c| c.contains(target))
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}

/// The packages of a workspace's members.
#[derive(Debug, Default)]
pub struct Packages {
    manifests: Vec<Manifest>,
    /// Member → the members it depends on through their packages, transitively.
    closure: HashMap<String, BTreeSet<String>>,
    /// Members with a manifest that names a package (their dependencies are known).
    known: BTreeSet<String>,
    /// Members that build a package (someone may declare them).
    producers: BTreeSet<String>,
}

fn line_at(text: &str, offset: usize) -> u32 {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
        + 1
}

/// `text` with every `<tag>…</tag>` block blanked (newlines kept, so offsets hold).
fn blank_blocks(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = text.to_string();
    let mut from = 0;
    while let Some(s) = out[from..].find(&open).map(|i| i + from) {
        let Some(e) = out[s..].find(&close).map(|i| i + s + close.len()) else {
            break;
        };
        let blank: String = out[s..e]
            .chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect();
        out.replace_range(s..e, &blank);
        from = s + blank.len();
    }
    out
}

/// Every `<tag>…</tag>` block: `(offset of the block, its inner text)`.
fn blocks<'t>(text: &'t str, tag: &str) -> Vec<(usize, &'t str)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(s) = text[from..].find(&open).map(|i| i + from) {
        let inner = s + open.len();
        let Some(e) = text[inner..].find(&close).map(|i| i + inner) else {
            break;
        };
        out.push((s, &text[inner..e]));
        from = e + close.len();
    }
    out
}

fn first_tag(text: &str, tag: &str) -> Option<String> {
    blocks(text, tag)
        .first()
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `pom.xml`: what it builds and what it depends on.
pub fn maven(text: &str) -> ManifestCoords {
    let parent = blocks(text, "parent").first().map(|(_, b)| *b);
    let pg = parent.and_then(|b| first_tag(b, "groupId"));
    let pv = parent.and_then(|b| first_tag(b, "version"));
    let mut own = text.to_string();
    for tag in [
        "parent",
        "dependencies",
        "dependencyManagement",
        "build",
        "profiles",
        "plugins",
        "properties",
        "reporting",
    ] {
        own = blank_blocks(&own, tag);
    }
    let group = first_tag(&own, "groupId").or(pg);
    let artifact = first_tag(&own, "artifactId");
    let version = first_tag(&own, "version").or(pv.clone());
    let props: HashMap<String, String> = blocks(text, "properties")
        .iter()
        .flat_map(|(_, b)| simple_elements(b))
        .collect();
    let subst = |v: &str| -> String {
        let mut out = v.to_string();
        for _ in 0..4 {
            let Some(s) = out.find("${") else { break };
            let Some(e) = out[s..].find('}').map(|i| i + s) else {
                break;
            };
            let name = &out[s + 2..e];
            let val = match name {
                "project.groupId" | "pom.groupId" | "groupId" => group.clone(),
                "project.version" | "pom.version" | "version" => version.clone(),
                "project.parent.version" | "parent.version" => pv.clone(),
                "project.artifactId" | "artifactId" => artifact.clone(),
                other => props.get(other).cloned(),
            };
            match val {
                Some(v) if !v.contains("${") => out.replace_range(s..=e, &v),
                _ => break,
            }
        }
        out
    };
    let own_at = own.find("<artifactId>").unwrap_or(0);
    let produces = match (&group, &artifact) {
        (Some(g), Some(a)) => Some(Coord {
            eco: "maven",
            name: format!("{}:{}", subst(g), subst(a)),
            version: version.as_deref().map(subst),
            line: line_at(text, own_at),
        }),
        _ => None,
    };
    let mut deps_text = text.to_string();
    for tag in ["dependencyManagement", "build", "profiles", "reporting"] {
        deps_text = blank_blocks(&deps_text, tag);
    }
    let deps = blocks(&deps_text, "dependency")
        .into_iter()
        .filter_map(|(at, b)| {
            let g = subst(&first_tag(b, "groupId")?);
            let a = subst(&first_tag(b, "artifactId")?);
            Some(Coord {
                eco: "maven",
                name: format!("{g}:{a}"),
                version: first_tag(b, "version").map(|v| subst(&v)),
                line: line_at(text, at),
            })
        })
        .collect();
    ManifestCoords { produces, deps }
}

/// `<name>value</name>` children of a block (no attributes, no nesting).
fn simple_elements(b: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = b;
    while let Some(s) = rest.find('<') {
        let after = &rest[s + 1..];
        let Some(e) = after.find('>') else { break };
        let name = &after[..e];
        if name.starts_with('/') || name.starts_with('!') || name.contains(' ') {
            rest = &after[e + 1..];
            continue;
        }
        let close = format!("</{name}>");
        let body = &after[e + 1..];
        match body.find(&close) {
            Some(c) => {
                out.push((name.to_string(), body[..c].trim().to_string()));
                rest = &body[c + close.len()..];
            }
            None => rest = body,
        }
    }
    out
}

/// Quoted strings of a line, in order.
fn quoted(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find(['\'', '"']) {
        let q = rest.as_bytes()[start] as char;
        let after = &rest[start + 1..];
        let Some(end) = after.find(q) else { break };
        out.push(&after[..end]);
        rest = &after[end + 1..];
    }
    out
}

fn coord_part(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// A Gradle build file of the project named `project`: what it builds and uses.
pub fn gradle(text: &str, project: &str) -> ManifestCoords {
    let mut group = None;
    let mut version = None;
    let mut deps = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with("//") {
            continue;
        }
        let q = quoted(line);
        let assigns = |key: &str| {
            line.strip_prefix(key).is_some_and(|r| {
                let r = r.trim_start();
                r.starts_with('=') || r.starts_with('\'') || r.starts_with('"')
            })
        };
        if assigns("group") && !line.contains("group:") {
            group = q.first().map(|s| s.to_string());
            continue;
        }
        if assigns("version") {
            version = q.first().map(|s| s.to_string());
            continue;
        }
        let at = i as u32 + 1;
        if line.contains("group:") && line.contains("name:") {
            let value = |key: &str| {
                line.split(key)
                    .nth(1)
                    .and_then(|r| quoted(r).first().map(|s| s.to_string()))
            };
            if let (Some(g), Some(n)) = (value("group:"), value("name:")) {
                deps.push(Coord {
                    eco: "maven",
                    name: format!("{g}:{n}"),
                    version: value("version:"),
                    line: at,
                });
            }
            continue;
        }
        for s in q {
            let parts: Vec<&str> = s.split(':').collect();
            if (parts.len() == 2 || parts.len() == 3)
                && coord_part(parts[0])
                && coord_part(parts[1])
                && parts[0].contains('.')
            {
                deps.push(Coord {
                    eco: "maven",
                    name: format!("{}:{}", parts[0], parts[1]),
                    version: parts.get(2).map(|v| v.to_string()),
                    line: at,
                });
            }
        }
    }
    let produces = group.map(|g| Coord {
        eco: "maven",
        name: format!("{g}:{project}"),
        version,
        line: 1,
    });
    ManifestCoords { produces, deps }
}

/// `package.json`: its name and version, and every dependency.
pub fn npm(text: &str) -> ManifestCoords {
    let Ok(v) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
        return ManifestCoords::default();
    };
    let line_of = |needle: &str| {
        text.find(&format!("\"{needle}\""))
            .map_or(1, |at| line_at(text, at))
    };
    let produces = v.get("name").and_then(Value::as_str).map(|n| Coord {
        eco: "npm",
        name: n.to_string(),
        version: v.get("version").and_then(Value::as_str).map(str::to_string),
        line: line_of("name"),
    });
    let mut deps = Vec::new();
    for k in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        for (name, ver) in v.get(k).and_then(Value::as_object).into_iter().flatten() {
            deps.push(Coord {
                eco: "npm",
                name: name.clone(),
                version: ver.as_str().map(str::to_string),
                line: line_of(name),
            });
        }
    }
    ManifestCoords { produces, deps }
}

/// Whether `wanted` (as a dependency writes it) does not take `have` (the producer's
/// version). `false` when it cannot tell (a range, a placeholder, a workspace or file
/// reference).
pub fn skewed(eco: &str, wanted: &str, have: &str) -> bool {
    let (w, h) = (wanted.trim(), have.trim());
    if w.is_empty() || h.is_empty() || w.contains("${") || h.contains("${") {
        return false;
    }
    if eco == "maven" {
        // A version range (`[1.0,2.0)`) is not judged.
        return !(w.starts_with('[') || w.starts_with('(')) && w != h;
    }
    let num = |s: &str| -> Option<Vec<u64>> {
        let core = s.split(['-', '+']).next()?;
        core.split('.').map(|p| p.parse().ok()).collect()
    };
    let (op, rest) = match w.chars().next() {
        Some('^') => ('^', &w[1..]),
        Some('~') => ('~', &w[1..]),
        Some('=') => ('=', &w[1..]),
        Some(c) if c.is_ascii_digit() => ('=', w),
        _ => return false,
    };
    let (Some(a), Some(b)) = (num(rest.trim()), num(h)) else {
        return false;
    };
    let at = |v: &Vec<u64>, i: usize| v.get(i).copied().unwrap_or(0);
    match op {
        '^' => {
            if at(&a, 0) > 0 {
                at(&a, 0) != at(&b, 0)
            } else {
                at(&a, 1) != at(&b, 1) || at(&b, 0) != 0
            }
        }
        '~' => at(&a, 0) != at(&b, 0) || at(&a, 1) != at(&b, 1),
        _ => rest.trim() != h,
    }
}

const BUILD_FILES: [&str; 4] = [
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "package.json",
];

impl Packages {
    /// Read the manifests of every member of `ix` (nothing for a single repo).
    pub fn build(ix: &Index<'_>) -> Packages {
        let members: Vec<&str> = ix
            .mounts
            .iter()
            .map(|(p, _)| *p)
            .filter(|p| !p.is_empty())
            .collect();
        if members.is_empty() {
            return Packages::default();
        }
        let mut paths: Vec<&str> = ix
            .files
            .iter()
            .copied()
            .filter(|f| BUILD_FILES.contains(&f.rsplit('/').next().unwrap_or(f)))
            .filter(|f| !f.split('/').any(|s| s == "node_modules"))
            .collect();
        paths.sort_unstable();
        let mut manifests = Vec::new();
        for p in paths {
            let member = ix.mount_root(p);
            if member.is_empty() {
                continue;
            }
            let Some(text) = ix.read_config(p) else {
                continue;
            };
            let name = p.rsplit('/').next().unwrap_or(p);
            let coords = match name {
                "pom.xml" => maven(&text),
                "package.json" => npm(&text),
                _ => {
                    let dir = p.rsplit_once('/').map_or("", |(d, _)| d);
                    let project = if dir == member {
                        gradle_root_name(ix, dir).unwrap_or_else(|| {
                            ix.mount_of(p)
                                .and_then(|(_, root)| root.file_name())
                                .and_then(|n| n.to_str())
                                .unwrap_or(member)
                                .to_string()
                        })
                    } else {
                        dir.rsplit('/').next().unwrap_or(dir).to_string()
                    };
                    gradle(&text, &project)
                }
            };
            manifests.push(Manifest {
                path: p.to_string(),
                member: member.to_string(),
                coords,
            });
        }
        let mut pk = Packages {
            manifests,
            ..Packages::default()
        };
        let producers = pk.producers_by_key();
        let mut direct: HashMap<String, BTreeSet<String>> = HashMap::new();
        let (mut known, mut builders) = (BTreeSet::new(), BTreeSet::new());
        for m in &pk.manifests {
            if m.coords.produces.is_some() || !m.coords.deps.is_empty() {
                known.insert(m.member.clone());
            }
            if m.coords.produces.is_some() {
                builders.insert(m.member.clone());
            }
            for d in &m.coords.deps {
                for (pm, _) in producers.get(&d.key()).into_iter().flatten() {
                    if *pm != m.member {
                        direct
                            .entry(m.member.clone())
                            .or_default()
                            .insert(pm.to_string());
                    }
                }
            }
        }
        drop(producers);
        pk.known = known;
        pk.producers = builders;
        for m in &members {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut stack: Vec<String> = direct.get(*m).into_iter().flatten().cloned().collect();
            while let Some(x) = stack.pop() {
                if seen.insert(x.clone()) {
                    stack.extend(direct.get(&x).into_iter().flatten().cloned());
                }
            }
            seen.remove(*m);
            pk.closure.insert(m.to_string(), seen);
        }
        pk
    }

    /// Coordinate key → `(member, manifest)` of each producer.
    fn producers_by_key(&self) -> BTreeMap<String, Vec<(&str, &Manifest)>> {
        let mut out: BTreeMap<String, Vec<(&str, &Manifest)>> = BTreeMap::new();
        for m in &self.manifests {
            if let Some(c) = &m.coords.produces {
                out.entry(c.key()).or_default().push((m.member.as_str(), m));
            }
        }
        out
    }

    /// Whether the code of member `target` is fair game for an import written in member
    /// `importer`: the same member, a member it depends on (through its packages,
    /// transitively), or a pair whose packages are unknown (nothing to check against).
    pub fn eligible(&self, importer: &str, target: &str) -> bool {
        self.eligibility().eligible(importer, target)
    }

    /// What the Java resolver needs to tell eligible members apart.
    pub fn eligibility(&self) -> Eligibility {
        Eligibility {
            closure: self.closure.clone(),
            known: self.known.clone(),
            producers: self.producers.clone(),
        }
    }

    /// Whether there is anything to check (a workspace whose members have manifests).
    pub fn active(&self) -> bool {
        !self.known.is_empty()
    }

    /// The endpoints (every package a member builds; every dependency on one) and the
    /// bridges (dependency → the manifest that builds it).
    pub fn rows(&self) -> (Vec<BridgeEndpoint>, Vec<Bridge>) {
        let producers = self.producers_by_key();
        let mut endpoints = Vec::new();
        let mut bridges = Vec::new();
        for m in &self.manifests {
            if let Some(c) = &m.coords.produces {
                endpoints.push(BridgeEndpoint {
                    kind: BridgeKind::Package,
                    role: BridgeRole::Provide,
                    key: c.key(),
                    label: c.version.clone().unwrap_or_default(),
                    file: m.path.clone(),
                    line: c.line,
                    symbol: None,
                    confidence: Confidence::Exact,
                    detail: None,
                });
            }
            for d in &m.coords.deps {
                let key = d.key();
                let Some(ps) = producers.get(&key) else {
                    continue;
                };
                let others: Vec<&(&str, &Manifest)> =
                    ps.iter().filter(|(pm, _)| *pm != m.member).collect();
                if others.is_empty() {
                    continue;
                }
                let skew = others.iter().find_map(|(_, p)| {
                    let have = p.coords.produces.as_ref()?.version.as_deref()?;
                    let wanted = d.version.as_deref()?;
                    skewed(d.eco, wanted, have).then(|| format!("{VERSION_SKEW} {have}"))
                });
                endpoints.push(BridgeEndpoint {
                    kind: BridgeKind::Package,
                    role: BridgeRole::Consume,
                    key: key.clone(),
                    label: d.version.clone().unwrap_or_default(),
                    file: m.path.clone(),
                    line: d.line,
                    symbol: None,
                    confidence: Confidence::Exact,
                    detail: skew,
                });
                for (_, p) in others {
                    bridges.push(Bridge {
                        kind: BridgeKind::Package,
                        subtype: d.eco.to_string(),
                        src_file: m.path.clone(),
                        src_line: d.line,
                        src: None,
                        dst_file: p.path.clone(),
                        dst_line: p.coords.produces.as_ref().map_or(1, |c| c.line),
                        dst: None,
                        key: key.clone(),
                        confidence: Confidence::Exact,
                    });
                }
            }
        }
        (endpoints, bridges)
    }
}

/// `rootProject.name` of the `settings.gradle(.kts)` in `dir`.
fn gradle_root_name(ix: &Index<'_>, dir: &str) -> Option<String> {
    ["settings.gradle", "settings.gradle.kts"]
        .iter()
        .filter_map(|f| ix.read_config(&format!("{dir}/{f}")))
        .find_map(|t| {
            t.lines()
                .find(|l| l.trim_start().starts_with("rootProject.name"))
                .and_then(|l| quoted(l).first().map(|s| s.to_string()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_coordinates_with_parent_properties_and_managed_dependencies() {
        let pom = r#"<project>
  <parent>
    <groupId>com.acme</groupId>
    <artifactId>parent</artifactId>
    <version>3.1.0</version>
  </parent>
  <artifactId>api</artifactId>
  <properties>
    <lib.version>1.2.0</lib.version>
  </properties>
  <dependencyManagement>
    <dependencies>
      <dependency><groupId>org.managed</groupId><artifactId>m</artifactId></dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
    <dependency>
      <groupId>${project.groupId}</groupId>
      <artifactId>lib</artifactId>
      <version>${lib.version}</version>
    </dependency>
    <dependency>
      <groupId>org.x</groupId>
      <artifactId>y</artifactId>
    </dependency>
  </dependencies>
  <build><plugins><plugin><dependencies><dependency><groupId>p</groupId><artifactId>q</artifactId></dependency></dependencies></plugin></plugins></build>
</project>"#;
        let c = maven(pom);
        let p = c.produces.unwrap();
        assert_eq!(
            (p.name.as_str(), p.version.as_deref(), p.line),
            ("com.acme:api", Some("3.1.0"), 7)
        );
        let deps: Vec<(String, Option<String>, u32)> = c
            .deps
            .iter()
            .map(|d| (d.name.clone(), d.version.clone(), d.line))
            .collect();
        assert_eq!(
            deps,
            vec![
                ("com.acme:lib".into(), Some("1.2.0".into()), 17),
                ("org.x:y".into(), None, 22),
            ]
        );
    }

    #[test]
    fn gradle_and_npm_coordinates() {
        let g = gradle(
            "plugins { id 'java' }\ngroup = 'com.acme'\nversion = '2.0'\ndependencies {\n  implementation 'com.acme:lib:1.4.0'\n  implementation group: 'org.a', name: 'b', version: '3'\n  testImplementation \"junit:junit:4.13\"\n}\n",
            "api",
        );
        assert_eq!(g.produces.unwrap().name, "com.acme:api");
        let names: Vec<&str> = g.deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["com.acme:lib", "org.a:b"]);
        let n = npm(r#"{
  "name": "@acme/ui",
  "version": "1.0.0",
  "dependencies": { "@acme/core": "^2.1.0" },
  "devDependencies": { "vitest": "1" }
}"#);
        assert_eq!(n.produces.unwrap().name, "@acme/ui");
        assert_eq!(n.deps[0].name, "@acme/core");
        assert_eq!(n.deps[0].line, 4);
    }

    #[test]
    fn version_skew_by_ecosystem() {
        assert!(skewed("maven", "1.2.0", "1.4.0-SNAPSHOT"));
        assert!(!skewed("maven", "1.4.0-SNAPSHOT", "1.4.0-SNAPSHOT"));
        assert!(!skewed("maven", "[1.0,2.0)", "1.4.0"));
        assert!(!skewed("maven", "${v}", "1.4.0"));
        assert!(!skewed("npm", "^2.1.0", "2.5.0"));
        assert!(skewed("npm", "^2.1.0", "3.0.0"));
        assert!(skewed("npm", "~2.1.0", "2.2.0"));
        assert!(skewed("npm", "2.1.0", "2.1.1"));
        assert!(!skewed("npm", "workspace:*", "2.1.1"));
        assert!(!skewed("npm", "file:../core", "2.1.1"));
        assert!(skewed("npm", "^0.3.0", "0.4.0"));
    }
}
