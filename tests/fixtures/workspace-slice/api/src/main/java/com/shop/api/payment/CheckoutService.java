package com.shop.api.payment;

import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.beans.factory.annotation.Qualifier;
import org.springframework.stereotype.Service;

@Service
public class CheckoutService {
    // Two gateways fit: the @Primary one.
    @Autowired
    private PaymentGateway gateway;

    @Autowired
    @Qualifier("wire")
    private PaymentGateway wireGateway;

    @Autowired
    private ReceiptRepository receipts;

    public void pay(long cents, boolean wire) {
        if (wire) {
            wireGateway.charge(cents);
        } else {
            gateway.charge(cents);
        }
        Receipt r = new Receipt();
        receipts.save(r);
    }
}
