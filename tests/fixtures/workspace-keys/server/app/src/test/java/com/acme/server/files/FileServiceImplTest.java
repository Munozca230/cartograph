package com.acme.server.files;

import org.junit.jupiter.api.Test;

class FileServiceImplTest {
    @Test
    void readsWithTheActionKey() {
        new FileServiceImpl().read("a.txt");
    }
}
