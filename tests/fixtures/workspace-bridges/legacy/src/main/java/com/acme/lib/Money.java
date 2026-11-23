package com.acme.lib;

// The same class name as the library's: an import of com.acme.lib.Money from a member
// that declares the library (and not this one) means the library's.
public class Money {
    public long cents() {
        return 1;
    }
}
