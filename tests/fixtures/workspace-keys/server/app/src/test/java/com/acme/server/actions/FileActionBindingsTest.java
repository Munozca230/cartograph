package com.acme.server.actions;

import org.junit.jupiter.api.Test;

class FileActionBindingsTest {
    @Test
    void bindsTheFileActions() {
        new FileActionBindings().bindings();
    }
}
