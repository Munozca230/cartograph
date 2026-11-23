package com.shop.api.payment;

import org.springframework.data.mongodb.core.mapping.Document;

@Document(collection = "receipts")
public class Receipt {
    public long cents;
}
