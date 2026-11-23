package com.acme.server.files;

import com.acme.core.Action;
import org.springframework.stereotype.Service;

@Service
public class FileServiceImpl implements FileService {
    @Override
    public String read(String path) {
        return Action.READ_FILE.key() + " " + path;
    }
}
