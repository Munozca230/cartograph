use crate::persist::{Named, Persist};
use std::fmt;

pub struct Store;

impl Named for Store {
    fn name(&self) -> String {
        String::new()
    }
}

impl Persist for Store {
    fn key(&self) -> String {
        self.label()
    }
}

impl fmt::Display for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl Store {
    pub fn flush(&self) -> bool {
        self.save_all()
    }
}
