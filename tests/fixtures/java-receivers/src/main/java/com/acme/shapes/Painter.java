package com.acme.shapes;

public class Painter {
    private Shape shape;
    private BaseShape base;

    void paint() {
        shape.draw();
        base.area();
    }
}
