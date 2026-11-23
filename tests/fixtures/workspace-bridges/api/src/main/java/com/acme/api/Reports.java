package com.acme.api;

import com.acme.legacy.OldPrices;

public class Reports {
    public String run() {
        return new OldPrices().list();
    }
}
