//! Spring dependency injection and data stores (Java): the beans of an index, the places
//! where Spring injects them, and where code reads or writes a collection or table.
//!
//! Which beans an injection point receives is decided by the queries (from the beans'
//! supertypes, `@Primary` and qualifiers), not stored. Everything here belongs to the
//! symbol layer and is cleared with it.

use crate::Confidence;
use serde::{Deserialize, Serialize};

/// Generic types whose argument is what they hold several of.
pub const COLLECTIONS: &[&str] = &[
    "List",
    "ArrayList",
    "LinkedList",
    "CopyOnWriteArrayList",
    "Set",
    "HashSet",
    "LinkedHashSet",
    "TreeSet",
    "SortedSet",
    "NavigableSet",
    "Collection",
    "Iterable",
    "Queue",
    "Deque",
    "ArrayDeque",
    "Stream",
];

/// Generic types whose second argument is what their values are.
pub const MAPS: &[&str] = &[
    "Map",
    "HashMap",
    "LinkedHashMap",
    "TreeMap",
    "SortedMap",
    "NavigableMap",
    "ConcurrentMap",
    "ConcurrentHashMap",
];

/// A Spring bean: a stereotype class or what a `@Bean` method returns.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SpringBean {
    /// The bean's type (`sym:…`): the annotated class, or the `@Bean` method's return type.
    pub ty: String,
    /// The bean name Spring matches qualifiers with (empty: written as an expression whose
    /// value is not known).
    pub name: String,
    /// Other names of it (`@Bean({"name", "alias"})`).
    pub aliases: Vec<String>,
    /// `Component`, `Service`, `Repository`, `Controller`, `RestController`,
    /// `Configuration` or `Bean`.
    pub stereotype: String,
    pub file: String,
    pub line: u32,
    /// The `@Bean` method that makes it (`sym:…`).
    pub factory: Option<String>,
    /// `@Primary`: the one chosen when several fit a single injection point.
    pub primary: bool,
    /// `@Qualifier` values.
    pub qualifiers: Vec<String>,
}

/// A place where Spring injects beans of a type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SpringInjection {
    /// The class that receives them (`sym:…`).
    pub owner: String,
    /// `field`, `constructor`, `setter`, `method` or `bean` (a `@Bean` method parameter).
    pub how: String,
    /// The field or parameter.
    pub member: String,
    /// What holds the beans (`List`, `Set`, `Map`, `Collection`, `ObjectProvider`,
    /// `Optional`, `[]`…); empty for one bean.
    pub wrapper: String,
    /// The type injected (`sym:…`).
    pub ty: String,
    /// `@Qualifier`, `@Named` or `@Resource(name)`: the name, empty when it is written as
    /// an expression whose value is not known.
    pub qualifier: Option<String>,
    pub file: String,
    pub line: u32,
    /// The calls made on what is injected here: on the field or parameter, on a field the
    /// parameter is assigned to, on the variable of a loop or lambda over it.
    pub uses: Vec<InjectedUse>,
}

/// A call on what an injection point receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InjectedUse {
    pub line: u32,
    /// Reached over every element (a stream, `forEach`, a loop), not over one.
    pub each: bool,
}

impl SpringInjection {
    /// It receives every bean of its type (a collection, a map, an array, a provider whose
    /// elements are read one by one), not one.
    pub fn takes_all(&self) -> bool {
        let w = self.wrapper.as_str();
        w == "[]"
            || COLLECTIONS.contains(&w)
            || MAPS.contains(&w)
            || (w == "ObjectProvider" && self.uses.iter().any(|u| u.each))
    }
}

/// Where code reads or writes a store: a collection (Mongo) or a table (JPA).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DataSink {
    /// `mongo`, `jpa`, `jdbc` or `cassandra`.
    pub store: String,
    /// The collection or table.
    pub name: String,
    /// The entity class it maps (`sym:…`), when known.
    pub entity: Option<String>,
    /// The method that makes the call (`sym:…`), or for a repository or entity the type.
    pub symbol: Option<String>,
    /// How: `getCollection`, `mongoTemplate.save`, `OrderRepository.save`,
    /// `MongoRepository<Order>`, `@Document`.
    pub via: String,
    pub file: String,
    pub line: u32,
    /// `exact` (a literal name), `resolved` (a constant, an entity's annotation),
    /// `heuristic` (the store's default name for a class).
    pub confidence: Confidence,
}

/// A store as people read it: `Mongo`, `JPA`.
pub fn store_label(store: &str) -> &str {
    match store {
        "mongo" => "Mongo",
        "jpa" => "JPA",
        "jdbc" => "JDBC",
        "cassandra" => "Cassandra",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collections_take_every_bean() {
        let mut i = SpringInjection {
            owner: "sym:A.java#A".into(),
            how: "constructor".into(),
            member: "all".into(),
            wrapper: "List".into(),
            ty: "sym:I.java#I".into(),
            qualifier: None,
            file: "A.java".into(),
            line: 3,
            uses: vec![InjectedUse {
                line: 5,
                each: false,
            }],
        };
        assert!(i.takes_all());
        i.wrapper = "ObjectProvider".into();
        assert!(!i.takes_all());
        i.uses[0].each = true;
        assert!(i.takes_all());
        i.wrapper = String::new();
        assert!(!i.takes_all());
        for w in COLLECTIONS.iter().chain(MAPS) {
            i.wrapper = w.to_string();
            assert!(i.takes_all(), "{w}");
        }
        assert_eq!(store_label("mongo"), "Mongo");
    }
}
