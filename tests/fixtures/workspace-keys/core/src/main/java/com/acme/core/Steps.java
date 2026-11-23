package com.acme.core;

/** Names of the document steps, and the states a step ends in. */
public final class Steps {
    public static final String STEP_OPEN = "OPEN";
    public static final String STEP_CLOSE = "CLOSE";
    public static final String STEP_SAVE = "SAVE";
    public static final String STEP_PRINT = "PRINT";
    public static final String STEP_SHARE = "SHARE";
    public static final String STEP_EXPORT = "EXPORT";
    public static final String STEP_IMPORT = "IMPORT";
    public static final String STEP_ARCHIVE = "ARCHIVE";
    public static final String STEP_RESTORE = "RESTORE";
    public static final String STEP_LOCK = "LOCK";
    public static final String STEP_UNLOCK = "UNLOCK";

    public static final String STATE_OK = "ok";
    public static final String STATE_FAILED = "failed";

    private Steps() {}
}
