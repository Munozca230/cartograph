package com.acme.shop.order.controller;

import com.acme.shop.order.dto.CreateOrderDto;
import com.acme.shop.order.service.OrderService;
import java.util.List;
import lombok.RequiredArgsConstructor;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("api/orders")
@RequiredArgsConstructor
public class OrderController {
    private final OrderService orderService;

    @PostMapping
    public ResponseEntity<String> create(@RequestBody CreateOrderDto dto) {
        return ResponseEntity.ok(orderService.create(dto));
    }

    @GetMapping("/{id}")
    public ResponseEntity<String> find(@PathVariable String id) {
        OrderService local = orderService;
        return ResponseEntity.ok(local.find(id));
    }

    @DeleteMapping
    public void cancelAll(@RequestBody List<String> ids) {
        // The lambda parameter shadows the field: its type is unknown.
        ids.forEach(orderService -> orderService.cancel(orderService));
    }

    @PostMapping("/{id}/audit")
    public void audit(@PathVariable String id, OrderReport orderService) {
        // The parameter shadows the field.
        orderService.print(id);
    }
}
