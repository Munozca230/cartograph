mod model;
mod routes;
mod util;

use clap::Subcommand;
use model::Store;

#[derive(Subcommand)]
enum Command {
    Index,
    ServeAll,
}

fn main() {
    let store = Store::new();
    store.persist_all();
    helper();
    util::format_name("x");
    let _router = routes::router();
    let _ = std::env::args();
}

fn helper() {}
