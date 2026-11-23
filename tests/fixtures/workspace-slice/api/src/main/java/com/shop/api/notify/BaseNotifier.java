package com.shop.api.notify;

// Not a bean itself: the bean below inherits this method.
public abstract class BaseNotifier implements Notifier {
    @Override
    public void send(String text) {
    }
}
