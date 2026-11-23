package com.acme.shop.order.dto;

import jakarta.validation.constraints.NotBlank;
import lombok.Data;

@Data
public class CreateOrderDto {
    @NotBlank
    private String title;

    private OrderLine line;
}
