package com.shop.api.pricing;

import java.util.List;
import org.springframework.stereotype.Component;

@Component
public class VolumeRule implements PriceRule {
    @Override
    public List<Discount> discounts() {
        return List.of(new Discount("BULK", 5));
    }
}
