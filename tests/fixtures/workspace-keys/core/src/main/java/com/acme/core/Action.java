package com.acme.core;

/** The file actions a step can run, by key. */
public enum Action {
    READ_FILE("READ-FILE"),
    WRITE_FILE("WRITE-FILE"),
    COPY_FILE("COPY-FILE"),
    MOVE_FILE("MOVE-FILE"),
    DELETE_FILE("DELETE-FILE"),
    RENAME_FILE("RENAME-FILE"),
    TOUCH_FILE("TOUCH-FILE"),
    LIST_DIR("LIST-DIR"),
    MAKE_DIR("MAKE-DIR"),
    REMOVE_DIR("REMOVE-DIR"),
    ZIP_DIR("ZIP-DIR"),
    UNZIP_DIR("UNZIP-DIR");

    private final String key;

    Action(String key) {
        this.key = key;
    }

    public String key() {
        return key;
    }
}
