package com.shop.api.order;

import com.shop.store.Order;
import lombok.RequiredArgsConstructor;
import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/orders")
@RequiredArgsConstructor
public class OrderController {
    private final OrderService orderService;

    @PostMapping
    public Order create(@RequestBody OrderDto dto) {
        return orderService.create(dto);
    }

    @GetMapping("/{id}")
    public Order get(@PathVariable String id) {
        return orderService.find(id);
    }
}
