use crate::model::Store;

pub struct Service {
    store: Store,
}

impl Service {
    pub fn run(&self) -> bool {
        self.store.save_all();
        self.store.to_string();
        Store::save_all(&self.store)
    }
}
