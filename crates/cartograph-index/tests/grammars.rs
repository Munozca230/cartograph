//! Grammar spike: every bundled grammar loads into ONE tree-sitter core and parses an
//! idiomatic snippet without ERROR or MISSING nodes.

use cartograph_index::grammars::{Grammar, parse, syntax_problems};

fn snippet(g: Grammar) -> &'static str {
    match g {
        Grammar::TypeScript => {
            r#"import { readFile } from "node:fs/promises";
export interface User { id: number; name?: string }
type Id<T> = T extends { id: infer I } ? I : never;
export class Repo<T extends User> {
  private items = new Map<number, T>();
  constructor(private readonly prefix: string) {}
  async load(path: string): Promise<T[]> {
    const raw = await readFile(path, "utf8");
    return (JSON.parse(raw) as T[]).filter((u) => u.id > 0);
  }
}
export enum Color { Red = "red", Blue = "blue" }
"#
        }
        Grammar::Tsx => {
            r#"import React, { useState } from "react";
type Props = { title: string; items: string[] };
export function List({ title, items }: Props): JSX.Element {
  const [open, setOpen] = useState<boolean>(false);
  return (
    <section className="list">
      <h2 onClick={() => setOpen(!open)}>{title}</h2>
      {open && <ul>{items.map((i) => <li key={i}>{i}</li>)}</ul>}
    </section>
  );
}
"#
        }
        Grammar::JavaScript => {
            r#"import path from "node:path";
const { a, ...rest } = require("./config.cjs");
export default async function main(argv = process.argv.slice(2)) {
  for await (const line of readLines(argv[0] ?? "-")) {
    console.log(`${path.basename(line)}: ${rest?.x ?? a}`);
  }
}
class Box { #v; constructor(v) { this.#v = v; } get value() { return this.#v; } }
const el = <div className="x">{new Box(1).value}</div>;
"#
        }
        Grammar::Python => {
            r#"from __future__ import annotations
import asyncio
from dataclasses import dataclass, field

@dataclass
class Item:
    name: str
    tags: list[str] = field(default_factory=list)

    def label(self) -> str:
        return f"{self.name} ({len(self.tags)})"

async def fetch(items: list[Item]) -> dict[str, int]:
    match items:
        case []:
            return {}
        case [first, *rest]:
            await asyncio.sleep(0)
            return {i.name: len(i.tags) for i in [first, *rest] if i.tags}
"#
        }
        Grammar::Rust => {
            r#"use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct Cache<K, V> { map: HashMap<K, V> }

impl<K: std::hash::Hash + Eq, V: Clone> Cache<K, V> {
    pub fn get_or<F: FnOnce() -> V>(&mut self, k: K, f: F) -> V {
        self.map.entry(k).or_insert_with(f).clone()
    }
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let Some(first) = args.first() else { return Ok(()) };
    println!("{first:?}");
    Ok(())
}
"#
        }
        Grammar::CSharp => {
            r#"using System;
using System.Collections.Generic;
using System.Threading.Tasks;

namespace Demo.Core;

public record Point(int X, int Y);

public sealed class Service : IDisposable
{
    private readonly Dictionary<string, int> _counts = new();
    public int this[string key] => _counts.TryGetValue(key, out var v) ? v : 0;

    public async Task<int> RunAsync(IEnumerable<string> keys)
    {
        foreach (var k in keys) { _counts[k] = this[k] + 1; }
        await Task.Yield();
        return _counts.Count is > 0 and < 100 ? _counts.Count : -1;
    }

    public void Dispose() => _counts.Clear();
}
"#
        }
        Grammar::Go => {
            r#"package store

import (
	"context"
	"fmt"
)

type Store[T any] struct {
	items map[string]T
}

func New[T any]() *Store[T] { return &Store[T]{items: map[string]T{}} }

func (s *Store[T]) Get(ctx context.Context, k string) (T, error) {
	select {
	case <-ctx.Done():
		var zero T
		return zero, ctx.Err()
	default:
	}
	v, ok := s.items[k]
	if !ok {
		return v, fmt.Errorf("missing %q", k)
	}
	return v, nil
}
"#
        }
        Grammar::Java => {
            r#"package com.example.demo;

import java.util.List;
import java.util.stream.Collectors;

record User(long id, String name) {}

@FunctionalInterface
interface Mapper<A, B> { B map(A a); }

public class UserService {
    private final List<User> users;

    public UserService(List<User> users) { this.users = users; }

    public List<String> names() {
        return users.stream().filter(u -> u.id() > 0).map(User::name).collect(Collectors.toList());
    }

    public String describe(Object o) {
        return switch (o) {
            case User u -> "user " + u.name();
            default -> "other";
        };
    }
}
"#
        }
        Grammar::C => {
            r#"#include <stdio.h>
#include "util.h"

#define MAX(a, b) ((a) > (b) ? (a) : (b))

typedef struct node {
    int value;
    struct node *next;
} node_t;

static int sum(const node_t *n) {
    int total = 0;
    for (; n != NULL; n = n->next) total += n->value;
    return total;
}

int main(int argc, char **argv) {
    node_t b = { .value = 2, .next = NULL }, a = { 1, &b };
    printf("%d %d\n", sum(&a), MAX(argc, 1));
    return 0;
}
"#
        }
        Grammar::Cpp => {
            r#"#include <memory>
#include <string>
#include <vector>

namespace demo {

template <typename T>
class Pool {
public:
    explicit Pool(std::size_t n) : items_(n) {}
    T& at(std::size_t i) { return items_.at(i); }
    auto size() const noexcept -> std::size_t { return items_.size(); }
private:
    std::vector<T> items_;
};

struct Node : public std::enable_shared_from_this<Node> {
    virtual ~Node() = default;
    virtual std::string name() const = 0;
};

}  // namespace demo

int main() {
    auto pool = std::make_unique<demo::Pool<int>>(4);
    for (auto& [i, v] : std::vector<std::pair<int, int>>{{1, 2}}) { pool->at(0) = i + v; }
    return static_cast<int>(pool->size());
}
"#
        }
        Grammar::GdScript => {
            "class_name Player
extends CharacterBody2D

signal died(reason: String)

const SPEED := 200.0
@export var max_health: int = 100
@onready var sprite: Sprite2D = $Sprite2D

var health := max_health:
\tset(value):
\t\thealth = clamp(value, 0, max_health)
\t\tif health == 0:
\t\t\tdied.emit(\"hp\")

func _physics_process(delta: float) -> void:
\tvar dir := Input.get_vector(\"left\", \"right\", \"up\", \"down\")
\tvelocity = dir * SPEED
\tmove_and_slide()

func take_damage(amount: int) -> void:
\tfor i in range(amount):
\t\thealth -= 1
\tawait get_tree().create_timer(0.1).timeout
"
        }
    }
}

#[test]
fn every_grammar_loads_into_the_same_core() {
    for g in Grammar::ALL {
        let abi = g.abi_version();
        assert!(
            (tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION)
                .contains(&abi),
            "{}: ABI {abi} outside {}..={}",
            g.name(),
            tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION,
            tree_sitter::LANGUAGE_VERSION
        );
        g.parser()
            .unwrap_or_else(|e| panic!("{}: set_language failed: {e}", g.name()));
    }
}

#[test]
fn every_grammar_parses_its_snippet_cleanly() {
    let mut failures = Vec::new();
    for g in Grammar::ALL {
        let tree = parse(g, snippet(g).as_bytes()).expect("parse");
        let problems = syntax_problems(&tree);
        if !problems.is_empty() || tree.root_node().has_error() {
            failures.push(format!("{}: {problems:?}", g.name()));
        }
    }
    assert!(
        failures.is_empty(),
        "syntax problems:\n{}",
        failures.join("\n")
    );
}

/// `syntax_problems` must not recurse: a 50k-deep left-nested expression would overflow
/// the test thread's stack.
#[test]
fn deep_trees_do_not_overflow_the_stack() {
    let mut src = String::from("x = 1");
    for _ in 0..50_000 {
        src.push_str(" + 1");
    }
    src.push_str(";\n");
    let tree = parse(Grammar::JavaScript, src.as_bytes()).unwrap();
    assert!(syntax_problems(&tree).is_empty());
    src.push_str("let = ;\n");
    let tree = parse(Grammar::JavaScript, src.as_bytes()).unwrap();
    assert!(!syntax_problems(&tree).is_empty());
}

#[test]
fn broken_snippets_are_detected() {
    // Guards against a vacuous pass: the checker must see ERROR and MISSING nodes.
    let tree = parse(Grammar::Python, b"def f(:\n    return )\n").unwrap();
    assert!(!syntax_problems(&tree).is_empty());
    let tree = parse(Grammar::Rust, b"fn main() { let x = 1 }").unwrap();
    let problems = syntax_problems(&tree);
    assert!(problems.iter().any(|p| p.missing), "{problems:?}");
    let tree = parse(Grammar::GdScript, b"func f(:\n\tpass\n").unwrap();
    assert!(!syntax_problems(&tree).is_empty());
}

#[test]
fn print_abi_versions() {
    // Run with `--nocapture` to regenerate the ABI column of docs/spike-grammars.md.
    for g in Grammar::ALL {
        println!("{:<11} ABI {}", g.name(), g.abi_version());
    }
}
