package com.acme.core;

import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.*;
import com.acme.core.Model.Inner;
import com.acme.core.Model.Outer.Deep;
import static com.acme.core.Model.MAX;
import static com.acme.core.Model.*;
import com.acme.core.util.*;
import com.acme.core.Fixtures;

class ModelTest {
    @Test void max() { assertEquals(10, MAX); new Inner(); new Deep(); Fixtures.make(); Strings.trim(""); }
}
