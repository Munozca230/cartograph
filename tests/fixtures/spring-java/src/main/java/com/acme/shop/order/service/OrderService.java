package com.acme.shop.order.service;

import com.acme.shop.order.dto.CreateOrderDto;

public interface OrderService {
    String create(CreateOrderDto dto);

    String find(String id);

    void cancel(String id);

    int purge();

    default void warmUp() {}
}
