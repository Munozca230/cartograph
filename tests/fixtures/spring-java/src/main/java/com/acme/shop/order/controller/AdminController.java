package com.acme.shop.order.controller;

import com.acme.shop.order.service.OrderService;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class AdminController {
    private final OrderService service;

    // A class of the same package: no import.
    @Autowired
    private OrderReport report;

    public AdminController(OrderService orderService) {
        this.service = orderService;
        orderService.warmUp();
    }

    @PostMapping("/admin/purge")
    public int purge() {
        report.print("purge");
        return service.purge();
    }
}
