package com.shop.api.pricing;

import java.util.ArrayList;
import java.util.List;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.stereotype.Component;

@Component
public class DiscountBoard {
    @Autowired
    private List<PriceRule> rules;

    public List<Discount> all() {
        List<Discount> out = new ArrayList<>();
        rules.forEach(r -> out.addAll(r.discounts()));
        return out;
    }
}
