package com.acme.server.files;

import org.junit.jupiter.api.Test;
import org.springframework.boot.test.context.SpringBootTest;

@SpringBootTest
class FileControllerIT {
    @Test
    void servesAFile() {
        new FileController(new FileServiceImpl()).read("a.txt");
    }
}
