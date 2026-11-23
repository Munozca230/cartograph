package com.shop.api.payment;

import org.springframework.context.annotation.Primary;
import org.springframework.stereotype.Service;

@Service
@Primary
public class CardGateway implements PaymentGateway {
    @Override
    public void charge(long cents) {
    }
}
