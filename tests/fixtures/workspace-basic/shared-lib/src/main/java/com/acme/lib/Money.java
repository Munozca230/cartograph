package com.acme.lib;

public final class Money {
    private final long cents;

    public Money(long cents) {
        this.cents = cents;
    }

    public long cents() {
        return cents;
    }
}
