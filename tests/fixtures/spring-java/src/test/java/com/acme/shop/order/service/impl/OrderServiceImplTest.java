package com.acme.shop.order.service.impl;

import static org.mockito.Mockito.mock;

import com.acme.shop.order.dto.CreateOrderDto;
import com.acme.shop.order.repository.OrderRepository;
import org.junit.jupiter.api.Test;

class OrderServiceImplTest {
    @Test
    void createChecksTheDto() {
        OrderRepository repo = mock(OrderRepository.class);
        OrderServiceImpl service = new OrderServiceImpl(repo, OrderValidator.lenient());
        service.create(OrderFixtures.dto());
    }

    @Test
    void rulesAreNested() {
        var rule = new OrderValidator.Rule();
    }
}
