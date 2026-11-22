package com.acme.core;

public class Repo extends BaseRepo implements Store {
    @Override
    public void load() {
        audit();
        this.audit();
        reload();
    }

    @Override
    public void saveEverything() {
        super.saveEverything();
    }
}
