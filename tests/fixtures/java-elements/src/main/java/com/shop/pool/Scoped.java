package com.shop.pool;

import java.util.List;

public class Scoped {
    private List<Out.Entry<Conn>> entries;

    void all() {
        entries.forEach(e -> e.flushEntry());
    }
}
