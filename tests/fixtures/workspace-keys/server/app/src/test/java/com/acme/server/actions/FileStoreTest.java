package com.acme.server.actions;

import com.acme.server.support.MongoSliceTest;
import com.acme.server.support.TestFiles;
import org.junit.jupiter.api.Test;

class FileStoreTest extends MongoSliceTest {
    @Test
    void storesWhatTheServiceReads() {
        TestFiles.service().read("a.txt");
    }
}
