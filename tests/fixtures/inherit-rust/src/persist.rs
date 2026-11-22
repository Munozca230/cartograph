pub trait Named {
    fn name(&self) -> String;

    fn label(&self) -> String {
        self.name()
    }
}

pub trait Persist: Named {
    fn key(&self) -> String;

    fn save_all(&self) -> bool {
        self.label();
        true
    }
}
