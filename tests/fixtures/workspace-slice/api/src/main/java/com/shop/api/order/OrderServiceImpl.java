package com.shop.api.order;

import com.shop.store.Order;
import com.shop.store.OrderStore;
import org.springframework.stereotype.Service;

@Service
public class OrderServiceImpl implements OrderService {
    @Override
    public Order create(OrderDto dto) {
        check(dto);
        Order order = new Order();
        return persist(order);
    }

    @Override
    public Order find(String id) {
        return OrderStore.byId(id);
    }

    private void check(OrderDto dto) {
        OrderStore.next("orders");
    }

    private Order persist(Order order) {
        return OrderStore.save(order);
    }
}
