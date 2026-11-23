package com.acme.shop.order.service.impl;

import com.acme.shop.order.dto.CreateOrderDto;
import com.acme.shop.order.repository.OrderRepository;
import com.acme.shop.order.service.OrderService;
import lombok.RequiredArgsConstructor;
import org.springframework.stereotype.Service;

@Service
@RequiredArgsConstructor
public class OrderServiceImpl implements OrderService {
    private final OrderRepository orderRepository;
    // Same package as this class: no import.
    private final OrderValidator validator;

    @Override
    @Audited
    public String create(CreateOrderDto dto) {
        validator.check(dto);
        return orderRepository.insertOrder(dto.getTitle());
    }

    @Override
    public String find(String id) {
        return orderRepository.findTitle(id);
    }

    @Override
    public void cancel(String id) {
        // Inherited from MongoRepository, outside the repo.
        orderRepository.deleteById(id);
    }

    @Override
    public int purge() {
        return OrderValidator.LIMIT;
    }
}
