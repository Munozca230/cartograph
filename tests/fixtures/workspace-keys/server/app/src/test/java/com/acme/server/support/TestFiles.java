package com.acme.server.support;

import com.acme.server.files.FileServiceImpl;

/** Builds services for tests; holds no test. */
public final class TestFiles {
    public static FileServiceImpl service() {
        return new FileServiceImpl();
    }

    private TestFiles() {}
}
