#pragma once

class Shape {
public:
    double area() const;
    static Shape make();
    int sides() const { return count(); }

private:
    int count() const { return 4; }
};
