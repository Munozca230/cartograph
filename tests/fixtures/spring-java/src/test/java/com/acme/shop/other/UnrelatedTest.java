package com.acme.shop.other;

import org.junit.jupiter.api.Test;

// Names a class of another package without importing it (it would not compile): no
// dependency, the same-package rule does not apply.
class UnrelatedTest {
    @Test
    void nothing() {
        OrderValidator validator = null;
    }
}
