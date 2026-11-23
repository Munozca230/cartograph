package com.shop.store;

import com.mongodb.client.MongoDatabase;

public final class Db {
    static MongoDatabase database;

    static MongoDatabase db() {
        return database;
    }
}
