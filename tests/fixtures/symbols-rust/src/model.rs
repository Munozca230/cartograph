use std::fmt;

pub const LIMIT: usize = 8;

pub struct Store {
    items: Vec<String>,
}

impl Store {
    pub fn new() -> Self {
        Self::init_log();
        Store { items: Vec::new() }
    }

    fn init_log() {}

    pub fn persist_all(&self) {
        self.flush_now();
    }

    fn flush_now(&self) {
        let _ = self.items.len();
    }
}

impl fmt::Display for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "store")
    }
}

pub fn shared() -> usize {
    LIMIT
}
