package com.acme.shop.order.dto;

import org.junit.jupiter.api.Test;

class CreateOrderDtoValidationTest {
    @Test
    void titleIsRequired() {
        CreateOrderDto dto = new CreateOrderDto();
        dto.setTitle(null);
    }
}
