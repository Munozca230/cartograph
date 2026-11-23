package com.shop.sum;

public class Sib extends Base {
    // The second call has no known receiver: it may run the bean's.
    public int totalSum() {
        return super.totalSum() + Reg.next().totalSum();
    }
}
