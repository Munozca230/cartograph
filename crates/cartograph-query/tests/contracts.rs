//! Contracts in `explain`: the block takes only the room the answer leaves, so nothing an
//! answer without it shows (importers, findings…) is lost.

use cartograph_core::{
    BodyType, Confidence, ContractField, Edge, EdgeKind, Graph, HttpBody, Node, NodeKind, Symbols,
    node_id,
};
use cartograph_query::{Config, Repo, explain::explain};

fn node(kind: NodeKind, path: &str) -> Node {
    Node {
        id: node_id(kind, path),
        kind,
        path: path.to_string(),
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        lang: (kind == NodeKind::File).then(|| "typescript".to_string()),
        loc: 1,
        bytes: 1,
        hash: None,
        meta: serde_json::Value::Null,
    }
}

fn ty(name: &str, file: &str, fields: usize) -> BodyType {
    BodyType {
        written: name.into(),
        name: Some(name.into()),
        file: Some(file.into()),
        line: 1,
        array: false,
        fields: Some(
            (0..fields)
                .map(|i| ContractField {
                    name: format!("{name}FieldNumber{i}"),
                    aliases: Vec::new(),
                    required: false,
                    optional: false,
                })
                .collect(),
        ),
    }
}

/// A DTO imported by 120 files in 30 folders and importing as many, the server type of 8
/// routes whose client types share none of its fields.
#[test]
fn contracts_never_cost_explain_what_it_showed_without_them() {
    let mut g = Graph::default();
    g.nodes.push(node(NodeKind::Repo, ""));
    g.nodes.push(node(NodeKind::Dir, "api"));
    g.nodes.push(node(NodeKind::Dir, "web"));
    g.nodes.push(node(NodeKind::File, "api/Dto.ts"));
    g.nodes.push(node(NodeKind::Dir, "lib"));
    for d in 0..30 {
        let dir = format!("web/folder{d}");
        let lib = format!("lib/shared{d}");
        g.nodes.push(node(NodeKind::Dir, &dir));
        g.nodes.push(node(NodeKind::Dir, &lib));
        for f in 0..4 {
            // Importers in `web/`, and files the DTO imports in `lib/`.
            let p = format!("{dir}/aFairlyLongImporterFileName{f}.ts");
            let q = format!("{lib}/aFairlyLongImportedFileName{f}.ts");
            g.nodes.push(node(NodeKind::File, &p));
            g.nodes.push(node(NodeKind::File, &q));
            g.edges.push(Edge::new(
                node_id(NodeKind::File, &p),
                node_id(NodeKind::File, "api/Dto.ts"),
                EdgeKind::Import,
                Confidence::Exact,
            ));
            g.edges.push(Edge::new(
                node_id(NodeKind::File, "api/Dto.ts"),
                node_id(NodeKind::File, &q),
                EdgeKind::Import,
                Confidence::Exact,
            ));
        }
    }
    let bodies: Vec<HttpBody> = (0..8)
        .map(|i| HttpBody {
            src_file: format!("web/folder{i}/aFairlyLongImporterFileName0.ts"),
            src_line: 3,
            dst_file: "api/Controller.java".into(),
            dst_line: 9,
            key: format!("POST /api/some/rather/long/route/number/{i}"),
            client: Some(ty(&format!("ClientPayloadType{i}"), "web/types.ts", 20)),
            server: Some(ty("ServerDto", "api/Dto.ts", 20)),
        })
        .collect();
    let plain = Repo::with_symbols(&g, &Symbols::default(), Config::default());
    let with = Repo::with_symbols(
        &g,
        &Symbols {
            bodies,
            ..Symbols::default()
        },
        Config::default(),
    );
    let before = explain(&plain, "api/Dto.ts").unwrap().text;
    let after = explain(&with, "api/Dto.ts").unwrap().text;
    assert!(after.contains("contracts (HTTP bodies"), "{after}");
    let lost: Vec<&str> = before.lines().filter(|l| !after.contains(l)).collect();
    assert!(lost.is_empty(), "lost {lost:#?}\n---\n{after}");
}
