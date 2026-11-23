package com.shop.api.pricing;

import java.util.List;

// Implements the interface but is not a bean: Spring never injects it.
public class LegacyRule implements PriceRule {
    @Override
    public List<Discount> discounts() {
        return List.of();
    }
}
