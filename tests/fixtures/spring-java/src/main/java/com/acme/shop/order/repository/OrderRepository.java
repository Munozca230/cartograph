package com.acme.shop.order.repository;

import org.springframework.data.mongodb.repository.MongoRepository;

public interface OrderRepository extends MongoRepository<OrderDocument, String> {
    String insertOrder(String title);

    String findTitle(String id);
}
