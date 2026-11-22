#pragma once
#include <vector>
#include "engine/fwd.hpp"

namespace engine {
template <typename T>
class Pool : public Base<T> {
public:
    std::vector<T> items;
    template <typename U> auto get() const -> const U& { return items[0]; }
};
}
