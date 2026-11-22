#include <algorithm>
#include "shape.hpp"

namespace app {
int run() {
    Shape s = Shape::make();
    return static_cast<int>(s.area());
}
}

int main() {
    std::max(1, 2);
    return app::run();
}
