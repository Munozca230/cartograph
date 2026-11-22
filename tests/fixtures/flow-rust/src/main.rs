mod routes;
mod steps;
mod store;
#[cfg(test)]
mod tests;

use clap::Subcommand;

#[derive(Subcommand)]
enum Command {
    Index,
    Serve,
}

fn main() {
    match parse() {
        Command::Index => cmd_index(),
        Command::Serve => cmd_serve(),
    }
}

fn parse() -> Command {
    Command::Index
}

fn cmd_index() {
    steps::run_all();
    store::save();
    steps::countdown(3);
}

fn cmd_serve() {
    let _app = routes::router();
}
