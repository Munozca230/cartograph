package com.shop.store;

import com.mongodb.client.MongoCollection;
import com.mongodb.client.model.Filters;

// Static access to the orders collection, named by a constant.
public class OrderStore {
    private static final String ORDERS = "orders";

    public static Order save(Order order) {
        MongoCollection<Order> coll = Db.db().getCollection(ORDERS, Order.class);
        coll.insertOne(order);
        return order;
    }

    public static Order byId(String id) {
        return Db.db().getCollection(ORDERS, Order.class).find(Filters.eq("_id", id)).first();
    }

    // Not a collection: a counter keyed by a name.
    public static long next(String name) {
        return Counters.bump(name);
    }
}
