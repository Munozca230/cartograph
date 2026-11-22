mod sqlite;
mod inline_mod {
    pub mod deeper;
}

use self::sqlite::Conn;
use self::inline_mod::deeper::Deep;

pub struct Store;
