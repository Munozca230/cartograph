package com.shop.store;

public class Counters {
    public static long bump(String name) {
        return Db.db().getCollection("counters").countDocuments();
    }
}
