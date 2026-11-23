package com.shop.cart;

import com.shop.left.*;
import java.util.List;

public class Box<T> {
    private List<T> all;
    private com.shop.left.Item one;

    void ship(List<com.shop.left.Item> some, List<Item> items) {
        items.forEach(i -> i.shipItem());
        some.forEach(i -> i.shipItem());
    }

    void each() {
        all.forEach(t -> t.weighCart());
    }
}
