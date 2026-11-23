package com.acme.server.files;

import org.junit.jupiter.api.Test;

class FilePathsTest {
    @Test
    void normalizes() {
        FilePaths.normalize("a\b");
    }
}
