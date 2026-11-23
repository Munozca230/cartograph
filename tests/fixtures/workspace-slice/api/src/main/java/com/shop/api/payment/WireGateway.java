package com.shop.api.payment;

import org.springframework.stereotype.Service;

@Service("wire")
public class WireGateway implements PaymentGateway {
    @Override
    public void charge(long cents) {
    }
}
