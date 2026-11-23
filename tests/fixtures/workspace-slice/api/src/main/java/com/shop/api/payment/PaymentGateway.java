package com.shop.api.payment;

public interface PaymentGateway {
    void charge(long cents);
}
