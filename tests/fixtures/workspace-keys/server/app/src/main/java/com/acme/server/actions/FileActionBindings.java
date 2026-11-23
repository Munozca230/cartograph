package com.acme.server.actions;

import com.acme.core.Action;
import java.util.List;
import org.springframework.stereotype.Component;

/** Binds the file actions to their code (zipping a folder is not supported yet). */
@Component
public class FileActionBindings {
    public List<ActionBinding> bindings() {
        return List.of(
                new ActionBinding(Action.READ_FILE, FileOps::read),
                new ActionBinding(Action.WRITE_FILE, FileOps::write),
                new ActionBinding(Action.COPY_FILE, FileOps::copy),
                new ActionBinding(Action.MOVE_FILE, FileOps::move),
                new ActionBinding(Action.DELETE_FILE, FileOps::delete),
                new ActionBinding(Action.RENAME_FILE, FileOps::rename),
                new ActionBinding(Action.TOUCH_FILE, FileOps::touch),
                new ActionBinding(Action.LIST_DIR, FileOps::list),
                new ActionBinding(Action.MAKE_DIR, FileOps::mkdir),
                new ActionBinding(Action.REMOVE_DIR, FileOps::rmdir),
                new ActionBinding(Action.UNZIP_DIR, FileOps::unzip));
    }
}
