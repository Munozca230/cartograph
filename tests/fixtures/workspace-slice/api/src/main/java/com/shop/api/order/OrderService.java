package com.shop.api.order;

import com.shop.store.Order;

public interface OrderService {
    Order create(OrderDto dto);

    Order find(String id);
}
