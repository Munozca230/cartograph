package com.acme.scope;

import java.util.*;

public class Caller<T extends Runnable> {
    private Service service;
    private T task;

    void blockScoped(boolean b) {
        if (b) {
            Report service = new Report();
            service.print();
        }
        // The local above is out of scope here: the field.
        service.create();
    }

    void inner() {
        new Base() {
            void run() {
                // Base's own field, not the enclosing class's.
                service.print();
            }
        };
    }

    void jdk(Scanner sc) {
        // java.util.Scanner (wildcard import), not the repo's com.acme.other.Scanner.
        sc.nextLine();
    }

    void typeParameter() {
        task.run();
    }

    void nested(Builder b) {
        // The nested Caller.Builder, not the repo's com.acme.other.Builder.
        b.build();
    }

    static class Builder {
        void build() {}
    }
}
