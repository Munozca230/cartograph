package com.shop.pool;

public class Pooled {
    private Provider<Conn> pool;

    void done() {
        pool.get().releaseLease();
    }
}
