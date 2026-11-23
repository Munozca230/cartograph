package com.acme.server.files;

/** Path helpers. */
public final class FilePaths {
    public static String normalize(String path) {
        return path.replace('\', '/');
    }

    private FilePaths() {}
}
