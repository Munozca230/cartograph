package com.acme.shapes;

public class Outer {
    public static class Inner implements Shape {
        @Override
        public void draw() {}
    }
}
