#pragma once
#include <memory>
#include "engine/pool.h"

namespace engine {
class Engine {
public:
    void tick();
private:
    std::unique_ptr<Pool<int>> pool_;
};
}
