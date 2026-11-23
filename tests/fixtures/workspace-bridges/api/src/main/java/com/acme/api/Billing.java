package com.acme.api;

import com.acme.lib.Money;

public class Billing {
    public long total(Money m) {
        return m.cents();
    }
}
