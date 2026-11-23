package com.shop.api.pricing;

import java.util.HashMap;
import java.util.List;
import java.util.Map;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.stereotype.Service;

@Service
public class PricingEngine {
    private final Map<String, Discount> byCode = new HashMap<>();

    // For tests: Spring calls the other one.
    public PricingEngine(List<PriceRule> rules, int unused) {
        this(rules);
    }

    @Autowired
    public PricingEngine(List<PriceRule> rules) {
        rules.stream()
                .flatMap(r -> r.discounts().stream())
                .forEach(d -> byCode.put(d.code(), d));
    }

    public Discount discount(String code) {
        return byCode.get(code);
    }
}
