package com.shop.pool;

// The repo's own: what it gives is not its type argument.
public interface Provider<T> {
    Lease<T> get();
}
