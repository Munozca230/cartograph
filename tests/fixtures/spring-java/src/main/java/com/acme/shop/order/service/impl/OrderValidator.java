package com.acme.shop.order.service.impl;

import com.acme.shop.order.dto.CreateOrderDto;
import org.springframework.stereotype.Component;

@Component
class OrderValidator {
    static final int LIMIT = 100;

    void check(CreateOrderDto dto) {}

    static OrderValidator lenient() {
        return new OrderValidator();
    }

    static class Rule {}
}
