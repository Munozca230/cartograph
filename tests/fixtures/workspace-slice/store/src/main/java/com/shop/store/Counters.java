package com.shop.store;

import com.mongodb.client.MongoDatabase;

public class Counters {
    public static long bump(String name) {
        MongoDatabase db = Db.db();
        return db.getCollection("counters").countDocuments();
    }
}
