#include "derived.hpp"

void Derived::run() {
    helper();
    this->helper();
    Base::run();
}
