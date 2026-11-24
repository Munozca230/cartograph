//! Contracts: what both sides of a link expect of the data they exchange.
//!
//! - **HTTP bodies** ([`HttpBody`]): for each HTTP bridge whose route takes a body
//!   (Spring `@RequestBody`) or whose client sends a typed one, the client's type and the
//!   server's, each with its fields when they can be read from the source ([`BodyType`]).
//!   A shape that cannot be read whole (a supertype outside the repo, an index signature,
//!   a union…) has no fields and is never compared.
//! - **MCP tools** ([`McpTool`]): the tools a server declares in its source, with the
//!   parameters of a schema written as a literal, so a diff can tell a tool or a parameter
//!   removed, or a parameter newly required.
//!
//! Everything here belongs to the symbol layer and is cleared with it. Only types that
//! take part in a contract are stored.

use serde::{Deserialize, Serialize};

/// A field of a body type as JSON sees it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContractField {
    /// The JSON name (`@JsonProperty` applied).
    pub name: String,
    /// Other names the server accepts (`@JsonAlias`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Server: validation rejects a body without it (`@NotNull`, `@NotBlank`, `@NotEmpty`
    /// on a field of a `@Valid` body).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// Client: declared optional (`name?: T`, a `Partial<…>`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

impl ContractField {
    /// The field answers to `name` (its own or an alias).
    pub fn answers(&self, name: &str) -> bool {
        self.name == name || self.aliases.iter().any(|a| a == name)
    }
}

/// The type of a body on one side of a bridge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BodyType {
    /// The type as written where the body is passed (`CreateUserDto`, `List<ItemDto>`,
    /// `Partial<User>`).
    pub written: String,
    /// The type it names, when it is one type of the repo: its name, file and line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default)]
    pub line: u32,
    /// A list of it (`List<T>`, `T[]`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub array: bool,
    /// Its fields, sorted by name; `None` when the shape is not known whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<ContractField>>,
}

impl BodyType {
    /// What the type is called for people: its name, else as written.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.written)
    }
}

/// The bodies of one HTTP bridge (see [`crate::Bridge`]): the client's and the route's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HttpBody {
    pub src_file: String,
    pub src_line: u32,
    pub dst_file: String,
    pub dst_line: u32,
    /// The route (as the bridge's key: `POST /api/users`).
    pub key: String,
    /// What the client sends; `None` when its type is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<BodyType>,
    /// What the route reads; `None` when it declares no typed body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<BodyType>,
}

/// How two body types compare, field by field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Server fields the client type does not declare.
    pub server_only: Vec<ContractField>,
    /// Client fields the server type does not read.
    pub client_only: Vec<ContractField>,
}

impl HttpBody {
    /// Both shapes are known whole and alike in being lists or not: what each side has
    /// that the other lacks. `None` when they cannot be compared.
    pub fn compare(&self) -> Option<Comparison> {
        let (c, s) = (self.client.as_ref()?, self.server.as_ref()?);
        if c.array != s.array {
            return None;
        }
        let (cf, sf) = (c.fields.as_ref()?, s.fields.as_ref()?);
        Some(Comparison {
            server_only: sf
                .iter()
                .filter(|f| !cf.iter().any(|x| f.answers(&x.name)))
                .cloned()
                .collect(),
            client_only: cf
                .iter()
                .filter(|x| !sf.iter().any(|f| f.answers(&x.name)))
                .cloned()
                .collect(),
        })
    }
}

/// A parameter of an MCP tool.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ToolParam {
    pub name: String,
    /// `Some(true)`: the schema requires it; `Some(false)`: optional; `None`: not known
    /// (a schema built by a helper).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// An MCP tool a server declares in its source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub file: String,
    pub line: u32,
    /// Its parameters, sorted by name; `None` when the schema is not a literal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<ToolParam>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str) -> ContractField {
        ContractField {
            name: name.into(),
            aliases: Vec::new(),
            required: false,
            optional: false,
        }
    }

    fn body(fields: Option<Vec<ContractField>>, array: bool) -> BodyType {
        BodyType {
            written: "T".into(),
            name: None,
            file: None,
            line: 0,
            array,
            fields,
        }
    }

    #[test]
    fn only_whole_shapes_alike_in_lists_compare() {
        let mut alias = field("fullName");
        alias.aliases = vec!["name".into()];
        let b = HttpBody {
            src_file: "a.ts".into(),
            src_line: 1,
            dst_file: "A.java".into(),
            dst_line: 2,
            key: "POST /x".into(),
            client: Some(body(Some(vec![field("name"), field("extra")]), false)),
            server: Some(body(Some(vec![alias, field("age")]), false)),
        };
        let c = b.compare().unwrap();
        assert_eq!(c.server_only, vec![field("age")]);
        assert_eq!(c.client_only, vec![field("extra")]);
        let mut list = b.clone();
        list.server.as_mut().unwrap().array = true;
        assert_eq!(list.compare(), None);
        let mut unknown = b;
        unknown.client.as_mut().unwrap().fields = None;
        assert_eq!(unknown.compare(), None);
    }
}
