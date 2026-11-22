package com.acme.core;

public interface Store {
    void load();

    default void reload() {
        load();
    }
}
