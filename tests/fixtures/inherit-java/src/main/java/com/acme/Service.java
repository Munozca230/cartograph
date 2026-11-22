package com.acme;

import com.acme.core.Repo;
import java.util.ArrayList;

public class Service {
    private Repo repo;

    void run() {
        this.repo.saveEverything();
        this.repo.reload();
        this.repo.audit();
    }
}

class Names extends ArrayList<String> {
    void fill() {
        audit();
    }
}
