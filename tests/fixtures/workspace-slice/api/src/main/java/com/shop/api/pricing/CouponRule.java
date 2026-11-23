package com.shop.api.pricing;

import java.util.List;
import org.springframework.stereotype.Component;

@Component("coupons")
public class CouponRule extends BaseRule {
    @Override
    public List<Discount> discounts() {
        return List.of();
    }
}
