#include "shape.hpp"

double Shape::area() const {
    return sides() * 1.0;
}

Shape Shape::make() {
    return Shape();
}
