package com.acme.core;

/** Runs one document step by name. */
public class Runner {
    public String run(String step, String path) {
        switch (step) {
            case Steps.STEP_OPEN:
                Documents.open(path);
                break;
            case Steps.STEP_CLOSE:
                Documents.close(path);
                break;
            case Steps.STEP_SAVE:
                Documents.save(path);
                break;
            case Steps.STEP_PRINT:
                Documents.print(path);
                break;
            case Steps.STEP_SHARE:
            case Steps.STEP_EXPORT:
                Documents.share(path);
                Documents.export(path);
                break;
            case Steps.STEP_IMPORT:
                Documents.load(path);
                break;
            case Steps.STEP_ARCHIVE:
                Documents.archive(path);
                break;
            case Steps.STEP_RESTORE:
                Documents.restore(path);
                break;
            case Steps.STEP_LOCK:
                Documents.lock(path, true);
                break;
            case Steps.STEP_UNLOCK:
                Documents.lock(path, false);
                break;
            default:
                return Steps.STATE_FAILED;
        }
        return Steps.STATE_OK;
    }
}
