package com.shop.cart;

import java.util.List;

public class Stock {
    private Cart cart;

    // The type of the elements is not visible from here: two classes have its name.
    void drop(List<Item> items) {
        items.forEach(i -> i.weighStock());
    }

    public void weighStock() {
    }
}
