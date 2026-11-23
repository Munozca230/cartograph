package com.acme.server.files;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class FileController {
    private final FileServiceImpl files;

    public FileController(FileServiceImpl files) {
        this.files = files;
    }

    @GetMapping("/files")
    public String read(String path) {
        return files.read(path);
    }
}
