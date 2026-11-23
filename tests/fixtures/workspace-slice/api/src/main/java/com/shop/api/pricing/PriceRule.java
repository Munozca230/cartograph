package com.shop.api.pricing;

import java.util.List;

// Every pricing rule is a bean; Spring injects them all as a list.
public interface PriceRule {
    List<Discount> discounts();
}
